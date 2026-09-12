//! Private, single-attempt journals. A journal records evidence; it never authorizes USB writes.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(any(unix, test))]
use std::fs;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
#[cfg(windows)]
use windows::{private_dir, publish, sync_directory};

const MAX_EVENT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Created,
    InputsVerified,
    AndroidBound,
    OriginalsSaved,
    StageBootPending,
    StageConnected,
    BackupsVerified,
    Writing,
    Verifying,
    Complete,
    Failed,
}
impl Phase {
    fn allows(self, next: Self) -> bool {
        use Phase::*;
        if matches!(self, Complete | Failed) {
            return false;
        }
        next == Failed
            || matches!(
                (self, next),
                (Created, InputsVerified)
                    | (InputsVerified, AndroidBound)
                    | (AndroidBound, OriginalsSaved)
                    | (OriginalsSaved, StageBootPending)
                    | (StageBootPending, StageConnected)
                    | (StageConnected, BackupsVerified)
                    | (BackupsVerified, Writing)
                    | (Writing, Verifying)
                    | (Verifying, Complete)
            )
    }
}

pub struct SessionGuard {
    path: PathBuf,
    _lock: File,
    #[cfg(windows)]
    _directories: [windows::DirectoryLease; 2],
    phase: Phase,
    sequence: u32,
    poisoned: bool,
}

/// Create a new private application-state parent before creating a session in it.
/// Never changes permissions of an existing directory. The caller must retain
/// the SessionGuard while storing or executing private session inputs beneath it.
pub fn create_private_parent(path: &Path) -> Result<()> {
    let name = path.file_name().context("private parent needs a name")?;
    ensure!(name != "." && name != "..", "invalid private parent name");
    let parent = path
        .parent()
        .context("private parent needs an existing parent")?
        .canonicalize()?;
    ensure!(
        parent.ancestors().all(|p| !p.join(".git").exists()),
        "keep private state outside Git"
    );
    let target = parent.join(name);
    private_dir(&target).context("private parent must be new")?;
    #[cfg(windows)]
    windows::validate_parent(&target)?;
    #[cfg(target_os = "macos")]
    macos::validate_parent(&target)?;
    sync_directory(&parent)?;
    Ok(())
}
impl SessionGuard {
    /// The parent must already exist. On Unix it must be owned by this user and private.
    /// Existing directories (including interrupted sessions) are always rejected.
    /// Host-wide USB/device exclusion is a separate transport-layer responsibility.
    pub fn create(new_dir: &Path) -> Result<Self> {
        let name = new_dir
            .file_name()
            .context("session needs a directory name")?;
        ensure!(
            name != "." && name != "..",
            "invalid session directory name"
        );
        let parent = new_dir
            .parent()
            .context("session needs a parent")?
            .canonicalize()?;
        ensure!(
            parent.ancestors().all(|p| !p.join(".git").exists()),
            "keep private sessions outside Git"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = fs::metadata(&parent)?;
            // geteuid only reads the effective identity; it cannot alter process state.
            ensure!(
                m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
                "session parent must be owned by this user and mode 0700"
            );
        }
        #[cfg(windows)]
        let parent_lease = windows::lease_directory(&parent)?;
        #[cfg(windows)]
        windows::validate_parent(&parent)?;
        #[cfg(target_os = "macos")]
        macos::validate_parent(&parent)?;
        let path = parent.join(name);
        private_dir(&path).context("session directory must be new")?;
        #[cfg(windows)]
        let session_lease = windows::lease_directory(&path)?;
        sync_directory(&parent)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(path.join("session.lock"))?;
        lock.try_lock()
            .context("session is already owned by another process")?;
        lock.sync_all()?;
        let mut guard = Self {
            path,
            _lock: lock,
            #[cfg(windows)]
            _directories: [parent_lease, session_lease],
            phase: Phase::Created,
            sequence: 0,
            poisoned: false,
        };
        guard.persist(Phase::Created, "created", &json!({}))?;
        Ok(guard)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Evidence is persisted privately, never printed. Callers must pass hashes/receipts,
    /// not credentials. Once persistence fails, discard this guard and preserve its files.
    pub fn transition(&mut self, next: Phase, evidence: &Value) -> Result<()> {
        ensure!(
            !self.poisoned,
            "session persistence failed; explicit recovery required"
        );
        ensure!(self.phase.allows(next), "invalid session phase transition");
        self.persist(next, "transition", evidence)
    }
    /// Persist a verified per-partition boundary before acknowledging the stage.
    pub fn checkpoint(&mut self, evidence: &Value) -> Result<()> {
        ensure!(
            !self.poisoned && !matches!(self.phase, Phase::Complete | Phase::Failed),
            "session cannot accept checkpoints"
        );
        self.persist(self.phase, "checkpoint", evidence)
    }
    pub fn finish(&mut self) -> Result<()> {
        self.transition(Phase::Complete, &json!({}))
    }

    fn persist(&mut self, next: Phase, kind: &str, evidence: &Value) -> Result<()> {
        // Poison before any filesystem operation: even an ambiguous rename/fsync failure
        // cannot be retried through this instance. Drop leaves all evidence intact.
        self.poisoned = true;
        let mut data = Bounded(Vec::new());
        #[derive(Serialize)]
        struct Event<'a> {
            schema: u32,
            sequence: u32,
            phase: Phase,
            kind: &'a str,
            evidence: &'a Value,
        }
        serde_json::to_writer(
            &mut data,
            &Event {
                schema: 1,
                sequence: self.sequence,
                phase: next,
                kind,
                evidence,
            },
        )?;
        data.write_all(b"\n")?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.path)?;
        temporary.write_all(&data.0)?;
        temporary.as_file().sync_all()?;
        let target = self.path.join(format!("event-{:05}.json", self.sequence));
        publish(temporary, &target)?;
        sync_directory(&self.path)?;
        self.phase = next;
        self.sequence += 1;
        self.poisoned = false;
        Ok(())
    }
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_EVENT {
            return Err(std::io::Error::other("session evidence exceeds size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(unix)]
fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
#[cfg(unix)]
fn publish(file: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    file.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("journal publication failed; preserve session"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn private_parent_creation_is_exclusive_and_accepts_sessions() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("private-state");
        super::create_private_parent(&parent).unwrap();
        let session = super::SessionGuard::create(&parent.join("new-run")).unwrap();
        assert_eq!(session.phase(), super::Phase::Created);
        assert!(super::create_private_parent(&parent).is_err());
        assert!(parent.join("new-run/event-00000.json").exists());
    }
    use super::*;
    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }
    const STEPS: [Phase; 9] = [
        Phase::InputsVerified,
        Phase::AndroidBound,
        Phase::OriginalsSaved,
        Phase::StageBootPending,
        Phase::StageConnected,
        Phase::BackupsVerified,
        Phase::Writing,
        Phase::Verifying,
        Phase::Complete,
    ];
    #[test]
    fn complete_sequence_is_immutable_and_existing_sessions_never_resume() {
        let root = private_root();
        let path = root.path().join("run");
        let mut session = SessionGuard::create(&path).unwrap();
        assert!(session.transition(Phase::Writing, &json!({})).is_err());
        for phase in STEPS {
            session
                .transition(phase, &json!({"verified_sha256":"fixture"}))
                .unwrap();
        }
        assert!(session.transition(Phase::Failed, &json!({})).is_err());
        for n in 0..10 {
            let data: Value =
                serde_json::from_slice(&fs::read(path.join(format!("event-{n:05}.json"))).unwrap())
                    .unwrap();
            assert_eq!(data["sequence"], n);
        }
        drop(session);
        assert!(SessionGuard::create(&path).is_err());
    }
    #[test]
    fn collision_and_oversized_evidence_poison_without_overwriting() {
        let root = private_root();
        let mut session = SessionGuard::create(&root.path().join("collision")).unwrap();
        let event = session.path().join("event-00001.json");
        fs::write(&event, b"preserve").unwrap();
        assert!(session
            .transition(Phase::InputsVerified, &json!({}))
            .is_err());
        assert_eq!(fs::read(event).unwrap(), b"preserve");
        assert!(session.transition(Phase::Failed, &json!({})).is_err());
        let mut session = SessionGuard::create(&root.path().join("oversized")).unwrap();
        assert!(session
            .transition(
                Phase::InputsVerified,
                &json!({"data":"x".repeat(MAX_EVENT)})
            )
            .is_err());
        assert!(!session.path().join("event-00001.json").exists());
    }
    #[test]
    fn interrupted_write_and_failed_sessions_cannot_continue() {
        let root = private_root();
        let path = root.path().join("run");
        let mut session = SessionGuard::create(&path).unwrap();
        for phase in &STEPS[..7] {
            session.transition(*phase, &json!({})).unwrap();
        }
        assert_eq!(session.phase(), Phase::Writing);
        session
            .checkpoint(&json!({"partition":"userdata","sha256":"fixture"}))
            .unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(path.join("event-00008.json")).unwrap()).unwrap();
        assert_eq!(saved["phase"], "writing");
        assert_eq!(saved["kind"], "checkpoint");
        drop(session);
        assert!(SessionGuard::create(&path).is_err());
        let mut session = SessionGuard::create(&root.path().join("failed")).unwrap();
        session
            .transition(Phase::Failed, &json!({"reason":"transport_lost"}))
            .unwrap();
        assert!(session.transition(Phase::Verifying, &json!({})).is_err());
        assert!(session.checkpoint(&json!({})).is_err());
        drop(session);
        assert!(SessionGuard::create(&path).is_err());
    }
    #[test]
    fn separate_process_cannot_acquire_owned_lock() {
        const ENV: &str = "COUCH_SESSION_LOCK_FIXTURE";
        if let Some(path) = std::env::var_os(ENV) {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            assert!(file.try_lock().is_err());
            return;
        }
        let root = private_root();
        let guard = SessionGuard::create(&root.path().join("run")).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tests::separate_process_cannot_acquire_owned_lock",
            ])
            .env(ENV, guard.path().join("session.lock"))
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
    }
    #[cfg(unix)]
    #[test]
    fn private_modes_and_symlink_collisions_are_enforced() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = private_root();
        let target = root.path().join("run");
        symlink(root.path().join("absent"), &target).unwrap();
        assert!(SessionGuard::create(&target).is_err());
        assert!(fs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_symlink());
        let mut guard = SessionGuard::create(&root.path().join("valid")).unwrap();
        assert_eq!(
            fs::metadata(guard.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["session.lock", "event-00000.json"] {
            assert_eq!(
                fs::metadata(guard.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        symlink(
            root.path().join("absent"),
            guard.path().join("event-00001.json"),
        )
        .unwrap();
        assert!(guard.transition(Phase::InputsVerified, &json!({})).is_err());
        assert!(!root.path().join("absent").exists());
    }
}
