//! One immutable configuration snapshot for all GUI controllers. Disk reads and
//! validation after startup happen only on the watcher, never during rendering.
use couch_model::Config;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};
pub struct Snapshot {
    pub config: Arc<Config>,
    pub serial: u64,
}
#[derive(Default)]
struct Cache {
    value: RwLock<Option<Arc<Snapshot>>>,
    /// How many updates have been thrown away. A rejection means the remote is
    /// knowingly showing stale configuration, which is worth a toast, and a
    /// counter is all the UI needs to notice one.
    rejected: AtomicU64,
}
impl Cache {
    fn reload(&self, path: &Path) -> bool {
        let at = Instant::now();
        let next = (|| {
            let file = fs::File::open(path).ok()?;
            let mut raw = Vec::new();
            use std::io::Read;
            file.take(4 * 1024 * 1024 + 1).read_to_end(&mut raw).ok()?;
            if raw.len() > 4 * 1024 * 1024 {
                return None;
            }
            let mut c: Config = serde_json::from_slice(&raw).ok()?;
            // couch-confd rewrites an old file on its next start; until then
            // (and for a file it never opened) the same migration runs here.
            c.migrate();
            c.validate().ok()?;
            Some(c)
        })();
        let Some(config) = next else {
            eprintln!("couch-gui: configuration update rejected; retaining last valid snapshot");
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let mut value = self.value.write().unwrap();
        if value.as_ref().is_some_and(|old| *old.config == config) {
            return false;
        }
        let serial = value.as_ref().map_or(1, |v| v.serial + 1);
        let revision = config.revision;
        *value = Some(Arc::new(Snapshot {
            config: Arc::new(config),
            serial,
        }));
        println!(
            "couch-gui: configuration snapshot {serial} revision {revision} loaded in {} us",
            at.elapsed().as_micros()
        );
        true
    }
    fn get(&self) -> Option<Arc<Snapshot>> {
        self.value.read().unwrap().clone()
    }
}
fn stamp(path: &Path) -> Option<(u64, u64, u64, i64, i64)> {
    fs::metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
}
static CACHE: OnceLock<Arc<Cache>> = OnceLock::new();
pub fn start(path: PathBuf) {
    CACHE.get_or_init(|| {
        let cache = Arc::new(Cache::default());
        let mut seen = stamp(&path);
        cache.reload(&path);
        let worker = cache.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let now = stamp(&path);
            if now != seen {
                seen = now;
                worker.reload(&path);
            }
        });
        cache
    });
}
/// Updates rejected so far. Monotonic, so a caller keeps the last value it saw
/// rather than a flag somebody has to clear.
pub fn rejected() -> u64 {
    CACHE.get().map_or(0, |c| c.rejected.load(Ordering::Relaxed))
}
pub fn current() -> Option<Arc<Snapshot>> {
    if CACHE.get().is_none() {
        start(crate::home::path("config.json"));
    }
    CACHE.get()?.get()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_and_invalid_files_preserve_snapshot_and_atomic_replacement_updates_it() {
        let path = std::env::temp_dir().join(format!("couch-snapshot-{}.json", std::process::id()));
        let cache = Cache::default();
        let mut c = Config::seed();
        fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
        assert!(cache.reload(&path));
        let first = cache.get().unwrap();
        assert!(!cache.reload(&path));
        assert!(Arc::ptr_eq(&first, &cache.get().unwrap()));
        fs::write(&path, b"incomplete JSON").unwrap();
        assert!(!cache.reload(&path));
        assert!(Arc::ptr_eq(&first, &cache.get().unwrap()));
        assert_eq!(cache.rejected.load(Ordering::Relaxed), 1);
        c.revision += 1;
        c.rooms[0].name = "Changed".into();
        let tmp = path.with_extension("new");
        fs::write(&tmp, serde_json::to_vec(&c).unwrap()).unwrap();
        fs::rename(tmp, &path).unwrap();
        assert!(cache.reload(&path));
        assert_eq!(cache.get().unwrap().serial, first.serial + 1);
        assert_ne!(first.config.rooms[0].name, "Changed");
        fs::remove_file(path).unwrap();
    }
}
