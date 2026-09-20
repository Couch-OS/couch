use super::{parse, Api, Reply};
use couch_model::{Id, Provider};
use couch_plugin::{Error, Failure, LocalRequest, Request, Response};
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
                ..
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

    fn plugin_request(&self, connection: &str, request: Request) -> Result<Response, Failure> {
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
                "GET" => self
                    .plugins
                    .settings(connection, &id)
                    .map_err(crate::plugins::Refusal::from),
                "POST" | "PUT" => {
                    let value = match parse(body) {
                        Ok(value) => value,
                        Err(reply) => return reply,
                    };
                    self.plugins.save_settings(connection, &id, value)
                }
                _ => return Reply::error(405, "Use GET or POST for integration settings"),
            };
            return match result {
                Ok(value) => Reply::json(200, &value),
                Err(refusal) => match &refusal.failure {
                    Some(failure) => refused(400, failure),
                    None => Reply::error(400, refusal.text),
                },
            };
        }
        let request = match (method, path) {
            ("GET", ["status"]) => Request::status(),
            ("GET", ["inputs"]) => Request::Inputs,
            ("POST", ["typed-action"]) => {
                let action: couch_model::TypedAction = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::action(action)
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
                Request::command(input.command)
            }
            _ => return Reply::error(404, "Unknown integration operation"),
        };
        match self.plugin_request(connection, request) {
            Ok(Response::Ok) => Reply::json(200, &json!({"accepted":true})),
            Ok(Response::Status { status }) => Reply::json(200, &status),
            Ok(Response::Inputs { inputs }) => Reply::json(200, &inputs),
            Ok(_) => Reply::error(502, "Invalid integration reply"),
            Err(failure) => refused(status_for(failure.code), &failure),
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
                            relay(&mut stream, |request| {
                                api.plugin_request(&request.connection_id, request.request)
                            });
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

/// The HTTP status for a refused integration request. `unpaired` is a conflict
/// with the state of the device, which only pairing again resolves; it is not
/// a bad gateway.
fn status_for(code: Error) -> u16 {
    match code {
        Error::Invalid | Error::Unsupported => 400,
        Error::Unpaired => 409,
        Error::Busy | Error::Expired => 503,
        _ => 502,
    }
}

/// `error` is the sentence every client already shows. `code` and, from a
/// protocol 3 package, `reason` sit beside it for a client that can do better:
/// mark the setting a reason names, or offer pairing for `unpaired`.
fn refused(status: u16, failure: &Failure) -> Reply {
    let mut body = json!({"error": failure.to_string(), "code": failure.code});
    if let Some(reason) = &failure.reason {
        body["reason"] = json!(reason);
    }
    Reply::json(status, &body)
}

/// One request from the panel, answered on its own stream. A refusal goes back
/// whole: the code and, when a protocol 3 package gave one, its reason.
fn relay(stream: &mut UnixStream, execute: impl FnOnce(LocalRequest) -> Result<Response, Failure>) {
    let response =
        match couch_plugin::read_frame_timeout::<LocalRequest>(stream, Duration::from_secs(2)) {
            Ok(request) => execute(request).unwrap_or_else(Response::error),
            Err(code) => Response::error(code),
        };
    let _ = couch_plugin::write_frame_timeout(stream, &response, Duration::from_secs(2));
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

#[cfg(test)]
mod tests {
    use super::*;
    use couch_plugin::Reason;
    use serde_json::Value;

    fn body(reply: &Reply) -> Value {
        serde_json::from_slice(&reply.body).unwrap()
    }

    #[test]
    fn a_refusal_over_http_keeps_its_sentence_and_gains_a_code_and_a_reason() {
        // Every code a package can send today: the status and the sentence are
        // the ones they were, and `code` is new beside them.
        for (code, status, word) in [
            (Error::Invalid, 400, "invalid"),
            (Error::Unsupported, 400, "unsupported"),
            (Error::Busy, 503, "busy"),
            (Error::Expired, 503, "expired"),
            (Error::Incompatible, 502, "incompatible"),
            (Error::Protocol, 502, "protocol"),
            (Error::Transport, 502, "transport"),
            (Error::Timeout, 502, "timeout"),
            (Error::Rejected, 502, "rejected"),
        ] {
            let reply = refused(status_for(code), &code.into());
            assert_eq!(reply.status, status, "{word}");
            assert_eq!(
                body(&reply),
                json!({"error": code.to_string(), "code": word}),
                "{word}"
            );
        }
        // Protocol 3, which no shipped build lets a package speak.
        let unpaired = Failure {
            code: Error::Unpaired,
            reason: Some(Reason::Message {
                text: "Pair this TV again".into(),
            }),
        };
        let reply = refused(status_for(unpaired.code), &unpaired);
        assert_eq!(reply.status, 409);
        assert_eq!(
            body(&reply),
            json!({"error":"Pair this TV again","code":"unpaired",
                "reason":{"kind":"message","text":"Pair this TV again"}})
        );
        let port = Failure {
            code: Error::Invalid,
            reason: Some(Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }),
        };
        assert_eq!(
            body(&refused(400, &port)),
            json!({"error":"The port must not be 0","code":"invalid",
                "reason":{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}})
        );
    }

    #[test]
    fn the_panel_is_told_the_reason_and_a_refusal_without_one_is_the_frame_it_always_was() {
        let directory = std::env::temp_dir().join(format!("couch-relay-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("p.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let locked = Failure {
            code: Error::Rejected,
            reason: Some(Reason::Message {
                text: "The TV is locked".into(),
            }),
        };
        let answers = [Err(locked.clone()), Err(Error::Unsupported.into())];
        let server = std::thread::spawn(move || {
            // Answered streams stay open until the end: macOS refuses to set a
            // deadline on a socket whose peer has already gone, which the
            // asking side does before it reads.
            let mut answered = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                relay(&mut stream, |request| {
                    assert_eq!(request.connection_id, "tv");
                    // How the key was pressed arrives with it; the host, not
                    // this socket, decides whether the package is told.
                    assert!(matches!(
                        request.request,
                        Request::Command { ref function, phase: couch_plugin::KeyPhase::Repeat, .. }
                            if function == "power-off"
                    ));
                    answer
                });
                answered.push(stream);
            }
            // The last one by hand, to see the bytes the panel is sent.
            let (mut stream, _) = listener.accept().unwrap();
            relay(&mut stream, |_| Err(Error::Unsupported.into()));
        });
        let ask = || {
            couch_plugin::local_request_detailed(
                &socket,
                "tv",
                Request::key("power-off", couch_plugin::KeyPhase::Repeat),
                Duration::from_secs(2),
            )
        };
        assert_eq!(ask(), Err(locked));
        assert_eq!(ask(), Err(Error::Unsupported.into()));
        let mut stream = UnixStream::connect(&socket).unwrap();
        couch_plugin::write_frame(
            &mut stream,
            &LocalRequest {
                connection_id: "tv".into(),
                request: Request::command("power-off"),
            },
        )
        .unwrap();
        let frame: Value = couch_plugin::read_frame(&mut stream).unwrap();
        assert_eq!(
            frame.to_string(),
            r#"{"code":"unsupported","type":"error"}"#
        );
        server.join().unwrap();
        let _ = fs::remove_dir_all(directory);
    }
}
