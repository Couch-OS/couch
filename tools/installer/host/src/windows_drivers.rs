//! Windows USB driver pre-flight for the remote's download-mode identity.
//!
//! libusb on Windows can only open a device whose driver is WinUSB. The
//! MediaTek preloader (USB `0e8d:0003`) is present for only a few seconds after
//! the remote restarts, so its binding has to exist before it appears. Windows
//! records the driver it assigned to every device instance it has ever
//! enumerated under `HKLM\SYSTEM\CurrentControlSet\Enum\USB`, and Zadig's
//! "Create New Device" writes the same record without the device present. This
//! module reads that record and classifies it. It changes nothing: the
//! installer never installs or replaces drivers.
//!
//! The classification is platform independent so it can be unit-tested
//! everywhere; only the registry reader is Windows-specific.

use std::fmt::Write;

/// USB vendor identifier shared by the preloader and the Android/Couch device.
pub const VENDOR: u16 = 0x0e8d;
/// Product identifier of the MT6580 preloader in download mode.
pub const PRELOADER: u16 = 0x0003;
/// Product identifier of the remote running Android or Couch.
pub const COUCH: u16 = 0x201c;

/// One device instance Windows has recorded, with the `Service` value naming
/// the driver bound to it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    pub service: Option<String>,
}

/// Every record for one vendor/product pair: the device instances and, for a
/// composite device, the per-interface (`MI_xx`) instances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub devices: Vec<Instance>,
    pub interfaces: Vec<Instance>,
}

/// Outcome of the preloader classification. `ready` means every recorded
/// instance can be opened by libusb; `summary` is user-facing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub ready: bool,
    pub summary: String,
}

pub fn registry_key(pid: u16) -> String {
    format!(r"USB\VID_{VENDOR:04X}&PID_{pid:04X}")
}

fn is_winusb(service: Option<&str>) -> bool {
    service.is_some_and(|service| service.eq_ignore_ascii_case("WinUSB"))
}

fn describe_service(service: Option<&str>) -> String {
    match service {
        None => "no driver".into(),
        Some(service) if service.eq_ignore_ascii_case("usbser") => {
            "usbser (a serial-port driver; libusb cannot open it)".into()
        }
        Some(service) if service.eq_ignore_ascii_case("usbccgp") => {
            "usbccgp (composite parent)".into()
        }
        Some(service) => service.into(),
    }
}

/// Classify the preloader record. Every recorded instance must be bound to
/// WinUSB, either directly or, for a composite parent, on all of its
/// interfaces. Mixed records are not ready because Windows keeps the driver an
/// existing instance already has when the device reappears on that port.
pub fn assess(inventory: &Inventory) -> Assessment {
    let key = registry_key(PRELOADER);
    if inventory.devices.is_empty() && inventory.interfaces.is_empty() {
        return Assessment {
            ready: false,
            summary: format!(
                "Windows has no record of the remote's preloader ({key}). It has never been \
                 enumerated on this machine, so Windows will spend the short download window \
                 setting it up and libusb will not see it."
            ),
        };
    }
    let interfaces_ready = !inventory.interfaces.is_empty()
        && inventory
            .interfaces
            .iter()
            .all(|interface| is_winusb(interface.service.as_deref()));
    let mut ready = !inventory.devices.is_empty();
    let mut summary = format!("Recorded preloader instances ({key}):\n");
    for device in &inventory.devices {
        let service = device.service.as_deref();
        let composite = service.is_some_and(|service| service.eq_ignore_ascii_case("usbccgp"));
        let instance_ready = is_winusb(service) || (composite && interfaces_ready);
        ready &= instance_ready;
        let _ = writeln!(
            summary,
            "  {} {}: {}",
            if instance_ready { "ok" } else { "!!" },
            device.id,
            describe_service(service)
        );
    }
    for interface in &inventory.interfaces {
        let _ = writeln!(
            summary,
            "  {} interface {}: {}",
            if is_winusb(interface.service.as_deref()) {
                "ok"
            } else {
                "!!"
            },
            interface.id,
            describe_service(interface.service.as_deref())
        );
    }
    if inventory.devices.is_empty() {
        ready = false;
        let _ = writeln!(
            summary,
            "  !! only interface records exist; the device itself has no driver record"
        );
    }
    summary.push_str(if ready {
        "Every recorded instance is bound to WinUSB."
    } else {
        "An instance marked !! is not bound to WinUSB, so the installer cannot open the \
         preloader on it."
    });
    Assessment { ready, summary }
}

/// Short, identity-free description of the Android/Couch record: the serial is
/// part of those instance ids, so only counts and drivers are reported.
pub fn describe_couch(inventory: &Inventory) -> String {
    if inventory.devices.is_empty() {
        return format!(
            "no record ({}); the remote has not been enumerated on this machine",
            registry_key(COUCH)
        );
    }
    let mut services: Vec<String> = inventory
        .devices
        .iter()
        .chain(&inventory.interfaces)
        .map(|instance| describe_service(instance.service.as_deref()))
        .collect();
    services.sort();
    services.dedup();
    format!(
        "{} device instance(s), {} interface instance(s); drivers: {}",
        inventory.devices.len(),
        inventory.interfaces.len(),
        services.join(", ")
    )
}

#[cfg(windows)]
pub use registry::inventory;

#[cfg(windows)]
mod registry {
    use super::{registry_key, Instance, Inventory};
    use anyhow::{anyhow, Context, Result};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::Registry::{
            RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
            KEY_READ, KEY_WOW64_64KEY, REG_SZ,
        },
    };

    const ENUM_USB: &str = r"SYSTEM\CurrentControlSet\Enum\USB";

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open a key read-only through the 64-bit view. `Ok(None)` means absent.
    fn open(path: &str) -> Result<Option<Key>> {
        let mut handle: HKEY = null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                wide(path).as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut handle,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(Some(Key(handle))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            code => Err(anyhow!("open registry key {path}: Windows error {code}")),
        }
    }

    fn subkeys(key: &Key) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for index in 0.. {
            let mut buffer = [0u16; 256];
            let mut length = buffer.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    buffer.as_mut_ptr(),
                    &mut length,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            match status {
                ERROR_SUCCESS => {
                    names.push(String::from_utf16_lossy(&buffer[..length as usize]));
                }
                ERROR_NO_MORE_ITEMS => break,
                code => return Err(anyhow!("enumerate registry subkeys: Windows error {code}")),
            }
        }
        Ok(names)
    }

    /// Read a `REG_SZ` value. `Ok(None)` when the value is absent or not a string.
    fn string_value(key: &Key, name: &str) -> Result<Option<String>> {
        let name = wide(name);
        let mut data = vec![0u8; 512];
        loop {
            let mut kind = 0u32;
            let mut size = data.len() as u32;
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    name.as_ptr(),
                    null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut size,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    if kind != REG_SZ {
                        return Ok(None);
                    }
                    let units: Vec<u16> = data[..size as usize]
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .take_while(|unit| *unit != 0)
                        .collect();
                    return Ok(Some(String::from_utf16_lossy(&units)));
                }
                ERROR_FILE_NOT_FOUND => return Ok(None),
                ERROR_MORE_DATA => data.resize(size as usize, 0),
                code => return Err(anyhow!("read registry value: Windows error {code}")),
            }
        }
    }

    fn instances(path: &str) -> Result<Vec<Instance>> {
        let Some(key) = open(path)? else {
            return Ok(Vec::new());
        };
        subkeys(&key)?
            .into_iter()
            .map(|id| {
                let instance = open(&format!(r"{path}\{id}"))?
                    .with_context(|| format!("device instance {id} disappeared"))?;
                Ok(Instance {
                    service: string_value(&instance, "Service")?,
                    id,
                })
            })
            .collect()
    }

    /// Read every device and interface instance Windows has recorded for the
    /// vendor/product pair. Absent keys yield an empty inventory.
    pub fn inventory(pid: u16) -> Result<Inventory> {
        let device_key = registry_key(pid);
        let devices = instances(&format!(r"SYSTEM\CurrentControlSet\Enum\{device_key}"))?;
        let mut interfaces = Vec::new();
        if let Some(usb) = open(ENUM_USB)? {
            let prefix = format!("{}&MI_", &device_key["USB\\".len()..]).to_ascii_uppercase();
            for name in subkeys(&usb)? {
                if name.to_ascii_uppercase().starts_with(&prefix) {
                    for instance in instances(&format!(r"{ENUM_USB}\{name}"))? {
                        interfaces.push(Instance {
                            id: format!("{name}\\{}", instance.id),
                            service: instance.service,
                        });
                    }
                }
            }
        }
        Ok(Inventory {
            devices,
            interfaces,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(id: &str, service: Option<&str>) -> Instance {
        Instance {
            id: id.into(),
            service: service.map(Into::into),
        }
    }

    #[test]
    fn never_seen_preloader_is_not_ready_and_names_the_key() {
        let assessment = assess(&Inventory::default());
        assert!(!assessment.ready);
        assert!(assessment.summary.contains(r"USB\VID_0E8D&PID_0003"));
    }

    #[test]
    fn winusb_on_every_instance_is_ready_regardless_of_case() {
        let inventory = Inventory {
            devices: vec![
                instance("5&1&0&13", Some("WinUSB")),
                instance("5&1&0&14", Some("winusb")),
            ],
            interfaces: vec![],
        };
        let assessment = assess(&inventory);
        assert!(assessment.ready, "{}", assessment.summary);
        assert!(!assessment.summary.contains("!!"));
    }

    #[test]
    fn serial_port_driver_blocks_and_is_explained() {
        let inventory = Inventory {
            devices: vec![instance("5&1&0&13", Some("usbser"))],
            interfaces: vec![],
        };
        let assessment = assess(&inventory);
        assert!(!assessment.ready);
        assert!(assessment.summary.contains("!! 5&1&0&13: usbser"));
        assert!(assessment.summary.contains("serial-port driver"));
    }

    #[test]
    fn missing_driver_blocks() {
        let inventory = Inventory {
            devices: vec![instance("5&1&0&13", None)],
            interfaces: vec![],
        };
        let assessment = assess(&inventory);
        assert!(!assessment.ready);
        assert!(assessment.summary.contains("no driver"));
    }

    #[test]
    fn mixed_instances_block_even_when_one_is_winusb() {
        let inventory = Inventory {
            devices: vec![
                instance("old", Some("usbser")),
                instance("new", Some("WinUSB")),
            ],
            interfaces: vec![],
        };
        assert!(!assess(&inventory).ready);
    }

    #[test]
    fn composite_parent_is_ready_only_when_every_interface_is_winusb() {
        let ready = Inventory {
            devices: vec![instance("5&1&0&13", Some("usbccgp"))],
            interfaces: vec![
                instance("MI_00\\7&1&0&0000", Some("WinUSB")),
                instance("MI_01\\7&1&0&0001", Some("WinUSB")),
            ],
        };
        assert!(assess(&ready).ready);
        let partial = Inventory {
            devices: vec![instance("5&1&0&13", Some("usbccgp"))],
            interfaces: vec![
                instance("MI_00\\7&1&0&0000", Some("WinUSB")),
                instance("MI_01\\7&1&0&0001", Some("usbser")),
            ],
        };
        assert!(!assess(&partial).ready);
        let bare = Inventory {
            devices: vec![instance("5&1&0&13", Some("usbccgp"))],
            interfaces: vec![],
        };
        assert!(!assess(&bare).ready);
    }

    #[test]
    fn interface_records_without_a_device_record_block() {
        let inventory = Inventory {
            devices: vec![],
            interfaces: vec![instance("MI_00\\7&1&0&0000", Some("WinUSB"))],
        };
        assert!(!assess(&inventory).ready);
    }

    #[test]
    fn couch_description_omits_instance_ids() {
        let inventory = Inventory {
            devices: vec![instance("SERIAL123", Some("usbccgp"))],
            interfaces: vec![
                instance("MI_00\\7&1&0&0000", Some("WinUSB")),
                instance("MI_01\\7&1&0&0001", Some("WinUSB")),
            ],
        };
        let text = describe_couch(&inventory);
        assert!(!text.contains("SERIAL123"));
        assert!(text.contains("1 device instance(s), 2 interface instance(s)"));
        assert!(text.contains("WinUSB"));
        assert!(describe_couch(&Inventory::default()).contains("no record"));
    }

    #[cfg(windows)]
    #[test]
    fn unknown_product_reads_as_empty_inventory() {
        assert_eq!(inventory(0xfffe).unwrap(), Inventory::default());
    }
}
