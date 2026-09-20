//! Command words a release before step validation (`ce1db37`) could save,
//! which `commands::Function::parse` has never accepted, rewritten to their
//! faithful modern equivalent wherever a step or binding carries a command.
//!
//! Before scene and activity steps were checked against `Function::parse`,
//! `Action::suggestions` advertised eight words the executor could never run:
//! `dim:30`, `dim:70`, `bright`, `volume:20`, `open`, `close`, `target:18` and
//! `target:21` (`ce1db37`'s commit message names all eight). Six of those
//! parse again on their own now that `Dim`, `Volume`, `Open` and `Close`
//! exist; `target:18`/`target:21` were never reachable from any picker -
//! `Action::suggestions` had no caller at all in `.130` - so no saved file can
//! hold them, and there is no absolute-temperature command to translate them
//! into today anyway. Only `"bright"` is both still unparseable and provably
//! real: releases up to about `.137` shipped it in the seed house's "Dinner"
//! and "Cooking" scenes, so any remote provisioned from one still has it in
//! `config.json`, and `couch-confd` refuses to start on that file
//! (`Store::open` validates before it migrates).
//!
//! `"bright"` meant a light at full brightness before a level of any kind
//! existed (there was no `Function::Dim` to send it through); `dim:100` is
//! that same meaning in today's vocabulary. It parses, and for the lights the
//! seed scenes named it against (Hue) it validates too.
use crate::{Action, Config, SequenceStep};

/// The one legacy word this rewrites, and what it becomes. `None` for
/// anything else, including every other word `ce1db37` orphaned - those stay
/// refused by `Function::parse` exactly as they are today.
fn rewrite(command: &str) -> Option<&'static str> {
    (command == "bright").then_some("dim:100")
}

impl Config {
    /// Rewrite every legacy command word this crate knows a faithful modern
    /// equivalent for, wherever a step or binding carries one: button
    /// bindings, activity steps, an activity's on/off sequence, its page
    /// widgets, and scene steps. Returns whether anything changed, the same
    /// way [`Config::migrate`] does, so a store can bump its revision and
    /// write once.
    ///
    /// Idempotent: a file with no legacy words - including one this already
    /// rewrote - changes nothing and returns `false`.
    pub fn migrate_commands(&mut self) -> bool {
        let mut changed = false;
        let mut fix = |action: &mut Action| {
            if let Some(replacement) = rewrite(&action.command) {
                action.command = replacement.into();
                changed = true;
            }
        };
        for scene in &mut self.scenes {
            for step in &mut scene.steps {
                fix(step);
            }
        }
        for activity in &mut self.activities {
            for step in &mut activity.steps {
                fix(step);
            }
            for binding in &mut activity.buttons {
                if let Some(action) = &mut binding.action {
                    fix(action);
                }
            }
            for step in activity
                .setup
                .on
                .iter_mut()
                .chain(activity.setup.off.iter_mut())
            {
                if let SequenceStep::Command { action } = step {
                    fix(action);
                }
            }
            for page in &mut activity.setup.pages {
                for widget in &mut page.widgets {
                    fix(&mut widget.action);
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buttons::{Binding, Button, Gesture},
        ActivityKind, ActivityPage, ActivityWidget, Id,
    };
    use alloc::{string::ToString, vec};

    /// The minimal relevant shape of the frozen `.130`-era fixture
    /// (`build/webui-review-empty.json`): a light device and two scenes,
    /// "Dinner" and "Cooking", each with one `"bright"` step against it.
    fn house_with_bright_scenes() -> Config {
        let mut config = Config::default();
        config.rooms.push(crate::Room {
            id: "kitchen".into(),
            name: "Kitchen".into(),
            icon: None,
            devices: vec![crate::Device::new(
                "kitchen-hue".into(),
                "Kitchen light",
                crate::DeviceKind::Light,
            )
            .with_integration(crate::Integration::Hue {
                light_id: "1".into(),
            })],
        });
        for (id, name) in [("dinner", "Dinner"), ("cooking", "Cooking")] {
            config.scenes.push(crate::Scene {
                id: id.into(),
                name: name.into(),
                icon: None,
                steps: vec![Action::new(Id::new("kitchen-hue"), "bright")],
                hue: None,
                resource: None,
                rooms: vec![],
            });
        }
        config
    }

    #[test]
    fn a_130_era_file_with_bright_scenes_fails_validation_until_migrated() {
        let mut config = house_with_bright_scenes();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unsupported command \"bright\""));
        assert!(config.migrate_commands());
        assert!(config.validate().is_ok());
        for scene in &config.scenes {
            assert_eq!(scene.steps[0].command, "dim:100");
        }
    }

    #[test]
    fn migrating_twice_changes_nothing_the_second_time() {
        let mut config = house_with_bright_scenes();
        assert!(config.migrate_commands());
        assert!(
            !config.migrate_commands(),
            "already rewritten, nothing left to do"
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn a_command_word_that_is_not_bright_stays_refused() {
        // `target:18` (a thermostat's old suggested word) never parses and is
        // not migrated: there is no absolute-temperature command to send it
        // to, and no picker ever let a user save it.
        let mut config = house_with_bright_scenes();
        for scene in &mut config.scenes {
            scene.steps[0].command = "target:18".into();
        }
        assert!(!config.migrate_commands());
        assert!(config.validate().is_err());
    }

    #[test]
    fn bright_is_rewritten_in_all_six_places_a_command_can_be_saved() {
        let mut config = house_with_bright_scenes();
        let device = Id::new("kitchen-hue");
        config.activities.push(crate::Activity {
            setup: crate::ActivitySetup {
                custom_screen: false,
                pages: vec![ActivityPage {
                    title: "Page".into(),
                    widgets: vec![ActivityWidget {
                        label: "Bright".into(),
                        icon: None,
                        action: Action::new(device.clone(), "bright"),
                    }],
                }],
                description: "".into(),
                devices: vec![device.clone()],
                keep_awake: false,
                on: vec![SequenceStep::Command {
                    action: Action::new(device.clone(), "bright"),
                }],
                off: vec![SequenceStep::Command {
                    action: Action::new(device.clone(), "bright"),
                }],
            },
            id: "dine".into(),
            name: "Dine".into(),
            kind: ActivityKind::Audio,
            room: "kitchen".into(),
            source: None,
            buttons: vec![Binding {
                button: Button::Lights,
                gesture: Gesture::Short,
                action: Some(Action::new(device.clone(), "bright")),
            }],
            steps: vec![Action::new(device, "bright")],
        });
        assert!(config.migrate_commands());
        assert!(!config.migrate_commands());
        let activity = &config.activities[0];
        assert_eq!(activity.steps[0].command, "dim:100");
        assert_eq!(
            activity.buttons[0].action.as_ref().unwrap().command,
            "dim:100"
        );
        assert_eq!(
            activity.setup.on[0],
            SequenceStep::Command {
                action: Action::new(Id::new("kitchen-hue"), "dim:100")
            }
        );
        assert_eq!(
            activity.setup.off[0],
            SequenceStep::Command {
                action: Action::new(Id::new("kitchen-hue"), "dim:100")
            }
        );
        assert_eq!(activity.setup.pages[0].widgets[0].action.command, "dim:100");
        assert!(config.validate().is_ok());
    }
}
