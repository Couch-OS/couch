//! The config file, and the only code allowed to write it.
//!
//! Every mutation goes through [`Store::mutate`], which validates the result
//! *before* it replaces the in-memory copy and only then writes the file. A
//! rejected edit therefore changes nothing at all, rather than leaving the
//! daemon serving a document that the next reader would refuse to load.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use couch_model::{Config, StoredConfig, ValidationError};

pub const DEFAULT_PATH: &str = "/opt/couch/config.json";

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Parse(serde_json::Error),
    Invalid(ValidationError),
    Compatibility(String),
    /// The client sent an `If-Match` that no longer matches.
    Stale {
        expected: u64,
        actual: u64,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Parse(e) => write!(f, "not valid JSON: {e}"),
            Error::Invalid(e) => write!(f, "{e}"),
            Error::Compatibility(message) => f.write_str(message),
            Error::Stale { expected, actual } => write!(
                f,
                "config changed underneath you (you had revision {expected}, it is now {actual})"
            ),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

pub struct Store {
    path: PathBuf,
    config: Config,
}

#[derive(serde::Serialize)]
pub struct IntegrationRecovery {
    pub integrations_active: bool,
    pub revision: Option<u64>,
    pub path: Option<String>,
    /// An attempted save, never advertised as a confirmed recovery export.
    pub pending_path: Option<String>,
}

impl Store {
    /// Load an existing configuration, or create an empty one on first boot.
    ///
    /// New installations start without example rooms or devices. A corrupt
    /// existing file is an error and is never replaced with a fresh config.
    pub fn open(path: impl Into<PathBuf>) -> Result<Store, Error> {
        let path = path.into();
        match fs::read(&path) {
            Ok(bytes) => {
                let stored: StoredConfig = serde_json::from_slice(&bytes).map_err(Error::Parse)?;
                let enveloped = stored.has_integrations();
                let mut config = stored
                    .into_config()
                    .map_err(|e| Error::Compatibility(e.into()))?;
                // A command word an old release could save but the executor
                // never ran (`"bright"`, before levels existed) has to be
                // rewritten before the very first validate below, which would
                // otherwise refuse the file outright and never reach the
                // general `migrate` further down.
                let commands_migrated = config.migrate_commands();
                if commands_migrated {
                    config.revision = config.revision.wrapping_add(1);
                }
                config.validate().map_err(Error::Invalid)?;
                let legacy = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .is_some_and(|v| v.get("connections").is_none());
                let needs_envelope = !enveloped && StoredConfig::new(&config).has_integrations();
                let mut store = Store { path, config };
                // Never infer that a legacy rewrite should restore integrations.
                // Only an exact match proves a pending export was committed.
                if enveloped {
                    store.confirm_recovery();
                }
                if needs_envelope || commands_migrated {
                    store.write()?;
                }
                if legacy {
                    let parent = store.path.parent().unwrap_or_else(|| Path::new("."));
                    for (file, name, provider) in [
                        (
                            "ha-connection.json",
                            "Home Assistant",
                            couch_model::Provider::HomeAssistant,
                        ),
                        (
                            "hue-connection.json",
                            "Philips Hue",
                            couch_model::Provider::LegacyHue,
                        ),
                    ] {
                        if parent.join(file).is_file() {
                            store.config.connections.push(couch_model::Connection {
                                id: couch_model::Id::from_name(name),
                                name: name.into(),
                                provider,
                            });
                        }
                    }
                    if !store.config.connections.is_empty() {
                        store.config.revision = store.config.revision.wrapping_add(1);
                        store.config.validate().map_err(Error::Invalid)?;
                        store.write()?;
                    }
                }
                store.migrate_connection_credentials()?;
                // Older shapes the model can bring forward itself (a
                // Bluetooth TV connection into a per-device bond): rewritten
                // once, so every reader sees the current shape.
                if store.config.migrate() {
                    store.config.revision = store.config.revision.wrapping_add(1);
                    store.config.validate().map_err(Error::Invalid)?;
                    store.write()?;
                }
                Ok(store)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let store = Store {
                    path,
                    config: Config::default(),
                };
                store.write()?;
                Ok(store)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Bind former singleton credentials once, preserving originals and existing scoped pairings.
    fn migrate_connection_credentials(&mut self) -> Result<(), Error> {
        let root = self.path.parent().unwrap_or(Path::new("."));
        let marker = root.join("connection-legacy-map.json");
        let mut mapped: std::collections::BTreeMap<String, String> = match fs::read(&marker) {
            Ok(b) => serde_json::from_slice(&b).map_err(Error::Parse)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e.into()),
        };
        for (kind, prefix) in [
            ("hue", "hue"),
            ("home-assistant", "ha"),
            ("web-os", "webos"),
        ] {
            if mapped.contains_key(kind) {
                continue;
            }
            let Some(c) = self
                .config
                .connections
                .iter()
                .find(|c| c.provider.kind() == kind)
            else {
                continue;
            };
            let directory = root.join("connections").join(c.id.as_str());
            fs::create_dir_all(&directory)?;
            for filename in [
                format!("{prefix}-connection.json"),
                format!("{prefix}-wake.json"),
            ] {
                let source = root.join(&filename);
                let target = directory.join(&filename);
                if source.is_file() && !target.exists() {
                    let data = fs::read(source)?;
                    write_private(&target, &data)?;
                }
            }
            mapped.insert(kind.into(), c.id.to_string());
        }
        let mut changed = false;
        for room in &mut self.config.rooms {
            for device in &mut room.devices {
                let legacy = match &device.integration {
                    couch_model::Integration::Hue { light_id } => Some(("hue", light_id.clone())),
                    couch_model::Integration::HomeAssistant { entity_id } => {
                        Some(("home-assistant", entity_id.clone()))
                    }
                    _ => None,
                };
                if let Some((kind, resource_id)) = legacy {
                    if let Some(id) = mapped.get(kind).filter(|id| {
                        self.config
                            .connections
                            .iter()
                            .any(|c| c.id.as_str() == id.as_str())
                    }) {
                        device.integration = couch_model::Integration::Connection {
                            connection_id: couch_model::Id::new(id.clone()),
                            resource_id,
                            child: None,
                        };
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.config.revision = self.config.revision.wrapping_add(1);
            self.config.validate().map_err(Error::Invalid)?;
            self.write()?;
        }
        let data = serde_json::to_vec(&mapped).map_err(Error::Parse)?;
        if fs::read(&marker).ok().as_deref() != Some(data.as_slice()) {
            write_private(&marker, &data)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn revision(&self) -> u64 {
        self.config.revision
    }

    fn recovery_path(&self) -> PathBuf {
        self.path.with_extension("integration-recovery.json")
    }

    fn recovery_pending_path(&self) -> PathBuf {
        self.path
            .with_extension("integration-recovery.pending.json")
    }

    /// Confirm only a candidate exactly matching the committed live document.
    /// Failure here cannot undo an already committed config. Retain the pending
    /// export and retry at startup instead of returning a misleading save error.
    fn confirm_recovery(&self) {
        let pending = self.recovery_pending_path();
        let matches = fs::read(&pending)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
            .is_some_and(|config| config == self.config);
        if !matches {
            return;
        }
        // Confirmation must follow a durable main-config rename, including on
        // restart after a previously interrupted save.
        let parent = self.path.parent().unwrap_or(Path::new("."));
        if let Err(error) = fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            eprintln!("couch-confd: cannot confirm integration recovery export: {error}");
            return;
        }
        if let Err(error) = fs::rename(&pending, self.recovery_path()) {
            eprintln!("couch-confd: integration recovery export remains pending: {error}");
            return;
        }
        if let Ok(dir) = fs::File::open(self.path.parent().unwrap_or(Path::new("."))) {
            let _ = dir.sync_all();
        }
    }

    /// A normal whole-config import is the explicit restore operation. Reading
    /// this export never merges it into a config edited by an older runtime.
    pub fn integration_recovery_config(&self) -> Result<Option<Config>, Error> {
        match fs::read(self.recovery_path()) {
            Ok(bytes) => {
                let config: Config = serde_json::from_slice(&bytes).map_err(Error::Parse)?;
                config.validate().map_err(Error::Invalid)?;
                Ok(Some(config))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn integration_recovery(&self) -> Option<IntegrationRecovery> {
        let recovery = self.integration_recovery_config().ok().flatten();
        let pending = self.recovery_pending_path();
        let pending_path = pending.exists().then(|| pending.display().to_string());
        if recovery.is_none() && pending_path.is_none() {
            return None;
        }
        Some(IntegrationRecovery {
            integrations_active: StoredConfig::new(&self.config).has_integrations(),
            revision: recovery.as_ref().map(|config| config.revision),
            path: recovery.map(|_| self.recovery_path().display().to_string()),
            pending_path,
        })
    }

    /// Apply `f` to a copy, and keep it only if the result validates.
    ///
    /// `if_match` is the revision the client believed it was editing. Two
    /// phones on the same page is the collision that actually happens here, and
    /// without this the second save silently wins.
    pub fn mutate<T>(
        &mut self,
        if_match: Option<u64>,
        f: impl FnOnce(&mut Config) -> T,
    ) -> Result<T, Error> {
        if let Some(expected) = if_match {
            if expected != self.config.revision {
                return Err(Error::Stale {
                    expected,
                    actual: self.config.revision,
                });
            }
        }
        let mut next = self.config.clone();
        let out = f(&mut next);
        // An edit may carry the old shape (a whole-config PUT of an export,
        // a device set to the old integration); it lands in the current one.
        next.migrate();
        next.revision = self.config.revision.wrapping_add(1);
        next.validate().map_err(Error::Invalid)?;
        let previous = std::mem::replace(&mut self.config, next);
        if let Err(e) = self.write() {
            // The file is the source of truth; if it did not take, neither did
            // the edit, or a restart would quietly undo what the UI just
            // confirmed.
            self.config = previous;
            return Err(e);
        }
        Ok(out)
    }

    /// Write, atomically.
    ///
    /// Temp file in the same directory, fsync, rename. Same directory matters:
    /// rename is only atomic within a filesystem, and `/tmp` on this device is
    /// tmpfs while `/opt/couch` is the rootfs partition. The fsync matters
    /// because the realistic way this device stops is a battery pull or a
    /// watchdog reset, and rename ordering without it can leave a
    /// present-but-empty file - which is exactly the state `open` above refuses
    /// to recover from.
    fn write(&self) -> Result<(), Error> {
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let tmp = self.path.with_extension("json.tmp");
        let stored = StoredConfig::new(&self.config);
        let integration_root = std::env::var_os("COUCH_INTEGRATIONS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                self.path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("integrations")
            });
        // Admission reads connection metadata under the package store's
        // exclusive lock. Serialize config commits with that check, including
        // deleting the last external connection. Plain installations need not
        // create a package store merely to save a room name.
        let _lease = if integration_root.exists() || stored.has_integrations() {
            Some(
                couch_integrations::Store::new(integration_root)
                    .read_lease()
                    .map_err(|e| Error::Compatibility(e.to_string()))?,
            )
        } else {
            None
        };
        let json = serde_json::to_vec_pretty(&stored).map_err(Error::Parse)?;
        if stored.has_integrations() {
            // A failed preparation changes neither the live config nor the
            // confirmed export. An interrupted main rename leaves this clearly
            // labelled candidate; it is never restored automatically.
            let export = serde_json::to_vec_pretty(&self.config).map_err(Error::Parse)?;
            write_private(&self.recovery_pending_path(), &export)?;
        }
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        // Durably record the rename too, not just the bytes it points at.
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                if let Ok(handle) = fs::File::open(dir) {
                    let _ = handle.sync_all();
                }
            }
        }
        if stored.has_integrations() {
            self.confirm_recovery();
        }
        Ok(())
    }
}

/// Write a credential file at 0600, atomically. The migration used a fixed
/// name with `create_new`, so one battery pull between the open and the rename
/// left the temp on the rootfs and every later `Store::open` failed
/// `AlreadyExists` - which `main` turns into `exit(1)`, so the web UI was gone
/// until somebody got a shell. Same shape as `access::atomic` in couch-system.
fn write_private(target: &Path, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // A PID can recur after reboot. Leave interrupted files alone and choose a
    // free name, so a battery pull cannot permanently block future saves.
    let mut attempt = 0u64;
    let (temp, mut file) = loop {
        let suffix = if attempt == 0 {
            format!("migrate-{}", std::process::id())
        } else {
            format!("migrate-{}-{attempt}", std::process::id())
        };
        let temp = target.with_extension(suffix);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => return Err(error),
        }
    };
    let result = (|| -> io::Result<()> {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temp, target)?;
        fs::File::open(target.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_model::Id;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("couch-confd-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir.join("config.json")
    }

    #[test]
    fn a_missing_file_starts_empty_and_persists() {
        let path = scratch("empty");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.config(), &Config::default());
        assert!(path.exists());
        // And it reloads to the same thing.
        let again = Store::open(&path).unwrap();
        assert_eq!(again.config(), store.config());
    }

    #[test]
    fn existing_configuration_is_preserved_byte_for_byte() {
        let path = scratch("existing");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut config = Config::seed();
        config.revision = 17;
        config.rooms[0].name = "My existing room".into();
        let original = serde_json::to_vec_pretty(&config).unwrap();
        fs::write(&path, &original).unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.config(), &config);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_reset() {
        let path = scratch("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(Store::open(&path), Err(Error::Parse(_))));
    }

    #[test]
    fn a_rejected_edit_changes_nothing() {
        let path = scratch("reject");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&Config::seed()).unwrap()).unwrap();
        let mut store = Store::open(&path).unwrap();
        let before = store.config().clone();
        let err = store
            .mutate(None, |cfg| cfg.rooms.push(cfg.rooms[0].clone()))
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
        assert_eq!(store.config().revision, before.revision);
        assert_eq!(store.config().rooms.len(), before.rooms.len());
        assert_eq!(Store::open(&path).unwrap().config(), &before);
    }

    #[test]
    fn revisions_advance_and_gate_writes() {
        let path = scratch("revision");
        let mut store = Store::open(&path).unwrap();
        let r0 = store.revision();
        store
            .mutate(Some(r0), |cfg| cfg.remove_room(&Id::new("loft")))
            .unwrap();
        assert_eq!(store.revision(), r0 + 1);
        let err = store.mutate(Some(r0), |_| {}).unwrap_err();
        assert!(matches!(err, Error::Stale { .. }));
    }

    #[test]
    fn an_old_bluetooth_tv_connection_becomes_a_device_bond_on_open_and_on_edit() {
        let path = scratch("bt-migrate");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"schema_version":1,"revision":7,
            "connections":[{"id":"bt","name":"Bluetooth TV","provider":{"kind":"bluetooth-tv"}}],
            "rooms":[{"id":"r","name":"R","devices":[{"id":"tv","name":"Bedroom TV","kind":"tv","integration":{"via":"connection","connection_id":"bt"}}]}]}"#).unwrap();
        let store = Store::open(&path).unwrap();
        assert!(store.config().connections.is_empty());
        let tv = &store.config().rooms[0].devices[0];
        assert_eq!(tv.integration, couch_model::Integration::None);
        assert_eq!(
            tv.bluetooth.as_ref().map(|b| b.name.as_str()),
            Some("Bedroom TV")
        );
        assert_eq!(store.revision(), 8, "rewritten once, with a new revision");
        // The file on disk is in the new shape: a second open changes nothing.
        let again = Store::open(&path).unwrap();
        assert_eq!(again.revision(), 8);
        assert!(!fs::read_to_string(&path).unwrap().contains("bluetooth-tv"));
        // An edit that brings the old shape back (a whole-config PUT of an
        // export) lands in the new one too.
        let mut store = again;
        store
            .mutate(None, |cfg| {
                cfg.rooms[0].devices[0].integration = couch_model::Integration::BluetoothTv;
            })
            .unwrap();
        assert_eq!(
            store.config().rooms[0].devices[0].integration,
            couch_model::Integration::None
        );
    }

    #[test]
    fn a_legacy_bright_command_loads_and_is_rewritten_once() {
        // The shape of a `.130`-era file (`build/webui-review-empty.json`):
        // `Store::open` used to validate this before it ever reached
        // `migrate`, so it refused to start at all.
        let path = scratch("bright-migrate");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"schema_version":1,"revision":22,"connections":[],
            "rooms":[{"id":"kitchen","name":"Kitchen","devices":[{"id":"kitchen-hue","name":"Kitchen light","kind":"light","integration":{"via":"hue","light_id":"1"}}]}],
            "scenes":[{"id":"dinner","name":"Dinner","steps":[{"device":"kitchen-hue","command":"bright"}]}]}"#).unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.config().scenes[0].steps[0].command, "dim:100");
        assert_eq!(store.revision(), 23, "rewritten once, with a new revision");
        assert!(!fs::read_to_string(&path).unwrap().contains("bright"));
        // The file on disk is already in the new shape: a second open leaves
        // the revision alone.
        let again = Store::open(&path).unwrap();
        assert_eq!(again.revision(), 23);
    }

    #[test]
    fn writes_leave_no_temp_file_behind() {
        let path = scratch("atomic");
        let mut store = Store::open(&path).unwrap();
        store
            .mutate(None, |cfg| cfg.remove_room(&Id::new("porch")))
            .unwrap();
        assert!(!path.with_extension("json.tmp").exists());
    }
}

#[cfg(test)]
mod connection_migration_tests {
    use super::*;
    #[test]
    fn paired_bridge_is_adopted_once_without_touching_credentials() {
        let dir =
            std::env::temp_dir().join(format!("couch-connection-migration-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let credential = dir.join("hue-connection.json");
        fs::write(&path, br#"{"schema_version":1,"revision":3,"rooms":[]}"#).unwrap();
        fs::write(&credential, b"private fixture").unwrap();
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.revision(), 4);
        assert_eq!(store.config().connections.len(), 1);
        store.mutate(Some(4), |c| c.connections.clear()).unwrap();
        drop(store);
        assert!(Store::open(&path).unwrap().config().connections.is_empty());
        assert_eq!(fs::read(&credential).unwrap(), b"private fixture");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod scoped_credentials_tests {
    use super::*;
    #[test]
    fn a_temp_left_by_an_interrupted_migration_does_not_block_startup() {
        let dir =
            std::env::temp_dir().join(format!("couch-migration-leftover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("connections/bridge")).unwrap();
        let mut config = Config::default();
        config.connections.push(couch_model::Connection {
            id: "bridge".into(),
            name: "Philips Hue".into(),
            provider: couch_model::Provider::LegacyHue,
        });
        fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        fs::write(dir.join("hue-connection.json"), b"private-pairing").unwrap();
        // What a battery pull between the open and the rename used to leave.
        fs::write(
            dir.join("connections/bridge/hue-connection.migrate-new"),
            b"half",
        )
        .unwrap();
        fs::write(dir.join("connection-legacy-map.new"), b"half").unwrap();
        Store::open(dir.join("config.json")).unwrap();
        assert_eq!(
            fs::read(dir.join("connections/bridge/hue-connection.json")).unwrap(),
            b"private-pairing"
        );
        assert!(dir.join("connection-legacy-map.json").is_file());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn legacy_pairing_is_copied_once_and_never_reassigned_to_second_tv() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("couch-multi-migration-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut config = Config::default();
        for id in ["tv-a", "tv-b"] {
            config.connections.push(couch_model::Connection {
                id: id.into(),
                name: id.into(),
                provider: couch_model::Provider::WebOs,
            });
        }
        fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        fs::write(dir.join("webos-connection.json"), b"private-pairing").unwrap();
        let mut store = Store::open(dir.join("config.json")).unwrap();
        let first = dir.join("connections/tv-a/webos-connection.json");
        assert_eq!(fs::read(&first).unwrap(), b"private-pairing");
        assert_eq!(
            fs::metadata(first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!dir.join("connections/tv-b/webos-connection.json").exists());
        store
            .mutate(None, |c| {
                c.connections.remove(0);
            })
            .unwrap();
        drop(store);
        Store::open(dir.join("config.json")).unwrap();
        assert!(!dir.join("connections/tv-b/webos-connection.json").exists());
        assert_eq!(
            fs::read(dir.join("webos-connection.json")).unwrap(),
            b"private-pairing"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod integration_storage_tests {
    use super::*;
    use couch_model::{Connection, Id, Integration, Provider};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "couch-integration-storage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("runtime")).unwrap();
            fs::write(path.join("build.json"), "{}").unwrap();
            fs::write(path.join("runtime/previous"), "base").unwrap();
            fs::write(path.join("runtime/pending"), "candidate").unwrap();
            Self(path)
        }
        fn open(&self) -> Store {
            Store::open(self.0.join("config.json")).unwrap()
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn plugin(config: &mut Config) {
        *config = Config::seed();
        config.connections.push(Connection {
            id: Id::new("external"),
            name: "External".into(),
            provider: Provider::Plugin {
                id: "echo".into(),
                label: "Echo".into(),
                capabilities: vec![],
                supports_inputs: false,
                presentation: vec![],
                actions: vec![],
                children: vec![],
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: Id::new("external"),
            resource_id: "zone1".into(),
            child: None,
        };
    }
    #[test]
    fn first_upgrade_preserves_old_runtime_configuration_and_confirms_recovery() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        let mut store = home.open();
        store.mutate(Some(0), plugin).unwrap();
        let rollback: Config = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        rollback.validate().unwrap();
        assert_eq!(rollback.rooms[0].devices[0].integration, Integration::None);
        assert!(rollback.connection(&Id::new("external")).is_none());
        assert_eq!(home.open().config(), store.config());
        assert_eq!(
            store.integration_recovery_config().unwrap().as_ref(),
            Some(store.config())
        );
        assert!(!store.recovery_pending_path().exists());
        assert_eq!(
            fs::metadata(store.recovery_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(home.0.join("runtime/previous")).unwrap(),
            "base"
        );
    }
    #[test]
    fn legacy_rewrites_preserve_edits_and_deletions_without_resurrecting_plugins() {
        let home = Home::new();
        let mut store = home.open();
        store.mutate(None, plugin).unwrap();
        let complete = store.config().clone();
        // An old daemon reads the projection and drops the unknown extension
        // while saving. Model a valid whole-config edit that deletes devices.
        let legacy = Config {
            rooms: vec![couch_model::Room {
                id: Id::new("kept"),
                name: "Renamed while rolled back".into(),
                icon: None,
                devices: vec![],
            }],
            revision: complete.revision + 1,
            ..Config::default()
        };
        fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
        let restored = home.open();
        assert_eq!(restored.config(), &legacy);
        assert_eq!(
            restored.integration_recovery_config().unwrap(),
            Some(complete)
        );
        assert!(!restored.integration_recovery().unwrap().integrations_active);
    }
    #[test]
    fn interrupted_candidate_never_replaces_confirmed_export_without_matching_commit() {
        let home = Home::new();
        let mut store = home.open();
        store.mutate(None, plugin).unwrap();
        let confirmed = store.config().clone();
        let mut candidate = confirmed.clone();
        candidate.revision += 1;
        candidate.rooms[0].name = "Attempted edit".into();
        write_private(
            &store.recovery_pending_path(),
            &serde_json::to_vec(&candidate).unwrap(),
        )
        .unwrap();
        let reopened = home.open();
        assert_eq!(reopened.config(), &confirmed);
        assert_eq!(
            reopened.integration_recovery_config().unwrap(),
            Some(confirmed)
        );
        assert!(reopened
            .integration_recovery()
            .unwrap()
            .pending_path
            .is_some());
        fs::write(
            store.path(),
            serde_json::to_vec(&StoredConfig::new(&candidate)).unwrap(),
        )
        .unwrap();
        let reopened = home.open();
        assert_eq!(reopened.config(), &candidate);
        assert_eq!(
            reopened.integration_recovery_config().unwrap(),
            Some(candidate)
        );
        assert!(!reopened.recovery_pending_path().exists());
    }
    #[test]
    fn failed_recovery_preparation_preserves_disk_memory_and_revision() {
        let home = Home::new();
        let mut store = home.open();
        let original = store.config().clone();
        let bytes = fs::read(store.path()).unwrap();
        fs::create_dir(store.recovery_pending_path()).unwrap();
        assert!(store.mutate(None, plugin).is_err());
        assert_eq!(store.config(), &original);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
    #[test]
    fn interrupted_temporary_export_does_not_block_a_later_save() {
        let home = Home::new();
        let mut store = home.open();
        let stale = store
            .recovery_pending_path()
            .with_extension(format!("migrate-{}", std::process::id()));
        fs::write(&stale, b"interrupted export").unwrap();
        store.mutate(None, plugin).unwrap();
        assert_eq!(
            store.integration_recovery_config().unwrap().as_ref(),
            Some(store.config())
        );
        assert_eq!(fs::read(stale).unwrap(), b"interrupted export");
    }
    #[test]
    fn earlier_plugin_files_migrate_without_revision_changes_or_installed_packages() {
        let home = Home::new();
        let mut config = Config::default();
        plugin(&mut config);
        config.revision = 19;
        fs::write(
            home.0.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let store = home.open();
        assert_eq!(store.config(), &config);
        let legacy: Config = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert!(legacy
            .connections
            .iter()
            .all(|c| !matches!(c.provider, Provider::Plugin { .. })));
        assert_eq!(home.open().revision(), 19);
        assert!(fs::read_dir(home.0.join("integrations/slots"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn package_admission_excludes_config_commits_without_losing_an_edit() {
        use std::os::fd::AsRawFd;
        let home = Home::new();
        let mut store = home.open();
        store.mutate(None, plugin).unwrap();
        let before = store.config().clone();
        let bytes = fs::read(store.path()).unwrap();
        let lock = fs::File::open(home.0.join("integrations/.lock")).unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(matches!(
            store.mutate(None, |cfg| cfg.rooms[0].name = "Later".into()),
            Err(Error::Compatibility(_))
        ));
        assert_eq!(store.config(), &before);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        drop(lock);
        store
            .mutate(None, |cfg| cfg.rooms[0].name = "Later".into())
            .unwrap();
        assert_eq!(home.open().config().rooms[0].name, "Later");
    }
}
