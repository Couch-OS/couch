//! Area quick-access keys: what the shortcut and color keys do on the hub.
//!
//! The decision is made on the UI thread from the configuration snapshot and
//! the projected area list, and comes back as a [`Dispatch`] the main loop
//! performs with the same calls a tap on the screen would make. Only a light
//! toggle talks to the network, and that goes through a worker like every
//! other device request, so a slow bridge never stalls a frame.
use crate::{connections, home::Area};
use couch_model::{buttons::Button, Config, Id, Integration, ShortcutAction};
use std::sync::{mpsc, Arc};

/// What the hub should do for a key. Ids for the device screens carry the
/// same `device:<id>` form the room list hands those screens.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    /// Slide to this index in the projected area list.
    ShowArea(usize),
    /// `open-activity`: an activity id, or `device:<id>` for a player.
    OpenActivity(String),
    OpenThermostat(String, String),
    OpenTv(String, String),
    OpenCamera(String, String),
    /// Show the room list with this device's row focused.
    OpenRoom(Id, usize),
    /// Switch the device on or off on the worker.
    Toggle(Id),
    /// Nothing the remote can do for this target; say so.
    Unavailable(String),
}

/// The action the current area assigns to a key, resolved against what the
/// house looks like now. `None` means the key keeps its usual (no) meaning.
pub fn plan(config: &Config, areas: &[Area], current: usize, button: Button) -> Option<Dispatch> {
    let area = areas.get(current)?;
    let action = &area.shortcuts.iter().find(|s| s.button == button)?.action;
    Some(match action {
        ShortcutAction::Area { area } => {
            let index = areas.iter().position(|a| a.id.as_ref() == Some(area));
            match index {
                Some(index) => Dispatch::ShowArea(index),
                None => Dispatch::Unavailable("That area was removed".into()),
            }
        }
        ShortcutAction::Activity { activity } => match config.activity(activity) {
            Some(a) => Dispatch::OpenActivity(a.id.to_string()),
            None => Dispatch::Unavailable("That activity was removed".into()),
        },
        ShortcutAction::Toggle { device } => {
            match config.devices().find(|(_, d)| &d.id == device) {
                Some((_, d)) if config.can_toggle(d) => Dispatch::Toggle(d.id.clone()),
                Some(_) => {
                    Dispatch::Unavailable("This device cannot be switched from a key".into())
                }
                None => Dispatch::Unavailable("That device was removed".into()),
            }
        }
        ShortcutAction::Device { device } => open_device(config, device),
    })
}

/// The screen a device row opens from the room list, for a device named by id.
fn open_device(config: &Config, id: &Id) -> Dispatch {
    let Some((room, device)) = config.devices().find(|(_, d)| &d.id == id) else {
        return Dispatch::Unavailable("That device was removed".into());
    };
    let resource = format!("device:{}", device.id);
    let name = device.name.clone();
    let integration = config.resolve_integration(&device.integration);
    match &integration {
        Some(Integration::HomeAssistant { entity_id }) if ha_domain(entity_id) == "climate" => {
            return Dispatch::OpenThermostat(resource, name);
        }
        Some(Integration::UnifiProtect { .. }) => return Dispatch::OpenCamera(resource, name),
        Some(Integration::Kodi { .. } | Integration::Sonos { .. }) => {
            return Dispatch::OpenActivity(resource);
        }
        _ => {}
    }
    if crate::lights::tv_connection(config, device.id.as_str()).is_some() {
        return Dispatch::OpenTv(resource, name);
    }
    if config.can_toggle(device) {
        // Lights and covers have no screen of their own: the room list is
        // where their brightness and position live.
        let row = crate::lights::row_of_device(config, &room.id, &device.id).unwrap_or(0);
        return Dispatch::OpenRoom(room.id.clone(), row);
    }
    Dispatch::Unavailable("Controls for this device are not available yet".into())
}

fn ha_domain(entity_id: &str) -> &str {
    connections::split(entity_id)
        .1
        .split_once('.')
        .map(|(domain, _)| domain)
        .unwrap_or("")
}

/// The one network action a key performs directly. Hue goes through the
/// shared push-maintained fleet the room list uses; Matter and Home Assistant
/// read the state first, the way the room list toggles them.
pub struct Controller {
    tx: mpsc::SyncSender<(Arc<Config>, Id)>,
    rx: mpsc::Receiver<String>,
}
impl Controller {
    pub fn new(hue: Arc<connections::HueFleet>) -> Self {
        let (tx, requests) = mpsc::sync_channel::<(Arc<Config>, Id)>(4);
        let (events, rx) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            let matter = connections::matter();
            while let Ok((config, id)) = requests.recv() {
                let message = match toggle(&config, &id, &hue, &matter) {
                    Ok(message) => message,
                    Err(error) => error,
                };
                let _ = events.try_send(message);
            }
        });
        Self { tx, rx }
    }
    /// Queue a toggle; a full queue means keys are being mashed faster than a
    /// bridge answers, and dropping the extra presses is the right outcome.
    pub fn toggle(&self, config: Arc<Config>, device: Id) -> bool {
        self.tx.try_send((config, device)).is_ok()
    }
    /// The result of the last toggle, worded for a toast.
    pub fn poll(&mut self) -> Option<String> {
        self.rx.try_iter().last()
    }
}

fn toggle(
    config: &Config,
    id: &Id,
    hue: &connections::HueFleet,
    matter: &connections::MatterFleet,
) -> Result<String, String> {
    let (_, device) = config
        .devices()
        .find(|(_, d)| &d.id == id)
        .ok_or("That device was removed")?;
    let name = device.name.clone();
    let state = match config.resolve_integration(&device.integration) {
        Some(Integration::Hue { light_id }) => hue.toggle(&light_id)?.on,
        Some(Integration::Matter { device }) => matter.toggle(&device)?.on,
        Some(Integration::HomeAssistant { entity_id }) => {
            let (client, raw) = connections::ha(&entity_id)?;
            if ha_domain(&entity_id) == "cover" {
                client
                    .cover_command(&raw, couch_ha::CoverCommand::Toggle)
                    .map_err(|e| e.to_string())?;
                let cover = client.cover(&raw).map_err(|e| e.to_string())?;
                return Ok(match cover.state.as_deref() {
                    Some("closed") => format!("{name} closed"),
                    Some("open") => format!("{name} open"),
                    Some(state) => format!("{name} {state}"),
                    None => name,
                });
            }
            let light = client.light(&raw).map_err(|e| e.to_string())?;
            let command = match light.on {
                Some(true) => couch_ha::Command::Off,
                Some(false) => couch_ha::Command::On,
                None => return Err(format!("{name} is unavailable")),
            };
            client.command(&raw, command).map_err(|e| e.to_string())?;
            client.light(&raw).map_err(|e| e.to_string())?.on
        }
        _ => return Err("This device cannot be switched from a key".into()),
    };
    Ok(match state {
        Some(true) => format!("{name} on"),
        Some(false) => format!("{name} off"),
        None => name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_model::{Shortcut, ShortcutAction};

    fn house() -> (Config, Vec<Area>) {
        let mut config = Config::seed();
        config.connections.push(couch_model::Connection {
            id: "ha".into(),
            name: "Home".into(),
            provider: couch_model::Provider::HomeAssistant,
        });
        let living = config.room_mut(&Id::new("living-room")).unwrap();
        living.devices.push(
            couch_model::Device::new(
                Id::new("living-heat"),
                "Heating",
                couch_model::DeviceKind::Thermostat,
            )
            .with_integration(Integration::Connection {
                connection_id: "ha".into(),
                resource_id: "climate.living".into(),
            }),
        );
        let upstairs = config.areas[1].id.clone();
        config.areas[0].shortcuts = vec![
            Shortcut {
                button: Button::Tv,
                action: ShortcutAction::Device {
                    device: Id::new("living-tv"),
                },
            },
            Shortcut {
                button: Button::Lights,
                action: ShortcutAction::Toggle {
                    device: Id::new("living-hue"),
                },
            },
            Shortcut {
                button: Button::Music,
                action: ShortcutAction::Activity {
                    activity: Id::new("watch-tv"),
                },
            },
            Shortcut {
                button: Button::Activity,
                action: ShortcutAction::Device {
                    device: Id::new("living-heat"),
                },
            },
            Shortcut {
                button: Button::Red,
                action: ShortcutAction::Area { area: upstairs },
            },
            Shortcut {
                button: Button::Green,
                action: ShortcutAction::Device {
                    device: Id::new("living-hue"),
                },
            },
            Shortcut {
                button: Button::Blue,
                action: ShortcutAction::Device {
                    device: Id::new("living-kodi"),
                },
            },
            Shortcut {
                button: Button::Yellow,
                action: ShortcutAction::Device {
                    device: Id::new("living-soundbar"),
                },
            },
        ];
        config.validate().unwrap();
        let areas = crate::home::project(&config);
        (config, areas)
    }

    #[test]
    fn every_kind_of_shortcut_maps_to_the_screen_a_tap_would_open() {
        let (config, areas) = house();
        let plan = |button| plan(&config, &areas, 0, button);
        assert_eq!(plan(Button::Red), Some(Dispatch::ShowArea(1)));
        assert_eq!(
            plan(Button::Music),
            Some(Dispatch::OpenActivity("watch-tv".into()))
        );
        assert_eq!(
            plan(Button::Lights),
            Some(Dispatch::Toggle(Id::new("living-hue")))
        );
        assert_eq!(
            plan(Button::Activity),
            Some(Dispatch::OpenThermostat(
                "device:living-heat".into(),
                "Heating".into()
            ))
        );
        assert_eq!(
            plan(Button::Tv),
            Some(Dispatch::OpenTv("device:living-tv".into(), "LG C3".into()))
        );
        // The Hue light is the second device, after the room's pinned activities.
        let pinned = crate::lights::activity_rows(&config, &Id::new("living-room"));
        assert!(pinned > 0);
        assert_eq!(
            plan(Button::Green),
            Some(Dispatch::OpenRoom(Id::new("living-room"), pinned + 1))
        );
        assert_eq!(
            plan(Button::Blue),
            Some(Dispatch::OpenActivity("device:living-kodi".into()))
        );
        assert!(matches!(
            plan(Button::Yellow),
            Some(Dispatch::Unavailable(_))
        ));
        // Unassigned keys, other areas and the synthetic ALL ROOMS page do nothing.
        assert_eq!(plan(Button::Ok), None);
        assert_eq!(super::plan(&config, &areas, 1, Button::Red), None);
        assert_eq!(
            super::plan(&config, &areas, areas.len() - 1, Button::Red),
            None
        );
    }

    #[test]
    fn targets_that_vanished_between_save_and_press_are_reported_not_ignored() {
        let (mut config, areas) = house();
        // The projected list is stale by design: it is rebuilt on the tick,
        // and a key can land in between.
        config.remove_area(&config.areas[1].id.clone());
        config.remove_activity(&Id::new("watch-tv"));
        config.remove_device(&Id::new("living-room"), &Id::new("living-hue"));
        let stale = |button| plan(&config, &areas, 0, button);
        assert!(
            matches!(stale(Button::Music), Some(Dispatch::Unavailable(m)) if m.contains("activity"))
        );
        assert!(
            matches!(stale(Button::Lights), Some(Dispatch::Unavailable(m)) if m.contains("device"))
        );
        assert!(
            matches!(stale(Button::Green), Some(Dispatch::Unavailable(m)) if m.contains("device"))
        );
        // The area list still has the page until the next projection, so the
        // slide still works; once reprojected the key is simply unassigned,
        // because removing the area removed the shortcut with it.
        assert_eq!(stale(Button::Red), Some(Dispatch::ShowArea(1)));
        let fresh = crate::home::project(&config);
        assert_eq!(plan(&config, &fresh, 0, Button::Red), None);
        assert_eq!(plan(&config, &fresh, 0, Button::Music), None);
    }
}
