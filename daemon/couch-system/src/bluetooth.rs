//! Bluetooth on and off.
//!
//! Turning Bluetooth on brings up the whole stack inside the Alpine root, where
//! dbus/bluetoothd live: couch-bt-bridge (opening the MediaTek transport is what
//! powers the radio and creates hci0), then dbus, bluetoothd, and couch-bt-hid
//! (the HID GATT app + raw-HCI advertising). Everything runs under one
//! `chroot /mnt/alpine` so they share the same dbus. The orchestration is here,
//! in Rust, rather than a shipped shell script: a new script - or a new binary -
//! in the runtime bundle is refused outright by the oldest deployed updater, and
//! that refusal strands the remote on the release it has
//! (`tools/release/update_floor.py`, docs/runtime-updates.md). So none of the
//! three binaries is published in the runtime bundle: they ride in the boot
//! ramdisk's `/extra`, and `base()` finds them there. Wi-Fi and Bluetooth share
//! one radio, so the stack starts only on the user's toggle.
//!
//! Bring-up takes a few seconds, so the service publishes its progress in a
//! state file (`starting`, `on`, `off`, or `error <sentence>`) that the GUI,
//! the web UI and the API read while they wait, instead of showing "off" until
//! the last piece is up.
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, thread, time::Duration};

const HCI0: &str = "/sys/class/bluetooth/hci0";
/// /tmp is shared between the initramfs root and Alpine.
pub const STATE_FILE: &str = "/tmp/couch-bt.state";
/// The dbus system socket, seen from the outer root.
const DBUS_SOCKET: &str = "/mnt/alpine/run/dbus/system_bus_socket";
/// bluetoothd's configuration, seen from the outer root. Written before every
/// start so an OS image's stock file (all comments) never wins.
const BLUETOOTHD_CONF: &str = "/mnt/alpine/etc/bluetooth/main.conf";
/// LE only: the MT6580 is a dual-mode controller and bluetoothd otherwise
/// answers classic inquiry and page scans, so a TV's Bluetooth menu can find
/// "Couch Remote" over BR/EDR, pair with SSP, search SDP for a HID record we
/// do not have, and give up (seen with an LG OLED77G5, 2026-09-15). The HID
/// service is GATT, so the classic side is nothing but a trap. Not pairable
/// by default: bluetoothd applies this after the adapter starts, later than
/// the HID daemon's own first Set, and the daemon opens pairing windows.
const BLUETOOTHD_CONF_TEXT: &str = "[General]\nControllerMode = le\nPairable = false\n";
/// The Wi-Fi interface's MAC as the kernel reports it, from the outer root.
/// The installer derives it from the chip id, so it is stable per remote and
/// different between remotes, which is exactly what the controller address
/// needs to be and what the MediaTek firmware does not give it.
const WIFI_MAC: &str = "/sys/class/net/wlan0/address";
/// Alpine's bluetoothd, inside the Alpine root.
const STOCK_BLUETOOTHD: &str = "/usr/lib/bluetooth/bluetoothd";
/// The patched bluetoothd, found next to couch-bt-hid wherever base() found
/// that: the boot ramdisk's /extra, or a runtime that carries a copy. Alpine's
/// BlueZ 5.79 plus a patch that stores a bonded TV's report subscriptions
/// (CCC values) and restores them when bluetoothd starts. Stock bluetoothd
/// keeps them only in memory, and a bonded TV does not write them again after
/// a reconnect, so every reboot, update or toggle left the TV's keys
/// silently dropped (third_party/bluez, docs/bluetooth.md).
const PATCHED_BLUETOOTHD: &str = "couch-bluetoothd";
/// Every bluetoothd as /proc/<pid>/comm names it. comm keeps 15 bytes, so the
/// patched one is `couch-bluetooth`; both are looked for and stopped, whichever
/// a previous runtime started.
const BLUETOOTHD_COMMS: &[&str] = &["bluetoothd", "couch-bluetooth"];

/// Where the Bluetooth binaries are, as an Alpine-relative directory: a
/// runtime slot copy wins over the base install, and then the boot image's
/// `/extra`, which is where a published release actually puts them (none of
/// these names can go in a runtime bundle, see the module comment). /tmp is
/// bind-mounted into Alpine, so one path names the copies in both roots.
///
/// The two that make the radio work decide the directory. couch-bluetoothd is
/// optional: without it `bluetoothd_path` falls back to Alpine's own, which
/// works apart from bonded devices' subscriptions, so an older boot image
/// without it still brings Bluetooth up.
fn base() -> Option<String> {
    for (probe, alpine) in [
        (
            "/mnt/alpine/opt/couch/runtime/current/couch-bt-hid",
            "/opt/couch/runtime/current",
        ),
        ("/mnt/alpine/opt/couch/couch-bt-hid", "/opt/couch"),
    ] {
        if Path::new(probe).exists() {
            return Some(alpine.into());
        }
    }
    if Path::new("/extra/couch-bt-hid").exists() && Path::new("/extra/couch-bt-bridge").exists() {
        let dir = Path::new("/tmp/couch-bt");
        fs::create_dir_all(dir).ok()?;
        for (name, required) in [
            ("couch-bt-hid", true),
            ("couch-bt-bridge", true),
            (PATCHED_BLUETOOTHD, false),
        ] {
            let (from, to) = (Path::new("/extra").join(name), dir.join(name));
            if to.exists() || (!required && !from.exists()) {
                continue;
            }
            match fs::copy(&from, &to) {
                Ok(_) => {
                    let _ = fs::set_permissions(&to, fs::Permissions::from_mode(0o755));
                }
                Err(_) if !required => {}
                Err(_) => return None,
            }
        }
        return Some("/tmp/couch-bt".into());
    }
    None
}

/// What a caller may ask the HID daemon to do, besides pressing keys. A
/// closed set: the request API names actions, and only this module knows the
/// daemon's words for them, so nothing arbitrary is written to the key socket
/// on behalf of the web page. The address and device that some actions take
/// travel as separate request fields, checked here before a word is formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairAction {
    /// Become pairable and discoverable for two minutes. With a `device`, the
    /// TV that bonds is recorded for that device (see [`pending_bond`]).
    Pair,
    /// Close the window early.
    Stop,
    /// Forget one bond (`address`) or, without one, every bond.
    Forget,
    /// Press and release Enter on the keyboard collection, for a TV that
    /// asks for a key press after pairing.
    Enter,
    /// Make one bond (`address`) the active link: the daemon drops any other
    /// TV and lets only this one connect. Without an address, no TV may.
    Activate,
}

/// Where a pairing window's target device is remembered: the id of the
/// device whose settings opened the window, so the daemon's `done` can be
/// attributed to it by whoever owns the configuration (couch-confd's tick
/// reads it through [`pending_bond`]). One line, the device id; a second word
/// `unpair` asks for the device's bond to be cleared instead. /tmp is shared
/// between the outer root and Alpine, like the daemon's own files.
pub const BOND_REQUEST_PATH: &str = "/tmp/couch-bt-bond.request";

/// Six uppercase hex pairs separated by colons, as the daemon writes them:
/// the daemon's own check, re-exported. couch-confd and this service would
/// otherwise each keep a copy to drift from it.
pub use couch_bt_hid::{normalize_address, valid_address};

/// A device id as the model allows them: safe to write to a file and read
/// back as one word.
fn valid_device(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The datagram an action turns into, or why it cannot. The control words
/// are the daemon's [`Control`](couch_bt_hid::Control), which renders them,
/// so this is only the mapping from a request onto one of those; `Enter` is
/// a key rather than a control and is the one word spelled here. Pure, so
/// the CLI's JSON forms are testable without a daemon.
pub fn control_for(
    action: PairAction,
    address: Option<&str>,
    device: Option<&str>,
) -> Result<String, String> {
    // The daemon accepts either case and writes uppercase, so a hand-typed
    // address is brought to its form rather than refused.
    let address = match address {
        Some(text) => Some(
            couch_bt_hid::normalize_address(text)
                .ok_or("A Bluetooth address is six uppercase hex pairs separated by colons")?,
        ),
        None => None,
    };
    if let Some(device) = device {
        if !valid_device(device) {
            return Err("Device ids are alphanumeric with dashes".into());
        }
    }
    Ok(match action {
        PairAction::Pair => couch_bt_hid::Control::Pair.word(),
        PairAction::Stop => couch_bt_hid::Control::PairStop.word(),
        PairAction::Forget => couch_bt_hid::Control::Forget(address).word(),
        PairAction::Activate => couch_bt_hid::Control::Activate(address).word(),
        PairAction::Enter => "enter".into(),
    })
}

/// Send one control word to the HID daemon. A datagram: the outcome is read
/// back from the daemon's state file (`ui_settings::bluetooth_pairing`).
///
/// `Pair` with a device remembers that device in [`BOND_REQUEST_PATH`] so the
/// TV that bonds gets stored on it; `Forget` with a device asks for the
/// device's stored bond to be cleared the same way. Either without a device
/// clears any pending request, so a window opened from the global settings
/// never lands a TV on a device whose window timed out earlier.
pub fn pair(action: PairAction, address: Option<&str>, device: Option<&str>) -> Result<(), String> {
    let word = control_for(action, address, device)?;
    if !crate::ui_settings::hid_running() {
        return Err("Turn Bluetooth on first".into());
    }
    match (action, device) {
        (PairAction::Pair, Some(device)) => write_bond_request(device, "pair")?,
        (PairAction::Forget, Some(device)) => write_bond_request(device, "unpair")?,
        (PairAction::Pair | PairAction::Stop | PairAction::Forget, None) => {
            let _ = fs::remove_file(BOND_REQUEST_PATH);
        }
        _ => {}
    }
    couch_bt_hid::send_word(&word)
        .map_err(|e| format!("The Bluetooth service is not answering: {e}"))
}

fn write_bond_request(device: &str, op: &str) -> Result<(), String> {
    fs::write(BOND_REQUEST_PATH, format!("{device} {op}\n"))
        .map_err(|e| format!("Could not record the pairing target: {e}"))
}

/// What a pairing window, or an unpair request, left for the configuration's
/// owner to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BondRequest {
    /// The window for this device ended with a bonded TV: store it.
    Bonded {
        device: String,
        address: String,
        name: String,
    },
    /// Clear this device's bond; the daemon was already told to forget it.
    Unpair { device: String },
}

/// The device a pairing window is for, while one is pending. For the web
/// page and the remote, so the card can say which device the TV will land on.
pub fn bond_request_device() -> Option<String> {
    let text = fs::read_to_string(BOND_REQUEST_PATH).ok()?;
    let mut words = text.split_whitespace();
    let device = words.next()?;
    (words.next() != Some("unpair") && valid_device(device)).then(|| device.to_owned())
}

/// Read the pending bond request against the daemon's state, and take it
/// when it has an outcome: the window reached `done` with a peer (the bond to
/// store), the window failed (nothing to store; the request is dropped), or
/// the request is an unpair. Pure over the two texts, so the tick's decision
/// is testable; [`take_pending_bond`] is the filesystem edge.
pub fn pending_bond(request: &str, state: &couch_bt_hid::PairStatus) -> Option<BondRequest> {
    let mut words = request.split_whitespace();
    let device = words.next().filter(|d| valid_device(d))?.to_owned();
    if words.next() == Some("unpair") {
        return Some(BondRequest::Unpair { device });
    }
    match state.phase {
        couch_bt_hid::PairPhase::Done => state.bonded.as_ref().map(|peer| BondRequest::Bonded {
            device,
            address: peer.address.clone(),
            name: peer.name.clone(),
        }),
        _ => None,
    }
}

/// [`pending_bond`] over the files, removing the request once it has an
/// outcome or once the window it belonged to is over without one. Called
/// once a second by couch-confd; cheap when there is no request.
pub fn take_pending_bond() -> Option<BondRequest> {
    let request = fs::read_to_string(BOND_REQUEST_PATH).ok()?;
    let state = crate::ui_settings::bluetooth_pairing();
    let outcome = pending_bond(&request, &state);
    let over = matches!(
        state.phase,
        couch_bt_hid::PairPhase::Done
            | couch_bt_hid::PairPhase::Failed
            | couch_bt_hid::PairPhase::Idle
    );
    if outcome.is_some() || over {
        let _ = fs::remove_file(BOND_REQUEST_PATH);
    }
    outcome
}

/// `aa:bb:cc:dd:ee:ff` to bytes, in the order written.
fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut n = 0;
    for part in text.trim().split(':') {
        if n == 6 || part.len() != 2 {
            return None;
        }
        out[n] = u8::from_str_radix(part, 16).ok()?;
        n += 1;
    }
    (n == 6).then_some(out)
}

fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The controller's public address, from the Wi-Fi MAC: the same bytes with
/// the last one plus one (wrapping), so it never equals the Wi-Fi address
/// and neighbouring remotes never collide with each other.
///
/// Why any of this: the MT6580 comes up as 00:00:46:65:80:01 on every remote
/// and after every boot. A TV keeps per-address state, so two remotes look
/// like one to it, and one bad pairing round leaves the TV listing the remote
/// but refusing to connect (an LG did, 2026-09-15) with no way to clear it
/// but forgetting the address. A stable, unique address makes bonds survive
/// reboots and keeps remotes apart.
pub fn derive_address(wifi: [u8; 6]) -> [u8; 6] {
    let mut address = wifi;
    address[5] = address[5].wrapping_add(1);
    address
}

fn wanted_address() -> Option<[u8; 6]> {
    fs::read_to_string(WIFI_MAC)
        .ok()
        .and_then(|text| parse_mac(&text))
        .filter(|mac| *mac != [0; 6])
        .map(derive_address)
}

/// The `BD Address:` line of `hciconfig hci0`.
fn parse_reported_address(text: &str) -> Option<[u8; 6]> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("BD Address:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(parse_mac)
}

fn reported_address() -> Option<[u8; 6]> {
    let output = Command::new("/bin/busybox")
        .args(["chroot", "/mnt/alpine", "/usr/bin/hciconfig", "hci0"])
        .output()
        .ok()?;
    parse_reported_address(&String::from_utf8_lossy(&output.stdout))
}

/// Program the controller's public address with MediaTek's vendor command
/// (OGF 0x3f, OCF 0x001a; the address goes out little-endian), then cycle
/// the device so the core reads it back. Sent with hci0 up and BEFORE
/// bluetoothd starts: bluetoothd binds its ATT server to the address it saw
/// at init, and after a live change every central got no MTU response and
/// hung up. The new address survives down/up and the toggle's func off/on
/// but not a reboot, which is why this runs on every bring-up. A random
/// static address through `btmgmt static-addr` is not an option: it
/// advertises but bluetoothd never answers ATT on it (checked twice).
///
/// hci0 is left DOWN afterwards. bluetoothd powers it on itself, and it must
/// be the one to: `ControllerMode = le` is applied by switching BR/EDR off
/// over MGMT, which the core rejects on a powered adapter, so an hci0 left up
/// here came back dual-mode and the classic-Bluetooth trap with it (.158).
fn set_controller_address() -> Result<(), String> {
    let Some(wanted) = wanted_address() else {
        println!("couch-system: bluetooth: no Wi-Fi MAC to derive an address from; keeping the controller's own");
        return Ok(());
    };
    if bluetoothd_running() {
        kill_comm(BLUETOOTHD_COMMS)?;
        wait_for(|| !bluetoothd_running(), 20, Duration::from_millis(100));
    }
    let bytes = wanted
        .iter()
        .rev()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    // Constants and hex digits only: nothing from a request reaches this line.
    alpine_sh(&format!(
        "hciconfig hci0 up && hcitool -i hci0 cmd 0x3f 0x001a {bytes} >/dev/null && hciconfig hci0 down && hciconfig hci0 up"
    ))
    .map_err(|_| "the set-address commands failed".to_string())?;
    let reported = reported_address();
    let _ = alpine_sh("hciconfig hci0 down");
    match reported {
        Some(now) if now == wanted => {
            println!(
                "couch-system: bluetooth: controller address {} (from the Wi-Fi MAC)",
                format_mac(&wanted)
            );
            Ok(())
        }
        now => Err(format!(
            "controller reports {} after setting {}",
            now.map(|a| format_mac(&a))
                .unwrap_or_else(|| "no address".into()),
            format_mac(&wanted)
        )),
    }
}

fn bluetoothd_running() -> bool {
    BLUETOOTHD_COMMS
        .iter()
        .any(|comm| crate::ui_settings::process_running(comm))
}

/// The bluetoothd to start, as an Alpine path: the runtime's patched one when
/// the runtime at BASE carries it, Alpine's otherwise (an older runtime, or
/// the boot image's /extra fallback, which has no bluetoothd).
fn bluetoothd_path(base: &str) -> String {
    let patched = format!("{base}/{PATCHED_BLUETOOTHD}");
    if Path::new(&format!("/mnt/alpine{patched}")).is_file() {
        patched
    } else {
        STOCK_BLUETOOTHD.into()
    }
}

/// Start bluetoothd unless one of either kind runs. A patched binary that
/// does not stay up (a runtime built against libraries this OS image does not
/// have) falls back to Alpine's, so Bluetooth still comes up, without the
/// stored subscriptions.
fn start_bluetoothd(base: &str) -> Result<(), String> {
    if bluetoothd_running() {
        return Ok(());
    }
    let path = bluetoothd_path(base);
    // Constant paths only: BASE is one of base()'s fixed directories.
    let start = |path: &str, log: &str| {
        alpine_sh(&format!(
            "setsid {path} </dev/null {log}/tmp/bluetoothd.log 2>&1 &"
        ))
    };
    start(&path, ">")?;
    if path == STOCK_BLUETOOTHD {
        return Ok(());
    }
    if wait_for(bluetoothd_running, 20, Duration::from_millis(100)) {
        thread::sleep(Duration::from_millis(500));
        if bluetoothd_running() {
            println!("couch-system: bluetooth: started {path}");
            return Ok(());
        }
    }
    eprintln!(
        "couch-system: bluetooth: {path} did not stay up (see /tmp/bluetoothd.log); starting {STOCK_BLUETOOTHD}"
    );
    start(STOCK_BLUETOOTHD, ">>")
}

fn publish(state: &str) {
    let _ = fs::write(STATE_FILE, format!("{state}\n"));
}

/// Run a fixed shell snippet inside the Alpine root. The snippets are internal
/// constants, never built from a request.
fn alpine_sh(script: &str) -> Result<(), String> {
    let status = Command::new("/bin/busybox")
        .args(["chroot", "/mnt/alpine", "/bin/sh", "-c", script])
        .status()
        .map_err(|_| "Could not run the Bluetooth helper")?;
    if status.success() {
        Ok(())
    } else {
        Err("The Bluetooth helper failed".into())
    }
}

/// Kill every process whose comm is one of NAMES, by walking /proc: busybox
/// pkill matches argv[0] (a full path for bluetoothd) and `-f` matches the
/// helper's own `sh -c` line, so neither is usable here.
fn kill_comm(names: &[&str]) -> Result<(), String> {
    let script = format!(
        "for p in /proc/[0-9]*; do c=$(cat $p/comm 2>/dev/null); case \"$c\" in {}) kill \"${{p#/proc/}}\" 2>/dev/null;; esac; done; true",
        names.join("|")
    );
    alpine_sh(&script)
}

/// Kernels built without the in-tree Bluetooth core carry the backported
/// 4.4 core as modules in the boot ramdisk (`/extra/*.ko`, see
/// docs/kernel-backports-research.md). Load whatever the ramdisk carries the
/// first time the toggle is used: the core, then either the in-kernel STP
/// driver (which registers hci0 itself) or the virtual HCI driver for the
/// userspace bridge, whose misc node mdev would have created at boot on
/// kernels where the driver is built in.
fn insmod(name: &str) -> Result<(), String> {
    let stem = name.trim_end_matches(".ko");
    if Path::new(&format!("/sys/module/{stem}")).exists()
        || !Path::new(&format!("/extra/{name}")).exists()
    {
        return Ok(());
    }
    let output = Command::new("/bin/busybox")
        .args(["insmod", &format!("/extra/{name}")])
        .output()
        .map_err(|_| "Could not run insmod")?;
    let text = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && !text.contains("File exists") {
        return Err(format!("Loading {name} failed: {}", text.trim()));
    }
    Ok(())
}

fn load_modules() -> Result<(), String> {
    insmod("compat.ko")?;
    insmod("bluetooth.ko")?;
    if crate::ui_settings::stp_driver_available() {
        return Ok(());
    }
    if Path::new("/sys/class/misc/vhci").exists() {
        return make_vhci_node();
    }
    if !Path::new("/extra/hci_vhci.ko").exists() {
        return Ok(());
    }
    insmod("hci_vhci.ko")?;
    if !wait_for(
        || Path::new("/sys/class/misc/vhci").exists(),
        20,
        Duration::from_millis(100),
    ) {
        return Err("hci_vhci loaded but no vhci device appeared".into());
    }
    make_vhci_node()
}

fn make_vhci_node() -> Result<(), String> {
    if Path::new("/dev/vhci").exists() {
        return Ok(());
    }
    let minor = fs::read_to_string("/sys/class/misc/vhci/dev")
        .ok()
        .and_then(|d| d.trim().split(':').nth(1).map(str::to_owned))
        .unwrap_or_else(|| "137".into());
    let status = Command::new("/bin/busybox")
        .args(["mknod", "-m", "660", "/dev/vhci", "c", "10", &minor])
        .status()
        .map_err(|_| "Could not run mknod")?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not create /dev/vhci".into())
    }
}

fn wait_for(what: impl Fn() -> bool, steps: u32, step: Duration) -> bool {
    for _ in 0..steps {
        if what() {
            return true;
        }
        thread::sleep(step);
    }
    what()
}

/// Stop in order: the HID daemon, then bluetoothd (which powers the adapter
/// down over HCI, through the bridge), then the bridge. Killing all three at
/// once left the MediaTek transport with a half-sent command and the next
/// open of /dev/stpbt failed with ENODEV for a while.
fn stop_stack(bridge: bool) -> Result<(), String> {
    kill_comm(&["couch-bt-hid"])?;
    kill_comm(BLUETOOTHD_COMMS)?;
    wait_for(|| !bluetoothd_running(), 20, Duration::from_millis(100));
    if bridge {
        kill_comm(&["couch-bt-bridge"])?;
        wait_for(
            || !crate::ui_settings::bridge_running(),
            20,
            Duration::from_millis(100),
        );
    }
    Ok(())
}

/// The in-kernel STP HCI driver, when the boot image carries it: loading
/// the module registers hci0 and the radio is powered on and off by the
/// adapter's own open and close, so there is no bridge to run.
fn load_stp_driver() -> Result<(), String> {
    if Path::new("/sys/module/hci_stp").exists() {
        return Ok(());
    }
    let output = Command::new("/bin/busybox")
        .args(["insmod", "/extra/hci_stp.ko"])
        .output()
        .map_err(|_| "Could not run insmod")?;
    if !output.status.success() {
        return Err(format!(
            "Loading hci_stp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn unload_stp_driver() {
    if Path::new("/sys/module/hci_stp").exists() {
        let _ = Command::new("/bin/busybox")
            .args(["rmmod", "hci_stp"])
            .status();
    }
}

fn down() -> Result<(), String> {
    let result = stop_stack(true);
    if crate::ui_settings::stp_driver_available() {
        unload_stp_driver();
    }
    // The HID daemon binds its key socket last; a stale path from the previous
    // run would otherwise look ready while the next one is still registering.
    // Its pairing state goes with it: there is no window and no link now.
    let _ = fs::remove_file(couch_bt_hid::SOCKET_PATH);
    let _ = fs::remove_file(couch_bt_hid::PAIR_STATE_PATH);
    let _ = fs::remove_file(BOND_REQUEST_PATH);
    publish("off");
    result
}

fn up() -> Result<(), String> {
    if !crate::ui_settings::bluetooth_available() {
        return Err(
            "This kernel has no Bluetooth support; install the current boot image first".into(),
        );
    }
    if crate::ui_settings::hid_running() {
        return Ok(());
    }
    let base = base().ok_or("couch-bt-hid is not part of this runtime")?;
    load_modules()?;
    // A bluetoothd or HID daemon left over from an earlier bridge holds stale
    // adapter state; when the bridge has to be (re)started, start them fresh.
    if !crate::ui_settings::transport_running() {
        stop_stack(false)?;
    }
    if crate::ui_settings::stp_driver_available() {
        load_stp_driver()?;
        if !wait_for(|| Path::new(HCI0).exists(), 30, Duration::from_millis(200)) {
            return Err("hci_stp loaded but no controller appeared".into());
        }
    }
    // Bridge first: opening the transport powers the radio and creates hci0.
    // WMT occasionally refuses the open right after a power-off (ENODEV) and
    // the bridge exits; a second try a moment later succeeds.
    let mut attempt = 0;
    while !crate::ui_settings::stp_driver_available() {
        alpine_sh(&format!(
            "for p in /proc/[0-9]*; do [ \"$(cat $p/comm 2>/dev/null)\" = couch-bt-bridge ] && exit 0; done; \
             setsid {base}/couch-bt-bridge </dev/null >/tmp/couch-bt-bridge.log 2>&1 &"
        ))?;
        if wait_for(
            || Path::new(HCI0).exists() && crate::ui_settings::bridge_running(),
            30,
            Duration::from_millis(200),
        ) {
            break;
        }
        attempt += 1;
        if attempt >= 4 || crate::ui_settings::bridge_running() {
            return Err(
                "Bluetooth started but no controller appeared; see /tmp/couch-bt-bridge.log".into(),
            );
        }
        thread::sleep(Duration::from_millis(1500));
    }
    // The backported 4.4 core runs its own setup pass on a new controller,
    // during which the MediaTek firmware raises an HCI hardware error and the
    // core resets the device (about 300 ms after open). bluetoothd powering
    // the adapter on in the middle of that reset times out, so give the core
    // a moment; the in-tree 3.18 core has no such pass.
    if Path::new("/sys/module/bluetooth").exists() {
        thread::sleep(Duration::from_millis(3000));
        // bluetoothd's managed advertisement uses the core's default interval
        // of 1.28 s, which a TV scanning briefly can miss (the raw-HCI path
        // advertised every 100 ms). The 4.4 core takes the interval from
        // debugfs, in 0.625 ms units: 100 to 150 ms.
        for (name, value) in [("adv_min_interval", "160"), ("adv_max_interval", "240")] {
            let _ = fs::write(format!("/sys/kernel/debug/bluetooth/hci0/{name}"), value);
        }
    }
    // The address goes in now, with hci0 registered and nothing on it yet.
    // Not fatal: a remote that keeps the firmware's default still works, it
    // just looks like every other remote to a TV.
    if let Err(error) = set_controller_address() {
        eprintln!(
            "couch-system: bluetooth: {error}; continuing with the controller's default address"
        );
    }
    // dbus, then bluetoothd, then the HID daemon. The HID daemon waits for
    // bluetoothd's adapter itself, so the three start back to back.
    alpine_sh(
        "[ -f /var/lib/dbus/machine-id ] || { mkdir -p /var/lib/dbus; cp /etc/machine-id /var/lib/dbus/machine-id 2>/dev/null; }; \
         mkdir -p /run/dbus; \
         pidof dbus-daemon >/dev/null || { rm -f /run/dbus/dbus.pid; setsid dbus-daemon --system --nopidfile </dev/null >/tmp/dbus.log 2>&1 & }",
    )?;
    if !wait_for(
        || Path::new(DBUS_SOCKET).exists(),
        30,
        Duration::from_millis(100),
    ) {
        return Err("Bluetooth started but dbus did not come up; see /tmp/dbus.log".into());
    }
    if fs::read_to_string(BLUETOOTHD_CONF).ok().as_deref() != Some(BLUETOOTHD_CONF_TEXT) {
        if let Some(dir) = Path::new(BLUETOOTHD_CONF).parent() {
            let _ = fs::create_dir_all(dir);
        }
        fs::write(BLUETOOTHD_CONF, BLUETOOTHD_CONF_TEXT)
            .map_err(|e| format!("Could not write bluetoothd's configuration: {e}"))?;
    }
    start_bluetoothd(&base)?;
    alpine_sh(&format!(
        "pidof couch-bt-hid >/dev/null || setsid {base}/couch-bt-hid </dev/null >/tmp/couch-bt-hid.log 2>&1 &"
    ))?;
    if wait_for(
        || crate::ui_settings::hid_running() && crate::ui_settings::transport_running(),
        30,
        Duration::from_millis(100),
    ) {
        return Ok(());
    }
    Err("Bluetooth started but the HID service did not come up; see /tmp/couch-bt-hid.log".into())
}

pub fn set(enabled: bool) -> Result<(), String> {
    if !enabled {
        return down();
    }
    publish("starting");
    // One retry from a clean stop: the first open after boot occasionally
    // leaves the controller stuck in its setup pass with nothing in the logs,
    // and a second open has always come up. Cheaper than making the user do it.
    let result = up().or_else(|first| {
        let _ = stop_stack(true);
        thread::sleep(Duration::from_secs(2));
        up().map_err(|second| format!("{second} (first try: {first})"))
    });
    match result {
        Ok(()) => {
            publish("on");
            Ok(())
        }
        Err(error) => {
            publish(&format!("error {error}"));
            Err(error)
        }
    }
}

/// At boot: start the stack when the setting says so and the kernel can. Not
/// wired into stage2 today (toggle-only), kept for when boot start is enabled.
pub fn auto() -> Result<(), String> {
    let settings = crate::ui_settings::load_from(
        Path::new("/mnt/alpine/opt/couch/settings.conf"),
        crate::ui_settings::Settings::defaults(false),
    );
    if settings.bluetooth && crate::ui_settings::bluetooth_available() {
        set(true)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_controller_address_is_the_wifi_mac_plus_one_and_never_equal_to_it() {
        let wifi = parse_mac("02:28:7d:8f:e1:6e\n").unwrap();
        let bt = derive_address(wifi);
        assert_eq!(format_mac(&bt), "02:28:7d:8f:e1:6f");
        assert_ne!(bt, wifi);
        // Wraps rather than carries: only the last byte ever differs.
        assert_eq!(
            derive_address([0x02, 0x28, 0x7d, 0x8f, 0xe1, 0xff]),
            [0x02, 0x28, 0x7d, 0x8f, 0xe1, 0x00]
        );
        for mac in [[0u8; 6], [0xff; 6], wifi] {
            assert_ne!(derive_address(mac), mac);
        }
    }

    #[test]
    fn mac_text_is_parsed_strictly_and_hciconfig_output_is_read() {
        assert_eq!(
            parse_mac("00:00:46:65:80:01"),
            Some([0, 0, 0x46, 0x65, 0x80, 1])
        );
        for bad in [
            "",
            "02:28:7d:8f:e1",
            "02:28:7d:8f:e1:6e:00",
            "02-28-7d-8f-e1-6e",
            "0g:28:7d:8f:e1:6e",
            "2:28:7d:8f:e1:6e",
        ] {
            assert_eq!(parse_mac(bad), None, "{bad:?}");
        }
        let hciconfig = "hci0:\tType: Primary  Bus: Virtual\n\tBD Address: 02:28:7D:8F:E1:6F  ACL MTU: 1021:7  SCO MTU: 184:1\n\tUP RUNNING\n";
        assert_eq!(
            parse_reported_address(hciconfig),
            Some([0x02, 0x28, 0x7d, 0x8f, 0xe1, 0x6f])
        );
        assert_eq!(parse_reported_address("hci0:\tType: Primary\n"), None);
    }

    #[test]
    fn both_bluetoothds_are_found_by_the_comm_the_kernel_gives_them() {
        // TASK_COMM_LEN is 16 including the NUL.
        let comm = |name: &'static str| &name[..name.len().min(15)];
        assert!(BLUETOOTHD_COMMS.contains(&comm(PATCHED_BLUETOOTHD)));
        assert!(BLUETOOTHD_COMMS.contains(&comm("bluetoothd")));
        assert!(STOCK_BLUETOOTHD.ends_with("/bluetoothd"));
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use couch_bt_hid::{PairPhase, PairStatus, Peer};

    #[test]
    fn controls_follow_the_daemon_contract_and_refuse_bad_addresses() {
        assert_eq!(control_for(PairAction::Pair, None, None).unwrap(), "pair");
        assert_eq!(
            control_for(PairAction::Pair, None, Some("living-tv")).unwrap(),
            "pair"
        );
        assert_eq!(
            control_for(PairAction::Stop, None, None).unwrap(),
            "pair-stop"
        );
        assert_eq!(control_for(PairAction::Enter, None, None).unwrap(), "enter");
        assert_eq!(
            control_for(PairAction::Forget, None, None).unwrap(),
            "forget"
        );
        assert_eq!(
            control_for(PairAction::Forget, Some("44:27:45:4E:33:25"), None).unwrap(),
            "forget 44:27:45:4E:33:25"
        );
        assert_eq!(
            control_for(PairAction::Activate, Some("44:27:45:4E:33:25"), None).unwrap(),
            "activate 44:27:45:4E:33:25"
        );
        assert_eq!(
            control_for(PairAction::Activate, None, None).unwrap(),
            "activate none"
        );
        // Either case is accepted and written the daemon's way.
        assert_eq!(
            control_for(PairAction::Activate, Some("44:27:45:4e:33:25"), None).unwrap(),
            "activate 44:27:45:4E:33:25"
        );
        // Every control this renders is one the daemon's own parser takes.
        for action in [
            PairAction::Pair,
            PairAction::Stop,
            PairAction::Forget,
            PairAction::Activate,
        ] {
            let word = control_for(action, Some("44:27:45:4E:33:25"), None).unwrap();
            assert!(
                couch_bt_hid::Control::parse(&word).unwrap().is_some(),
                "{word:?} is not a control the daemon parses"
            );
        }
        for bad in ["", "vol+", "44:27:45:4E:33:25 pair", "none"] {
            assert!(
                control_for(PairAction::Activate, Some(bad), None).is_err(),
                "{bad:?}"
            );
            assert!(
                control_for(PairAction::Forget, Some(bad), None).is_err(),
                "{bad:?}"
            );
        }
        for bad in ["", "../x", "a b", "x".repeat(129).as_str()] {
            assert!(
                control_for(PairAction::Pair, None, Some(bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_pending_request_resolves_only_on_done_with_a_peer_or_on_unpair() {
        let done = PairStatus {
            phase: PairPhase::Done,
            detail: "LG".into(),
            bonded: Some(Peer {
                address: "44:27:45:4E:33:25".into(),
                name: "LG".into(),
            }),
            ..PairStatus::idle()
        };
        assert_eq!(
            pending_bond("living-tv pair\n", &done),
            Some(BondRequest::Bonded {
                device: "living-tv".into(),
                address: "44:27:45:4E:33:25".into(),
                name: "LG".into()
            })
        );
        // The daemon before this contract reports done without a peer line:
        // nothing to store, so the request waits for the tick to drop it.
        let nameless = PairStatus {
            bonded: None,
            ..done.clone()
        };
        assert_eq!(pending_bond("living-tv pair\n", &nameless), None);
        for phase in [
            PairPhase::Pairing,
            PairPhase::Paired,
            PairPhase::Failed,
            PairPhase::Idle,
        ] {
            let s = PairStatus {
                phase,
                ..done.clone()
            };
            assert_eq!(pending_bond("living-tv pair\n", &s), None, "{phase:?}");
        }
        assert_eq!(
            pending_bond("living-tv unpair\n", &PairStatus::idle()),
            Some(BondRequest::Unpair {
                device: "living-tv".into()
            })
        );
        assert_eq!(pending_bond("", &done), None);
        assert_eq!(pending_bond("../etc pair", &done), None);
    }
}
