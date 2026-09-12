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
        Integration::WebOs => &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
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
        Integration::HomeAssistant { entity_id } if !entity_id.rsplit('/').next().unwrap_or("").starts_with("light.") => &[],
        Integration::Hue { .. } | Integration::HomeAssistant { .. } => {
            &[("on", "On"), ("off", "Off"), ("toggle", "Toggle on / off")]
        }
        _ => &[],
    }
}
pub fn repeatable(command:&str)->bool {crate::commands::Function::parse(command).is_some_and(|f|f.repeatable())}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    #[test]
    fn ha_environment_entities_do_not_advertise_light_commands() {
        for entity_id in ["cover.office", "climate.office", "ha-one/cover.office", "ha-two/climate.office"] {
            assert!(functions(&Integration::HomeAssistant { entity_id: entity_id.into() }).is_empty());
        }
        assert!(!functions(&Integration::HomeAssistant { entity_id: "ha-one/light.office".into() }).is_empty());
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
