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
    UnifiProtect { camera_id: String },
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
            Integration::UnifiProtect { .. } => "unifi-protect",
            Integration::HomeAssistant { .. } => "home-assistant",
            Integration::Ir { .. } => "ir",
            Integration::Denon { .. } => "denon",
            Integration::Sonos { .. } => "sonos",
        }
    }
}

/// One step of a scene or an activity: put this device into this state.
///
/// `command` is free text rather than an enum because what a device accepts is
/// the integration's business, not the config's - `on`, `off`, `input:hdmi2`,
/// `volume:35`. Validating it here would mean this crate knowing every
/// integration's vocabulary, and would reject a command a newer daemon
/// understands.
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
    /// Only a hint for the web UI's picker; the field stays free text.
    pub fn suggestions(kind: DeviceKind) -> Vec<&'static str> {
        let mut v = alloc::vec!["on", "off"];
        if kind.is_playable() {
            v.extend_from_slice(&["play", "pause", "stop"]);
        }
        match kind {
            DeviceKind::Light => v.extend_from_slice(&["dim:30", "dim:70", "bright"]),
            DeviceKind::Tv => v.extend_from_slice(&["input:hdmi1", "input:hdmi2"]),
            DeviceKind::Speaker => v.extend_from_slice(&["volume:20", "volume:40"]),
            DeviceKind::Blind => v.extend_from_slice(&["open", "close"]),
            DeviceKind::Thermostat => v.extend_from_slice(&["target:18", "target:21"]),
            _ => {}
        }
        v
    }
}

#[cfg(test)]
mod ir_tests {
    use super::*;
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
