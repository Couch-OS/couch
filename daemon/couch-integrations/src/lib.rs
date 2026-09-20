//! Verified, isolated lifecycle manager for integration APKs.
//!
//! `apk` runs only in a fresh private root. It verifies package signatures and
//! extracts there with scripts and networking disabled; Couch then admits only
//! one regular-file integration payload into its own versioned store.

mod feed;
pub mod management;
pub use couch_plugin::Manifest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_ROOT: &str = "/opt/couch/integrations";
/// Every installed package's own user, at the store root rather than under
/// `state/`, where each entry is a package id, or inside a selection file,
/// which is `deny_unknown_fields`. An older Couch reads neither: its recovery
/// pass deletes only `.staging-*` here and its listing reads only `state/`, so
/// this file is simply ignored after a rollback and the packages run as they
/// did before, all under one user.
pub const IDENTITY_FILE: &str = "uids.json";
/// Well above every Alpine system account in the OS image and well below
/// 65534, so a package's user can never be mistaken for `nobody` or collide
/// with one the image adds later.
const FIRST_PACKAGE_UID: u32 = 60000;
const LAST_PACKAGE_UID: u32 = 64999;
/// Trust only the Couch integration signing key by default. Alpine's system
/// repository keys authorize OS packages and must not also authorize plugins.
pub const DEFAULT_KEYS_DIR: &str = "/opt/couch/integration-keys/official";
pub const PROTOCOL_VERSION: u32 = couch_plugin::PROTOCOL_VERSION;
const MAX_APK_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 512;
// A catalog integrity read may overlap a user install/remove request. Wait
// before starting the mutation, but never retry a partially executed operation.
const MUTATION_LOCK_WAIT: Duration = Duration::from_secs(3);
// Reading which user a package runs as is on the path a key press takes, and
// its budget is a press's, not an install's.
const IDENTITY_READ_WAIT: Duration = Duration::from_millis(250);
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);
impl Error {
    /// Store contention is transient and must not be reported as an invalid
    /// package by request paths that can wait briefly before device I/O.
    pub fn is_busy(&self) -> bool {
        self.0 == STORE_BUSY
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
fn err(s: impl Into<String>) -> Error {
    Error(s.into())
}
fn io(s: &str, e: std::io::Error) -> Error {
    err(format!("{s}: {e}"))
}
const STORE_BUSY: &str = "integration store is busy";
fn busy() -> Error {
    err(STORE_BUSY)
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledIntegration {
    pub manifest: Manifest,
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    version: String,
    sha256: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    active: Option<Slot>,
    previous: Option<Slot>,
}
/// Which user each installed package runs as.
///
/// A table and not a hash of the id: two packages must never share a user, and
/// a custom repository could otherwise name a package so that it hashes onto
/// the user of a package already installed. `next` only ever moves forward, so
/// a removed package keeps its row and a later package with the same id cannot
/// inherit a user something else once ran as.
/// Deliberately not `Default`: an empty table starts at the first user of the
/// range, never at zero, and [`Identities::fresh`] is the only way to make one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identities {
    next: u32,
    packages: std::collections::BTreeMap<String, u32>,
}
impl Identities {
    fn fresh() -> Self {
        Self {
            next: FIRST_PACKAGE_UID,
            packages: std::collections::BTreeMap::new(),
        }
    }
    /// A table Couch did not write, or wrote and then lost half of, is
    /// replaced rather than repaired. Nothing on disk is owned by these users,
    /// so the worst a rebuild costs is that a package's running children are
    /// replaced by children under a different user.
    fn usable(&self) -> bool {
        (FIRST_PACKAGE_UID..=LAST_PACKAGE_UID.saturating_add(1)).contains(&self.next)
            && self.packages.len() <= (LAST_PACKAGE_UID - FIRST_PACKAGE_UID + 1) as usize
            && self.packages.keys().all(|id| valid_component(id))
            && self
                .packages
                .values()
                .all(|uid| (FIRST_PACKAGE_UID..self.next).contains(uid))
            && {
                let mut seen: Vec<u32> = self.packages.values().copied().collect();
                seen.sort_unstable();
                let before = seen.len();
                seen.dedup();
                seen.len() == before
            }
    }
}

/// The file a feed's signed metadata promises for a package: checked after
/// the download and before apk is asked to open it.
#[derive(Clone, Debug)]
pub(crate) struct ExpectedApk {
    file: String,
    size: u64,
    sha256: String,
}
impl ExpectedApk {
    /// apk checks the package's signature next; this is what ties the file to
    /// the feed that was current when it was chosen, not only to the key.
    fn check(&self, package: &Path) -> Result<()> {
        let mismatch = || {
            err("The downloaded package is not the one the package feed's signed metadata describes")
        };
        let file = File::open(package).map_err(|e| io("read fetched APK", e))?;
        let size = file
            .metadata()
            .map_err(|e| io("read fetched APK", e))?
            .len();
        if package.file_name() != Some(OsStr::new(&self.file)) || size != self.size {
            return Err(mismatch());
        }
        let mut hash = Sha256::new();
        std::io::copy(&mut file.take(MAX_APK_BYTES), &mut hash)
            .map_err(|e| io("read fetched APK", e))?;
        if !self
            .sha256
            .eq_ignore_ascii_case(&format!("{:x}", hash.finalize()))
        {
            return Err(mismatch());
        }
        Ok(())
    }
}
/// Hold this lease while changing settings/configuration that package admission reads.
pub struct ReadLease {
    _lock: Lock,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    apk: PathBuf,
    keys_dir: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            apk: std::env::var_os("COUCH_APK")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("apk")),
            keys_dir: PathBuf::from(DEFAULT_KEYS_DIR),
        }
    }
    pub fn from_environment() -> Self {
        Self::new(
            std::env::var_os("COUCH_INTEGRATIONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT)),
        )
    }
    pub fn with_apk(mut self, path: impl Into<PathBuf>) -> Self {
        self.apk = path.into();
        self
    }
    pub fn with_keys_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.keys_dir = path.into();
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn read_lease(&self) -> Result<ReadLease> {
        self.layout()?;
        Ok(ReadLease {
            _lock: Lock::acquire_shared_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT)?,
        })
    }
    /// The user and group one package's children run as, allocating it if this
    /// package has never run before.
    ///
    /// A package that already has a row is answered under the shared lock, so
    /// a key press never waits for an exclusive one; only the first sight of a
    /// package takes it. There is no fallback: a store that cannot say who a
    /// package is is busy or out of users, and the caller refuses the request
    /// rather than start it as somebody else.
    ///
    /// Callers that hold a lease must ask before taking it. The daemon does,
    /// and the startup pass means a lease holder finds every installed package
    /// already allocated.
    ///
    /// This allocates for any well-formed id, because admission asks for a
    /// package's user before that package is selected. A caller answering a
    /// browser asks whether the package is installed first, so a stream of
    /// invented names cannot use the range up.
    pub fn identity(&self, id: &str) -> Result<(u32, u32)> {
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        self.layout()?;
        {
            // The answer a package that has run before gets, and the one on
            // the path a key press takes. Its bound is the caller's request
            // budget, not the one a store mutation may wait.
            let _lock = Lock::acquire_shared_wait(self.root.join(".lock"), IDENTITY_READ_WAIT)?;
            if let Some(uid) = self.identities().packages.get(id) {
                return Ok((*uid, *uid));
            }
        }
        let _lock = Lock::acquire_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT, libc::LOCK_EX)?;
        self.identity_locked(id)
    }
    /// Give every installed package a user once, at daemon start, so the first
    /// key press of the day finds a table it only has to read. Packages
    /// installed before this Couch have no row and would otherwise each take
    /// the exclusive lock at the moment they are first used.
    pub fn assign_identities(&self) -> Result<()> {
        self.layout()?;
        let _lock = Lock::acquire_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT, libc::LOCK_EX)?;
        let mut table = self.identities();
        let installed: Vec<String> = fs::read_dir(self.root.join("state"))
            .map_err(|e| io("read integration selections", e))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|id| valid_component(id))
            .collect();
        let mut changed = false;
        for id in installed {
            if table.packages.contains_key(&id) {
                continue;
            }
            allocate(&mut table, &id)?;
            changed = true;
        }
        if changed {
            self.write_identities(&table)?;
        }
        Ok(())
    }
    /// The table as it should be read, under a lock the caller already holds.
    /// An absent, unreadable or inconsistent table reads as an empty one and
    /// is rewritten by the next allocation.
    fn identities(&self) -> Identities {
        let path = self.root.join(IDENTITY_FILE);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Identities::fresh();
        };
        if !metadata.is_file()
            || metadata.len() > 512 * 1024
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Identities::fresh();
        }
        match fs::read(&path).map(|bytes| serde_json::from_slice::<Identities>(&bytes)) {
            Ok(Ok(table)) if table.usable() => table,
            _ => Identities::fresh(),
        }
    }
    fn write_identities(&self, table: &Identities) -> Result<()> {
        atomic_write(
            &self.root.join(IDENTITY_FILE),
            &serde_json::to_vec(table).map_err(|e| err(e.to_string()))?,
        )?;
        // The table is not a secret and every reader of it is root; what
        // matters is that no other user may write it.
        fs::set_permissions(
            self.root.join(IDENTITY_FILE),
            fs::Permissions::from_mode(0o644),
        )
        .map_err(|e| io("protect integration user table", e))
    }
    fn identity_locked(&self, id: &str) -> Result<(u32, u32)> {
        let mut table = self.identities();
        if let Some(uid) = table.packages.get(id) {
            return Ok((*uid, *uid));
        }
        let uid = allocate(&mut table, id)?;
        self.write_identities(&table)?;
        Ok((uid, uid))
    }
    /// A local sideload remains authenticated: it must be signed by a key in
    /// `keys_dir`. This is intentionally not an `--allow-untrusted` escape.
    pub fn install_sideload(&self, package: &Path) -> Result<InstalledIntegration> {
        self.install(package)
    }
    /// Use after fetching from an authenticated repository. APK verification is
    /// repeated here to defend the boundary between repository and local store.
    pub fn install_repository_apk(&self, package: &Path) -> Result<InstalledIntegration> {
        self.install(package)
    }
    pub fn install_repository(
        &self,
        package: &str,
        repository: &str,
    ) -> Result<InstalledIntegration> {
        self.fetch_repository(package, repository, None, None)
    }
    fn trust_keys(&self) -> Result<PathBuf> {
        if self.keys_dir != Path::new(DEFAULT_KEYS_DIR) {
            return Ok(self.keys_dir.clone());
        }
        // The publisher key travels inside the signed core executable, avoiding
        // a new OTA payload filename that old updaters would reject. Custom
        // feeds always supply their own scoped directory.
        let path = self.root.join(".official-keys");
        fs::create_dir_all(&path).map_err(|e| io("create official trust directory", e))?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| io("inspect official trust directory", e))?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
            return Err(err("invalid official trust directory"));
        }
        // No additional key files are admitted to the built-in trust domain.
        for entry in fs::read_dir(&path).map_err(|e| io("inspect official keys", e))? {
            let entry = entry.map_err(|e| io("inspect official key", e))?;
            if entry.file_name() != "couch-integrations.rsa.pub" {
                return Err(err("unexpected key in official trust directory"));
            }
        }
        atomic_write(
            &path.join("couch-integrations.rsa.pub"),
            include_bytes!("official.rsa.pub"),
        )?;
        Ok(path)
    }
    fn fetch_repository(
        &self,
        package: &str,
        repository: &str,
        expected: Option<(&str, &str)>,
        promised: Option<&ExpectedApk>,
    ) -> Result<InstalledIntegration> {
        let valid_package = package.split_once('=').map_or_else(
            || valid_component(package),
            |(name, version)| valid_component(name) && valid_version(version),
        );
        if !valid_package || repository.is_empty() || repository.contains('\n') {
            return Err(err("invalid repository package or URL"));
        }
        self.layout()?;
        let fetch = self
            .root
            .join(format!(".fetch-{}-{}", std::process::id(), nonce()));
        fs::create_dir(&fetch).map_err(|e| io("create repository fetch directory", e))?;
        let repositories = fetch.join("repositories");
        fs::write(&repositories, format!("{repository}\n"))
            .map_err(|e| io("write repository selection", e))?;
        let mut command = Command::new(&self.apk);
        command
            .arg("--keys-dir")
            .arg(self.trust_keys()?)
            .arg("--repositories-file")
            .arg(&repositories)
            .arg("--no-cache")
            .arg("fetch")
            .arg("--output")
            .arg(&fetch)
            .arg(package);
        if let Err(error) = run_bounded(&mut command, Duration::from_secs(120)) {
            let _ = fs::remove_dir_all(&fetch);
            return Err(error);
        }
        let candidates: Vec<_> = fs::read_dir(&fetch)
            .map_err(|e| io("read fetched APK", e))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "apk"))
            .collect();
        if candidates.len() != 1 {
            let _ = fs::remove_dir_all(&fetch);
            return Err(err("repository fetch did not produce exactly one APK"));
        }
        let result = promised
            .map_or(Ok(()), |promised| promised.check(&candidates[0]))
            .and_then(|_| self.install_expected(&candidates[0], expected));
        let _ = fs::remove_dir_all(&fetch);
        result
    }
    pub fn install(&self, package: &Path) -> Result<InstalledIntegration> {
        self.install_expected(package, None)
    }
    fn install_expected(
        &self,
        package: &Path,
        expected: Option<(&str, &str)>,
    ) -> Result<InstalledIntegration> {
        let package = package
            .canonicalize()
            .map_err(|e| io("resolve APK path", e))?;
        self.layout()?;
        let _lock = Lock::acquire_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT, libc::LOCK_EX)?;
        self.recover()?;
        let meta = fs::metadata(&package).map_err(|e| io("read APK metadata", e))?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_APK_BYTES {
            return Err(err("APK is not a regular file within the size limit"));
        }
        let staging = self
            .root
            .join(format!(".staging-{}-{}", std::process::id(), nonce()));
        fs::create_dir(&staging).map_err(|e| io("create private staging root", e))?;
        let result = self
            .run_apk(&package, &staging)
            .and_then(|_| self.admit_expected(&staging, expected));
        let _ = fs::remove_dir_all(&staging);
        result
    }
    pub fn list(&self) -> Result<Vec<Manifest>> {
        self.layout()?;
        let _lock = Lock::acquire_shared_wait(self.root.join(".lock"), Duration::ZERO)?;
        let mut result = Vec::new();
        for entry in fs::read_dir(self.root.join("state"))
            .map_err(|e| io("read integration selections", e))?
        {
            let entry = entry.map_err(|e| io("read active pointer", e))?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if let Ok((_, manifest)) = self.resolve_locked(&id) {
                result.push(manifest);
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }
    pub fn resolve(&self, id: &str) -> Result<(PathBuf, Manifest)> {
        self.resolve_wait(id, Duration::ZERO)
    }
    /// Resolve an installed payload after waiting briefly for an in-progress
    /// store operation. The bound belongs to the caller's request budget; APK
    /// mutations wait separately for exclusive access before making any change.
    pub fn resolve_wait(&self, id: &str, wait: Duration) -> Result<(PathBuf, Manifest)> {
        self.layout()?;
        let _lock = Lock::acquire_shared_wait(self.root.join(".lock"), wait)?;
        self.resolve_locked(id)
    }
    /// Cheap activation token for a running host. Full payload integrity is
    /// checked by resolve before launching; commands need only detect a change
    /// of the immutable selected version, not reread megabytes on every key.
    pub fn generation(&self, id: &str) -> Result<String> {
        let state = self.selection(id)?;
        if state.active.is_none() {
            return Err(err("integration is not installed"));
        }
        serde_json::to_string(&state).map_err(|e| err(e.to_string()))
    }
    pub fn rollback(&self, id: &str) -> Result<Manifest> {
        self.layout()?;
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        let _lock = Lock::acquire_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT, libc::LOCK_EX)?;
        self.recover()?;
        let state = self.selection(id)?;
        let previous = state
            .previous
            .ok_or_else(|| err("no previous version is available"))?;
        self.slot(id, &previous)?;
        let path = self.slot_path(id, &previous.version);
        let manifest = read_manifest(&path.join("manifest.json"))?;
        let (uid, gid) = self.identity_locked(id)?;
        let mut candidate = couch_plugin::Host::spawn_with_policy(
            &path,
            &manifest,
            Duration::from_secs(3),
            couch_plugin::HostPolicy::for_package(uid, gid),
        )
        .map_err(|e| err(format!("rollback integration handshake failed: {e}")))?;
        self.check_saved_settings(&mut candidate, &manifest)?;
        drop(candidate);
        self.select(
            id,
            &Selection {
                active: Some(previous),
                previous: state.active,
            },
        )?;
        self.resolve_locked(id).map(|(_, manifest)| manifest)
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        self.layout()?;
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        let _lock = Lock::acquire_wait(self.root.join(".lock"), MUTATION_LOCK_WAIT, libc::LOCK_EX)?;
        self.recover()?;
        self.select(id, &Selection::default())?;
        let path = self.root.join("slots").join(id);
        if path.is_dir() {
            fs::remove_dir_all(path).map_err(|e| io("remove integration slots", e))?;
            sync_dir(&self.root.join("slots"))?;
        }
        Ok(())
    }
    fn layout(&self) -> Result<()> {
        for name in ["slots", "state"] {
            fs::create_dir_all(self.root.join(name))
                .map_err(|e| io("create integration store", e))?;
            let meta = fs::symlink_metadata(self.root.join(name))
                .map_err(|e| io("inspect store directory", e))?;
            if !meta.is_dir() || meta.permissions().mode() & 0o022 != 0 {
                return Err(err(
                    "integration store directory must not be a link or writable by other users",
                ));
            }
        }
        let meta = fs::symlink_metadata(&self.root).map_err(|e| io("inspect store root", e))?;
        if !meta.is_dir() || meta.permissions().mode() & 0o022 != 0 {
            return Err(err(
                "integration store root must be a private writable directory",
            ));
        }
        sync_dir(&self.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    }
    fn run_apk(&self, package: &Path, staging: &Path) -> Result<()> {
        preflight_apk(package)?;
        let mut command = Command::new(&self.apk);
        command
            .arg("--root")
            .arg(staging)
            .arg("--keys-dir")
            .arg(self.trust_keys()?)
            .arg("--repositories-file")
            .arg("/dev/null")
            .arg("--no-network")
            .arg("--no-scripts")
            .arg("--initdb")
            .arg("add")
            .arg("--force-non-repository")
            .arg(package);
        run_bounded(&mut command, Duration::from_secs(60))
    }
    fn admit_expected(
        &self,
        staging: &Path,
        expected: Option<(&str, &str)>,
    ) -> Result<InstalledIntegration> {
        let base = staging.join("usr/lib/couch/integrations");
        let ids = directories(&base)?;
        if ids.len() != 1 || !valid_component(&ids[0]) {
            return Err(err(
                "APK must contain exactly one valid integration directory",
            ));
        }
        let id = &ids[0];
        self.audit(staging, id)?;
        let package_root = base.join(id);
        let manifest = read_manifest(&package_root.join("manifest.json"))?;
        validate_manifest(&manifest)?;
        if manifest.id != *id || !valid_version(&manifest.version) {
            return Err(err("manifest id does not match package directory"));
        }
        if expected.is_some_and(|(id, version)| manifest.id != id || manifest.version != version) {
            return Err(err(
                "package manifest differs from the selected repository identity",
            ));
        }
        let executable = package_root.join(&manifest.executable);
        if !fs::metadata(&executable)
            .map_err(|_| err("manifest executable is absent"))?
            .is_file()
        {
            return Err(err("manifest executable is not a regular file"));
        }
        // The user this package will run as from now on, allocated before the
        // version it came with is admitted, so the check below is made by a
        // child of exactly the identity the daemon will start later.
        let (uid, gid) = self.identity_locked(id)?;
        let mut candidate = couch_plugin::Host::spawn_with_policy(
            &package_root,
            &manifest,
            Duration::from_secs(3),
            couch_plugin::HostPolicy::for_package(uid, gid),
        )
        .map_err(|e| err(format!("integration handshake failed: {e}")))?;
        self.check_saved_settings(&mut candidate, &manifest)?;
        drop(candidate);
        let digest = tree_digest(&package_root)?;
        sync_tree(&package_root)?;
        let final_path = self.slot_path(id, &manifest.version);
        fs::create_dir_all(final_path.parent().unwrap())
            .map_err(|e| io("create slot parent", e))?;
        if final_path.exists() {
            audit_payload(&final_path)?;
            let existing = tree_digest(&final_path)?;
            if existing != digest {
                return Err(err(
                    "refusing to replace an existing version with different contents",
                ));
            }
        } else {
            fs::rename(&package_root, &final_path).map_err(|e| io("publish package slot", e))?;
            sync_dir(final_path.parent().unwrap())?;
            sync_dir(&self.root.join("slots"))?;
        }
        let slot = Slot {
            version: manifest.version.clone(),
            sha256: digest.clone(),
        };
        let state = self.selection(id)?;
        if state.active.as_ref() != Some(&slot) {
            // Keep the actually usable version when repairing an incompatible
            // or corrupt active selection after a core rollback.
            let previous = [state.active, state.previous]
                .into_iter()
                .flatten()
                .find(|old| old != &slot && self.slot(id, old).is_ok());
            self.select(
                id,
                &Selection {
                    active: Some(slot),
                    previous,
                },
            )?;
        }
        Ok(InstalledIntegration {
            manifest,
            path: final_path,
            sha256: digest,
        })
    }
    fn audit(&self, staging: &Path, id: &str) -> Result<()> {
        let allow = [
            Path::new("usr/lib/couch/integrations").join(id),
            PathBuf::from("lib/apk"),
            PathBuf::from("etc/apk"),
        ];
        let mut files = 0;
        let mut bytes = 0;
        audit_tree(staging, Path::new(""), &allow, &mut files, &mut bytes, true)
    }
    fn check_saved_settings(
        &self,
        host: &mut couch_plugin::Host,
        manifest: &Manifest,
    ) -> Result<()> {
        // The integration store sits beside config.json and its private
        // connections directory. Package updates cannot migrate those files.
        // Validate every existing configuration before switching versions.
        let home = self.root.parent().unwrap_or_else(|| Path::new("."));
        let config_path = home.join("config.json");
        let bytes = match fs::read(&config_path) {
            Ok(bytes) if bytes.len() <= 1024 * 1024 => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            _ => return Err(err("cannot inspect existing integration configuration")),
        };
        let stored: couch_model::StoredConfig = serde_json::from_slice(&bytes)
            .map_err(|_| err("cannot parse existing integration configuration"))?;
        let config = stored.into_config().map_err(err)?;
        config
            .validate()
            .map_err(|_| err("existing integration configuration is invalid"))?;
        for connection in &config.connections {
            if !matches!(&connection.provider, couch_model::Provider::Plugin { id, .. } if id == &manifest.id)
            {
                continue;
            }
            let id = connection.id.as_str();
            if !valid_component(id) {
                return Err(err("invalid saved connection identifier"));
            }
            let path = home
                .join("connections")
                .join(id)
                .join("plugin-connection.json");
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(err("cannot read existing integration settings")),
            };
            if !metadata.is_file() || metadata.len() > 64 * 1024 {
                return Err(err("invalid existing integration settings file"));
            }
            let value = serde_json::from_reader(
                File::open(path).map_err(|_| err("cannot open existing integration settings"))?,
            )
            .map_err(|_| err("cannot parse existing integration settings"))?;
            host.configure(value).map_err(|_| err("candidate integration cannot read the saved connection settings; keep the current version or update the settings first"))?;
        }
        Ok(())
    }
    fn resolve_locked(&self, id: &str) -> Result<(PathBuf, Manifest)> {
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        let state = self.selection(id)?;
        for slot in [state.active, state.previous].into_iter().flatten() {
            if let Ok(path) = self.slot(id, &slot) {
                return Ok((path.clone(), read_manifest(&path.join("manifest.json"))?));
            }
        }
        Err(err(
            "integration is not installed or no compatible version remains",
        ))
    }
    fn slot_path(&self, id: &str, version: &str) -> PathBuf {
        self.root.join("slots").join(id).join(version)
    }
    fn slot(&self, id: &str, slot: &Slot) -> Result<PathBuf> {
        if !valid_component(id)
            || !valid_version(&slot.version)
            || slot.sha256.len() != 64
            || !slot.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(err("invalid integration pointer"));
        }
        let path = self.slot_path(id, &slot.version);
        audit_payload(&path)?;
        let manifest = read_manifest(&path.join("manifest.json"))?;
        validate_manifest(&manifest)?;
        if manifest.id != id || manifest.version != slot.version {
            return Err(err("slot manifest does not match its pointer"));
        }
        let file = fs::metadata(path.join(&manifest.executable))
            .map_err(|_| err("slot executable is missing"))?;
        if !file.is_file() || file.permissions().mode() & 0o111 == 0 {
            return Err(err("slot executable is not a file"));
        }
        if tree_digest(&path)? != slot.sha256 {
            return Err(err("installed integration integrity check failed"));
        }
        Ok(path)
    }
    fn recover(&self) -> Result<()> {
        // Staging is never referenced by a pointer. It is therefore the only
        // state that recovery may delete. Pointer compatibility is evaluated
        // by `resolve_locked`, which can select a valid previous slot without
        // losing the record of an incompatible current release.
        for entry in fs::read_dir(&self.root).map_err(|e| io("read store", e))? {
            let entry = entry.map_err(|e| io("read store entry", e))?;
            if entry.file_name().to_string_lossy().starts_with(".staging-") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }
    fn selection(&self, id: &str) -> Result<Selection> {
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        let path = self.root.join("state").join(id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Selection::default()),
            Err(e) => return Err(io("read integration selection", e)),
        };
        if !metadata.is_file()
            || metadata.len() > 4096
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(err("integration selection is not a bounded protected file"));
        }
        serde_json::from_reader(File::open(path).map_err(|e| io("open integration selection", e))?)
            .map_err(|_| err("invalid integration selection"))
    }
    fn select(&self, id: &str, state: &Selection) -> Result<()> {
        if !valid_component(id) {
            return Err(err("invalid integration id"));
        }
        // Active + previous + integrity receipts commit together. A power cut
        // observes the complete old pair or the complete new pair.
        atomic_write(
            &self.root.join("state").join(id),
            &serde_json::to_vec(state).map_err(|e| err(e.to_string()))?,
        )
    }
}
/// Parse the `couch-confd integrations` command without printing.
pub fn run_cli(args: Vec<String>) -> Result<String> {
    enum Operation {
        Sideload(PathBuf),
        Repository { package: String, repository: String },
        List,
        Rollback(String),
        Remove(String),
    }
    let mut args = args.into_iter();
    let mut root = std::env::var_os("COUCH_INTEGRATIONS_DIR").map(PathBuf::from);
    let mut apk = None;
    let mut keys = None;
    let command = loop {
        match args.next().as_deref() {
            Some("--root") => root = Some(PathBuf::from(args.next().ok_or_else(|| err("--root needs a directory"))?)),
            Some("--apk") => apk = Some(PathBuf::from(args.next().ok_or_else(|| err("--apk needs a path"))?)),
            Some("--keys-dir") => keys = Some(PathBuf::from(args.next().ok_or_else(|| err("--keys-dir needs a directory"))?)),
            Some(value) if !value.starts_with('-') => break value.to_owned(),
            _ => return Err(err("usage: integrations [--root DIR] [--keys-dir DIR] [--apk APK] install-sideload APK | install-repository NAME --repository URL | list | rollback ID | remove ID")),
        }
    };
    // Parse the entire command before creating directories, invoking apk, or
    // changing selections. A syntax error must never perform half an action.
    let operation = match command.as_str() {
        "install-sideload" => Operation::Sideload(PathBuf::from(
            args.next()
                .ok_or_else(|| err("install-sideload needs an APK"))?,
        )),
        "install-repository" => {
            let package = args
                .next()
                .ok_or_else(|| err("install-repository needs a package name"))?;
            if args.next().as_deref() != Some("--repository") {
                return Err(err("install-repository needs --repository URL"));
            }
            let repository = args
                .next()
                .ok_or_else(|| err("install-repository needs a URL"))?;
            Operation::Repository {
                package,
                repository,
            }
        }
        "list" => Operation::List,
        "rollback" => Operation::Rollback(args.next().ok_or_else(|| err("rollback needs an ID"))?),
        "remove" => Operation::Remove(args.next().ok_or_else(|| err("remove needs an ID"))?),
        _ => return Err(err("unknown integrations command")),
    };
    if args.next().is_some() {
        return Err(err("unexpected integrations argument"));
    }
    let mut store = Store::new(root.unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT)));
    if let Some(apk) = apk {
        store = store.with_apk(apk);
    }
    if let Some(keys) = keys {
        store = store.with_keys_dir(keys);
    }
    match operation {
        Operation::Sideload(path) => {
            let item = store.install_sideload(&path)?;
            Ok(format!(
                "installed {} {} {}",
                item.manifest.id, item.manifest.version, item.sha256
            ))
        }
        Operation::Repository {
            package,
            repository,
        } => {
            let item = store.install_repository(&package, &repository)?;
            Ok(format!(
                "installed {} {} {}",
                item.manifest.id, item.manifest.version, item.sha256
            ))
        }
        Operation::List => Ok(store
            .list()?
            .into_iter()
            .map(|m| format!("{} {}", m.id, m.version))
            .collect::<Vec<_>>()
            .join("\n")),
        Operation::Rollback(id) => {
            let item = store.rollback(&id)?;
            Ok(format!("active {} {}", item.id, item.version))
        }
        Operation::Remove(id) => {
            store.remove(&id)?;
            Ok(String::new())
        }
    }
}

#[allow(dead_code)] // owns the file descriptor on which the advisory lock lives
struct Lock(File);
impl Lock {
    // Package slots are immutable and selection files commit by atomic rename,
    // so integrity checks may share the lock. Installs, rollback, removal and
    // staging recovery use the exclusive path and cannot overlap a reader.
    #[cfg(test)]
    fn acquire(path: PathBuf) -> Result<Self> {
        Self::acquire_wait(path, Duration::ZERO, libc::LOCK_EX)
    }
    fn acquire_shared_wait(path: PathBuf, wait: Duration) -> Result<Self> {
        Self::acquire_wait(path, wait, libc::LOCK_SH)
    }
    fn acquire_wait(path: PathBuf, wait: Duration, operation: libc::c_int) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|e| io("open integration lock", e))?;
        let started = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(io("lock integration store", error));
            }
            let remaining = wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(busy());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }
}
/// The next user, never one that has been handed out before in this store.
fn allocate(table: &mut Identities, id: &str) -> Result<u32> {
    let uid = table.next;
    if uid > LAST_PACKAGE_UID {
        return Err(err(
            "no user is left for another integration package; remove one and reinstall",
        ));
    }
    table.next = uid + 1;
    table.packages.insert(id.to_owned(), uid);
    Ok(uid)
}
fn validate_manifest(manifest: &Manifest) -> Result<()> {
    manifest
        .validate()
        .map_err(|e| err(format!("invalid or incompatible integration manifest: {e}")))
}
fn directories(path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(path).map_err(|_| err("APK has no integration payload"))? {
        let entry = entry.map_err(|e| io("read integration payload", e))?;
        if entry
            .file_type()
            .map_err(|e| io("inspect payload", e))?
            .is_dir()
        {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}
fn audit_tree(
    root: &Path,
    relative: &Path,
    allowed: &[PathBuf],
    files: &mut usize,
    bytes: &mut u64,
    apk_staging: bool,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative)).map_err(|e| io("audit package directory", e))? {
        let entry = entry.map_err(|e| io("audit package entry", e))?;
        let name = entry.file_name();
        if name == OsStr::new(".") || name == OsStr::new("..") {
            return Err(err("invalid package path"));
        }
        let child = relative.join(name);
        // apk initializes these housekeeping trees even with scripts disabled.
        // preflight_apk forbids package entries anywhere in them. They remain
        // in staging and are never copied into an integration slot.
        if apk_staging
            && relative.as_os_str().is_empty()
            && ["dev", "proc", "tmp", "var"]
                .iter()
                .any(|name| child == Path::new(name))
        {
            continue;
        }
        if !allowed
            .iter()
            .any(|prefix| child.starts_with(prefix) || prefix.starts_with(&child))
        {
            return Err(err(format!(
                "APK tried to install forbidden path {}",
                child.display()
            )));
        }
        let meta =
            fs::symlink_metadata(entry.path()).map_err(|e| io("inspect package entry", e))?;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(err("APK payload must be owned by the package manager user"));
        }
        if meta.file_type().is_symlink()
            || meta.file_type().is_block_device()
            || meta.file_type().is_char_device()
            || meta.file_type().is_fifo()
            || meta.file_type().is_socket()
        {
            return Err(err("APK contains a link or special file"));
        }
        if meta.is_dir() {
            if meta.permissions().mode() & 0o7022 != 0 {
                return Err(err("APK directory has unsafe permissions"));
            }
            audit_tree(root, &child, allowed, files, bytes, apk_staging)?;
        } else if meta.is_file() {
            if meta.permissions().mode() & 0o7022 != 0 || meta.nlink() != 1 {
                return Err(err("APK file has unsafe permissions"));
            }
            *files += 1;
            *bytes = bytes
                .checked_add(meta.len())
                .ok_or_else(|| err("APK size overflow"))?;
            if *files > MAX_FILES || meta.len() > MAX_FILE_BYTES || *bytes > MAX_TOTAL_BYTES {
                return Err(err("APK payload exceeds file or total size limit"));
            }
        } else {
            return Err(err("APK contains an unsupported file type"));
        }
    }
    Ok(())
}
fn read_manifest(path: &Path) -> Result<Manifest> {
    let metadata = fs::metadata(path).map_err(|_| err("integration manifest is missing"))?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(err("integration manifest is not a bounded regular file"));
    }
    serde_json::from_reader(File::open(path).map_err(|_| err("integration manifest is missing"))?)
        .map_err(|e| err(format!("invalid integration manifest: {e}")))
}
fn tree_digest(root: &Path) -> Result<String> {
    fn visit(root: &Path, relative: &Path, hash: &mut Sha256) -> Result<()> {
        let mut entries = fs::read_dir(root.join(relative))
            .map_err(|e| io("hash package tree", e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| io("hash package tree", e))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let child = relative.join(&name);
            let meta =
                fs::symlink_metadata(entry.path()).map_err(|e| io("hash package file", e))?;
            let name = child.as_os_str().as_encoded_bytes();
            hash.update((name.len() as u64).to_be_bytes());
            hash.update(name);
            hash.update((meta.permissions().mode() & 0o7777).to_be_bytes());
            if meta.is_dir() {
                hash.update(b"d");
                visit(root, &child, hash)?;
            } else {
                hash.update(b"f");
                hash.update(meta.len().to_be_bytes());
                let mut file = File::open(entry.path()).map_err(|e| io("read package file", e))?;
                let mut buf = [0; 8192];
                loop {
                    let n = file
                        .read(&mut buf)
                        .map_err(|e| io("read package file", e))?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buf[..n]);
                }
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, Path::new(""), &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn audit_payload(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root).map_err(|e| io("inspect integration slot", e))?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o7022 != 0 {
        return Err(err("integration slot is not a protected directory"));
    }
    audit_tree(
        root,
        Path::new(""),
        &[PathBuf::new()],
        &mut 0,
        &mut 0,
        false,
    )
}
fn sync_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).map_err(|e| io("sync package", e))? {
        let entry = entry.map_err(|e| io("sync package entry", e))?;
        if entry
            .file_type()
            .map_err(|e| io("inspect package entry", e))?
            .is_dir()
        {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())
                .and_then(|file| file.sync_all())
                .map_err(|e| io("sync package file", e))?;
        }
    }
    sync_dir(root)
}

/// APK v2 consists of concatenated gzip/tar streams. Bound decompression and
/// reject unsafe archive entries before invoking apk's verified extraction.
/// This admission profile intentionally does not accept APK v3 yet.
fn preflight_apk(path: &Path) -> Result<()> {
    let decoder =
        flate2::read::MultiGzDecoder::new(File::open(path).map_err(|e| io("open APK", e))?);
    let mut limited = decoder.take(MAX_TOTAL_BYTES + 1024 * 1024 + 1);
    let mut archive = tar::Archive::new(&mut limited);
    archive.set_ignore_zeros(true);
    let mut count = 0;
    let mut total = 0u64;
    for entry in archive
        .entries()
        .map_err(|_| err("APK must use the supported v2 gzip/tar format"))?
    {
        let mut entry = entry.map_err(|_| err("invalid or oversized APK archive"))?;
        count += 1;
        let name = entry.path().map_err(|_| err("invalid APK path"))?;
        if name.components().count() == 1 {
            let name = name.to_string_lossy();
            if name.starts_with('.') && name != ".PKGINFO" && !name.starts_with(".SIGN.") {
                return Err(err(
                    "integration APKs must not contain package scripts or triggers",
                ));
            }
        }
        if name.is_absolute()
            || name.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(err("APK contains an escaping path"));
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(err("APK contains a link or special file"));
        }
        let payload = Path::new("usr/lib/couch/integrations");
        let metadata = name.components().count() == 1
            && (name == Path::new(".PKGINFO") || name.to_string_lossy().starts_with(".SIGN."));
        if !metadata
            && !name.starts_with(payload)
            && !(kind.is_dir() && (payload.starts_with(&name) || name == Path::new(".")))
        {
            return Err(err("APK entry is outside the integration payload"));
        }
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| err("APK size overflow"))?;
        if count > MAX_FILES || size > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(err("APK exceeds admission limits"));
        }
        std::io::copy(&mut entry, &mut std::io::sink()).map_err(|_| err("invalid APK contents"))?;
    }
    drop(archive);
    std::io::copy(&mut limited, &mut std::io::sink())
        .map_err(|_| err("invalid APK compression"))?;
    if limited.limit() == 0 {
        return Err(err("APK decompression limit exceeded"));
    }
    Ok(())
}

fn run_bounded(command: &mut Command, timeout: Duration) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: MAX_APK_BYTES as _,
                rlim_max: MAX_APK_BYTES as _,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| io("run apk", e))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(err("apk refused the package or repository; check architecture, signature, trust keys and dependencies")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
                let _ = child.kill(); let _ = child.wait();
                return Err(err("apk operation exceeded its deadline"));
            }
        }
    }
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| io("sync integration directory", e))
}
fn atomic_write(path: &Path, value: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", nonce()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| io("create pointer", e))?;
    file.write_all(value).map_err(|e| io("write pointer", e))?;
    file.sync_all().map_err(|e| io("sync pointer", e))?;
    fs::rename(temporary, path).map_err(|e| io("replace pointer", e))?;
    File::open(path.parent().ok_or_else(|| err("pointer has no parent"))?)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| io("sync pointer directory", e))
}
fn nonce() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !value.starts_with('-')
        && !value.ends_with('-')
}
fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
        && !value.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "couch-package-{}-{}",
                std::process::id(),
                nonce()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> Store {
            let store = Store::new(self.0.join("integrations"));
            store.layout().unwrap();
            store
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn slot(store: &Store, version: &str) -> Slot {
        let path = store.slot_path("example", version);
        fs::create_dir_all(path.join("bin")).unwrap();
        fs::write(path.join("bin/plugin"), format!("executable {version}")).unwrap();
        fs::set_permissions(path.join("bin/plugin"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "protocol_version":1,"id":"example","label":"Example","version":version,
                "executable":"bin/plugin","capabilities":[],"settings":[]
            }))
            .unwrap(),
        )
        .unwrap();
        Slot {
            version: version.into(),
            sha256: tree_digest(&path).unwrap(),
        }
    }

    #[test]
    fn interrupted_selection_write_preserves_the_complete_old_pair() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = slot(&store, "1.0.0");
        let new = slot(&store, "2.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(old.clone()),
                    previous: None,
                },
            )
            .unwrap();
        // Power loss before atomic rename leaves only an unreferenced partial
        // write, which must never replace the last complete selection.
        fs::write(
            store.root.join("state/example.tmp-interrupted"),
            b"{\"active\":",
        )
        .unwrap();
        assert_eq!(store.resolve("example").unwrap().1.version, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(new.clone()),
                    previous: Some(old.clone()),
                },
            )
            .unwrap();
        let state = store.selection("example").unwrap();
        assert_eq!(state.active, Some(new));
        assert_eq!(state.previous, Some(old));
        assert_eq!(store.resolve("example").unwrap().1.version, "2.0.0");
    }

    #[test]
    fn corrupt_or_incompatible_active_payload_falls_back_without_changing_history() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = slot(&store, "1.0.0");
        let new = slot(&store, "2.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(new.clone()),
                    previous: Some(old),
                },
            )
            .unwrap();
        let generation = store.generation("example").unwrap();
        fs::write(
            store.slot_path("example", "2.0.0").join("bin/plugin"),
            b"truncated",
        )
        .unwrap();
        assert_eq!(store.resolve("example").unwrap().1.version, "1.0.0");
        assert_eq!(store.generation("example").unwrap(), generation);
        let path = store.slot_path("example", "2.0.0");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(path.join("manifest.json")).unwrap()).unwrap();
        manifest["protocol_version"] = serde_json::json!(2);
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        store
            .select(
                "example",
                &Selection {
                    active: Some(Slot {
                        version: "2.0.0".into(),
                        sha256: tree_digest(&path).unwrap(),
                    }),
                    previous: store.selection("example").unwrap().previous,
                },
            )
            .unwrap();
        assert_eq!(store.resolve("example").unwrap().1.version, "1.0.0");
        assert_eq!(
            store.selection("example").unwrap().active.unwrap().version,
            "2.0.0"
        );
    }

    #[test]
    fn content_permissions_and_links_are_part_of_admission_integrity() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let original = slot(&store, "1.0.0");
        let root = store.slot_path("example", "1.0.0");
        fs::set_permissions(root.join("bin/plugin"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(tree_digest(&root).unwrap(), original.sha256);
        fs::set_permissions(root.join("bin"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(audit_payload(&root).is_err());
        fs::set_permissions(root.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
        assert!(audit_payload(&root).is_err());
        fs::remove_file(root.join("link")).unwrap();
        fs::hard_link(root.join("bin/plugin"), root.join("hardlink")).unwrap();
        assert!(audit_payload(&root).is_err());
    }

    #[test]
    fn stale_lock_file_does_not_block_recovery_and_removal_commits_first() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(old),
                    previous: None,
                },
            )
            .unwrap();
        let lock = Lock::acquire(store.root.join(".lock")).unwrap();
        assert!(Lock::acquire(store.root.join(".lock")).is_err());
        drop(lock);
        assert!(store.root.join(".lock").exists());
        store.remove("example").unwrap();
        assert!(store.generation("example").is_err());
        assert!(store.resolve("example").is_err());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn resolve_wait_survives_brief_store_contention() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let active = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(active),
                    previous: None,
                },
            )
            .unwrap();
        let held = Lock::acquire(store.root.join(".lock")).unwrap();
        let waiting = store.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            waiting.resolve_wait("example", Duration::from_millis(250))
        });
        started_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
        assert_eq!(task.join().unwrap().unwrap().1.version, "1.0.0");
    }

    #[test]
    fn concurrent_resolves_share_the_store_lock() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let active = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(active),
                    previous: None,
                },
            )
            .unwrap();
        let _held = Lock::acquire_shared_wait(store.root.join(".lock"), Duration::ZERO).unwrap();
        assert!(
            Lock::acquire(store.root.join(".lock")).is_err(),
            "a mutation must not overlap an integrity read"
        );
        assert_eq!(
            store
                .resolve_wait("example", Duration::ZERO)
                .unwrap()
                .1
                .version,
            "1.0.0"
        );
    }

    #[test]
    fn removal_waits_for_an_active_catalog_reader() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let active = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(active),
                    previous: None,
                },
            )
            .unwrap();
        let held = Lock::acquire_shared_wait(store.root.join(".lock"), Duration::ZERO).unwrap();
        let removing = store.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            tx.send(()).unwrap();
            removing.remove("example")
        });
        rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(store.generation("example").is_ok());
        drop(held);
        task.join().unwrap().unwrap();
        assert!(store.generation("example").is_err());
    }

    #[test]
    fn resolve_wait_reports_typed_busy_when_its_bound_is_exhausted() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let _held = Lock::acquire(store.root.join(".lock")).unwrap();
        let started = Instant::now();
        let error = store
            .resolve_wait("example", Duration::from_millis(20))
            .unwrap_err();
        assert!(error.is_busy());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn apk_preflight_accepts_concatenated_streams_but_rejects_links() {
        use flate2::{write::GzEncoder, Compression};
        fn stream(name: &str, link: bool) -> Vec<u8> {
            let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(if link { 0 } else { 1 });
            if link {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("/etc/passwd").unwrap();
            }
            header.set_cksum();
            tar.append_data(&mut header, name, if link { &b""[..] } else { &b"x"[..] })
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap()
        }
        let fixture = Fixture::new();
        let path = fixture.0.join("sample.apk");
        let mut good = stream(".SIGN.RSA.test", false);
        good.extend(stream(".PKGINFO", false));
        good.extend(stream(
            "usr/lib/couch/integrations/example/bin/plugin",
            false,
        ));
        fs::write(&path, good).unwrap();
        assert!(preflight_apk(&path).is_ok());
        fs::write(
            &path,
            stream("usr/lib/couch/integrations/example/link", true),
        )
        .unwrap();
        assert!(preflight_apk(&path).is_err());
        fs::write(&path, stream(".post-install", false)).unwrap();
        assert!(preflight_apk(&path).is_err());
        for forbidden in [
            "dev/null",
            "proc/config",
            "tmp/script",
            "var/cache/apk/package",
            "etc/apk/repositories",
            "lib/apk/db/installed",
        ] {
            fs::write(&path, stream(forbidden, false)).unwrap();
            assert!(preflight_apk(&path).is_err(), "accepted {forbidden}");
        }
        fs::write(&path, b"not an APK").unwrap();
        assert!(preflight_apk(&path).is_err());
    }

    #[test]
    fn apk_housekeeping_is_not_confused_with_package_content() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let staging = fixture.0.join("staging");
        fs::create_dir_all(staging.join("usr/lib/couch/integrations/example/bin")).unwrap();
        fs::create_dir_all(staging.join("tmp")).unwrap();
        fs::set_permissions(staging.join("tmp"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(store.audit(&staging, "example").is_ok());
        // A same-named directory within a package is still subject to every
        // permission/link check, both during admission and later resolution.
        let payload = staging.join("usr/lib/couch/integrations/example");
        fs::create_dir_all(payload.join("tmp")).unwrap();
        fs::set_permissions(payload.join("tmp"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(store.audit(&staging, "example").is_err());
        assert!(audit_payload(&payload).is_err());
    }

    #[test]
    fn every_package_gets_one_user_of_its_own_and_keeps_it() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert_eq!(store.identity("denon").unwrap(), (60000, 60000));
        assert_eq!(store.identity("sonos").unwrap(), (60001, 60001));
        assert_eq!(store.identity("kodi").unwrap(), (60002, 60002));
        // The same package asked again is the same user, from the file and
        // not from anything remembered in this process.
        let reopened = Store::new(store.root.clone());
        assert_eq!(reopened.identity("denon").unwrap(), (60000, 60000));
        assert_eq!(reopened.identity("sonos").unwrap(), (60001, 60001));
        let table: serde_json::Value =
            serde_json::from_slice(&fs::read(store.root.join(IDENTITY_FILE)).unwrap()).unwrap();
        assert_eq!(
            table,
            serde_json::json!({"next":60003,"packages":{"denon":60000,"kodi":60002,"sonos":60001}})
        );
        assert_eq!(
            fs::metadata(store.root.join(IDENTITY_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        assert!(store.identity("../escape").is_err());
    }

    #[test]
    fn a_removed_package_keeps_its_user_and_a_new_one_never_inherits_it() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert_eq!(store.identity("denon").unwrap(), (60000, 60000));
        assert_eq!(store.identity("sonos").unwrap(), (60001, 60001));
        store.remove("sonos").unwrap();
        assert_eq!(store.identity("kodi").unwrap(), (60002, 60002));
        // Reinstalling the removed package gets the row it always had, which
        // is the one thing a recycled user could not promise.
        assert_eq!(store.identity("sonos").unwrap(), (60001, 60001));
    }

    #[test]
    fn a_missing_or_damaged_user_table_is_rebuilt_rather_than_repaired() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert_eq!(store.identity("denon").unwrap(), (60000, 60000));
        for damage in [
            "".as_bytes(),
            b"{\"next\":60001,\"packages\":{\"denon\"",
            // Two packages on one user, a user outside the range, a counter
            // that went backwards, and a field this Couch does not know.
            br#"{"next":60002,"packages":{"denon":60000,"sonos":60000}}"#,
            br#"{"next":60002,"packages":{"denon":70000}}"#,
            br#"{"next":60000,"packages":{"denon":60000}}"#,
            br#"{"next":60001,"packages":{"denon":60000},"colour":"red"}"#,
        ] {
            fs::write(store.root.join(IDENTITY_FILE), damage).unwrap();
            assert_eq!(store.identity("denon").unwrap(), (60000, 60000));
            assert_eq!(store.identity("sonos").unwrap(), (60001, 60001));
            fs::remove_file(store.root.join(IDENTITY_FILE)).unwrap();
        }
        assert_eq!(store.identity("denon").unwrap(), (60000, 60000));
    }

    #[test]
    fn the_last_user_in_the_range_is_handed_out_and_then_the_store_says_so() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store
            .write_identities(&Identities {
                next: LAST_PACKAGE_UID,
                packages: Default::default(),
            })
            .unwrap();
        assert_eq!(
            store.identity("last").unwrap(),
            (LAST_PACKAGE_UID, LAST_PACKAGE_UID)
        );
        let error = store.identity("one-too-many").unwrap_err();
        assert!(error.to_string().contains("no user is left"), "{error}");
        assert!(!error.is_busy());
        // Never a quiet fall back to the user every package used to share.
        assert_eq!(store.identity("last").unwrap().0, LAST_PACKAGE_UID);
    }

    #[test]
    fn users_are_allocated_under_the_store_lock_and_a_locked_store_is_busy() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let store = Store::new(store.root.clone());
                std::thread::spawn(move || store.identity(&format!("package-{n}")).unwrap().0)
            })
            .collect();
        let mut allocated: Vec<u32> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        allocated.sort_unstable();
        assert_eq!(allocated, (60000..60008).collect::<Vec<_>>());

        let held = Lock::acquire(store.root.join(".lock")).unwrap();
        let error = store.identity("while-locked").unwrap_err();
        assert!(error.is_busy(), "{error}");
        drop(held);
        assert_eq!(store.identity("while-locked").unwrap(), (60008, 60008));
    }

    /// The table sits at the store root because the release the remote can be
    /// rolled back to reads only `state/` and deletes only `.staging-*`. This
    /// is that claim, made against this store's own code paths.
    #[test]
    fn an_older_couch_neither_lists_nor_deletes_the_user_table() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let installed = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(installed),
                    previous: None,
                },
            )
            .unwrap();
        let before: Vec<String> = store.list().unwrap().into_iter().map(|m| m.id).collect();
        store.assign_identities().unwrap();
        assert_eq!(store.identity("example").unwrap(), (60000, 60000));
        let after: Vec<String> = store.list().unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(before, after);
        assert_eq!(after, vec!["example".to_owned()]);
        assert_eq!(store.resolve("example").unwrap().1.version, "1.0.0");
        // The recovery pass an older Couch runs before every install: the
        // table is not `.staging-*`, so it survives untouched.
        let bytes = fs::read(store.root.join(IDENTITY_FILE)).unwrap();
        store.recover().unwrap();
        assert_eq!(fs::read(store.root.join(IDENTITY_FILE)).unwrap(), bytes);
        // And the layout check an older Couch makes still passes with it there.
        assert!(store.layout().is_ok());
    }

    /// The store hands a user to any well-formed id, because admission asks
    /// before the package is selected. The daemon is what refuses a name of
    /// nothing, so a browser cannot spend the range; this pins the pair the
    /// daemon relies on.
    #[test]
    fn a_package_that_is_not_installed_has_no_generation_to_go_with_its_user() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert!(store.generation("invented").is_err());
        assert_eq!(store.identity("invented").unwrap(), (60000, 60000));
        let installed = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(installed),
                    previous: None,
                },
            )
            .unwrap();
        assert!(store.generation("example").is_ok());
        assert_eq!(store.identity("example").unwrap(), (60001, 60001));
    }

    #[test]
    fn the_startup_pass_names_what_is_installed_and_changes_nothing_else() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let installed = slot(&store, "1.0.0");
        store
            .select(
                "example",
                &Selection {
                    active: Some(installed),
                    previous: None,
                },
            )
            .unwrap();
        store.assign_identities().unwrap();
        let after_first = fs::read(store.root.join(IDENTITY_FILE)).unwrap();
        assert_eq!(store.identity("example").unwrap(), (60000, 60000));
        // A second pass has nothing to give out and must not rewrite the file.
        store.assign_identities().unwrap();
        assert_eq!(
            fs::read(store.root.join(IDENTITY_FILE)).unwrap(),
            after_first
        );
    }

    #[test]
    fn rejects_incompatible_protocol_and_path_traversal() {
        let manifest: Manifest = serde_json::from_str(r#"{"protocol_version":2,"id":"echo","label":"Echo","version":"1","executable":"bin/e","capabilities":[],"settings":[]}"#).unwrap();
        assert!(validate_manifest(&manifest).is_err());
        assert!(!valid_component("../escape"));
    }
}
