//! Power actions the remote's Settings menu asks for: power off, restart, and
//! a restart into the recovery image.
//!
//! Recovery is selected the way init itself selects it: `boot-recovery` in the
//! first 512 bytes of the bootloader control block (`para`, `mmcblk0p10`). A
//! normal boot arms that marker before its own checks and clears it once the
//! GUI is healthy, so writing it here and restarting lands in recovery once;
//! recovery keeps the flag until the operator clears it (docs/device-recovery.md).
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    process::Command,
};

/// The bootloader control block: first sector of the `para` partition.
pub const BCB: &str = "/dev/mmcblk0p10";
const MARKER: &[u8] = b"boot-recovery";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Off,
    Restart,
    Recovery,
}

impl Action {
    /// The message the menu shows once the request is accepted.
    pub fn label(self) -> &'static str {
        match self {
            Action::Off => "Powering off…",
            Action::Restart => "Restarting…",
            Action::Recovery => "Restarting into recovery…",
        }
    }
}

/// The 512-byte block init writes to arm recovery: the marker, zero padded.
pub fn recovery_block() -> [u8; 512] {
    let mut block = [0u8; 512];
    block[..MARKER.len()].copy_from_slice(MARKER);
    block
}

/// Write the recovery marker over the first sector, leaving the rest alone.
pub fn arm_recovery(bcb: &Path) -> io::Result<()> {
    let mut device = OpenOptions::new().write(true).open(bcb)?;
    device.write_all(&recovery_block())?;
    device.sync_all()
}

/// Zero the first sector, the way init does once the GUI is healthy. For a
/// deliberate restart from a running system: init arms the marker at every
/// boot and clears it only after health checks that start 90 s in, so a
/// restart requested before then would otherwise land in recovery, and the
/// next boot arms it again before anything can hang.
pub fn clear_recovery(bcb: &Path) -> io::Result<()> {
    let mut device = OpenOptions::new().write(true).open(bcb)?;
    device.write_all(&[0u8; 512])?;
    device.sync_all()
}

/// Perform the action. Only returns on failure: the busybox call replaces the
/// system state. The caller has already answered the request.
pub fn perform(action: Action, bcb: &Path) -> Result<(), String> {
    match action {
        Action::Recovery => {
            arm_recovery(bcb).map_err(|e| format!("Could not arm recovery: {e}"))?
        }
        Action::Restart => {
            clear_recovery(bcb).map_err(|e| format!("Could not clear the recovery flag: {e}"))?
        }
        Action::Off => {}
    }
    let _ = Command::new("/bin/busybox").arg("sync").status();
    let verb = match action {
        Action::Off => "poweroff",
        Action::Restart | Action::Recovery => "reboot",
    };
    match Command::new("/bin/busybox").args([verb, "-f"]).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{verb} exited with {status}")),
        Err(e) => Err(format!("Could not run {verb}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_zeroes_only_the_first_sector() {
        let path = std::env::temp_dir().join(format!("couch-bcb-clear-{}", std::process::id()));
        let mut image = vec![0xaau8; 2048];
        image[..13].copy_from_slice(b"boot-recovery");
        std::fs::write(&path, &image).unwrap();
        clear_recovery(&path).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert!(after[..512].iter().all(|b| *b == 0));
        assert_eq!(&after[512..], &image[512..]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn the_recovery_block_matches_what_init_writes() {
        let block = recovery_block();
        assert_eq!(&block[..13], b"boot-recovery");
        assert!(block[13..].iter().all(|b| *b == 0));
        assert_eq!(block.len(), 512);
    }

    #[test]
    fn arming_overwrites_only_the_first_sector() {
        let dir = std::env::temp_dir().join(format!("couch-power-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bcb = dir.join("para");
        std::fs::write(&bcb, vec![0xaa; 1024]).unwrap();
        arm_recovery(&bcb).unwrap();
        let bytes = std::fs::read(&bcb).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert_eq!(&bytes[..512], &recovery_block());
        assert!(bytes[512..].iter().all(|b| *b == 0xaa));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn actions_serialize_as_kebab_case_and_nothing_else_parses() {
        assert_eq!(
            serde_json::to_string(&Action::Recovery).unwrap(),
            "\"recovery\""
        );
        assert_eq!(
            serde_json::from_str::<Action>("\"off\"").unwrap(),
            Action::Off
        );
        assert!(serde_json::from_str::<Action>("\"halt now\"").is_err());
    }
}
