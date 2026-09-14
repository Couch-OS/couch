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
use std::{path::Path, process::Command, thread, time::Duration};

const HCI0: &str = "/sys/class/bluetooth/hci0";

/// The runtime slot's Alpine-relative directory (a slot copy wins over base),
/// checked by the presence of couch-bt-hid under /mnt/alpine.
fn base() -> Option<&'static str> {
    for (probe, alpine) in [
        (
            "/mnt/alpine/opt/couch/runtime/current/couch-bt-hid",
            "/opt/couch/runtime/current",
        ),
        ("/mnt/alpine/opt/couch/couch-bt-hid", "/opt/couch"),
    ] {
        if Path::new(probe).exists() {
            return Some(alpine);
        }
    }
    None
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

fn down() -> Result<(), String> {
    alpine_sh(
        "pkill -f couch-bt-hid 2>/dev/null; pkill bluetoothd 2>/dev/null; \
         for p in /proc/[0-9]*; do [ \"$(cat $p/comm 2>/dev/null)\" = couch-bt-bridge ] && kill \"$(basename $p)\" 2>/dev/null; done; true",
    )
}

pub fn set(enabled: bool) -> Result<(), String> {
    if !enabled {
        return down();
    }
    if !crate::ui_settings::bluetooth_available() {
        return Err(
            "This kernel has no Bluetooth support; install the current boot image first".into(),
        );
    }
    if crate::ui_settings::hid_running() {
        return Ok(());
    }
    let base = base().ok_or("couch-bt-hid is not part of this runtime")?;
    // Bridge first: opening the transport powers the radio and creates hci0.
    alpine_sh(&format!(
        "for p in /proc/[0-9]*; do [ \"$(cat $p/comm 2>/dev/null)\" = couch-bt-bridge ] && exit 0; done; \
         setsid {base}/couch-bt-bridge </dev/null >/tmp/couch-bt-bridge.log 2>&1 &"
    ))?;
    for _ in 0..25 {
        if Path::new(HCI0).exists() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    if !Path::new(HCI0).exists() {
        return Err("Bluetooth started but no controller appeared".into());
    }
    // dbus, then bluetoothd, then the HID daemon.
    alpine_sh(
        "[ -f /var/lib/dbus/machine-id ] || { mkdir -p /var/lib/dbus; cp /etc/machine-id /var/lib/dbus/machine-id 2>/dev/null; }; \
         mkdir -p /run/dbus; \
         pidof dbus-daemon >/dev/null || { rm -f /run/dbus/dbus.pid; setsid dbus-daemon --system --nopidfile </dev/null >/tmp/dbus.log 2>&1 & }",
    )?;
    thread::sleep(Duration::from_secs(1));
    alpine_sh(
        "pidof bluetoothd >/dev/null || setsid /usr/lib/bluetooth/bluetoothd </dev/null >/tmp/bluetoothd.log 2>&1 &",
    )?;
    thread::sleep(Duration::from_secs(2));
    alpine_sh(&format!(
        "pidof couch-bt-hid >/dev/null || setsid {base}/couch-bt-hid </dev/null >/tmp/couch-bt-hid.log 2>&1 &"
    ))?;
    for _ in 0..20 {
        if crate::ui_settings::hid_running() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err("Bluetooth started but the HID service did not come up; see /tmp/couch-bt-hid.log".into())
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
