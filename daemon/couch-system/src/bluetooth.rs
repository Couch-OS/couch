//! Bluetooth on and off: start or stop `couch-bt-bridge`, the daemon that
//! turns the MediaTek STP transport (`/dev/stpbt`) into a Linux HCI device
//! through `/dev/vhci`. The kernel powers the radio on when the bridge opens
//! `/dev/stpbt` and off again when it closes, so the bridge process *is* the
//! switch; nothing else needs to happen. Only available on a kernel with the
//! Bluetooth core and the virtual HCI driver (both device nodes present).
use std::{path::Path, process::Command, thread, time::Duration};

pub const LOG: &str = "/tmp/couch-bt-bridge.log";
const HCI0: &str = "/sys/class/bluetooth/hci0";

/// The bridge next to this executable (a runtime slot that carries it), else
/// the boot ramdisk's copy (`/extra`, where the kernel that needs it came
/// from), else the base runtime's.
fn binary() -> Option<std::path::PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("couch-bt-bridge")));
    [
        beside,
        Some(Path::new("/extra/couch-bt-bridge").to_path_buf()),
        Some(Path::new("/mnt/alpine/opt/couch/couch-bt-bridge").to_path_buf()),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.is_file())
}

pub fn set(enabled: bool) -> Result<(), String> {
    if !enabled {
        let _ = Command::new("/bin/busybox")
            .args(["killall", "couch-bt-bridge"])
            .status();
        return Ok(());
    }
    if !crate::ui_settings::bluetooth_available() {
        return Err(
            "This kernel has no Bluetooth support; install the current boot image first".into(),
        );
    }
    if crate::ui_settings::bridge_running() {
        return Ok(());
    }
    let binary = binary().ok_or("couch-bt-bridge is not part of this runtime")?;
    // Let the shell background it so the bridge is init's child, not ours:
    // the service never has to reap it and its log outlives the request.
    let started = Command::new("/bin/busybox")
        .args([
            "sh",
            "-c",
            &format!("exec {} >>{LOG} 2>&1 &", binary.display()),
        ])
        .status()
        .map_err(|_| "Could not start the Bluetooth bridge")?;
    if !started.success() {
        return Err("Could not start the Bluetooth bridge".into());
    }
    for _ in 0..20 {
        if Path::new(HCI0).exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "The Bluetooth bridge started but no hci0 appeared; see {LOG}"
    ))
}

/// At boot: start the bridge when the setting says so and the kernel can.
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
