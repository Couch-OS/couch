//! Devices, and how the remote reaches them.
//!
//! A device is two separable things: what it *is* (a TV, a speaker), which
//! decides how it is drawn and which keys make sense, and how it is *reached*,
//! which is an address on the LAN or a code the IR blaster sends. Keeping them
//! apart means an LG C3 driven over HDMI-CEC and one driven by IR are the same
//! kind of thing with different plumbing.

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::icon::Icon;
use crate::id::Id;
use crate::DeviceId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    #[serde(default)]
    pub kind: DeviceKind,
    /// Overrides the icon [`DeviceKind`] would pick.
    #[serde(default)]
    pub icon: Option<Icon>,
    #[serde(default)]
    pub integration: Integration,
    /// Exact per-function IR overrides; unassigned functions use the integration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ir: Option<DeviceIr>,
    /// The TV bonded to the remote over Bluetooth LE for this device, if one
    /// is. A device may carry this next to a network integration and an IR
    /// codeset: each is a transport, and [`Device::transport_order`] says
    /// which is tried first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth: Option<DeviceBluetooth>,
    /// Which of the device's transports a key press tries first. `None` is
    /// the default order (infrared, then network, then Bluetooth); a
    /// preference for a transport the device does not have is ignored rather
    /// than refused, so removing a codeset never invalidates the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_transport: Option<Transport>,
}

impl Device {
    pub fn new(id: DeviceId, name: impl Into<String>, kind: DeviceKind) -> Device {
        Device {
            id,
            name: name.into(),
            kind,
            icon: None,
            integration: Integration::None,
            ir: None,
            bluetooth: None,
            preferred_transport: None,
        }
    }

    pub fn with_integration(mut self, integration: Integration) -> Device {
        self.integration = integration;
        self
    }

    /// Supplemental IR takes precedence; legacy IR devices remain readable.
    pub fn effective_ir_codeset<'a>(&'a self, config: &'a crate::Config) -> Option<&'a str> {
        if let Some(ir) = &self.ir { return Some(&ir.codeset); }
        match &self.integration {
            Integration::Ir { codeset } => Some(codeset),
            Integration::Connection { connection_id, resource_id }
                if config.connection(connection_id).is_some_and(|c| c.provider == crate::Provider::Ir) => Some(resource_id),
            _ => None,
        }
    }

    pub fn effective_icon(&self) -> Icon {
        self.icon.unwrap_or_else(|| self.kind.default_icon())
    }

    /// The integration reached over the network, if the device has one:
    /// anything resolvable that is not "nothing", infrared or the Bluetooth
    /// marker.
    pub fn network_integration(&self, config: &crate::Config) -> Option<Integration> {
        config
            .resolve_integration(&self.integration)
            .filter(|i| !matches!(i, Integration::None | Integration::Ir { .. } | Integration::BluetoothTv))
    }

    /// Whether the device can be reached this way at all (configuration, not
    /// liveness: a bonded TV that is off still counts as having Bluetooth).
    pub fn has_transport(&self, config: &crate::Config, transport: Transport) -> bool {
        match transport {
            Transport::Ir => self.effective_ir_codeset(config).is_some(),
            Transport::Ip => self.network_integration(config).is_some(),
            Transport::Bluetooth => self.bluetooth.is_some(),
        }
    }

    /// The transports the device has, in the default order.
    pub fn transports(&self, config: &crate::Config) -> Vec<Transport> {
        ALL_TRANSPORTS
            .iter()
            .copied()
            .filter(|t| self.has_transport(config, *t))
            .collect()
    }

    /// The preference that applies: the stored one if the device has that
    /// transport, otherwise the first it has.
    pub fn preferred(&self, config: &crate::Config) -> Option<Transport> {
        let have = self.transports(config);
        self.preferred_transport
            .filter(|t| have.contains(t))
            .or_else(|| have.first().copied())
    }

    /// The order a key press tries the device's transports: the preferred one
    /// first, then the rest in the default order. An executor moves down the
    /// list when a transport is unavailable right now (no IR code for that
    /// key, the TV not on the Bluetooth link, a network client that cannot
    /// connect) and stops at the first that takes the key.
    pub fn transport_order(&self, config: &crate::Config) -> Vec<Transport> {
        let mut order = self.transports(config);
        if let Some(first) = self.preferred(config) {
            order.retain(|t| *t != first);
            order.insert(0, first);
        }
        order
    }
}

/// A TV bonded to the remote's Bluetooth HID peripheral, stored on the device
/// it belongs to so two TVs can each have their own bond and an activity can
/// say which one the remote should be connected to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBluetooth {
    /// The TV's Bluetooth address, uppercase colon-hex (`44:27:45:4E:33:25`),
    /// as the HID daemon reports it. Empty for a bond migrated from the old
    /// "Bluetooth TV" connection, which kept one TV without recording its
    /// address: such a device is driven over whatever TV is on the link and
    /// gets an address the next time it is paired.
    #[serde(default)]
    pub address: String,
    /// The name the TV gave when it paired, for the editor and the room row.
    #[serde(default)]
    pub name: String,
}

impl DeviceBluetooth {
    /// Six uppercase hex pairs separated by colons, as the daemon writes them.
    pub fn valid_address(text: &str) -> bool {
        let bytes = text.as_bytes();
        bytes.len() == 17
            && bytes.iter().enumerate().all(|(i, b)| {
                if i % 3 == 2 {
                    *b == b':'
                } else {
                    b.is_ascii_digit() || (b'A'..=b'F').contains(b)
                }
            })
    }

    /// The address is known: the bond can be activated and forgotten by it.
    pub fn addressed(&self) -> bool {
        Self::valid_address(&self.address)
    }

    /// What to call the TV: its name, or the address when it gave none.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
}

/// How a key press reaches a device. A device has one or more; see
/// [`Device::transport_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// The infrared blaster, with the device's codeset.
    Ir,
    /// The device's network integration (webOS, Kodi, Sonos, ...).
    Ip,
    /// The remote as a Bluetooth HID peripheral, bonded to the device's TV.
    Bluetooth,
}

/// The default order: an exact IR assignment has always won over the network
/// integration for the keys it covers, and Bluetooth comes last so adding a
/// bond to a network TV changes nothing until the user prefers it.
pub const ALL_TRANSPORTS: &[Transport] = &[Transport::Ir, Transport::Ip, Transport::Bluetooth];

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Ir => "ir",
            Transport::Ip => "ip",
            Transport::Bluetooth => "bluetooth",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Transport::Ir => "Infrared",
            Transport::Ip => "Network",
            Transport::Bluetooth => "Bluetooth",
        }
    }
    pub fn from_name(name: &str) -> Option<Transport> {
        ALL_TRANSPORTS.iter().copied().find(|t| t.name() == name)
    }
    /// The integration an executor dispatches through for this transport;
    /// `None` for the network, whose integration is the device's own.
    pub fn marker(self, device: &Device, config: &crate::Config) -> Option<Integration> {
        match self {
            Transport::Ir => device
                .effective_ir_codeset(config)
                .map(|codeset| Integration::Ir { codeset: codeset.into() }),
            Transport::Ip => device.network_integration(config),
            Transport::Bluetooth => device.bluetooth.as_ref().map(|_| Integration::BluetoothTv),
        }
    }
}

impl core::fmt::Display for Transport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIr {
    pub codeset: String,
}
impl DeviceIr {
    pub fn valid_codeset(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    Light,
    Tv,
    Speaker,
    MediaPlayer,
    Camera,
    Blind,
    Thermostat,
    Switch,
    Sensor,
    #[default]
    Other,
}

/// Every kind, in the order the web UI should offer them.
pub const ALL_DEVICE_KINDS: &[DeviceKind] = &[
    DeviceKind::Light,
    DeviceKind::Tv,
    DeviceKind::Speaker,
    DeviceKind::MediaPlayer,
    DeviceKind::Camera,
    DeviceKind::Blind,
    DeviceKind::Thermostat,
    DeviceKind::Switch,
    DeviceKind::Sensor,
    DeviceKind::Other,
];

impl DeviceKind {
    pub fn name(self) -> &'static str {
        match self {
            DeviceKind::Light => "light",
            DeviceKind::Tv => "tv",
            DeviceKind::Speaker => "speaker",
            DeviceKind::MediaPlayer => "media-player",
            DeviceKind::Camera => "camera",
            DeviceKind::Blind => "blind",
            DeviceKind::Thermostat => "thermostat",
            DeviceKind::Switch => "switch",
            DeviceKind::Sensor => "sensor",
            DeviceKind::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Option<DeviceKind> {
        ALL_DEVICE_KINDS.iter().copied().find(|k| k.name() == name)
    }

    pub fn default_icon(self) -> Icon {
        match self {
            DeviceKind::Light => Icon::Lightbulb,
            DeviceKind::Tv => Icon::Tv,
            DeviceKind::Speaker => Icon::Speaker,
            DeviceKind::MediaPlayer => Icon::Monitor,
            DeviceKind::Camera => Icon::Monitor,
            DeviceKind::Blind => Icon::Blinds,
            DeviceKind::Thermostat => Icon::Thermometer,
            DeviceKind::Switch => Icon::Lamp,
            DeviceKind::Sensor => Icon::Thermometer,
            DeviceKind::Other => Icon::House,
        }
    }

    /// Whether the transport keys (play, pause, skip) mean anything here.
    pub fn is_playable(self) -> bool {
        matches!(
            self,
            DeviceKind::Tv | DeviceKind::Speaker | DeviceKind::MediaPlayer
        )
    }
}

impl core::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// How the remote talks to a device.
///
/// Externally tagged with a `via` discriminator so the JSON reads as
/// `{"via": "kodi", "host": "10.0.0.5", "port": 9090}` rather than a nested
/// object per variant - the file is meant to be legible, and the web UI's form
/// maps one-to-one onto these fields.
///
/// Only the transports this repo can actually drive today are listed: `couch-
/// kodi` speaks the first, the remote is a Home Assistant device so the second
/// is where most of it will end up, and `/dev/irtx` is the third. Adding a
/// variant requires updated readers before configurations use that variant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "kebab-case")]
pub enum Integration {
    Sonos { host: String },
    Denon { host: String, port: u16 },
    /// Configured but not wired to anything: the device exists so it can be
    /// counted and named, which is what the hub's room rows show.
    #[default]
    None,
    Connection { connection_id: Id, #[serde(default)] resource_id: String },
    Kodi {
        host: String,
        #[serde(default = "default_kodi_port")]
        port: u16,
    },
    // UUID for a light or room:GROUPED_LIGHT_UUID. Scenes use Scene.hue.
    // Keep the legacy field name for backwards-compatible saved configurations.
    Hue { light_id: String },
    WebOs,
    AndroidTv,
    AppleTv,
    Tizen,
    /// The Bluetooth transport marker: the remote is a HID keyboard and
    /// consumer-control peripheral and keys go straight to the TV bonded to
    /// the device. No longer stored on a device (the bond lives in
    /// [`Device::bluetooth`]; `Config::migrate` moves an old device over), but
    /// kept as the resolved form an executor and the button catalog match on,
    /// and so an old file still reads.
    BluetoothTv,
    UnifiProtect { camera_id: String },
    // Resolved form only: `<connection_id>/<node_id>/<endpoint>`. Saved devices
    // refer to the Matter connection with `<node_id>/<endpoint>` as resource ID.
    Matter { device: String },
    HomeAssistant {
        entity_id: String,
    },
    /// A codeset name plus the button map the blaster sends.
    Ir {
        codeset: String,
    },
}

fn default_kodi_port() -> u16 {
    // couch-kodi::DEFAULT_TCP_PORT. Not imported: this crate stays free of
    // dependencies on the clients so the wasm build never sees a socket.
    9090
}

impl Integration {
    pub fn via(&self) -> &'static str {
        match self {
            Integration::Connection { .. } => "connection",
            Integration::None => "none",
            Integration::Kodi { .. } => "kodi",
            Integration::Hue { .. } => "hue",
            Integration::WebOs => "web-os",
            Integration::AndroidTv => "android-tv",
            Integration::AppleTv => "apple-tv",
            Integration::Tizen => "tizen",
            Integration::BluetoothTv => "bluetooth-tv",
            Integration::UnifiProtect { .. } => "unifi-protect",
            Integration::Matter { .. } => "matter",
            Integration::HomeAssistant { .. } => "home-assistant",
            Integration::Ir { .. } => "ir",
            Integration::Denon { .. } => "denon",
            Integration::Sonos { .. } => "sonos",
        }
    }
}

/// One step of a scene or an activity: put this device into this state.
///
/// `command` is a string rather than a `Function` because that is what the
/// document stores and the web UI edits - `on`, `off`, `input:hdmi2`. It is not
/// free text: `Config::validate` refuses a step whose command
/// `commands::Function::parse` does not understand, because the executor parses
/// it the same way and a step that does not parse fails on the first press.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub device: DeviceId,
    pub command: String,
}

impl Action {
    pub fn new(device: impl Into<Id>, command: impl Into<String>) -> Action {
        Action {
            device: device.into(),
            command: command.into(),
        }
    }

    /// `on` and `off` for every device, plus whatever its kind suggests.
    ///
    /// A hint for the web UI's picker, so every string here has to be one the
    /// executor runs: the list used to offer `dim:30`, `bright` and `open`,
    /// which no `Function` parses, so anything built from it failed on the
    /// first press. `every_command_suggestion_parses` keeps it honest.
    pub fn suggestions(kind: DeviceKind) -> Vec<&'static str> {
        let mut v = alloc::vec!["on", "off"];
        if kind.is_playable() {
            v.extend_from_slice(&["play", "pause", "stop"]);
        }
        match kind {
            DeviceKind::Tv => v.extend_from_slice(&["input:hdmi1", "input:hdmi2"]),
            DeviceKind::Speaker => v.extend_from_slice(&["volume-up", "volume-down", "mute"]),
            DeviceKind::Light | DeviceKind::Blind | DeviceKind::Switch => v.push("toggle"),
            _ => {}
        }
        v
    }
}

#[cfg(test)]
mod ir_tests {
    use super::*;
    #[test]
    fn every_command_suggestion_parses() {
        for &kind in ALL_DEVICE_KINDS {
            for command in Action::suggestions(kind) {
                assert!(
                    crate::commands::Function::parse(command).is_some(),
                    "{kind} suggests \"{command}\", which the executor cannot parse"
                );
            }
        }
    }
    #[test]
    fn old_json_and_legacy_ir_work_without_a_supplemental_field() {
        let old:Device=serde_json::from_str(r#"{"id":"tv","name":"TV","integration":{"via":"ir","codeset":"lg-tv"}}"#).unwrap();
        let config=crate::Config::default();
        assert!(old.ir.is_none());assert_eq!(old.effective_ir_codeset(&config),Some("lg-tv"));
        assert!(serde_json::to_value(&old).unwrap().get("ir").is_none());
        let mut override_device=old;override_device.ir=Some(DeviceIr{codeset:"device-tv".into()});
        assert_eq!(override_device.effective_ir_codeset(&config),Some("device-tv"));
    }
    #[test]
    fn per_device_ir_exposes_mapping_capabilities_without_inventing_network_apps() {
        let config=crate::Config::default();
        let mut device=Device::new("receiver".into(),"Receiver",DeviceKind::Speaker);
        assert!(!crate::commands::Function::VolumeUp.supports_device(&device,&config));
        device.ir=Some(DeviceIr{codeset:"receiver".into()});
        assert!(crate::commands::Function::VolumeUp.supports_device(&device,&config));
        assert!(crate::commands::Function::PowerOn.supports_device(&device,&config));
        assert!(!crate::commands::Function::App("netflix".into()).supports_device(&device,&config));
    }
    #[test]
    fn supplemental_ir_keeps_network_and_rejects_unsafe_ids() {
        let mut config=crate::Config::seed();
        let device=&mut config.rooms[0].devices[0];let integration=device.integration.clone();
        device.ir=Some(DeviceIr{codeset:"device-tv".into()});assert_eq!(device.integration,integration);assert!(config.validate().is_ok());
        for id in ["","../secret","with space","UPPER"] {
            config.rooms[0].devices[0].ir=Some(DeviceIr{codeset:id.into()});assert!(config.validate().is_err());
        }
    }
}
