//! The remote's own settings: brightness, key backlight, the two standby
//! timeouts, and whether SSH and Bluetooth should run.
//!
//! One small file, `/opt/couch/settings.conf`, written atomically and read
//! leniently. It lives here rather than in the GUI so the web UI's daemon can
//! show and change the same values: the GUI applies them and writes them when
//! its menu changes one, the daemon writes them for the web page, and the GUI
//! notices the file change and applies it. Neither side owns the format alone.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PATH: &str = "/opt/couch/settings.conf";

/// Dim-after choices, seconds and their labels, index-aligned with the menu.
pub const DIM_SECS: [u64; 5] = [15, 30, 60, 120, 300];
pub const DIM_LABELS: [&str; 5] = ["15s", "30s", "1m", "2m", "5m"];
/// Screen-off choices; 0 is "Never", the one that only dims.
pub const OFF_SECS: [u64; 6] = [30, 60, 120, 300, 600, 0];
pub const OFF_LABELS: [&str; 6] = ["30s", "1m", "2m", "5m", "10m", "Never"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// 10..=100 percent, in tens on the menu.
    pub brightness: i32,
    /// Light the keypad while the screen is awake. The keypad backlight is a
    /// GPIO: lit or not, no levels.
    pub keys: bool,
    pub dim_index: i32,
    pub off_index: i32,
    /// Whether SSH should be running. The preference, persisted so a reboot
    /// keeps it; couch-system reads it at boot. Only meaningful when a key or
    /// password is enrolled.
    pub ssh: bool,
    /// Whether the Bluetooth bridge should run (the radio is on exactly while
    /// it does). Off by default: it costs power and only a kernel with the
    /// Bluetooth core can honour it. Absent from older files.
    #[serde(default)]
    pub bluetooth: bool,
}

impl Settings {
    /// 100% bright, keys lit, dim after 30s, off after 5m: the timings the
    /// menu has always defaulted to. `ssh` follows enrolment, which
    /// the caller knows and this crate's file does not.
    pub fn defaults(ssh: bool) -> Self {
        Settings {
            brightness: 100,
            keys: true,
            dim_index: 1,
            off_index: 3,
            ssh,
            bluetooth: false,
        }
    }
    /// Every field within its range; the file and the web API both go
    /// through this.
    pub fn clamped(mut self) -> Self {
        self.brightness = self.brightness.clamp(10, 100);
        self.dim_index = self.dim_index.clamp(0, DIM_SECS.len() as i32 - 1);
        self.off_index = self.off_index.clamp(0, OFF_SECS.len() as i32 - 1);
        self
    }
    pub fn dim_secs(&self) -> u64 {
        DIM_SECS[self.dim_index as usize]
    }
    pub fn off_secs(&self) -> u64 {
        OFF_SECS[self.off_index as usize]
    }
    /// The file's text, one `key=value` per line.
    pub fn render(&self) -> String {
        format!(
            "brightness={}\nkeys={}\ndim={}\noff={}\nssh={}\nbluetooth={}\n",
            self.brightness,
            u8::from(self.keys),
            self.dim_index,
            self.off_index,
            u8::from(self.ssh),
            u8::from(self.bluetooth)
        )
    }
    /// The file's text over `defaults`: unknown keys and unparsable values
    /// are ignored, so a file from an older or newer build still reads.
    pub fn parse(text: &str, defaults: Settings) -> Self {
        let mut s = defaults;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "brightness" => {
                    if let Ok(n) = v.parse::<i32>() {
                        s.brightness = n;
                    }
                }
                "keys" => s.keys = v != "0",
                "dim" => {
                    if let Ok(n) = v.parse::<i32>() {
                        s.dim_index = n;
                    }
                }
                "off" => {
                    if let Ok(n) = v.parse::<i32>() {
                        s.off_index = n;
                    }
                }
                "ssh" => s.ssh = v == "1",
                "bluetooth" => s.bluetooth = v == "1",
                _ => {}
            }
        }
        s.clamped()
    }
}

/// Where the file is: the fixed device path, or `COUCH_SETTINGS_FILE` for a
/// host run.
pub fn path() -> PathBuf {
    std::env::var_os("COUCH_SETTINGS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(PATH))
}

pub fn load(defaults: Settings) -> Settings {
    load_from(&path(), defaults)
}
pub fn load_from(path: &Path, defaults: Settings) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => Settings::parse(&text, defaults),
        Err(_) => defaults.clamped(),
    }
}

/// Write atomically: temp file then rename, so a crash mid-write cannot leave
/// a half-line.
pub fn save(settings: &Settings) -> std::io::Result<()> {
    save_to(&path(), settings)
}
pub fn save_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, settings.render())?;
    std::fs::rename(&tmp, path)
}

/// Whether sshd is running: the process list, which both roots share.
pub fn sshd_running() -> bool {
    process_running("sshd")
}
/// Whether the Bluetooth bridge is running, and with it the radio.
pub fn bridge_running() -> bool {
    process_running("couch-bt-bridge")
}
/// A boot image carrying the in-kernel STP HCI driver (hci_stp.ko in the
/// ramdisk's /extra): the toggle loads it instead of running the bridge.
pub fn stp_driver_available() -> bool {
    Path::new("/extra/hci_stp.ko").exists()
}
/// Whether the transport between BlueZ and the radio is up: the loaded
/// in-kernel driver, or the userspace bridge on images without it.
pub fn transport_running() -> bool {
    if stp_driver_available() {
        Path::new("/sys/module/hci_stp").exists()
    } else {
        bridge_running()
    }
}
/// Whether the HID GATT daemon is running (Bluetooth is fully up).
pub fn hid_running() -> bool {
    process_running("couch-bt-hid")
}
/// Where the Bluetooth stack is between off and on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothState {
    Off,
    /// The service is bringing the stack up; a few seconds.
    Starting,
    On,
    /// The last attempt failed, with the service's sentence.
    Error(String),
}
impl BluetoothState {
    pub fn word(&self) -> &'static str {
        match self {
            BluetoothState::Off => "off",
            BluetoothState::Starting => "starting",
            BluetoothState::On => "on",
            BluetoothState::Error(_) => "error",
        }
    }
}
/// The stack's state: the processes are the truth for on and off, and the
/// service's state file adds "starting" and the last error in between. A
/// "starting" older than the bring-up could take is a crashed attempt, so it
/// reads as off rather than spinning forever.
pub fn bluetooth_state() -> BluetoothState {
    if hid_running() && transport_running() {
        return BluetoothState::On;
    }
    let path = Path::new(crate::bluetooth::STATE_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return BluetoothState::Off;
    };
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(40));
    let text = text.trim();
    if text == "starting" && fresh {
        BluetoothState::Starting
    } else if let Some(error) = text.strip_prefix("error ") {
        BluetoothState::Error(error.trim().to_owned())
    } else {
        BluetoothState::Off
    }
}
/// Where pairing mode is, from the HID daemon's state file, with the lib's
/// staleness rule (a window whose daemon died reads as idle). Idle, with no
/// peer, whenever Bluetooth is not up: a file left by a daemon that is gone
/// says nothing about now.
pub fn bluetooth_pairing() -> couch_bt_hid::PairStatus {
    if !hid_running() {
        return couch_bt_hid::PairStatus::idle();
    }
    couch_bt_hid::PairStatus::read(&couch_bt_hid::PAIR_STATE_PATHS)
}
/// The name of the TV connected over Bluetooth right now, if one is.
pub fn bluetooth_peer() -> Option<String> {
    bluetooth_pairing().peer
}
/// Whether this kernel can do Bluetooth at all: the virtual HCI driver and
/// the MediaTek transport both present. Older boot images have neither.
pub fn bluetooth_available() -> bool {
    // A boot image built for the backported Bluetooth core has no /dev/vhci
    // until the toggle loads the modules it carries in /extra; one with the
    // in-kernel STP driver never needs it.
    Path::new("/dev/stpbt").exists()
        && (Path::new("/dev/vhci").exists()
            || Path::new("/extra/hci_vhci.ko").exists()
            || stp_driver_available())
}
/// Whether a process with exactly this `comm` is running.
///
/// The GUI asks once a second while Settings is open, `GET /api/remote/device`
/// asks twice, and a Bluetooth bring-up waits on it up to thirty times, so it
/// skips the non-pid entries in /proc (`meminfo`, `net`, `self`, ~40 more),
/// reuses one path and one read buffer instead of allocating per entry, and
/// stops at the first match. `comm` is at most 15 bytes plus a newline.
pub(crate) fn process_running(comm: &str) -> bool {
    use std::io::Read;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let mut path = String::with_capacity(24);
    let mut buffer = [0u8; 32];
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str() else { continue };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        path.clear();
        path.push_str("/proc/");
        path.push_str(pid);
        path.push_str("/comm");
        // The process can exit between the readdir and the open; that is not
        // an error, it is the answer.
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(read) = file.read(&mut buffer) else {
            continue;
        };
        if buffer[..read].trim_ascii() == comm.as_bytes() {
            return true;
        }
    }
    false
}

/// The file's modification time, for noticing another writer.
pub fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_parse_round_trip_and_older_files_still_read() {
        let s = Settings {
            brightness: 40,
            keys: false,
            dim_index: 4,
            off_index: 5,
            ssh: true,
            bluetooth: true,
        };
        assert_eq!(Settings::parse(&s.render(), Settings::defaults(false)), s);
        // A file from before `keys` existed: keys keep the default (lit).
        let old = "brightness=70\ndim=2\noff=0\nssh=0\n";
        let parsed = Settings::parse(old, Settings::defaults(true));
        assert_eq!(
            parsed,
            Settings {
                brightness: 70,
                keys: true,
                dim_index: 2,
                off_index: 0,
                ssh: false,
                bluetooth: false,
            }
        );
        // Junk and out-of-range values clamp or fall back rather than fail.
        let junk = "brightness=900\nkeys=maybe\ndim=-3\noff=99\nfuture=1\nnot a line\n";
        let parsed = Settings::parse(junk, Settings::defaults(false));
        assert_eq!(
            (parsed.brightness, parsed.dim_index, parsed.off_index),
            (100, 0, 5)
        );
        assert!(parsed.keys);
    }

    #[test]
    fn save_is_atomic_and_load_reads_it_back() {
        let dir = std::env::temp_dir().join(format!("couch-ui-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.conf");
        let s = Settings::defaults(false).clamped();
        save_to(&file, &s).unwrap();
        assert_eq!(load_from(&file, Settings::defaults(true)), s);
        assert!(!dir.join("settings.conf.tmp").exists());
        assert!(modified(&file).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn choices_stay_index_aligned_with_their_labels() {
        assert_eq!(DIM_SECS.len(), DIM_LABELS.len());
        assert_eq!(OFF_SECS.len(), OFF_LABELS.len());
        assert_eq!(Settings::defaults(false).dim_secs(), 30);
        assert_eq!(Settings::defaults(false).off_secs(), 300);
    }
}
