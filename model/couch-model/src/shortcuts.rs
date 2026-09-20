//! Quick-access keys: what the four shortcut keys and four color keys do on
//! the home screen while an area is showing.
//!
//! An area is the page the remote sits on; the keys along its bottom edge
//! are the fastest thing a person can reach without looking. Which thing each
//! key reaches is the area's business, the way its rooms and scenes are: the
//! climate key in the living room opens that room's thermostat, the same key
//! upstairs opens the bedroom's. Nothing here runs a free-form command - each
//! action names one thing already in the configuration, and the remote does
//! with it what a tap on the screen would.
use crate::{
    buttons::Button, ActivityId, Area, AreaId, Config, Device, DeviceId, Integration, Problem,
};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// One of the four things a quick-access key can reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ShortcutAction {
    /// Open the device's controls: the thermostat, TV, player or camera
    /// screen, or the room list with the device highlighted.
    Device { device: DeviceId },
    /// Switch a light, light group or cover on or off without leaving the
    /// home screen. Only devices whose state the remote can read qualify, see
    /// [`Config::can_toggle`].
    Toggle { device: DeviceId },
    /// Open an activity, running its start sequence when it has one.
    Activity { activity: ActivityId },
    /// Show another area's page.
    Area { area: AreaId },
}

/// A key on the home screen and what it reaches. One entry per key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub button: Button,
    pub action: ShortcutAction,
}

/// The keys an area may assign, in front-panel order: the four shortcut keys
/// above the color row, then the color row.
pub const SHORTCUT_BUTTONS: &[Button] = &[
    Button::Lights,
    Button::Activity,
    Button::Music,
    Button::Tv,
    Button::Red,
    Button::Green,
    Button::Blue,
    Button::Yellow,
];

impl Button {
    /// Whether an area may assign this key. Navigation, volume, power and the
    /// microphone keep their global meaning on the home screen.
    pub fn is_shortcut(self) -> bool {
        SHORTCUT_BUTTONS.contains(&self)
    }
}

impl ShortcutAction {
    pub fn device(&self) -> Option<&DeviceId> {
        match self {
            Self::Device { device } | Self::Toggle { device } => Some(device),
            _ => None,
        }
    }
}

impl Config {
    /// The action an area assigns to a key, if any.
    pub fn shortcut<'a>(&self, area: &'a Area, button: Button) -> Option<&'a ShortcutAction> {
        area.shortcuts
            .iter()
            .find(|s| s.button == button)
            .map(|s| &s.action)
    }

    /// Whether a quick-access key can switch this device on and off: the
    /// integrations whose state the room list already reads and toggles.
    pub fn can_toggle(&self, device: &Device) -> bool {
        match self.resolve_integration(&device.integration) {
            Some(Integration::Hue { light_id }) => !light_id.starts_with("scene:"),
            Some(Integration::Matter { .. }) => true,
            Some(Integration::HomeAssistant { entity_id }) => {
                let entity = entity_id.rsplit('/').next().unwrap_or("");
                matches!(
                    entity.split_once('.').map(|(d, _)| d),
                    Some("light" | "cover")
                )
            }
            // Protocol 3 (unreleased): a light or cover that is a child of a
            // packaged connection, when its kind declares `toggle`.
            Some(Integration::Plugin {
                child: Some(_),
                capabilities,
                ..
            }) => {
                capabilities.iter().any(|c| c.id == "toggle")
                    && self
                        .device_child_kind(&device.integration)
                        .is_some_and(|kind| {
                            matches!(
                                kind.component,
                                crate::ChildComponent::Light | crate::ChildComponent::Cover
                            )
                        })
            }
            _ => false,
        }
    }

    pub(crate) fn validate_shortcuts(&self, problems: &mut Vec<Problem>) {
        for (i, area) in self.areas.iter().enumerate() {
            for (j, shortcut) in area.shortcuts.iter().enumerate() {
                let at = alloc::format!("areas[{i}].shortcuts[{j}]");
                if !shortcut.button.is_shortcut() {
                    problems.push(Problem {
                        at: at.clone(),
                        message: "Only the shortcut and color keys can be assigned".into(),
                    });
                }
                if area.shortcuts[..j]
                    .iter()
                    .any(|s| s.button == shortcut.button)
                {
                    problems.push(Problem {
                        at: at.clone(),
                        message: "Choose one action per key".into(),
                    });
                }
                let message = match &shortcut.action {
                    ShortcutAction::Device { device } => self
                        .devices()
                        .all(|(_, d)| &d.id != device)
                        .then_some("Choose an existing device"),
                    ShortcutAction::Toggle { device } => {
                        match self.devices().find(|(_, d)| &d.id == device) {
                            None => Some("Choose an existing device"),
                            Some((_, d)) if !self.can_toggle(d) => Some(
                                "Choose a Hue, Matter or Home Assistant light or cover to toggle",
                            ),
                            Some(_) => None,
                        }
                    }
                    ShortcutAction::Activity { activity } => self
                        .activity(activity)
                        .is_none()
                        .then_some("Choose an existing activity"),
                    ShortcutAction::Area { area: target } => {
                        if target == &area.id {
                            Some("Choose a different area to switch to")
                        } else {
                            self.area(target)
                                .is_none()
                                .then_some("Choose an existing area")
                        }
                    }
                };
                if let Some(message) = message {
                    problems.push(Problem {
                        at,
                        message: message.into(),
                    });
                }
            }
        }
    }

    /// Drop every shortcut that reaches the given area; called when it is removed.
    pub(crate) fn forget_area_shortcuts(&mut self, id: &AreaId) {
        for area in &mut self.areas {
            area.shortcuts
                .retain(|s| !matches!(&s.action, ShortcutAction::Area { area } if area == id));
        }
    }

    pub(crate) fn forget_activity_shortcuts(&mut self, id: &ActivityId) {
        for area in &mut self.areas {
            area.shortcuts.retain(
                |s| !matches!(&s.action, ShortcutAction::Activity { activity } if activity == id),
            );
        }
    }

    pub(crate) fn forget_device_shortcuts(&mut self, id: &DeviceId) {
        for area in &mut self.areas {
            area.shortcuts.retain(|s| s.action.device() != Some(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Id, Provider};
    use alloc::{string::ToString, vec};

    fn shortcut(button: Button, action: ShortcutAction) -> Shortcut {
        Shortcut { button, action }
    }

    #[test]
    fn old_areas_have_no_shortcuts_and_new_ones_round_trip() {
        let old: Area = serde_json::from_str(r#"{"id":"a","name":"A"}"#).unwrap();
        assert!(old.shortcuts.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("shortcuts"));

        let mut cfg = Config::seed();
        cfg.areas[0].shortcuts = vec![
            shortcut(
                Button::Tv,
                ShortcutAction::Device {
                    device: Id::new("living-tv"),
                },
            ),
            shortcut(
                Button::Lights,
                ShortcutAction::Toggle {
                    device: Id::new("living-hue"),
                },
            ),
            shortcut(
                Button::Music,
                ShortcutAction::Activity {
                    activity: Id::new("watch-tv"),
                },
            ),
            shortcut(
                Button::Red,
                ShortcutAction::Area {
                    area: cfg.areas[1].id.clone(),
                },
            ),
        ];
        cfg.validate().expect("seed shortcuts validate");
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""action":{"kind":"area","area":"#));
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, cfg);
        assert!(matches!(
            cfg.shortcut(&cfg.areas[0], Button::Tv),
            Some(ShortcutAction::Device { device }) if device.as_str() == "living-tv"
        ));
        assert!(cfg.shortcut(&cfg.areas[0], Button::Green).is_none());
    }

    #[test]
    fn shortcuts_need_real_targets_assignable_keys_and_one_action_per_key() {
        let mut cfg = Config::seed();
        let area_id = cfg.areas[0].id.clone();
        let cases: Vec<(Shortcut, &str)> = vec![
            (
                shortcut(
                    Button::Ok,
                    ShortcutAction::Activity {
                        activity: Id::new("watch-tv"),
                    },
                ),
                "Only the shortcut",
            ),
            (
                shortcut(
                    Button::Red,
                    ShortcutAction::Device {
                        device: Id::new("ghost"),
                    },
                ),
                "existing device",
            ),
            (
                shortcut(
                    Button::Red,
                    ShortcutAction::Toggle {
                        device: Id::new("ghost"),
                    },
                ),
                "existing device",
            ),
            (
                shortcut(
                    Button::Red,
                    ShortcutAction::Toggle {
                        device: Id::new("living-tv"),
                    },
                ),
                "to toggle",
            ),
            (
                shortcut(
                    Button::Red,
                    ShortcutAction::Activity {
                        activity: Id::new("ghost"),
                    },
                ),
                "existing activity",
            ),
            (
                shortcut(
                    Button::Red,
                    ShortcutAction::Area {
                        area: Id::new("ghost"),
                    },
                ),
                "existing area",
            ),
            (
                shortcut(Button::Red, ShortcutAction::Area { area: area_id }),
                "different area",
            ),
        ];
        for (bad, expect) in cases {
            cfg.areas[0].shortcuts = vec![bad];
            let err = cfg.validate().unwrap_err();
            assert_eq!(err.problems.len(), 1, "{expect}: {err}");
            assert_eq!(err.problems[0].at, "areas[0].shortcuts[0]");
            assert!(err.problems[0].message.contains(expect), "{expect}: {err}");
        }
        cfg.areas[0].shortcuts = vec![
            shortcut(
                Button::Blue,
                ShortcutAction::Activity {
                    activity: Id::new("watch-tv"),
                },
            ),
            shortcut(
                Button::Blue,
                ShortcutAction::Activity {
                    activity: Id::new("watch-tv"),
                },
            ),
        ];
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems[0].at, "areas[0].shortcuts[1]");
        assert!(err.problems[0].message.contains("one action per key"));
    }

    #[test]
    fn toggling_is_offered_for_lights_the_room_list_can_read() {
        let mut cfg = Config::seed();
        cfg.connections.push(crate::Connection {
            id: "ha".into(),
            name: "Home".into(),
            provider: Provider::HomeAssistant,
        });
        let light = Device::new(Id::new("l"), "L", crate::DeviceKind::Light).with_integration(
            Integration::Connection {
                connection_id: "ha".into(),
                resource_id: "light.desk".into(),
                child: None,
            },
        );
        let cover = Device::new(Id::new("c"), "C", crate::DeviceKind::Blind).with_integration(
            Integration::Connection {
                connection_id: "ha".into(),
                resource_id: "cover.desk".into(),
                child: None,
            },
        );
        let climate = Device::new(Id::new("t"), "T", crate::DeviceKind::Thermostat)
            .with_integration(Integration::Connection {
                connection_id: "ha".into(),
                resource_id: "climate.desk".into(),
                child: None,
            });
        let hue = Device::new(Id::new("h"), "H", crate::DeviceKind::Light).with_integration(
            Integration::Hue {
                light_id: "abc".into(),
            },
        );
        let hue_scene = Device::new(Id::new("s"), "S", crate::DeviceKind::Light).with_integration(
            Integration::Hue {
                light_id: "scene:abc".into(),
            },
        );
        let matter = Device::new(Id::new("m"), "M", crate::DeviceKind::Light).with_integration(
            Integration::Matter {
                device: "c/1/1".into(),
            },
        );
        let tv = Device::new(Id::new("tv"), "TV", crate::DeviceKind::Tv)
            .with_integration(Integration::WebOs);
        assert!(cfg.can_toggle(&light));
        assert!(cfg.can_toggle(&cover));
        assert!(!cfg.can_toggle(&climate));
        assert!(cfg.can_toggle(&hue));
        assert!(!cfg.can_toggle(&hue_scene));
        assert!(cfg.can_toggle(&matter));
        assert!(!cfg.can_toggle(&tv));
        assert!(!cfg.can_toggle(&Device::new(Id::new("x"), "X", crate::DeviceKind::Light)));
    }

    #[test]
    fn removing_a_target_removes_the_shortcuts_that_reach_it() {
        let mut cfg = Config::seed();
        let upstairs = cfg.areas[1].id.clone();
        cfg.areas[0].shortcuts = vec![
            shortcut(
                Button::Tv,
                ShortcutAction::Device {
                    device: Id::new("living-tv"),
                },
            ),
            shortcut(
                Button::Lights,
                ShortcutAction::Toggle {
                    device: Id::new("living-hue"),
                },
            ),
            shortcut(
                Button::Music,
                ShortcutAction::Activity {
                    activity: Id::new("watch-tv"),
                },
            ),
            shortcut(
                Button::Red,
                ShortcutAction::Area {
                    area: upstairs.clone(),
                },
            ),
            shortcut(
                Button::Green,
                ShortcutAction::Activity {
                    activity: Id::new("kitchen-radio"),
                },
            ),
        ];
        cfg.validate().unwrap();
        cfg.remove_device(&Id::new("living-room"), &Id::new("living-tv"));
        cfg.remove_activity(&Id::new("watch-tv"));
        cfg.remove_area(&upstairs);
        // The kitchen radio goes with its room, and its shortcut with it.
        cfg.remove_room(&Id::new("kitchen"));
        let left: Vec<Button> = cfg.areas[0].shortcuts.iter().map(|s| s.button).collect();
        assert_eq!(left, vec![Button::Lights]);
        cfg.validate().unwrap();
        // A hand-edited file that still points at a removed device is rejected
        // rather than silently ignored.
        cfg.areas[0].shortcuts.push(shortcut(
            Button::Yellow,
            ShortcutAction::Device {
                device: Id::new("living-tv"),
            },
        ));
        assert_eq!(
            cfg.validate().unwrap_err().problems[0].at.to_string(),
            "areas[0].shortcuts[1]"
        );
    }
}
