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
        // Everything below the connection's own record works in its private
        // folder. Deleting the connection waits for these to finish and holds
        // new ones off until the folder is gone, so none of them can look the
        // connection up, lose it, and then write its settings back.
        let _in_use = match path {
            [id, _, ..] => Some(gate_for(&self.with(|s| stored_root(s.path())).join(id))),
            _ => None,
        };
        let _in_use = _in_use
            .as_ref()
            .map(|gate| gate.read().unwrap_or_else(|e| e.into_inner()));
        if let [id, "plugin", rest @ ..] = path {
            return self.plugin_route(method, id, rest, body);
        }
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
            // One household API key, resolved by the client's own rule so the
            // daemon, the GUI and the CLI cannot read different files.
            let target = self.with(|s| match &s.config().connection(&Id::new(*id))?.provider {
                Provider::Sonos { host } => Some((
                    host.clone(),
                    couch_sonos::key_file_in(
                        s.path().parent().unwrap_or(std::path::Path::new(".")),
                    ),
                )),
                _ => None,
            });
            return match target {
                Some((host, key_file)) => super::sonos::route(method, rest, body, &host, &key_file),
                None => Reply::error(404, "Sonos connection not found"),
            };
        }
        if let [id, "matter", rest @ ..] = path {
            let dir = self.with(|s| match s.config().connection(&Id::new(*id))?.provider {
                Provider::Matter => Some(
                    s.path()
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("connections")
                        .join(id)
                        .join("matter"),
                ),
                _ => None,
            });
            return match dir {
                Some(dir) => super::matter::route_at(method, rest, body, dir),
                None => Reply::error(404, "Matter connection not found"),
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
        if let [id, kind @ ("protect" | "hue" | "ha" | "webos" | "kodi" | "androidtv" | "appletv"
        | "tizen"), rest @ ..] = path
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
                    Provider::Tizen => "tizen",
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
                "tizen" => super::tizen::route(method, rest, body, file),
                _ => super::webos::route_at(method, rest, body, file),
            };
        }
        match (method, path) {
            ("POST", []) | ("PUT", [_]) => {
                let mut input: Setup = match parse(body) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                // A built-in that left the OS is only ever read from an older
                // file; a new connection of that kind belongs to its package.
                if let ("POST", Some(row)) = (method, input.provider.legacy_builtin()) {
                    return Reply::error(
                        400,
                        format!(
                            "{} is now an integration package: install it from Integrations, then add the connection",
                            row.name
                        ),
                    );
                }
                if let Provider::Plugin {
                    id,
                    label,
                    capabilities,
                    supports_inputs,
                    presentation,
                    actions,
                    children,
                } = &mut input.provider
                {
                    match self.plugins.manifest(id) {
                        Ok(manifest) => {
                            // Like everything else in this snapshot, the kinds
                            // of child a connection offers come from the
                            // package, never from the browser. No manifest
                            // this build accepts can declare any (protocol 3,
                            // unreleased), so there are none.
                            *children = manifest.children;
                            *label = manifest.label;
                            *capabilities = manifest
                                .capabilities
                                .into_iter()
                                .map(|c| couch_model::PluginCapability {
                                    id: c.id,
                                    label: c.label,
                                })
                                .collect();
                            *supports_inputs = manifest.supports_inputs;
                            *presentation = manifest.presentation;
                            *actions = manifest.actions;
                        }
                        Err(_) => {
                            return Reply::error(
                                400,
                                "Install this integration before adding or changing its connection",
                            )
                        }
                    }
                }
                if method == "PUT" {
                    let id = Id::new(path[0]);
                    if !self.with(|s| {
                        s.config().connection(&id).is_some_and(|c| {
                            c.provider.kind() == input.provider.kind()
                                && match (&c.provider, &input.provider) {
                                    (
                                        Provider::Plugin { id: a, .. },
                                        Provider::Plugin { id: b, .. },
                                    ) => a == b,
                                    _ => true,
                                }
                        })
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
            ("DELETE", [id]) => self.delete_connection(id, revision),
            _ => Reply::error(404, "Unknown connection operation"),
        }
    }

    /// Take a connection out of the configuration and then, only once that is
    /// saved, remove what the remote stored for it: pairing keys, tokens,
    /// passwords, a package's settings. A deletion that is refused (devices
    /// still use the connection, the page was stale) removes nothing, and a
    /// power cut between the two steps leaves a folder nobody reads, which is
    /// what every deletion used to leave.
    ///
    /// Matter is the exception. Its folder holds the remote's own fabric: the
    /// keys every paired device trusts, which nothing can issue again. It
    /// stays, as does a `matter` folder under any other connection.
    ///
    /// Replacing or resetting the whole configuration never comes here, so a
    /// backup that drops a connection and a later one that brings it back
    /// still find its pairing.
    fn delete_connection(&self, name: &str, revision: Option<u64>) -> Reply {
        let id = Id::new(name);
        let remove = move |c: &mut couch_model::Config| {
            let at = c.connections.iter().position(|c| c.id == id)?;
            c.connections.remove(at);
            c.app_shortcuts.remove(&id);
            Some(())
        };
        let root = self.with(|s| {
            let connection = s.config().connection(&Id::new(name))?;
            (connection.provider != Provider::Matter && stored_name(name))
                .then(|| stored_root(s.path()))
        });
        let Some(root) = root else {
            // Config validation rejects a deletion while devices refer to it.
            return self.edit_found(revision, remove);
        };
        let folder = root.join(name);
        let busy = || Reply::error(503, "This connection is busy; try again in a moment");
        let gate = gate_for(&folder);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let Some(_closed) = patiently(deadline, || match gate.try_write() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }) else {
            return busy();
        };
        // The locks the settings writers and the package host hold while they
        // work, for the paths that do not come through `connection_route`: the
        // panel's socket and the conversion of a former built-in.
        let locks: Vec<_> = STORED_LOCKS
            .iter()
            .map(|file| lock_for(&folder.join(file)))
            .collect();
        let mut held = Vec::new();
        for lock in &locks {
            match patiently(deadline, || match lock.try_lock() {
                Ok(guard) => Some(guard),
                Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            }) {
                Some(guard) => held.push(guard),
                None => return busy(),
            }
        }
        self.edit_found_then(revision, remove, || {
            self.plugins.retire(name);
            if let Err(e) = remove_stored(&root, name) {
                println!(
                    "couch-confd: connection {name}: stored settings were not all removed: {e}"
                );
            }
        })
    }
}

/// Where every connection's private folder lives, beside `config.json`.
fn stored_root(config: &std::path::Path) -> std::path::PathBuf {
    config
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("connections")
}

/// The names `lock_for` is asked for inside one connection's folder.
const STORED_LOCKS: [&str; 11] = [
    "protect-connection.json",
    "hue-connection.json",
    "ha-connection.json",
    "webos-connection.json",
    "kodi-connection.json",
    "coreelec-connection.json",
    "androidtv-connection.json",
    "appletv-connection.json",
    "appletv-metadata-connection.json",
    "tizen-connection.json",
    "plugin-connection.json",
];

/// The remote's Matter fabric under a connection. Never removed.
const MATTER: &str = "matter";

/// A folder name this file will remove: exactly what configuration validation
/// allows a connection ID to be, so never empty, never a path and never `..`.
fn stored_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Keep trying until the deadline, a couple of seconds for the whole deletion:
/// a status read or a command may be passing through, a pairing that waits on
/// somebody's TV is not worth waiting for.
pub(crate) fn patiently<T>(
    deadline: std::time::Instant,
    mut attempt: impl FnMut() -> Option<T>,
) -> Option<T> {
    loop {
        if let Some(value) = attempt() {
            return Some(value);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Remove `<root>/<name>` and what is in it, except a Matter fabric. Links are
/// removed as links and never followed, so nothing outside `root` is touched
/// whatever somebody planted there. `Ok(false)` says a fabric kept the folder.
fn remove_stored(root: &std::path::Path, name: &str) -> std::io::Result<bool> {
    use std::io::ErrorKind::{DirectoryNotEmpty, InvalidInput, NotFound};
    if !stored_name(name) {
        return Err(InvalidInput.into());
    }
    let folder = root.join(name);
    let gone = |result: std::io::Result<()>| match result {
        Err(e) if e.kind() != NotFound => Err(e),
        _ => Ok(()),
    };
    match std::fs::symlink_metadata(&folder) {
        Err(e) if e.kind() == NotFound => return Ok(true),
        Err(e) => return Err(e),
        Ok(found) if !found.is_dir() => return gone(std::fs::remove_file(&folder)).map(|_| true),
        Ok(_) => {}
    }
    // The panel may save a TV's wake address while this runs; go round again
    // rather than leave the folder behind for one late file.
    for _ in 0..3 {
        let mut kept = false;
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            if entry.file_name() == MATTER {
                kept = true;
            } else if entry.file_type()?.is_dir() {
                gone(std::fs::remove_dir_all(entry.path()))?;
            } else {
                gone(std::fs::remove_file(entry.path()))?;
            }
        }
        if kept {
            return Ok(false);
        }
        match std::fs::remove_dir(&folder) {
            Err(e) if e.kind() == DirectoryNotEmpty => continue,
            result => return gone(result).map(|_| true),
        }
    }
    Err(DirectoryNotEmpty.into())
}

/// Held for reading by every operation inside one connection's folder, and for
/// writing while the connection is deleted.
fn gate_for(folder: &std::path::Path) -> std::sync::Arc<std::sync::RwLock<()>> {
    use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
    static GATES: OnceLock<Mutex<std::collections::HashMap<std::path::PathBuf, Weak<RwLock<()>>>>> =
        OnceLock::new();
    let mut gates = GATES
        .get_or_init(|| Mutex::new(Default::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(folder).and_then(|v| v.upgrade()) {
        return gate;
    }
    let gate = Arc::new(RwLock::new(()));
    gates.insert(folder.into(), Arc::downgrade(&gate));
    gate
}

pub(crate) fn lock_for(path: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<()>> {
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

#[cfg(test)]
mod delete_tests {
    use super::*;
    use crate::{assets::Assets, auth::Auth, store::Store};
    use serde_json::{json, Value};
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::{Path, PathBuf},
        sync::Arc,
    };

    struct House {
        api: Api,
        home: PathBuf,
    }
    impl House {
        fn new(name: &str, config: Value) -> Self {
            let home = std::env::temp_dir().join(format!(
                "couch-connection-delete-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            fs::write(
                home.join("config.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            let api = Api::new(
                Store::open(home.join("config.json")).unwrap(),
                Assets::embedded(),
                Arc::new(Auth::new(home.join("pin"), true)),
            );
            Self { api, home }
        }
        fn stored(&self, connection: &str, file: &str) -> PathBuf {
            let path = self.home.join("connections").join(connection).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"{\"token\":\"private\"}").unwrap();
            path
        }
        fn folder(&self, connection: &str) -> PathBuf {
            self.home.join("connections").join(connection)
        }
        fn ids(&self) -> Vec<String> {
            self.api.with(|s| {
                s.config()
                    .connections
                    .iter()
                    .map(|c| c.id.to_string())
                    .collect()
            })
        }
        fn delete(&self, connection: &str, revision: Option<u64>) -> u16 {
            self.api
                .connection_route("DELETE", &[connection], b"", revision)
                .status
        }
        fn create(&self, body: Value) -> String {
            let reply =
                self.api
                    .connection_route("POST", &[], &serde_json::to_vec(&body).unwrap(), None);
            assert_eq!(
                reply.status,
                200,
                "{}",
                String::from_utf8_lossy(&reply.body)
            );
            self.ids().pop().unwrap()
        }
    }
    impl Drop for House {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    fn house() -> Value {
        json!({
            "schema_version": 1, "revision": 7,
            "connections": [
                {"id":"hue-bridge","name":"Hue Bridge","provider":{"kind":"hue"}},
                {"id":"tv","name":"TV","provider":{"kind":"web-os"}},
                {"id":"fabric","name":"Fabric","provider":{"kind":"matter"}}
            ],
            "rooms": [{"id":"den","name":"Den","devices":[
                {"id":"lg","name":"LG","kind":"tv","integration":{"via":"connection","connection_id":"tv"}}]}]
        })
    }

    #[test]
    fn deleting_a_connection_removes_what_was_stored_for_it_and_nothing_else() {
        let house = House::new("removes", house());
        let key = house.stored("hue-bridge", "hue-connection.json");
        house.stored("hue-bridge", "cache/nested/state.json");
        let other = house.stored("tv", "webos-connection.json");
        // The former singleton file and the record of where it went.
        let legacy = house.home.join("hue-connection.json");
        fs::write(&legacy, b"{}").unwrap();
        let map = house.home.join("connection-legacy-map.json");
        let mapped = fs::read(&map).unwrap();

        assert_eq!(house.delete("hue-bridge", Some(7)), 200);

        assert!(!key.exists() && !house.folder("hue-bridge").exists());
        assert!(other.exists() && legacy.exists());
        assert_eq!(fs::read(&map).unwrap(), mapped);
        assert_eq!(house.ids(), ["tv", "fabric"]);
        // Nothing reserves the name any more, and nothing is inherited.
        assert_eq!(
            house.create(json!({"name":"Hue Bridge","provider":{"kind":"hue"}})),
            "hue-bridge"
        );
        let fresh =
            house
                .api
                .connection_route("GET", &["hue-bridge", "hue", "connection"], b"", None);
        assert_eq!(
            serde_json::from_slice::<Value>(&fresh.body).unwrap(),
            json!({"url":"","token_set":false})
        );
    }

    #[test]
    fn a_refused_delete_removes_nothing() {
        let house = House::new("refused", house());
        let tv = house.stored("tv", "webos-connection.json");
        let key = house.stored("hue-bridge", "hue-connection.json");
        // A room's device still uses the TV.
        assert_eq!(house.delete("tv", Some(7)), 422);
        // The page was out of date.
        assert_eq!(house.delete("hue-bridge", Some(6)), 409);
        // Not a connection at all, however it is spelled.
        for name in ["nobody", "..", ".", "connections", "hue-bridge%2F..", "a b"] {
            assert_eq!(house.delete(name, None), 404, "{name}");
        }
        assert!(tv.exists() && key.exists() && house.home.join("config.json").exists());
        assert_eq!(house.ids(), ["hue-bridge", "tv", "fabric"]);
    }

    #[test]
    fn a_connection_in_use_is_not_deleted_under_whoever_is_using_it() {
        let house = House::new("busy", house());
        let key = house.stored("hue-bridge", "hue-connection.json");
        let lock = lock_for(&key);
        let pairing = lock.lock().unwrap();
        assert_eq!(house.delete("hue-bridge", None), 503);
        assert!(key.exists());
        assert_eq!(house.ids().len(), 3);
        drop(pairing);
        // Any request working in the folder, whichever file it is after.
        let gate = gate_for(&house.folder("hue-bridge"));
        let request = gate.read().unwrap();
        assert_eq!(house.delete("hue-bridge", None), 503);
        assert!(key.exists());
        drop(request);
        assert_eq!(house.delete("hue-bridge", None), 200);
        assert!(!key.exists());
    }

    #[test]
    fn a_matter_fabric_outlives_its_connection() {
        let house = House::new("matter", house());
        let fabric = house.stored("fabric", "matter/config.json");
        let ca = house.stored("fabric", "matter/pem/ca-private.pem");
        assert_eq!(house.delete("fabric", None), 200);
        assert_eq!(house.ids(), ["hue-bridge", "tv"]);
        assert_eq!(fs::read(&fabric).unwrap(), b"{\"token\":\"private\"}");
        assert!(ca.exists());
        // Its folder still reserves the name, so a new connection cannot
        // walk into the old fabric.
        assert_eq!(
            house.create(json!({"name":"Fabric","provider":{"kind":"matter"}})),
            "fabric-2"
        );
        // A fabric found under any other kind of connection is left as well.
        let stray = house.stored("hue-bridge", "matter/pem/ca-private.pem");
        let key = house.stored("hue-bridge", "hue-connection.json");
        assert_eq!(house.delete("hue-bridge", None), 200);
        assert!(stray.exists() && !key.exists());
    }

    #[test]
    fn links_and_hostile_names_never_reach_outside_the_connections_folder() {
        let house = House::new("hostile", house());
        let outside = house.home.join("integration-keys");
        fs::create_dir_all(&outside).unwrap();
        let precious = outside.join("feed.pub");
        fs::write(&precious, b"key").unwrap();
        let root = house.home.join("connections");
        fs::create_dir_all(&root).unwrap();

        for name in [
            "",
            ".",
            "..",
            "../integration-keys",
            "tv/..",
            "/etc",
            "a\\b",
            "a\0",
            "tv ",
        ] {
            assert!(remove_stored(&root, name).is_err(), "{name:?}");
        }
        assert!(precious.exists() && house.home.join("config.json").exists());

        // The whole folder is a link: the link goes, what it pointed at stays.
        // (Opening the store made the first bridge's folder; it is empty.)
        fs::remove_dir(root.join("hue-bridge")).unwrap();
        symlink(&outside, root.join("hue-bridge")).unwrap();
        assert_eq!(house.delete("hue-bridge", None), 200);
        assert!(fs::symlink_metadata(root.join("hue-bridge")).is_err());
        assert!(precious.exists());

        // Links inside the folder, at any depth, to a folder and to a file.
        house.stored("kodi", "kodi-connection.json");
        house.stored("kodi", "deep/er/file.json");
        symlink(&outside, root.join("kodi/keys")).unwrap();
        symlink(&outside, root.join("kodi/deep/er/keys")).unwrap();
        symlink(&precious, root.join("kodi/deep/feed.pub")).unwrap();
        assert!(remove_stored(&root, "kodi").unwrap());
        assert!(!root.join("kodi").exists());
        assert_eq!(fs::read(&precious).unwrap(), b"key");
        // Nothing there is nothing to do, not an error.
        assert!(remove_stored(&root, "kodi").unwrap());
    }

    const SAMPLE: &str = r#"{
        "protocol_version":1,"id":"sample","label":"Sample","version":"1.0.0",
        "executable":"bin/couch-plugin-sample","capabilities":[{"id":"power-on","label":"On"}],
        "settings":[
            {"id":"host","label":"Host","kind":"text","required":true},
            {"id":"token","label":"Token","kind":"secret"}
        ]
    }"#;

    /// A package whose child answers the handshake, configure and one status
    /// read, writes down its process ID and then stays alive.
    fn install_sample(home: &Path, pids: &Path) {
        let manifest: Value = serde_json::from_str(SAMPLE).unwrap();
        let payload = home.join("payload");
        let directory = payload.join("usr/lib/couch/integrations/sample");
        fs::create_dir_all(directory.join("bin")).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let mut script = format!("#!/bin/sh\necho $$ >> '{}'\n", pids.display());
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":{"type":"ok"}}),
            json!({"id":3,"body":{"type":"status","status":{"on":true}}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str("exec /bin/sleep 30\n");
        let executable = directory.join("bin/couch-plugin-sample");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = home.join("sample-fixture.apk");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&package)
            .arg("-C")
            .arg(payload)
            .args([
                "usr/lib/couch/integrations/sample/manifest.json",
                "usr/lib/couch/integrations/sample/bin/couch-plugin-sample"
            ])
            .status()
            .unwrap()
            .success());
        let apk = home.join("fixture-apk");
        fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
        fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
        couch_integrations::Store::new(home.join("integrations"))
            .with_apk(apk)
            .install(&package)
            .unwrap();
    }

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// The two locks this file and `plugins.rs` hold are taken in one order
    /// and one order only: the configuration store first, then the package
    /// registry. A deletion does exactly that. The ten-second sweep has to
    /// read the configuration too - it is what says which connections a
    /// device still refers to - and it reads it **before** it locks the
    /// registry, never while holding it. Taken the other way round, one
    /// sweep and one delete would wait on each other for good, and every
    /// configuration read on the remote would queue behind them.
    #[test]
    fn a_sweep_never_holds_the_package_registry_while_it_reads_the_configuration() {
        use std::time::{Duration, Instant};
        let house = House::new("sweep-order", json!({"schema_version":1,"revision":0}));
        let pids = house.home.join("pids");
        install_sample(&house.home, &pids);
        assert_eq!(
            house.create(
                json!({"name":"Receiver","provider":{"kind":"plugin","id":"sample","label":""}})
            ),
            "receiver"
        );
        assert_eq!(
            house
                .api
                .connection_route(
                    "POST",
                    &["receiver", "plugin", "settings"],
                    br#"{"host":"avr.invalid"}"#,
                    None,
                )
                .status,
            200
        );
        assert_eq!(
            house
                .api
                .connection_route("GET", &["receiver", "plugin", "status"], b"", None)
                .status,
            200
        );
        // A child that nothing will ask for again, which is what `keep_alive`
        // makes of one.
        house.api.plugins.keep_alive_for_test("receiver");

        // The configuration is held, as it is while a deletion saves.
        let held = house.api.store.lock().unwrap();
        std::thread::scope(|scope| {
            let sweeping = scope.spawn(|| {
                let in_use = house.api.connections_in_use();
                house
                    .api
                    .plugins
                    .reap(&|connection| in_use.contains(connection));
            });
            // The sweep is now waiting for the configuration. While it does,
            // anything that needs a package child has to get through.
            std::thread::sleep(Duration::from_millis(200));
            let at = Instant::now();
            house.api.plugins.retire("receiver");
            assert!(
                at.elapsed() < Duration::from_secs(2),
                "the sweep was holding the package registry while it waited"
            );
            drop(held);
            sweeping.join().unwrap();
        });
    }

    #[test]
    fn a_packaged_connection_loses_its_settings_and_its_running_child() {
        let house = House::new("packaged", json!({"schema_version":1,"revision":0}));
        let pids = house.home.join("pids");
        install_sample(&house.home, &pids);
        let plugin =
            json!({"name":"Receiver","provider":{"kind":"plugin","id":"sample","label":""}});
        assert_eq!(house.create(plugin.clone()), "receiver");
        let saved = house.api.connection_route(
            "POST",
            &["receiver", "plugin", "settings"],
            br#"{"host":"avr.invalid","token":"private"}"#,
            None,
        );
        assert_eq!(
            saved.status,
            200,
            "{}",
            String::from_utf8_lossy(&saved.body)
        );
        let settings = house.folder("receiver").join("plugin-connection.json");
        assert!(fs::read_to_string(&settings).unwrap().contains("private"));
        // Protocol 3 (unreleased): a pairing key and the line beside it live
        // in the same folder and go the same way. Nothing a shipped build
        // runs writes them; planted here so the removal is stated.
        let key = house.stored("receiver", "plugin-credential.json");
        let paired = house.stored("receiver", "plugin-pairing.json");
        let status =
            house
                .api
                .connection_route("GET", &["receiver", "plugin", "status"], b"", None);
        assert_eq!(
            status.status,
            200,
            "{}",
            String::from_utf8_lossy(&status.body)
        );
        let child: i32 = fs::read_to_string(&pids)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .parse()
            .unwrap();
        assert!(alive(child));

        assert_eq!(house.delete("receiver", None), 200);

        assert!(!alive(child));
        assert!(!settings.exists() && !house.folder("receiver").exists());
        assert!(!key.exists() && !paired.exists());
        // The panel's route to the package finds neither connection nor child.
        assert!(house
            .api
            .plugins
            .execute("receiver", "sample", None, couch_plugin::Request::status())
            .is_err());
        assert!(!house.folder("receiver").exists());
        // The same name again is a new connection that knows nothing.
        assert_eq!(house.create(plugin), "receiver");
        let fresh =
            house
                .api
                .connection_route("GET", &["receiver", "plugin", "settings"], b"", None);
        assert_eq!(
            serde_json::from_slice::<Value>(&fresh.body).unwrap(),
            json!({"settings":{},"configured":false,"secrets":[]})
        );
    }
}
