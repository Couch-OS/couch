//! What the UI needs to know about the device it is running on.

/// Only called after a real GUI frame, then from the functioning main loop.
/// Atomic replacement keeps init from reading half a heartbeat.
pub fn report_gui_health() -> std::io::Result<()> {
    // /proc/uptime includes system suspend, matching init's freshness clock.
    let uptime = std::fs::read_to_string("/proc/uptime")?;
    let seconds = uptime
        .split('.')
        .next()
        .unwrap_or("")
        .parse::<u64>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let pid = std::process::id();
    let temporary = format!("/tmp/couch-gui.health.{pid}");
    std::fs::write(&temporary, format!("{pid} {seconds}\n"))?;
    std::fs::rename(temporary, "/tmp/couch-gui.health")
}

const BATTERY: &str = "/sys/class/power_supply/battery/";

pub struct Battery {
    pub percent: i32,
    pub charging: bool,
}

/// Reads the kernel's own gauge. "status" is the authoritative charging signal:
/// usb/online only reports that a cable is present, which it always is while
/// the remote sits on a bench being debugged, so it would read as charging for
/// ever. Returns None rather than inventing a figure if the gauge is missing.
pub fn battery() -> Option<Battery> {
    let percent: i32 = read_trimmed(&format!("{BATTERY}capacity"))?.parse().ok()?;
    let status = read_trimmed(&format!("{BATTERY}status")).unwrap_or_default();
    Some(Battery {
        percent: percent.clamp(0, 100),
        charging: status == "Charging" || status == "Full",
    })
}

/// Wi-Fi signal, as bars: 0 is disconnected, 1 through 4 climb with the RSSI.
///
/// `operstate` is the association signal - it reads "up" only once the link
/// layer is associated and the carrier is on, and drops to "down"/"dormant"
/// the moment the radio loses the AP - so it, not the mere presence of the
/// interface, decides connected from not. The strength comes from the driver's
/// own line in /proc/net/wireless, whose fourth field is the level in dBm
/// (e.g. "-63."). Read from files, not `iw` or `wpa_cli`: those are process
/// spawns, and this runs every second.
pub fn wifi_level() -> i32 {
    if read_trimmed("/sys/class/net/wlan0/operstate").as_deref() != Some("up") {
        return 0;
    }
    let dbm = wifi_dbm();
    match dbm {
        Some(d) if d >= -55 => 4,
        Some(d) if d >= -67 => 3,
        Some(d) if d >= -78 => 2,
        // Associated but weak, or associated with no readable level: still
        // connected, so never 0 here - 0 means down, and down is the one
        // state a person needs to see at a glance.
        Some(_) | None => 1,
    }
}

/// RSSI from the MT6580 driver's wireless statistics, not a guessed bar count.
pub fn wifi_dbm() -> Option<i32> {
    std::fs::read_to_string("/proc/net/wireless")
        .ok()
        .and_then(|text| crate::wifi::signal_dbm(&text))
}

/// Setup mode is a marker file, not an environment variable: the GUI is
/// supervised in a restart loop whose environment is fixed at boot, so an env
/// flag could never be cleared while running - which is exactly what joining a
/// network has to do.
pub fn in_setup_mode() -> bool {
    std::path::Path::new("/tmp/couch.setup").exists()
}

pub fn setup_ssid() -> String {
    read_trimmed("/tmp/portal.ssid").unwrap_or_else(|| "Couch-Setup".into())
}

/// The portal asks for a physical button press before granting SSH access, and
/// couch-system has no way to say so on the panel - its output goes to a log.
pub enum Approval {
    Idle,
    Waiting,
    Granted,
    TimedOut,
}

pub fn approval_state() -> Approval {
    if std::path::Path::new("/tmp/press.request").exists() {
        return Approval::Waiting;
    }
    match read_trimmed("/tmp/press.result").as_deref() {
        Some("ok") => Approval::Granted,
        Some("timeout") => Approval::TimedOut,
        _ => Approval::Idle,
    }
}

/// The four digits couch-confd wants shown, and how many seconds they have
/// left, if a browser is trying to pair.
///
/// The deadline comes out of the file rather than being counted from when this
/// process first saw it. Timing it locally meant a `couch-gui` restart handed a
/// stale PIN a fresh two minutes - which, with a daemon that only expired the
/// challenge when a request arrived, left the panel stuck on a PIN forever.
/// Both processes read the same clock, so an absolute deadline agrees between
/// them even if the device's clock is wrong.
pub fn pairing_pin() -> Option<(String, i32)> {
    let raw = read_trimmed("/tmp/couch.pin")?;
    let mut parts = raw.split_whitespace();
    let pin = parts.next()?.to_string();
    // Anything else is a file we did not write.
    if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let deadline: i64 = parts.next()?.parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((pin, (deadline - now).clamp(0, 3600) as i32))
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// The wpa_supplicant control socket stage2 opens, and the tools that talk to
/// it. Absolute, because couch-gui's PATH at boot is not guaranteed to carry
/// /sbin.
// --- user settings ----------------------------------------------------------
//
// Brightness, key backlight and the two standby timeouts, chosen in the
// settings menu or on the web UI's Remote settings page, kept across restarts
// in one small file that couch-system's `ui_settings` module owns the format
// of, so the web daemon reads and writes the same values.
pub use couch_system::ui_settings::{DIM_LABELS, DIM_SECS, OFF_LABELS, OFF_SECS};
pub type UiSettings = couch_system::ui_settings::Settings;

pub fn load_settings() -> UiSettings {
    couch_system::ui_settings::load(UiSettings::defaults(ssh_available()))
}

pub fn save_settings(s: &UiSettings) {
    let _ = couch_system::ui_settings::save(s);
}

/// Percent to an 8-bit backlight level, with a floor: 10% must still be
/// visible, not off. 0 is reserved for standby power-down.
pub fn brightness_level(percent: i32) -> u8 {
    let p = percent.clamp(10, 100) as u32;
    // 10% -> ~30, 100% -> 255, linear between.
    (30 + (p - 10) * (255 - 30) / 90) as u8
}

/// Cached supplicant status, refreshed in a worker without spawning tools or
/// blocking rendering. An unreadable network name does not imply link loss.
pub fn wifi_ssid() -> String {
    crate::wifi::status().ssid
}

/// Whether sshd is listening.
pub fn ssh_running() -> bool {
    couch_system::ui_settings::sshd_running()
}

/// Whether anyone is enrolled to use SSH: a key, or a root password. Without
/// one, sshd would listen for nobody, so the toggle offers nothing to turn on.
pub fn ssh_available() -> bool {
    matches!(
        couch_system::client::call(couch_system::protocol::Request::SshAvailable),
        Ok(couch_system::protocol::Reply::Available(true))
    )
}

// --- SSH control ------------------------------------------------------------
//
// Start and stop sshd at runtime. Enrolment - a key or a root password - stays
// the setup portal's job, gated on a physical button press; this only runs or
// stops the daemon for someone who is already enrolled and is, by holding the
// remote to reach this menu, physically present.

/// Start sshd. Generates host keys on first use (never baked into the image,
/// or every device would share them). Returns whether sshd is listening after.
pub fn ssh_start() -> bool {
    couch_system::client::action(couch_system::protocol::Request::Ssh { enabled: true }).is_ok()
}

/// Stop sshd. Returns whether it is stopped after.
pub fn ssh_stop() -> bool {
    couch_system::client::action(couch_system::protocol::Request::Ssh { enabled: false }).is_ok()
}

// Bluetooth is on when the HID daemon runs, the last piece of the stack. Only
// a kernel carrying the Bluetooth core offers it (both device nodes present).
pub fn bluetooth_available() -> bool {
    couch_system::ui_settings::bluetooth_available()
}
pub fn bluetooth_running() -> bool {
    couch_system::ui_settings::hid_running()
}
/// off, starting, on or error: the service publishes the two in-between states
/// while it brings the stack up, so the menu can say "starting" instead of
/// "off" for those seconds.
pub fn bluetooth_state() -> &'static str {
    couch_system::ui_settings::bluetooth_state().word()
}
/// Start or stop the stack through the system service; the error is the
/// service's own sentence, for the toast. Takes seconds: call off the UI thread.
pub fn bluetooth_set(enabled: bool) -> Result<(), String> {
    couch_system::client::action(couch_system::protocol::Request::Bluetooth { enabled })
}
/// Where pairing mode is and who is on the link, from the HID daemon's state
/// file (idle with no peer while Bluetooth is off).
pub fn bluetooth_pairing() -> couch_bt_hid::PairStatus {
    couch_system::ui_settings::bluetooth_pairing()
}
/// One word to the HID daemon's key socket: a pairing-mode control word or a
/// key. A datagram, so this neither waits nor blocks the UI thread.
pub fn bluetooth_word(word: &str) -> Result<(), String> {
    couch_bt_hid::send_word(word).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "Turn Bluetooth on in Settings first".into(),
        _ => "Bluetooth is not running; turn it on in Settings".into(),
    })
}
