//! Shared external integration ownership for HTTP and the panel's private socket.
//! Settings remain daemon-owned; children receive only their own connection data.
use couch_plugin::{Endpoint, Error, FieldKind, Manifest, Request, Response};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const IDLE: Duration = Duration::from_secs(60);
const MAX_ENDPOINTS: usize = 64;
const STORE_READ_WAIT: Duration = Duration::from_millis(250);

fn store_request_error(error: couch_integrations::Error) -> Error {
    if error.is_busy() {
        Error::Busy
    } else {
        Error::Invalid
    }
}

/// Introducing a new config enum variant cannot be made readable by an older
/// binary. Wait until the retained rollback runtime also understands plugins.
/// A host development directory has neither a base identity nor runtime slots.
pub fn check_core_rollback(home: &Path) -> Result<(), String> {
    let runtime = home.join("runtime");
    if !home.join("build.json").exists() && !runtime.exists() {
        return Ok(());
    }
    if runtime.join("pending").exists() {
        return Err(
            "Wait for the current core update to finish before enabling integrations".into(),
        );
    }
    let previous = match fs::read_to_string(runtime.join("previous")) {
        Ok(value) => value.trim().to_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "base".into(),
        Err(_) => return Err("Cannot inspect the rollback runtime".into()),
    };
    let binary = if previous == "base" {
        home.join("couch-confd")
    } else if previous.len() == 64
        && previous
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        runtime.join("slots").join(previous).join("couch-confd")
    } else {
        return Err("The rollback runtime identity is invalid".into());
    };
    if supports_protocol(&binary) {
        Ok(())
    } else {
        Err("Install a second integration-capable core runtime before enabling integrations, so the retained rollback can still read your configuration".into())
    }
}

fn supports_protocol(binary: &Path) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(binary)
        .arg("--supports-integration-protocol=1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

struct Running {
    generation: String,
    settings: Value,
    endpoint: Arc<Endpoint>,
    used: Instant,
}

pub struct Runtime {
    home: PathBuf,
    packages: couch_integrations::Store,
    endpoints: Mutex<HashMap<String, Running>>,
    catalog_generations: Mutex<HashMap<String, String>>,
}

impl Runtime {
    pub fn new(home: PathBuf) -> Self {
        let directory = std::env::var_os("COUCH_INTEGRATIONS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("integrations"));
        Self {
            home,
            packages: couch_integrations::Store::new(directory),
            endpoints: Mutex::new(HashMap::new()),
            catalog_generations: Mutex::new(HashMap::new()),
        }
    }

    pub fn catalog(&self) -> Result<Vec<Manifest>, String> {
        self.packages.list().map_err(|e| e.to_string())
    }

    pub fn manifest(&self, id: &str) -> Result<Manifest, String> {
        self.packages
            .resolve_wait(id, STORE_READ_WAIT)
            .map(|(_, manifest)| manifest)
            .map_err(|e| e.to_string())
    }
    pub fn changed_manifests(&self, ids: &[String]) -> Vec<Manifest> {
        let Ok(mut seen) = self.catalog_generations.lock() else {
            return Vec::new();
        };
        seen.retain(|id, _| ids.contains(id));
        let mut updates = Vec::new();
        for id in ids {
            let Ok(generation) = self.packages.generation(id) else {
                seen.remove(id);
                continue;
            };
            if seen.get(id) == Some(&generation) {
                continue;
            }
            if let Ok(manifest) = self.manifest(id) {
                updates.push(manifest);
                seen.insert(id.clone(), generation);
            }
        }
        updates
    }

    fn settings_path(&self, connection: &str) -> Result<PathBuf, Error> {
        if connection.is_empty() {
            return Err(Error::Invalid);
        }
        couch_sdk::connection_file(&self.home, connection, "plugin").map_err(|_| Error::Invalid)
    }

    pub fn settings(&self, connection: &str, plugin: &str) -> Result<Value, String> {
        let manifest = self.manifest(plugin)?;
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let saved = load_settings(&path).map_err(|e| e.to_string())?;
        Ok(redacted(&manifest, saved.as_ref()))
    }

    pub fn save_settings(
        &self,
        connection: &str,
        plugin: &str,
        patch: Value,
    ) -> Result<Value, String> {
        let (directory, manifest) = self
            .packages
            .resolve_wait(plugin, STORE_READ_WAIT)
            .map_err(|e| e.to_string())?;
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock
            .try_lock()
            .map_err(|_| "Integration connection is busy")?;
        let saved = load_settings(&path).map_err(|e| e.to_string())?;
        let settings =
            merge_settings(&manifest, saved.as_ref(), patch).map_err(|e| e.to_string())?;
        // Configure validates the adapter's typed settings without requiring an
        // online TV. Do not save a schema-valid but unusable host/port.
        let mut host = couch_plugin::Host::spawn(&directory, &manifest, Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        host.configure(settings.clone())
            .map_err(|e| e.to_string())?;
        let parent = path.parent().ok_or("Invalid settings path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create connection settings directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot protect connection settings directory")?;
        }
        couch_sdk::save_private(&path, &settings)
            .map_err(|_| "Cannot save integration settings")?;
        self.endpoints
            .lock()
            .map_err(|_| "Integration registry lock failed")?
            .remove(connection);
        Ok(redacted(&manifest, Some(&settings)))
    }

    pub fn execute(
        &self,
        connection: &str,
        plugin: &str,
        request: Request,
    ) -> Result<Response, Error> {
        let queued = Instant::now();
        // The bridge cannot reconfigure a child or bypass the package handshake.
        if !matches!(
            request,
            Request::Command { .. } | Request::Status | Request::Inputs
        ) {
            return Err(Error::Unsupported);
        }
        let path = self.settings_path(connection)?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock.try_lock().map_err(|_| Error::Busy)?;
        let generation = self
            .packages
            .generation(plugin)
            .map_err(|_| Error::Invalid)?;
        let settings = load_settings(&path)?.ok_or(Error::Invalid)?;
        let existing = {
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            endpoints.retain(|_, entry| {
                entry.used.elapsed() < IDLE || Arc::strong_count(&entry.endpoint) > 1
            });
            if endpoints
                .get(connection)
                .is_some_and(|entry| entry.generation != generation || entry.settings != settings)
            {
                endpoints.remove(connection);
            }
            endpoints.get_mut(connection).map(|entry| {
                entry.used = Instant::now();
                entry.endpoint.clone()
            })
        };
        let endpoint = if let Some(endpoint) = existing {
            endpoint
        } else {
            // Handshake one child without holding the global registry lock:
            // another room's dead integration must not block healthy devices.
            let remaining = couch_plugin::QUEUE_TTL.saturating_sub(queued.elapsed());
            if remaining.is_zero() {
                return Err(Error::Expired);
            }
            let (directory, manifest) = self
                .packages
                .resolve_wait(plugin, STORE_READ_WAIT.min(remaining))
                .map_err(store_request_error)?;
            manifest.validate_settings(&settings)?;
            let endpoint = Arc::new(Endpoint::start(&directory, manifest, settings.clone())?);
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            if endpoints.len() >= MAX_ENDPOINTS {
                return Err(Error::Busy);
            }
            endpoints.insert(
                connection.to_owned(),
                Running {
                    generation,
                    settings,
                    endpoint: endpoint.clone(),
                    used: Instant::now(),
                },
            );
            endpoint
        };
        if queued.elapsed() >= couch_plugin::QUEUE_TTL {
            return Err(Error::Expired);
        }
        endpoint.request(request)
    }

    pub fn reap(&self) {
        if let Ok(mut endpoints) = self.endpoints.lock() {
            endpoints.retain(|_, entry| {
                entry.used.elapsed() < IDLE || Arc::strong_count(&entry.endpoint) > 1
            });
        }
    }
}

fn load_settings(path: &Path) -> Result<Option<Value>, Error> {
    match couch_sdk::load_private(path) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Invalid),
    }
}

fn merge_settings(
    manifest: &Manifest,
    saved: Option<&Value>,
    patch: Value,
) -> Result<Value, Error> {
    let mut merged = saved
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let patch = patch.as_object().ok_or(Error::Invalid)?;
    for (key, value) in patch {
        let field = manifest
            .settings
            .iter()
            .find(|field| &field.id == key)
            .ok_or(Error::Invalid)?;
        if value.is_null() {
            merged.remove(key);
        } else if field.kind != FieldKind::Secret
            || value.as_str() != Some("")
            || !merged.contains_key(key)
        {
            merged.insert(key.clone(), value.clone());
        }
    }
    manifest.with_defaults(Value::Object(merged))
}

fn redacted(manifest: &Manifest, saved: Option<&Value>) -> Value {
    let mut settings = serde_json::Map::new();
    let mut secrets = Vec::new();
    for field in &manifest.settings {
        let value = saved
            .and_then(|s| s.get(&field.id))
            .or(field.default.as_ref());
        if field.kind == FieldKind::Secret {
            if value.and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
                secrets.push(field.id.clone());
            }
        } else if let Some(value) = value {
            settings.insert(field.id.clone(), value.clone());
        }
    }
    json!({"settings":settings,"configured":saved.is_some_and(|v| manifest.validate_settings(v).is_ok()),"secrets":secrets})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::fd::AsRawFd};

    #[test]
    fn store_lock_contention_is_reported_as_busy_not_invalid() {
        let home = std::env::temp_dir().join(format!(
            "couch-plugin-store-busy-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(home.join("integrations")).unwrap();
        let runtime = Runtime::new(home.clone());
        // Create and validate the store layout before taking its advisory lock.
        assert!(runtime.packages.resolve("sample").is_err());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(runtime.packages.root().join(".lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let error = runtime
            .packages
            .resolve_wait("sample", Duration::ZERO)
            .unwrap_err();
        assert!(error.is_busy());
        assert_eq!(store_request_error(error), Error::Busy);
        drop(lock);
        let _ = fs::remove_dir_all(home);
    }

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "protocol_version":1,"id":"sample","label":"Sample","version":"1.0.0","executable":"plugin",
            "capabilities":[],"settings":[
                {"id":"host","label":"Host","kind":"text","required":true},
                {"id":"token","label":"Token","kind":"secret"},
                {"id":"port","label":"Port","kind":"integer","default":23}
            ]
        })).unwrap()
    }
    #[test]
    fn secrets_are_never_returned_and_blank_preserves_them() {
        let manifest = manifest();
        let saved = json!({"host":"tv.local","token":"private"});
        let merged = merge_settings(
            &manifest,
            Some(&saved),
            json!({"host":"new.local","token":""}),
        )
        .unwrap();
        assert_eq!(merged["token"], "private");
        assert_eq!(merged["port"], 23);
        let view = redacted(&manifest, Some(&merged));
        assert!(view["settings"].get("token").is_none());
        assert!(!view.to_string().contains("private"));
        assert_eq!(view["secrets"], json!(["token"]));
        let cleared = merge_settings(&manifest, Some(&merged), json!({"token":null})).unwrap();
        assert!(cleared.get("token").is_none());
    }
    #[test]
    fn unknown_fields_and_invalid_values_cannot_be_persisted() {
        let manifest = manifest();
        for patch in [
            json!({"host":""}),
            json!({"host":"tv","port":"23"}),
            json!({"host":"tv","argv":"rm"}),
        ] {
            assert!(merge_settings(&manifest, None, patch).is_err());
        }
        assert_eq!(redacted(&manifest, None)["configured"], false);
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "couch-plugin-rollback-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn binary(&self, path: &Path, body: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const SUPPORT: &str = "test \"$#\" -eq 1 && test \"$1\" = '--supports-integration-protocol=1'";

    #[test]
    fn host_development_is_allowed_but_device_base_must_support_protocol() {
        let home = Home::new();
        assert!(check_core_rollback(&home.0).is_ok());
        fs::write(home.0.join("build.json"), "{}").unwrap();
        assert!(check_core_rollback(&home.0).is_err());
        home.binary(&home.0.join("couch-confd"), "exit 2");
        assert!(check_core_rollback(&home.0).is_err());
        home.binary(&home.0.join("couch-confd"), SUPPORT);
        assert!(check_core_rollback(&home.0).is_ok());
    }

    #[test]
    fn retained_slot_is_probed_and_pending_or_invalid_identity_fails_closed() {
        let home = Home::new();
        let runtime = home.0.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let digest = "a".repeat(64);
        fs::write(runtime.join("previous"), format!("{digest}\n")).unwrap();
        let binary = runtime.join("slots").join(&digest).join("couch-confd");
        home.binary(&home.0.join("couch-confd"), SUPPORT);
        assert!(
            check_core_rollback(&home.0).is_err(),
            "a compatible base cannot stand in for the selected rollback slot"
        );
        home.binary(&binary, SUPPORT);
        assert!(check_core_rollback(&home.0).is_ok());
        fs::write(runtime.join("pending"), "update").unwrap();
        assert!(check_core_rollback(&home.0).is_err());
        fs::remove_file(runtime.join("pending")).unwrap();
        for invalid in [
            "../base",
            "",
            "BASE",
            "a",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            fs::write(runtime.join("previous"), invalid).unwrap();
            assert!(check_core_rollback(&home.0).is_err());
        }
    }

    #[test]
    fn stalled_probe_has_a_bounded_deadline() {
        let home = Home::new();
        let binary = home.0.join("probe");
        home.binary(&binary, "exec /bin/sleep 10");
        let start = Instant::now();
        assert!(!supports_protocol(&binary));
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
