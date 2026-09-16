//! Which transport an activity's Bluetooth link goes to, and the migration
//! that turned the old "Bluetooth TV" connection into a per-device bond.
//!
//! The remote is one HID peripheral and a peripheral holds one link, so an
//! activity with two bonded TVs has to choose: the on-screen device gets the
//! link and the other TV is driven over whatever else it has. A TV with
//! nothing else is a configuration problem worth showing while editing, not
//! a key press that silently goes nowhere.
use crate::{Activity, Config, Device, DeviceBluetooth, Integration, Provider};
use alloc::vec::Vec;

impl Config {
    /// The device whose bond the remote links when this activity starts: the
    /// source (the on-screen device) if it is bonded, otherwise the first
    /// bonded member in the activity's own order. `None` when no member has
    /// a bond, in which case the link is left alone.
    pub fn bluetooth_link_device(&self, activity: &Activity) -> Option<&Device> {
        let bonded = |id: &crate::DeviceId| {
            self.devices()
                .map(|(_, d)| d)
                .find(|d| &d.id == id)
                .filter(|d| d.bluetooth.is_some())
        };
        activity
            .source
            .as_ref()
            .and_then(bonded)
            .or_else(|| activity.setup.devices.iter().find_map(bonded))
    }

    /// Bonded members of the activity that will not hold the link and have
    /// no other transport to fall back to: their keys cannot reach them
    /// while the activity runs.
    pub fn bluetooth_conflicts(&self, activity: &Activity) -> Vec<&Device> {
        let Some(link) = self.bluetooth_link_device(activity) else {
            return Vec::new();
        };
        let mut members: Vec<&crate::DeviceId> = activity.setup.devices.iter().collect();
        if let Some(source) = &activity.source {
            if !members.contains(&source) {
                members.push(source);
            }
        }
        members
            .into_iter()
            .filter_map(|id| self.devices().map(|(_, d)| d).find(|d| &d.id == id))
            .filter(|d| d.id != link.id && d.bluetooth.is_some())
            .filter(|d| d.transports(self).len() == 1)
            .collect()
    }

    /// Bring an older file up to the current shape. Returns whether anything
    /// changed, so a store can bump its revision and write.
    ///
    /// A device that reached its TV through the old `bluetooth-tv`
    /// integration (inline, or via a connection of that provider) becomes a
    /// device with no integration and a bond whose address is unknown; the
    /// connection, which carried nothing, is dropped once no device refers to
    /// it. The empty address keeps today's behaviour (keys go to whichever TV
    /// the daemon has) until the device is paired again from its own settings.
    pub fn migrate(&mut self) -> bool {
        let mut changed = false;
        let bluetooth_connections: Vec<crate::Id> = self
            .connections
            .iter()
            .filter(|c| c.provider == Provider::BluetoothTv)
            .map(|c| c.id.clone())
            .collect();
        for room in &mut self.rooms {
            for device in &mut room.devices {
                let legacy = match &device.integration {
                    Integration::BluetoothTv => true,
                    Integration::Connection { connection_id, .. } => {
                        bluetooth_connections.contains(connection_id)
                    }
                    _ => false,
                };
                if !legacy {
                    continue;
                }
                device.integration = Integration::None;
                if device.bluetooth.is_none() {
                    device.bluetooth = Some(DeviceBluetooth {
                        address: alloc::string::String::new(),
                        name: device.name.clone(),
                    });
                }
                changed = true;
            }
        }
        if !bluetooth_connections.is_empty() {
            self.connections
                .retain(|c| c.provider != Provider::BluetoothTv);
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActivitySetup, Connection, DeviceKind, Room, Transport};
    use alloc::{string::String, vec};

    fn house() -> Config {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "lg".into(),
            name: "LG".into(),
            provider: Provider::WebOs,
        });
        c.rooms.push(Room {
            id: "den".into(),
            name: "Den".into(),
            icon: None,
            devices: vec![
                Device {
                    bluetooth: Some(DeviceBluetooth {
                        address: "44:27:45:4E:33:25".into(),
                        name: "LG".into(),
                    }),
                    ..Device::new("lg-tv".into(), "LG TV", DeviceKind::Tv).with_integration(
                        Integration::Connection {
                            connection_id: "lg".into(),
                            resource_id: String::new(),
                        },
                    )
                },
                Device {
                    bluetooth: Some(DeviceBluetooth {
                        address: "AA:BB:CC:DD:EE:FF".into(),
                        name: "Bedroom".into(),
                    }),
                    ..Device::new("bt-tv".into(), "Bedroom TV", DeviceKind::Tv)
                },
                Device {
                    ir: Some(crate::DeviceIr {
                        codeset: "amp".into(),
                    }),
                    ..Device::new("amp".into(), "Amp", DeviceKind::Speaker)
                },
            ],
        });
        c.activities.push(Activity {
            setup: ActivitySetup {
                devices: vec!["lg-tv".into(), "bt-tv".into(), "amp".into()],
                ..Default::default()
            },
            id: "watch".into(),
            name: "Watch".into(),
            kind: Default::default(),
            room: "den".into(),
            source: Some("lg-tv".into()),
            buttons: vec![],
            steps: vec![],
        });
        c
    }

    #[test]
    fn the_source_gets_the_link_and_a_bluetooth_only_bystander_is_a_conflict() {
        let mut c = house();
        assert!(c.validate().is_ok());
        let watch = c.activities[0].clone();
        assert_eq!(
            c.bluetooth_link_device(&watch).map(|d| d.id.as_str()),
            Some("lg-tv")
        );
        let conflicts: Vec<_> = c
            .bluetooth_conflicts(&watch)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(conflicts, ["bt-tv"]);
        // The LG has the network too, so with the bedroom TV on screen it is
        // no conflict: its keys go over webOS.
        let mut swapped = watch.clone();
        swapped.source = Some("bt-tv".into());
        assert_eq!(
            c.bluetooth_link_device(&swapped).map(|d| d.id.as_str()),
            Some("bt-tv")
        );
        assert!(c.bluetooth_conflicts(&swapped).is_empty());
        // No source: the first bonded member holds the link.
        swapped.source = None;
        assert_eq!(
            c.bluetooth_link_device(&swapped).map(|d| d.id.as_str()),
            Some("lg-tv")
        );
        // An IR codeset on the bedroom TV resolves the conflict.
        c.rooms[0].devices[1].ir = Some(crate::DeviceIr {
            codeset: "bedroom".into(),
        });
        assert!(c.bluetooth_conflicts(&watch).is_empty());
        // No bonded member at all: nothing to link, nothing to warn about.
        for d in &mut c.rooms[0].devices {
            d.bluetooth = None;
        }
        assert!(c.bluetooth_link_device(&watch).is_none());
        assert!(c.bluetooth_conflicts(&watch).is_empty());
    }

    #[test]
    fn transports_follow_the_preference_and_ignore_one_the_device_lacks() {
        let mut c = house();
        let lg = c.rooms[0].devices[0].clone();
        assert_eq!(lg.transports(&c), [Transport::Ip, Transport::Bluetooth]);
        assert_eq!(
            lg.transport_order(&c),
            [Transport::Ip, Transport::Bluetooth]
        );
        c.rooms[0].devices[0].preferred_transport = Some(Transport::Bluetooth);
        let lg = c.rooms[0].devices[0].clone();
        assert_eq!(
            lg.transport_order(&c),
            [Transport::Bluetooth, Transport::Ip]
        );
        // A preference for infrared on a device with no codeset changes nothing.
        c.rooms[0].devices[0].preferred_transport = Some(Transport::Ir);
        let lg = c.rooms[0].devices[0].clone();
        assert_eq!(lg.preferred(&c), Some(Transport::Ip));
        assert_eq!(
            lg.transport_order(&c),
            [Transport::Ip, Transport::Bluetooth]
        );
        // With a codeset, infrared leads by default, as it always has.
        c.rooms[0].devices[0].preferred_transport = None;
        c.rooms[0].devices[0].ir = Some(crate::DeviceIr {
            codeset: "lg".into(),
        });
        let lg = c.rooms[0].devices[0].clone();
        assert_eq!(
            lg.transport_order(&c),
            [Transport::Ir, Transport::Ip, Transport::Bluetooth]
        );
        let bt = &c.rooms[0].devices[1];
        assert_eq!(bt.transport_order(&c), [Transport::Bluetooth]);
        assert!(crate::commands::Function::VolumeUp.supports_device(bt, &c));
        assert!(crate::commands::Function::VolumeUp.supports_transport(
            bt,
            &c,
            Transport::Bluetooth
        ));
        assert!(!crate::commands::Function::VolumeUp.supports_transport(bt, &c, Transport::Ip));
        // Power-on is not a Bluetooth key (an off TV has no link); the LG
        // gets it over the network, the bedroom TV not at all.
        assert!(!crate::commands::Function::PowerOn.supports_transport(
            bt,
            &c,
            Transport::Bluetooth
        ));
        assert!(!crate::commands::Function::PowerOn.supports_device(bt, &c));
        assert!(crate::commands::Function::PowerOn.supports_device(&c.rooms[0].devices[0], &c));
        let bare = Device::new("x".into(), "X", DeviceKind::Other);
        assert!(bare.transports(&c).is_empty() && bare.preferred(&c).is_none());
    }

    #[test]
    fn old_bluetooth_tv_devices_become_bonds_with_no_address() {
        let mut c: Config = serde_json::from_str(r#"{"schema_version":1,"revision":4,
            "connections":[{"id":"bt","name":"Bluetooth TV","provider":{"kind":"bluetooth-tv"}},{"id":"lg","name":"LG","provider":{"kind":"web-os"}}],
            "rooms":[{"id":"r","name":"R","devices":[
                {"id":"tv","name":"Bedroom TV","kind":"tv","integration":{"via":"connection","connection_id":"bt"}},
                {"id":"inline","name":"Inline","kind":"tv","integration":{"via":"bluetooth-tv"}},
                {"id":"lg","name":"LG","kind":"tv","integration":{"via":"connection","connection_id":"lg"}}]}]}"#).unwrap();
        assert!(c.validate().is_ok());
        assert!(c.migrate());
        assert!(c.validate().is_ok());
        assert!(c
            .connections
            .iter()
            .all(|x| x.provider != Provider::BluetoothTv));
        assert_eq!(c.connections.len(), 1);
        for id in ["tv", "inline"] {
            let d = c.rooms[0]
                .devices
                .iter()
                .find(|d| d.id.as_str() == id)
                .unwrap();
            assert_eq!(d.integration, Integration::None, "{id}");
            let bond = d.bluetooth.as_ref().unwrap();
            assert!(bond.address.is_empty() && !bond.addressed());
            assert_eq!(bond.name, d.name);
            assert_eq!(d.transports(&c), [Transport::Bluetooth]);
            assert!(crate::commands::Function::VolumeUp.supports_device(d, &c));
        }
        let lg = c.rooms[0]
            .devices
            .iter()
            .find(|d| d.id.as_str() == "lg")
            .unwrap();
        assert!(lg.bluetooth.is_none());
        // Idempotent, and a current file is untouched.
        assert!(!c.migrate());
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("bluetooth-tv"));
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), c);
        assert!(!Config::seed().migrate());
    }

    #[test]
    fn bonds_round_trip_and_addresses_are_checked() {
        for good in ["44:27:45:4E:33:25", "00:00:46:65:80:01"] {
            assert!(DeviceBluetooth::valid_address(good), "{good}");
        }
        for bad in [
            "",
            "44:27:45:4e:33:25",
            "44-27-45-4E-33-25",
            "44:27:45:4E:33",
            "44:27:45:4E:33:25:",
            "GG:27:45:4E:33:25",
            "44:27:45:4E:33:255",
        ] {
            assert!(!DeviceBluetooth::valid_address(bad), "{bad:?}");
        }
        let bond = DeviceBluetooth {
            address: "44:27:45:4E:33:25".into(),
            name: String::new(),
        };
        assert_eq!(bond.label(), "44:27:45:4E:33:25");
        let mut c = house();
        assert!(c.validate().is_ok());
        c.rooms[0].devices[1].bluetooth = Some(DeviceBluetooth {
            address: "nope".into(),
            name: "X".into(),
        });
        assert!(c.validate().is_err());
        // A device without a bond or a preference serialises as before.
        let plain = serde_json::to_value(Device::new("d".into(), "D", DeviceKind::Light)).unwrap();
        assert!(plain.get("bluetooth").is_none() && plain.get("preferred_transport").is_none());
        let old: Device = serde_json::from_str(r#"{"id":"tv","name":"TV"}"#).unwrap();
        assert!(old.bluetooth.is_none() && old.preferred_transport.is_none());
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"preferred_transport\"") || !json.contains("preferred"));
        for t in crate::ALL_TRANSPORTS {
            assert_eq!(Transport::from_name(t.name()), Some(*t));
        }
        assert_eq!(
            serde_json::to_value(Transport::Bluetooth).unwrap(),
            serde_json::json!("bluetooth")
        );
    }
}
