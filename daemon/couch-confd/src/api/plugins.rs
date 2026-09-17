use super::{parse, Api, Reply};
use couch_model::{Id, Provider};
use couch_plugin::{Error, LocalRequest, Request, Response};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

impl Api {
    pub(super) fn refresh_plugin_metadata(&self) {
        let ids = self.with(|s| {
            s.config()
                .connections
                .iter()
                .filter_map(|c| match &c.provider {
                    Provider::Plugin { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let manifests = self.plugins.changed_manifests(&ids);
        if manifests.is_empty() {
            return;
        }
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = store.config().clone();
        for connection in &mut next.connections {
            if let Provider::Plugin {
                id,
                label,
                capabilities,
                supports_inputs,
                presentation,
                actions,
            } = &mut connection.provider
            {
                let Some(manifest) = manifests.iter().find(|manifest| &manifest.id == id) else {
                    continue;
                };
                *label = manifest.label.clone();
                // Preserve old bindings when a package removes a capability.
                // The live manifest, not this display cache, gates execution.
                for capability in &manifest.capabilities {
                    if let Some(old) = capabilities.iter_mut().find(|old| old.id == capability.id) {
                        old.label = capability.label.clone();
                    } else {
                        capabilities.push(couch_model::PluginCapability {
                            id: capability.id.clone(),
                            label: capability.label.clone(),
                        });
                    }
                }
                *supports_inputs |= manifest.supports_inputs;
                *presentation = manifest.presentation.clone();
                *actions = manifest.actions.clone();
            }
        }
        if next.connections != store.config().connections {
            let _ = store.mutate(None, |cfg| cfg.connections = next.connections);
        }
    }
    pub(super) fn integration_route(&self, method: &str, path: &[&str], body: &[u8]) -> Reply {
        match (method, path) {
            ("GET", []) => match self.plugins.catalog() {
                Ok(integrations) => Reply::json(200, &json!({"integrations":integrations})),
                Err(_) => Reply::error(503, "Cannot read the installed integration catalog"),
            },
            _ => self.integration_package_route(method, path, body),
        }
    }

    fn plugin_id(&self, connection: &str) -> Option<String> {
        self.with(
            |s| match &s.config().connection(&Id::new(connection))?.provider {
                Provider::Plugin { id, .. } => Some(id.clone()),
                _ => None,
            },
        )
    }

    fn plugin_request(&self, connection: &str, request: Request) -> Result<Response, Error> {
        let id = self.plugin_id(connection).ok_or(Error::Invalid)?;
        self.plugins.execute(connection, &id, request)
    }

    pub(super) fn plugin_route(
        &self,
        method: &str,
        connection: &str,
        path: &[&str],
        body: &[u8],
    ) -> Reply {
        let Some(id) = self.plugin_id(connection) else {
            return Reply::error(404, "Integration connection not found");
        };
        if path == ["settings"] {
            let result = match method {
                "GET" => self.plugins.settings(connection, &id),
                "POST" | "PUT" => {
                    let value = match parse(body) {
                        Ok(value) => value,
                        Err(reply) => return reply,
                    };
                    if id == "denon" {
                        // Serialize target edits with migration preflight and
                        // its commit, including edits to other Denon packages.
                        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
                        if store
                            .config()
                            .migrated_denon(&Id::new(connection))
                            .is_some()
                        {
                            return Reply::error(409, "Restore built-in Denon before changing a migrated receiver's settings");
                        }
                        if !store.config().connection(&Id::new(connection)).is_some_and(
                            |c| matches!(&c.provider, Provider::Plugin { id, .. } if id == "denon"),
                        ) {
                            return Reply::error(
                                409,
                                "Denon connection changed; refresh before saving settings",
                            );
                        }
                        self.plugins.save_denon_settings(
                            connection,
                            value,
                            crate::plugins::protected_denon_targets(store.config()),
                        )
                    } else {
                        self.plugins.save_settings(connection, &id, value)
                    }
                }
                _ => return Reply::error(405, "Use GET or POST for integration settings"),
            };
            return match result {
                Ok(value) => Reply::json(200, &value),
                Err(message) => Reply::error(400, message),
            };
        }
        let request = match (method, path) {
            ("GET", ["status"]) => Request::Status,
            ("GET", ["inputs"]) => Request::Inputs,
            ("POST", ["typed-action"]) => {
                let action: couch_model::TypedAction = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::Action { action }
            }
            ("POST", ["action"]) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Action {
                    command: String,
                }
                let input: Action = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::Command {
                    function: input.command,
                }
            }
            _ => return Reply::error(404, "Unknown integration operation"),
        };
        match self.plugin_request(connection, request) {
            Ok(Response::Ok) => Reply::json(200, &json!({"accepted":true})),
            Ok(Response::Status { status }) => Reply::json(200, &status),
            Ok(Response::Inputs { inputs }) => Reply::json(200, &inputs),
            Ok(_) => Reply::error(502, "Invalid integration reply"),
            Err(error) => Reply::error(
                match error {
                    Error::Invalid | Error::Unsupported => 400,
                    Error::Busy | Error::Expired => 503,
                    _ => 502,
                },
                error.to_string(),
            ),
        }
    }

    /// Same-uid, owner-only socket. GUI commands pass through the exact same
    /// endpoint registry and capability gate as authenticated HTTP commands.
    pub fn serve_plugins(self: &Arc<Self>) -> std::io::Result<()> {
        let path = self.with(|s| s.path().with_file_name("plugin.sock"));
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            if UnixStream::connect(&path).is_ok() {
                return Err(std::io::ErrorKind::AddrInUse.into());
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(
            self.with(|s| s.path().with_file_name("plugin.sock")),
            fs::Permissions::from_mode(0o600),
        )?;
        let api = self.clone();
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("integration-socket".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    if !same_uid(&stream) || active.load(Ordering::SeqCst) >= 8 {
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let api = api.clone();
                    let count = active.clone();
                    count.fetch_add(1, Ordering::SeqCst);
                    let spawned = std::thread::Builder::new()
                        .name("integration-request".into())
                        .spawn(move || {
                            let response = match couch_plugin::read_frame_timeout::<LocalRequest>(
                                &mut stream,
                                Duration::from_secs(2),
                            ) {
                                Ok(request) => api
                                    .plugin_request(&request.connection_id, request.request)
                                    .unwrap_or_else(|code| Response::Error { code }),
                                Err(code) => Response::Error { code },
                            };
                            let _ = couch_plugin::write_frame_timeout(
                                &mut stream,
                                &response,
                                Duration::from_secs(2),
                            );
                            count.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })?;
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("integration-reaper".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(10));
                let Some(api) = weak.upgrade() else { break };
                api.plugins.reap();
            })?;
        Ok(())
    }
}

fn same_uid(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    unsafe {
        let mut peer: libc::ucred = std::mem::zeroed();
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut length,
        ) == 0
            && peer.uid == libc::geteuid()
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    unsafe {
        let mut uid = 0;
        let mut gid = 0;
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = stream;
        false
    }
}
