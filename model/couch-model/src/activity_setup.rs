//! Explicit activity membership and executable on/off sequences. Legacy `steps`
//! remain untouched and are never silently promoted into executable commands.
use crate::{Action, Config, DeviceId, Icon};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivitySetup {
    #[serde(default)]
    pub custom_screen: bool,
    #[serde(default)]
    pub pages: Vec<ActivityPage>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub devices: Vec<DeviceId>,
    #[serde(default)]
    pub keep_awake: bool,
    #[serde(default)]
    pub on: Vec<SequenceStep>,
    #[serde(default)]
    pub off: Vec<SequenceStep>,
}
/// Ordered fixed-grid pages. Commands use the same typed catalog as mappings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityPage {
    pub title: String,
    #[serde(default)]
    pub widgets: Vec<ActivityWidget>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityWidget {
    pub label: String,
    #[serde(default)]
    pub icon: Option<Icon>,
    pub action: Action,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SequenceStep {
    Command { action: Action },
    Delay { ms: u32 },
}
impl ActivitySetup {
    pub fn validate(&self, config: &Config) -> Result<(), &'static str> {
        if self.description.len() > 1024 {
            return Err("Description must be at most 1024 bytes");
        }
        for (i, id) in self.devices.iter().enumerate() {
            if self.devices[..i].contains(id) || !config.devices().any(|(_, d)| &d.id == id) {
                return Err("Included devices must exist and appear only once");
            }
        }
        if self.pages.len() > 8 || self.custom_screen && self.pages.is_empty() {
            return Err("Custom screens require 1 to 8 pages");
        }
        for page in &self.pages {
            if page.title.trim().is_empty() || page.title.len() > 64 {
                return Err("Page titles must contain 1 to 64 bytes");
            }
            if page.widgets.len() > 6 {
                return Err("Each page supports at most 6 command buttons");
            }
            for widget in &page.widgets {
                if widget.label.trim().is_empty() || widget.label.len() > 80 {
                    return Err("Button labels must contain 1 to 80 bytes");
                }
                if !self.supports_action(config, &widget.action) {
                    return Err("Choose a supported command from an included device");
                }
            }
        }
        for steps in [&self.on, &self.off] {
            if steps.len() > 64 {
                return Err("A sequence supports at most 64 steps");
            }
            let mut wait = 0u32;
            for step in steps {
                match step {
                    SequenceStep::Delay { ms } => {
                        if *ms == 0 || *ms > 30_000 {
                            return Err("Delays must be between 1 and 30000 ms");
                        }
                        wait += ms;
                    }
                    SequenceStep::Command { action } => {
                        let valid = self.supports_action(config, action);
                        if !valid {
                            return Err("Choose a supported command from an included device");
                        }
                    }
                }
            }
            if wait > 120_000 {
                return Err("Total delay must not exceed two minutes per sequence");
            }
        }
        Ok(())
    }
    fn supports_action(&self, config: &Config, action: &Action) -> bool {
        self.devices.contains(&action.device)
            && config
                .devices()
                .find(|(_, d)| d.id == action.device)
                .is_some_and(|(_, d)| {
                    crate::commands::Function::parse(&action.command)
                        .is_some_and(|f| f.supports_device(d, config))
                })
    }
    pub fn forget_device(&mut self, id: &DeviceId) {
        self.devices.retain(|d| d != id);
        for page in &mut self.pages {
            page.widgets.retain(|w| &w.action.device != id);
        }
        for steps in [&mut self.on, &mut self.off] {
            steps
                .retain(|s| !matches!(s, SequenceStep::Command { action } if &action.device == id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_pages_validate_commands_labels_and_size_limits() {
        let config = Config::seed();
        let mut setup = ActivitySetup {
            custom_screen: true,
            devices: alloc::vec!["living-kodi".into()],
            pages: alloc::vec![ActivityPage {
                title: "Playback".into(),
                widgets: alloc::vec![ActivityWidget {
                    label: "Play / pause".into(),
                    icon: None,
                    action: Action::new("living-kodi", "play-pause")
                }]
            }],
            ..Default::default()
        };
        assert!(setup.validate(&config).is_ok());
        setup.pages[0].widgets[0].action.command = "power-on".into();
        assert!(setup.validate(&config).is_err());
        setup.pages[0].widgets[0].action.command = "play-pause".into();
        setup.pages[0].widgets = alloc::vec![setup.pages[0].widgets[0].clone();7];
        assert!(setup.validate(&config).is_err());
        setup.pages[0].widgets.truncate(1);
        setup.pages[0].title = " ".into();
        assert!(setup.validate(&config).is_err());
        setup.pages[0].title = "Playback".into();
        setup.forget_device(&"living-kodi".into());
        assert!(setup.pages[0].widgets.is_empty());
        assert!(setup.validate(&config).is_ok());
        setup.pages = alloc::vec![setup.pages[0].clone();9];
        assert!(setup.validate(&config).is_err());
        setup.pages.clear();
        assert!(setup.validate(&config).is_err());
        setup.custom_screen = false;
        assert!(setup.validate(&config).is_ok());
    }
    #[test]
    fn legacy_setups_default_to_source_screen_and_empty_pages() {
        let setup: ActivitySetup =
            serde_json::from_str(r#"{"devices":[],"on":[],"off":[]}"#).unwrap();
        assert!(!setup.custom_screen);
        assert!(setup.pages.is_empty());
    }
    #[test]
    fn sequences_require_supported_commands_from_members_and_bound_delays() {
        let config = Config::seed();
        let mut setup = ActivitySetup {
            devices: alloc::vec!["living-kodi".into()],
            on: alloc::vec![
                SequenceStep::Command {
                    action: Action::new("living-kodi", "home")
                },
                SequenceStep::Delay { ms: 1000 }
            ],
            ..Default::default()
        };
        assert!(setup.validate(&config).is_ok());
        setup.devices.clear();
        assert!(setup.validate(&config).is_err());
        setup.devices.push("living-kodi".into());
        setup.on[0] = SequenceStep::Command {
            action: Action::new("living-kodi", "power-on"),
        };
        assert!(setup.validate(&config).is_err());
        setup.on = alloc::vec![SequenceStep::Delay { ms: 30001 }];
        assert!(setup.validate(&config).is_err());
        setup.on = alloc::vec![SequenceStep::Delay{ms:30000};5];
        assert!(setup.validate(&config).is_err());
    }
    #[test]
    fn config_device_removal_cleans_both_lifecycle_sequences() {
        let mut config = Config::seed();
        config.activities[0].setup = ActivitySetup {
            devices: alloc::vec!["living-kodi".into()],
            on: alloc::vec![SequenceStep::Command {
                action: Action::new("living-kodi", "home")
            }],
            off: alloc::vec![SequenceStep::Command {
                action: Action::new("living-kodi", "stop")
            }],
            ..Default::default()
        };
        config
            .remove_device(&"living-room".into(), &"living-kodi".into())
            .unwrap();
        assert!(config.activities[0].setup.devices.is_empty());
        assert!(config.activities[0].setup.on.is_empty());
        assert!(config.activities[0].setup.off.is_empty());
        assert!(config.validate().is_ok());
    }
    #[test]
    fn removing_device_cleans_sequences_but_preserves_delays() {
        let mut setup = ActivitySetup {
            devices: alloc::vec!["player".into()],
            on: alloc::vec![
                SequenceStep::Command {
                    action: Action::new("player", "home")
                },
                SequenceStep::Delay { ms: 1000 }
            ],
            ..Default::default()
        };
        setup.forget_device(&"player".into());
        assert!(setup.devices.is_empty());
        assert_eq!(setup.on, alloc::vec![SequenceStep::Delay { ms: 1000 }]);
    }
}
