//! Shared external integration ownership for HTTP and the panel's private socket.
//! Settings remain daemon-owned; children receive only their own connection data.
use couch_plugin::{Endpoint, Error, Failure, FieldKind, HostPolicy, Manifest, Request, Response};
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

/// Why settings were not saved, or a connection not prepared: the words for a
/// person, and, when it was the host or the package that refused, the code and
/// the reason a protocol 3 package gave. The words are the ones this always
/// said; only a reason changes them, to the package's own.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub text: String,
    pub failure: Option<Failure>,
}
impl From<Failure> for Refusal {
    fn from(failure: Failure) -> Self {
        Self {
            text: failure.to_string(),
            failure: Some(failure),
        }
    }
}
impl From<Error> for Refusal {
    fn from(code: Error) -> Self {
        Failure::from(code).into()
    }
}
impl From<String> for Refusal {
    fn from(text: String) -> Self {
        Self {
            text,
            failure: None,
        }
    }
}
impl From<&str> for Refusal {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
impl From<Refusal> for String {
    fn from(refusal: Refusal) -> Self {
        refusal.text
    }
}
impl From<couch_integrations::Error> for Refusal {
    fn from(error: couch_integrations::Error) -> Self {
        error.to_string().into()
    }
}

/// Start the package and let it check these settings. Configure contacts no
/// device, so what comes back is about the settings alone; a protocol 3
/// package may say which one.
fn check_settings(
    directory: &Path,
    manifest: &Manifest,
    settings: &Value,
    policy: HostPolicy,
) -> Result<(), Failure> {
    let mut host =
        couch_plugin::Host::spawn_with_policy(directory, manifest, Duration::from_secs(3), policy)?;
    match host.request_detailed(Request::Configure {
        settings: settings.clone(),
    })? {
        Response::Ok => Ok(()),
        _ => Err(Error::Protocol.into()),
    }
}

/// A conversion has no form to mark a field on, so the setting is named in
/// the sentence.
fn named(manifest: &Manifest, failure: Failure) -> Refusal {
    let label = failure
        .reason
        .as_ref()
        .and_then(|reason| reason.field())
        .and_then(|id| manifest.settings.iter().find(|field| field.id == id))
        .map(|field| field.label.clone());
    let mut refusal = Refusal::from(failure);
    if let Some(label) = label {
        refusal.text = format!("{label}: {}", refusal.text);
    }
    refusal
}

struct Running {
    generation: String,
    settings: Value,
    endpoint: Arc<Endpoint>,
    used: Instant,
}

/// Settings a package has accepted for a legacy connection, and the package
/// selection they were accepted by.
pub struct LegacyAdoption {
    package: String,
    generation: String,
    manifest: Manifest,
    settings: Value,
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
        let runtime = Self {
            home,
            packages: couch_integrations::Store::new(directory),
            endpoints: Mutex::new(HashMap::new()),
            catalog_generations: Mutex::new(HashMap::new()),
        };
        // Packages installed by an older Couch have no user of their own yet.
        // Give them one here, once, rather than have the first key press of
        // the day wait for the store's exclusive lock. A store that has never
        // held a package is left alone: there is nothing to name.
        if runtime.packages.root().join("state").is_dir() {
            if let Err(error) = runtime.packages.assign_identities() {
                eprintln!("couch-confd: cannot give integration packages their own users: {error}");
            }
        }
        runtime
    }

    /// Who this package's children run as. No fallback: a store that cannot
    /// say is reported, and the caller refuses rather than start a package
    /// under a user that belongs to another one.
    fn policy(&self, plugin: &str) -> Result<HostPolicy, couch_integrations::Error> {
        let (uid, gid) = self.packages.identity(plugin)?;
        Ok(HostPolicy::for_package(uid, gid))
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
    ) -> Result<Value, Refusal> {
        // Before the lease: allocating a user of its own for a package seen
        // for the first time needs the store's exclusive lock, which a lease
        // held here would block.
        let policy = self.policy(plugin)?;
        // Admission validates every saved connection before activating a new
        // package. Keep its selection stable until these settings are durable,
        // so validation by an old child cannot race a package activation.
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(plugin, STORE_READ_WAIT)?;
        let path = self.settings_path(connection)?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock
            .try_lock()
            .map_err(|_| "Integration connection is busy")?;
        let saved = load_settings(&path)?;
        let settings = merge_settings(&manifest, saved.as_ref(), patch)?;
        // Configure validates the adapter's typed settings without requiring an
        // online TV. Do not save a schema-valid but unusable host/port.
        check_settings(&directory, &manifest, &settings, policy)?;
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

    /// First half of giving a connection whose built-in client has left the
    /// OS to its installed package, and the slow half: start the package and
    /// let it check the settings carried over from the old connection
    /// (configure validates them and contacts no device). Nothing is written
    /// and no configuration lock is needed.
    pub fn prepare_legacy(
        &self,
        package: &str,
        settings: Value,
    ) -> Result<LegacyAdoption, Refusal> {
        let policy = self.policy(package)?;
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(package, STORE_READ_WAIT)?;
        let generation = self.packages.generation(package)?;
        let settings = manifest.with_defaults(settings)?;
        check_settings(&directory, &manifest, &settings, policy)
            .map_err(|failure| named(&manifest, failure))?;
        Ok(LegacyAdoption {
            package: package.to_owned(),
            generation,
            manifest,
            settings,
        })
    }

    /// Second half, quick, for the caller to run with the configuration
    /// locked: save the checked settings, then let `commit` switch the
    /// configuration. The package selection is held still from here to the end
    /// of the commit, and must be the one the settings were checked against;
    /// if a package operation came in between, the caller prepares again. A
    /// failed commit leaves an inert settings file the next attempt rewrites,
    /// and the old configuration in charge.
    pub fn adopt_legacy<T>(
        &self,
        connection: &str,
        prepared: &LegacyAdoption,
        commit: impl FnOnce(&Manifest) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lease = self.packages.read_lease().map_err(|e| e.to_string())?;
        if self.packages.generation(&prepared.package).ok().as_ref() != Some(&prepared.generation) {
            return Err("The package changed while this connection was being prepared".into());
        }
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock
            .try_lock()
            .map_err(|_| "Integration connection is busy")?;
        let parent = path.parent().ok_or("Invalid settings path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create connection settings directory")?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect connection settings directory")?;
        // The address saved with the old connection is the one in use. A file
        // already under this id is a leftover (an earlier trial of the
        // package, a deleted connection) and gives way to it.
        couch_sdk::save_private(&path, &prepared.settings)
            .map_err(|_| "Cannot save integration settings")?;
        let result = commit(&prepared.manifest)?;
        self.endpoints
            .lock()
            .map_err(|_| "Integration registry lock failed")?
            .remove(connection);
        Ok(result)
    }

    /// The reason a protocol 3 package gives for a refusal comes back with the
    /// code. Everything decided here, before the package is asked, is a code
    /// alone.
    pub fn execute(
        &self,
        connection: &str,
        plugin: &str,
        request: Request,
    ) -> Result<Response, Failure> {
        let queued = Instant::now();
        // The bridge cannot reconfigure a child or bypass the package handshake.
        if !matches!(
            request,
            Request::Command { .. } | Request::Action { .. } | Request::Status | Request::Inputs
        ) {
            return Err(Error::Unsupported.into());
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
                return Err(Error::Expired.into());
            }
            let (directory, manifest) = self
                .packages
                .resolve_wait(plugin, STORE_READ_WAIT.min(remaining))
                .map_err(store_request_error)?;
            manifest.validate_settings(&settings)?;
            let policy = self.policy(plugin).map_err(store_request_error)?;
            let endpoint = Arc::new(Endpoint::start_as(
                &directory,
                manifest,
                settings.clone(),
                couch_plugin::REQUEST_TIMEOUT,
                policy,
            )?);
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            if endpoints.len() >= MAX_ENDPOINTS {
                return Err(Error::Busy.into());
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
            return Err(Error::Expired.into());
        }
        endpoint.request_detailed(request)
    }

    /// Stop the package child of a connection that has just been deleted.
    /// The caller holds the connection's settings lock, which `execute` holds
    /// for a whole request, so nothing is in flight and the last reference
    /// goes here: the child is killed and waited for before this returns.
    pub fn retire(&self, connection: &str) {
        let retired = self
            .endpoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(connection);
        drop(retired);
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
        // Another test forking a child at the wrong moment leaves that child
        // holding a copy of the descriptor `resolve` just closed, and with it
        // the shared lock, until its exec. Wait that out rather than fail.
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            assert!(Instant::now() < deadline, "the store lock never came free");
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = runtime
            .packages
            .resolve_wait("sample", Duration::ZERO)
            .unwrap_err();
        assert!(error.is_busy());
        assert_eq!(store_request_error(error), Error::Busy);
        drop(lock);
        let _ = fs::remove_dir_all(home);
    }

    /// Protocol 3 is unreleased. Its only switch is couch-plugin's
    /// `protocol-3-preview` feature, and Cargo unifies features across a
    /// build, so one dependency (or dev-dependency) anywhere in this workspace
    /// that enabled it would switch it on in the daemon that ships. This runs
    /// with the daemon's own feature set and fails if that ever happens.
    #[test]
    fn the_daemon_is_never_built_with_the_protocol_3_preview() {
        assert_eq!(
            couch_plugin::accepted_protocol_version(),
            couch_plugin::PROTOCOL_VERSION
        );
        let mut next = manifest();
        next.protocol_version = couch_plugin::NEXT_PROTOCOL_VERSION;
        next.min_core_protocol_version = couch_plugin::NEXT_PROTOCOL_VERSION;
        assert_eq!(next.validate(), Err(Error::Incompatible));
    }

    /// A package that answers the handshake and then says `reply` to
    /// configure: a shell script printing two frames, as the conversion tests
    /// use.
    fn scripted(name: &str, protocol: u32, reply: Value) -> (PathBuf, Manifest) {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "couch-confd-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(&directory).unwrap();
        let mut described = serde_json::to_value(manifest()).unwrap();
        described["protocol_version"] = json!(protocol);
        described["min_core_protocol_version"] = json!(protocol);
        let manifest: Manifest = serde_json::from_value(described).unwrap();
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":reply}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str("sleep 5\n");
        let executable = directory.join("plugin");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        (directory, manifest)
    }

    #[test]
    fn a_refused_setting_comes_back_as_its_code_and_never_with_words_from_an_older_package() {
        let settings = json!({"host":"tv.local","port":23});
        let (directory, manifest) =
            scripted("refused", 1, json!({"type":"error","code":"invalid"}));
        let refused = check_settings(&directory, &manifest, &settings).unwrap_err();
        assert_eq!(refused, Failure::from(Error::Invalid));
        // What the settings form and the conversion say is what they always
        // said: the code's own sentence.
        let refusal = Refusal::from(refused);
        assert_eq!(refusal.text, Error::Invalid.to_string());
        assert_eq!(String::from(refusal), Error::Invalid.to_string());
        let _ = fs::remove_dir_all(directory);

        // Protocol 3 is switched off, so the only packages there are may not
        // give a reason. One that does is broken, and its words go nowhere.
        let (directory, manifest) = scripted(
            "worded",
            2,
            json!({"type":"error","code":"invalid","reason":
                {"kind":"invalid_setting","field":"port","text":"The port must not be 0"}}),
        );
        assert_eq!(
            check_settings(&directory, &manifest, &settings),
            Err(Error::Protocol.into())
        );
        let _ = fs::remove_dir_all(directory);

        let (directory, manifest) = scripted("accepted", 2, json!({"type":"ok"}));
        assert_eq!(check_settings(&directory, &manifest, &settings), Ok(()));
        let _ = fs::remove_dir_all(directory);
    }

    /// The panel now says how a key was pressed. Denon 0.2.1 is a protocol 2
    /// package built from an SDK that refuses a field it does not know, so a
    /// held volume key has to reach it as the bytes a tap always was. This is
    /// the daemon's own path (`execute`, the endpoint, the host's gate) with
    /// that package's manifest, and a child that keeps what it was sent.
    #[test]
    fn a_held_key_reaches_a_protocol_2_package_as_the_tap_it_always_was() {
        use couch_plugin::KeyPhase;
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "couch-confd-held-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        let manifest: Value =
            serde_json::from_str(include_str!("../tests/fixtures/denon-0.2.1-plugin.json"))
                .unwrap();
        assert_eq!(manifest["protocol_version"], 2);
        let directory = home.join("payload/usr/lib/couch/integrations/denon");
        fs::create_dir_all(directory.join("bin")).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let seen = home.join("seen");
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":{"type":"ok"}}),
            json!({"id":3,"body":{"type":"ok"}}),
            json!({"id":4,"body":{"type":"ok"}}),
            json!({"id":5,"body":{"type":"ok"}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str(&format!("exec /bin/cat > '{}'\n", seen.display()));
        let executable = directory.join("bin/couch-plugin-denon");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = home.join("denon-fixture.apk");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&package)
            .arg("-C")
            .arg(home.join("payload"))
            .args([
                "usr/lib/couch/integrations/denon/manifest.json",
                "usr/lib/couch/integrations/denon/bin/couch-plugin-denon"
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
        let runtime = Runtime::new(home.clone());
        let settings = runtime.settings_path("receiver").unwrap();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        couch_sdk::save_private(&settings, &json!({"host":"avr.invalid","port":23})).unwrap();

        for phase in [KeyPhase::Tap, KeyPhase::Repeat, KeyPhase::LongPress] {
            assert_eq!(
                runtime.execute("receiver", "denon", Request::key("volume-up", phase)),
                Ok(Response::Ok),
                "{phase:?}"
            );
        }
        let frame = |text: &str| {
            let mut bytes = (text.len() as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes
        };
        // The exact bytes, in the host's own field order. The command frame is
        // the one couch-plugin's golden file holds from the SDK revision the
        // published packages were built with.
        let expected = [
            frame(r#"{"id":1,"body":{"method":"hello","protocol_version":2}}"#),
            frame(
                r#"{"id":2,"body":{"method":"configure","settings":{"host":"avr.invalid","port":23}}}"#,
            ),
            frame(r#"{"id":3,"body":{"method":"command","function":"volume-up"}}"#),
            frame(r#"{"id":4,"body":{"method":"command","function":"volume-up"}}"#),
            frame(r#"{"id":5,"body":{"method":"command","function":"volume-up"}}"#),
        ]
        .concat();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(&seen).unwrap_or_default().len() < expected.len()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let received = fs::read(&seen).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&received),
            String::from_utf8_lossy(&expected)
        );
        assert_eq!(received, expected);
        runtime.retire("receiver");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_reason_replaces_the_sentence_and_a_conversion_names_the_setting_it_blames() {
        let port = Failure {
            code: Error::Invalid,
            reason: Some(couch_plugin::Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }),
        };
        let refusal = Refusal::from(port.clone());
        assert_eq!(refusal.text, "The port must not be 0");
        assert_eq!(refusal.failure.as_ref(), Some(&port));
        // The form marks the field; a conversion has no form.
        assert_eq!(
            named(&manifest(), port.clone()).text,
            "Port: The port must not be 0"
        );
        assert_eq!(named(&manifest(), port.clone()).failure, Some(port));
        let pairing = Failure {
            code: Error::Unpaired,
            reason: Some(couch_plugin::Reason::Message {
                text: "Pair this TV again".into(),
            }),
        };
        assert_eq!(named(&manifest(), pairing).text, "Pair this TV again");
        assert_eq!(
            named(&manifest(), Error::Timeout.into()).text,
            Error::Timeout.to_string()
        );
        // Words of this daemon's own carry no code.
        assert_eq!(
            Refusal::from("Integration connection is busy").failure,
            None
        );
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
