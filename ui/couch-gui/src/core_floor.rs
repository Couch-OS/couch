//! Release the display's CPU floor only after successful panel power-down.
//! MediaTek HPS can still bring extra cores online for background work.
use std::{fs, io, path::PathBuf};

pub struct CoreFloor {
    path: PathBuf,
    active: Option<u32>,
    applied: Option<u32>,
}

impl CoreFloor {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        // MT6580 is a single four-core cluster. Preserve a configured floor;
        // an absent or unfamiliar interface must not result in guessed writes.
        let active = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|floor| (1..=4).contains(floor));
        Self {
            path,
            active,
            applied: active,
        }
    }

    pub fn display_off(&mut self, off: bool) -> io::Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let wanted = if off { 1 } else { active };
        if self.applied != Some(wanted) {
            fs::write(&self.path, format!("{wanted}\n"))?;
            self.applied = Some(wanted);
        }
        Ok(())
    }
}

impl Drop for CoreFloor {
    fn drop(&mut self) {
        let _ = self.display_off(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "couch-core-floor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn standby_releases_the_floor_and_wake_restores_the_original_setting() {
        let root = fixture();
        let path = root.join("floor");
        for floor in [2, 3, 4] {
            fs::write(&path, format!("{floor}\n")).unwrap();
            let mut policy = CoreFloor::new(&path);
            policy.display_off(true).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "1\n");
            policy.display_off(false).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), format!("{floor}\n"));
            policy.display_off(true).unwrap();
            drop(policy);
            assert_eq!(fs::read_to_string(&path).unwrap(), format!("{floor}\n"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_write_is_retried_and_unknown_interfaces_are_left_alone() {
        let root = fixture();
        let path = root.join("floor");
        fs::write(&path, "3\n").unwrap();
        let mut policy = CoreFloor::new(&path);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(policy.display_off(true).is_err());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, "3\n").unwrap();
        policy.display_off(true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "1\n");
        drop(policy);
        fs::write(&path, "3 0\n").unwrap();
        CoreFloor::new(&path).display_off(true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "3 0\n");
        fs::remove_file(&path).unwrap();
        CoreFloor::new(&path).display_off(true).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
