//! Bluetooth on and off.
//!
//! Turning Bluetooth on brings up the whole stack inside the Alpine root, where
//! dbus/bluetoothd live: couch-bt-bridge (opening the MediaTek transport is what
//! powers the radio and creates hci0), then dbus, bluetoothd, and couch-bt-hid
//! (the HID GATT app + raw-HCI advertising). Everything runs under one
//! `chroot /mnt/alpine` so they share the same dbus. The orchestration is here,
//! in Rust, rather than a shipped shell script: a new script in the runtime
//! bundle would be refused by updaters older than this one, whereas the new
//! couch-bt-hid binary is accepted (a couch-* executable). Wi-Fi and Bluetooth
//! share one radio, so the stack starts only on the user's toggle.
//!
//! Bring-up takes a few seconds, so the service publishes its progress in a
//! state file (`starting`, `on`, `off`, or `error <sentence>`) that the GUI,
//! the web UI and the API read while they wait, instead of showing "off" until
//! the last piece is up.
use std::{fs, path::Path, process::Command, thread, time::Duration};

const HCI0: &str = "/sys/class/bluetooth/hci0";
/// /tmp is shared between the initramfs root and Alpine.
pub const STATE_FILE: &str = "/tmp/couch-bt.state";
/// The dbus system socket, seen from the outer root.
const DBUS_SOCKET: &str = "/mnt/alpine/run/dbus/system_bus_socket";

/// Where the Bluetooth binaries are, as an Alpine-relative directory: a
/// runtime slot copy wins over the base install, and a boot image's `/extra`
/// fallback (copied into Alpine's /tmp, which is shared) covers runtimes that
/// do not carry them yet.
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
        for name in ["couch-bt-hid", "couch-bt-bridge"] {
            let to = dir.join(name);
            if !to.exists() {
                fs::copy(Path::new("/extra").join(name), &to).ok()?;
            }
        }
        return Some("/tmp/couch-bt".into());
    }
    None
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
    kill_comm(&["bluetoothd"])?;
    wait_for(
        || !crate::ui_settings::process_running("bluetoothd"),
        20,
        Duration::from_millis(100),
    );
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

fn down() -> Result<(), String> {
    let result = stop_stack(true);
    // The HID daemon binds its key socket last; a stale path from the previous
    // run would otherwise look ready while the next one is still registering.
    let _ = fs::remove_file("/tmp/couch-bt-hid.sock");
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
    // A bluetoothd or HID daemon left over from an earlier bridge holds stale
    // adapter state; when the bridge has to be (re)started, start them fresh.
    if !crate::ui_settings::bridge_running() {
        stop_stack(false)?;
    }
    // Bridge first: opening the transport powers the radio and creates hci0.
    // WMT occasionally refuses the open right after a power-off (ENODEV) and
    // the bridge exits; a second try a moment later succeeds.
    let mut attempt = 0;
    loop {
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
    // dbus, then bluetoothd, then the HID daemon. The HID daemon waits for
    // bluetoothd's adapter itself, so the three start back to back.
    alpine_sh(
        "[ -f /var/lib/dbus/machine-id ] || { mkdir -p /var/lib/dbus; cp /etc/machine-id /var/lib/dbus/machine-id 2>/dev/null; }; \
         mkdir -p /run/dbus; \
         pidof dbus-daemon >/dev/null || { rm -f /run/dbus/dbus.pid; setsid dbus-daemon --system --nopidfile </dev/null >/tmp/dbus.log 2>&1 & }",
    )?;
    if !wait_for(|| Path::new(DBUS_SOCKET).exists(), 30, Duration::from_millis(100)) {
        return Err("Bluetooth started but dbus did not come up; see /tmp/dbus.log".into());
    }
    alpine_sh(
        "pidof bluetoothd >/dev/null || setsid /usr/lib/bluetooth/bluetoothd </dev/null >/tmp/bluetoothd.log 2>&1 &",
    )?;
    alpine_sh(&format!(
        "pidof couch-bt-hid >/dev/null || setsid {base}/couch-bt-hid </dev/null >/tmp/couch-bt-hid.log 2>&1 &"
    ))?;
    if wait_for(
        || crate::ui_settings::hid_running() && crate::ui_settings::bridge_running(),
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
    match up() {
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
