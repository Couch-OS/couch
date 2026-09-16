//! Shared physical-button vocabulary and executable function catalog.
use crate::{Action, Integration};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Button {
    Back,
    Home,
    Power,
    Up,
    Down,
    Left,
    Right,
    Ok,
    VolumeUp,
    VolumeDown,
    ChannelUp,
    ChannelDown,
    Mute,
    Microphone,
    Menu,
    Lights,
    Activity,
    Music,
    Tv,
    Red,
    Green,
    Blue,
    Yellow,
}
/// Missing binding means the activity's normal controls. A null action disables
/// this button; it is also retained when its target device is removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub button: Button,
    #[serde(default)]
    pub gesture: Gesture,
    pub action: Option<Action>,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gesture {
    #[default]
    Short,
    Long,
}
impl Button {
    pub fn supports_long(self) -> bool {
        !matches!(
            self,
            Self::Up
                | Self::Down
                | Self::Left
                | Self::Right
                | Self::VolumeUp
                | Self::VolumeDown
                | Self::ChannelUp
                | Self::ChannelDown
        )
    }
}
impl Button {
    pub fn from_evdev(code: u16) -> Option<Self> {
        Some(match code {
            158 | 1 => Self::Back,
            59 | 172 => Self::Home,
            60 => Self::Power,
            103 => Self::Up,
            108 => Self::Down,
            105 => Self::Left,
            106 => Self::Right,
            28 | 96 | 352 | 353 => Self::Ok,
            115 => Self::VolumeUp,
            114 => Self::VolumeDown,
            104 | 402 => Self::ChannelUp,
            109 | 403 => Self::ChannelDown,
            113 => Self::Mute,
            62 => Self::Lights,
            63 => Self::Activity,
            64 => Self::Music,
            65 => Self::Tv,
            61 => Self::Microphone,
            139 => Self::Menu,
            66 | 398 => Self::Red,
            67 | 399 => Self::Green,
            68 | 401 => Self::Blue,
            87 | 400 => Self::Yellow,
            _ => return None,
        })
    }
}
const POWER: &[(&str, &str)] = &[("on", "On"), ("off", "Off"), ("toggle", "Toggle on / off")];
/// The entity's domain: saved devices carry `<connection_id>/<entity_id>`, and
/// `validate::valid_ha_resource` has already tied each domain to one DeviceKind.
pub(crate) fn ha_domain(entity_id: &str) -> &str {
    entity_id
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
}
/// Deliberately finite: never accept arbitrary RPC or shell commands in mappings.
pub fn functions(integration: &Integration) -> &'static [(&'static str, &'static str)] {
    match integration {
        Integration::Sonos { .. } => &[
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Mute"),
            ("mute-on", "Mute on"),
            ("mute-off", "Mute off"),
        ],
        Integration::Kodi { .. } => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Context menu"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next item"),
            ("previous", "Previous item"),
        ],
        Integration::AndroidTv => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            ("power-on", "Wake"),
            ("power-off", "Sleep"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
        ],
        Integration::AppleTv => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("power-on", "Wake"),
            ("power-off", "Sleep"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
        ],
        // Tizen: power-on is Wake-on-LAN, power-off is the TV's toggle key.
        // There is no next/previous or play-pause key in the documented set.
        Integration::Tizen => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            ("power-on", "Wake (Wake-on-LAN)"),
            ("power-off", "Power key (toggle)"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
            ("red", "Red"),
            ("green", "Green"),
            ("blue", "Blue"),
            ("yellow", "Yellow"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("stop", "Stop"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
        ],
        // Bluetooth: HID consumer-control usages, one per key. Which ones a
        // TV honours varies by make; power is the toggle usage and only
        // reaches a TV that is on (an off TV has no Bluetooth link to wake).
        Integration::BluetoothTv => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            ("power-off", "Power key (toggle)"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
        ],
        Integration::WebOs => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            // The TV's single power key: the connection's IR `power` line, or
            // wake / power-off over the network by the TV's current state.
            ("toggle", "Power toggle"),
            ("power-on", "Power on"),
            ("power-off", "Power off"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
            ("red", "Red"),
            ("green", "Green"),
            ("blue", "Blue"),
            ("yellow", "Yellow"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("stop", "Stop"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
        ],
        Integration::Denon { .. } => &[
            ("power-on", "Main zone on"),
            ("power-off", "Main zone off"),
            ("volume-up", "Volume up (0.5 dB)"),
            ("volume-down", "Volume down (0.5 dB)"),
            ("mute", "Toggle mute"),
            ("mute-on", "Mute"),
            ("mute-off", "Unmute"),
        ],
        Integration::Ir { .. } => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            ("toggle", "Power toggle"),
            ("power-on", "Power on (discrete)"),
            ("power-off", "Power off (discrete)"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Mute toggle"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
            ("red", "Red"),
            ("green", "Green"),
            ("blue", "Blue"),
            ("yellow", "Yellow"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
        ],
        // Per entity domain, not per integration: couch-ha drives lights, covers
        // and climates through three separate service sets, and validation has
        // already tied each domain to one DeviceKind. `position:<n>` and
        // `dim:<n>` are not here because a level needs a number from the picker.
        Integration::HomeAssistant { entity_id } => match ha_domain(entity_id) {
            "light" => POWER,
            "cover" => &[("open", "Open"), ("close", "Close"), ("stop", "Stop")],
            // set_hvac_mode and one step of the thermostat's own increment;
            // an entity that lacks a mode rejects it rather than guessing.
            "climate" => &[
                ("mode:off", "Off"),
                ("mode:heat", "Heat"),
                ("mode:cool", "Cool"),
                ("mode:heat_cool", "Heat / cool"),
                ("mode:auto", "Auto"),
                ("mode:dry", "Dry"),
                ("mode:fan_only", "Fan only"),
                ("temperature-up", "Warmer"),
                ("temperature-down", "Cooler"),
            ],
            _ => &[],
        },
        Integration::Hue { .. } | Integration::Matter { .. } => POWER,
        _ => &[],
    }
}
pub fn repeatable(command: &str) -> bool {
    crate::commands::Function::parse(command).is_some_and(|f| f.repeatable())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    #[test]
    fn ha_entities_advertise_their_own_domain_and_nothing_else() {
        let ids = |entity_id: &str| -> Vec<&str> {
            functions(&Integration::HomeAssistant {
                entity_id: entity_id.into(),
            })
            .iter()
            .map(|f| f.0)
            .collect()
        };
        for entity_id in ["light.office", "ha-one/light.office"] {
            assert_eq!(ids(entity_id), ["on", "off", "toggle"], "{entity_id}");
        }
        for entity_id in ["cover.office", "ha-one/cover.office"] {
            assert_eq!(ids(entity_id), ["open", "close", "stop"], "{entity_id}");
        }
        for entity_id in ["climate.office", "ha-two/climate.office"] {
            let offered = ids(entity_id);
            assert!(
                offered.contains(&"mode:heat") && offered.contains(&"temperature-up"),
                "{entity_id}"
            );
            assert!(
                !offered.contains(&"on") && !offered.contains(&"open"),
                "{entity_id}"
            );
        }
        // An unknown domain never inherits another domain's keys.
        for entity_id in ["switch.office", "ha-one/sensor.office", "nonsense"] {
            assert!(ids(entity_id).is_empty(), "{entity_id}");
        }
    }
    #[test]
    fn old_activities_keep_defaults_and_new_bindings_validate() {
        let mut config = crate::Config::seed();
        let action = Action::new("living-kodi", "ok");
        config.activities[0].buttons = vec![
            Binding {
                button: Button::Ok,
                gesture: Gesture::Short,
                action: Some(action.clone()),
            },
            Binding {
                button: Button::Ok,
                gesture: Gesture::Long,
                action: None,
            },
        ];
        assert!(config.validate().is_ok());
        let raw = serde_json::to_vec(&config).unwrap();
        let restored: crate::Config = serde_json::from_slice(&raw).unwrap();
        assert_eq!(restored, config);
        let duplicate = config.activities[0].buttons[0].clone();
        config.activities[0].buttons.push(duplicate);
        assert!(config.validate().is_err());
        config.activities[0].buttons = vec![Binding {
            button: Button::Up,
            gesture: Gesture::Long,
            action: None,
        }];
        assert!(config.validate().is_err());
        config.activities[0].buttons = vec![Binding {
            button: Button::Ok,
            gesture: Gesture::Short,
            action: Some(Action::new("living-kodi", "arbitrary-rpc")),
        }];
        assert!(config.validate().is_err());
        config.activities[0].buttons = vec![Binding {
            button: Button::Ok,
            gesture: Gesture::Short,
            action: Some(action),
        }];
        config.remove_device(&"living-room".into(), &"living-kodi".into());
        assert!(config.validate().is_ok());
        assert!(
            config.activities[0].buttons[0].action.is_none(),
            "removing target disables binding instead of silently reverting to a different device"
        );
        let legacy: crate::Activity =
            serde_json::from_str(r#"{"id":"watch","name":"Watch","room":"room"}"#).unwrap();
        assert_eq!(legacy.buttons, Vec::new());
    }
    #[test]
    fn measured_shortcuts_and_hold_support_match_the_hardware() {
        for (code, button) in [
            (62, Button::Lights),
            (63, Button::Activity),
            (64, Button::Music),
            (65, Button::Tv),
        ] {
            assert_eq!(Button::from_evdev(code), Some(button));
            assert!(button.supports_long());
        }
        for code in [103, 108, 105, 106, 115, 114, 104, 109] {
            assert!(!Button::from_evdev(code).unwrap().supports_long());
        }
        assert!(!repeatable("mute"));
        assert!(!repeatable("ok"));
        assert!(repeatable("volume-up"));
    }
}
