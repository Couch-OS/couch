use super::{parse, Api, Reply};
use couch_model::{Connection, Id, Provider};
use serde::Deserialize;
#[derive(Deserialize)]
struct Setup {
    name: String,
    provider: Provider,
}
impl Api {
    pub(super) fn connection_route(
        &self,
        method: &str,
        path: &[&str],
        body: &[u8],
        revision: Option<u64>,
    ) -> Reply {
        if let [id, "androidtv", "apps"] = path {
            let id = Id::new(*id);
            if !self.with(|s| {
                s.config()
                    .connection(&id)
                    .is_some_and(|c| c.provider == Provider::AndroidTv)
            }) {
                return Reply::error(404, "Android TV connection not found");
            }
            return match method {
                "GET" => self.with(|s| {
                    Reply::json(
                        200,
                        &s.config()
                            .app_shortcuts
                            .get(&id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                }),
                "PUT" => {
                    let apps: Vec<couch_model::AppShortcut> = match parse(body) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                    self.edit_found(revision, move |c| {
                        if c.connection(&id)?.provider != Provider::AndroidTv {
                            return None;
                        }
                        if apps.is_empty() {
                            c.app_shortcuts.remove(&id);
                        } else {
                            c.app_shortcuts.insert(id, apps);
                        }
                        Some(())
                    })
                }
                _ => Reply::error(405, "Use GET or PUT for app shortcuts"),
            };
        }
        if let [id, "sonos", rest @ ..] = path {
            let host = self.with(|s| match &s.config().connection(&Id::new(*id))?.provider {
                Provider::Sonos { host } => Some(host.clone()),
                _ => None,
            });
            return match host {
                Some(host) => super::sonos::route(method, rest, body, &host),
                None => Reply::error(404, "Sonos connection not found"),
            };
        }
        if let [id, "coreelec", rest @ ..] = path {
            let settings = self.with(|s| match &s.config().connection(&Id::new(*id))?.provider {
                Provider::CoreElec { host, port } => Some((
                    host.clone(),
                    *port,
                    s.path()
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("connections")
                        .join(id)
                        .join("coreelec-connection.json"),
                )),
                _ => None,
            });
            return match settings {
                Some((host, port, file)) => {
                    super::coreelec::route(method, rest, body, file, &host, port)
                }
                None => Reply::error(404, "CoreELEC connection not found"),
            };
        }
        if let [id, "denon", rest @ ..] = path {
            let settings = self.with(|s| match &s.config().connection(&Id::new(*id))?.provider {
                Provider::Denon { host, port } => Some(couch_denon::Settings {
                    host: host.clone(),
                    port: *port,
                }),
                _ => None,
            });
            return match settings {
                Some(settings) => super::denon::route(method, rest, body, settings),
                None => Reply::error(404, "Denon connection not found"),
            };
        }
        if let [id, kind @ ("protect" | "hue" | "ha" | "webos" | "kodi" | "androidtv" | "appletv"), rest @ ..] =
            path
        {
            let file = self.with(|s| {
                let c = s.config().connection(&Id::new(*id))?;
                let expected = match c.provider {
                    Provider::UnifiProtect => "protect",
                    Provider::Hue => "hue",
                    Provider::HomeAssistant => "ha",
                    Provider::WebOs => "webos",
                    Provider::AndroidTv => "androidtv",
                    Provider::AppleTv => "appletv",
                    Provider::Kodi { .. } | Provider::CoreElec { .. } => "kodi",
                    _ => return None,
                };
                if *kind != expected {
                    return None;
                }
                Some(
                    s.path()
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("connections")
                        .join(id)
                        .join(format!("{kind}-connection.json")),
                )
            });
            let Some(file) = file else {
                return Reply::error(404, "Connection not found or wrong provider");
            };
            if *kind == "kodi" {
                let host = self.with(|s| match &s.config().connection(&Id::new(*id))?.provider {
                    Provider::Kodi { host, .. } | Provider::CoreElec { host, .. } => {
                        Some(host.clone())
                    }
                    _ => None,
                });
                let Some(host) = host else {
                    return Reply::error(404, "Kodi connection was removed");
                };
                return super::kodi::route(method, rest, body, file, &host);
            }
            return match *kind {
                "protect" => super::protect::route_at(method, rest, body, file),
                "hue" => super::hue::route_at(method, rest, body, file),
                "ha" => super::ha::route_at(method, rest, body, file),
                "androidtv" | "appletv" => {
                    super::streaming_tv::route(method, rest, body, file, *kind == "appletv")
                }
                _ => super::webos::route_at(method, rest, body, file),
            };
        }
        match (method, path) {
            ("POST", []) | ("PUT", [_]) => {
                let input: Setup = match parse(body) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                if method == "PUT" {
                    let id = Id::new(path[0]);
                    if !self.with(|s| {
                        s.config()
                            .connection(&id)
                            .is_some_and(|c| c.provider.kind() == input.provider.kind())
                    }) {
                        return Reply::error(
                            400,
                            "A connection's type cannot be changed; add a new connection instead",
                        );
                    }
                    self.edit_found(revision, move |c| {
                        let slot = c.connections.iter_mut().find(|c| c.id == id)?;
                        slot.name = input.name;
                        slot.provider = input.provider;
                        Some(())
                    })
                } else {
                    let reserved = self.with(|s| {
                        let root = s
                            .path()
                            .parent()
                            .unwrap_or(std::path::Path::new("."))
                            .join("connections");
                        let mut ids = s
                            .config()
                            .connections
                            .iter()
                            .map(|c| c.id.clone())
                            .collect::<Vec<_>>();
                        if let Ok(entries) = std::fs::read_dir(root) {
                            ids.extend(
                                entries
                                    .flatten()
                                    .filter_map(|e| e.file_name().into_string().ok())
                                    .map(Id::new),
                            );
                        }
                        ids
                    });
                    self.edit(revision, move |c| {
                        let id = Id::unique(&input.name, reserved.iter());
                        c.connections.push(Connection {
                            id,
                            name: input.name,
                            provider: input.provider,
                        });
                    })
                }
            }
            ("DELETE", [id]) => {
                let id = Id::new(*id);
                // Config validation rejects a deletion while devices refer to it.
                self.edit_found(revision, move |c| {
                    let at = c.connections.iter().position(|c| c.id == id)?;
                    c.connections.remove(at);
                    c.app_shortcuts.remove(&id);
                    Some(())
                })
            }
            _ => Reply::error(404, "Unknown connection operation"),
        }
    }
}

pub(super) fn lock_for(path: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<
        Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Weak<Mutex<()>>>>,
    > = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(Default::default()))
        .lock()
        .unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(|v| v.upgrade()) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.into(), Arc::downgrade(&lock));
    lock
}

#[cfg(test)]
mod app_tests {
    use super::*;
    #[test]
    fn shortcuts_are_revisioned_validated_and_deleted_with_connection() {
        use crate::{assets::Assets, auth::Auth, store::Store};
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("couch-app-shortcuts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = couch_model::Config::default();
        config.connections.push(Connection {
            id: "tv".into(),
            name: "TV".into(),
            provider: Provider::AndroidTv,
        });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let api = Api::new(
            Store::open(dir.join("config.json")).unwrap(),
            Assets::embedded(),
            Arc::new(Auth::new(dir.join("pin"), true)),
        );
        let path = ["tv", "androidtv", "apps"];
        let body = br#"[{"name":"YouTube","url":"https://www.youtube.com/"}]"#;
        assert_eq!(
            api.connection_route("PUT", &path, body, Some(0)).status,
            200
        );
        assert_eq!(
            api.connection_route("PUT", &path, b"[]", Some(0)).status,
            409
        );
        let invalid = br#"[{"name":"Bad","url":"https://user:secret@host/"}]"#;
        assert_eq!(
            api.connection_route("PUT", &path, invalid, Some(1)).status,
            422
        );
        assert_eq!(
            api.with(|s| s.config().app_shortcuts[&Id::new("tv")].len()),
            1
        );
        assert_eq!(api.connection_route("GET", &path, b"", None).status, 200);
        assert_eq!(
            api.connection_route("DELETE", &["tv"], b"", Some(1)).status,
            200
        );
        assert!(api.with(|s| s.config().app_shortcuts.is_empty()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
