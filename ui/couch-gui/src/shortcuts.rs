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

/// Integrations whose device opens the player screen: Kodi and Sonos. The
/// room list and the shortcut keys both ask here, so they cannot disagree. A
/// packaged device is not one of them: it opens the core control screen, as
/// `lights::tv_connection` says.
pub fn opens_player(integration: &Integration) -> bool {
    matches!(
        integration,
        Integration::Kodi { .. } | Integration::Sonos { .. }
    )
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
        Some(integration) if opens_player(integration) => {
            return Dispatch::OpenActivity(resource);
        }
        // Saved while its client was built in; couch-confd has not handed it
        // to the package yet. The same sentence its keys and its row give.
        Some(integration) if integration.legacy_builtin().is_some() => {
            let row = integration.legacy_builtin().expect("checked by the guard");
            return Dispatch::Unavailable(row.needs_package());
        }
        _ => {}
    }
    if crate::lights::tv_connection(config, device.id.as_str()).is_some() {
        return Dispatch::OpenTv(resource, name);
    }
    // Lights and covers have no screen of their own: the room list is where
    // their brightness and position live. A packaged lamp or blind is one of
    // them even when its kind cannot be toggled from a key.
    if config.can_toggle(device) || crate::lights::plugin_row(config, device).is_some() {
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
    toggle_with(config, id, hue, matter, &mut crate::lights::plugin_socket)
}

fn toggle_with(
    config: &Config,
    id: &Id,
    hue: &connections::HueFleet,
    matter: &connections::MatterFleet,
    ask: crate::lights::Ask,
) -> Result<String, String> {
    let (_, device) = config
        .devices()
        .find(|(_, d)| &d.id == id)
        .ok_or("That device was removed")?;
    let name = device.name.clone();
    // A packaged lamp or blind, when its kind says it can be toggled: the
    // same `toggle` the room row sends, through the same worker-side call.
    if config.can_toggle(device) {
        if let Some(row) = crate::lights::plugin_row(config, device) {
            return match crate::lights::plugin_toggle(&row, &name, ask)? {
                Some(crate::lights::DeviceState::Cover(cover)) => {
                    Ok(match cover.state.as_deref() {
                        Some("closed") => format!("{name} closed"),
                        Some("open") => format!("{name} open"),
                        Some(state) => format!("{name} {state}"),
                        None => name,
                    })
                }
                Some(crate::lights::DeviceState::Light(light)) => Ok(match light.on {
                    Some(true) => format!("{name} on"),
                    Some(false) => format!("{name} off"),
                    None => name,
                }),
                // Busy, or a reading that said nothing about this child: the
                // press was not lost, there is simply nothing to report.
                _ => Ok(name),
            };
        }
    }
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
                child: None,
            }),
        );
        // A receiver behind an installed integration package.
        config.connections.push(couch_model::Connection {
            id: "avr".into(),
            name: "Theater AVR".into(),
            provider: couch_model::Provider::Plugin {
                id: "denon".into(),
                label: "Denon AVR".into(),
                capabilities: vec![],
                supports_inputs: true,
                presentation: vec![],
                actions: vec![],
                children: vec![],
            },
        });
        config
            .room_mut(&Id::new("living-room"))
            .unwrap()
            .devices
            .push(
                couch_model::Device::new(
                    Id::new("living-avr"),
                    "Theater AVR",
                    couch_model::DeviceKind::Speaker,
                )
                .with_integration(Integration::Connection {
                    connection_id: "avr".into(),
                    resource_id: String::new(),
                    child: None,
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
        // A packaged device opens the core control screen. Every shortcut key
        // is taken above, so ask for the device directly, as the room list does.
        assert_eq!(
            open_device(&config, &Id::new("living-avr")),
            Dispatch::OpenTv("device:living-avr".into(), "Theater AVR".into())
        );
        assert!(!opens_player(
            &config
                .resolve_integration(&Integration::Connection {
                    connection_id: "avr".into(),
                    resource_id: String::new(),
                    child: None,
                })
                .unwrap()
        ));
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
    fn a_shortcut_to_a_receiver_waiting_for_its_package_says_what_it_needs() {
        let (mut config, _) = house();
        // The same receiver as saved by a release with the built-in client.
        config
            .connections
            .iter_mut()
            .find(|c| c.id.as_str() == "avr")
            .unwrap()
            .provider = couch_model::Provider::LegacyDenon {
            host: "avr.invalid".into(),
            port: 23,
        };
        config.validate().unwrap();
        assert_eq!(
            open_device(&config, &Id::new("living-avr")),
            Dispatch::Unavailable("Needs the Denon package".into())
        );
    }

    /// A packaged lamp: the shortcut key switches it with the child's own
    /// `toggle`, and a key aimed at its row opens the room list rather than a
    /// screen it does not have.
    #[test]
    fn a_shortcut_to_a_packaged_lamp_toggles_the_child_and_opens_its_room() {
        let (mut config, _) = house();
        let bridge = couch_model::Provider::Plugin {
            id: "hue".into(),
            label: "Philips Hue".into(),
            capabilities: vec![],
            supports_inputs: false,
            presentation: vec![],
            actions: vec![],
            children: vec![couch_model::PluginChildKind {
                kind: "light".into(),
                label: "Light".into(),
                device_kind: couch_model::DeviceKind::Light,
                component: couch_model::ChildComponent::Light,
                capabilities: vec![couch_model::PluginCapability {
                    id: "toggle".into(),
                    label: "Toggle".into(),
                }],
                actions: vec![couch_model::PluginActionSchema::SetLight {}],
            }],
        };
        config.connections.push(couch_model::Connection {
            id: "bridge".into(),
            name: "Hue bridge".into(),
            provider: bridge,
        });
        config
            .room_mut(&Id::new("living-room"))
            .unwrap()
            .devices
            .push(
                couch_model::Device::new(
                    Id::new("desk"),
                    "Desk lamp",
                    couch_model::DeviceKind::Light,
                )
                .with_integration(Integration::Connection {
                    connection_id: "bridge".into(),
                    resource_id: "lamp/1".into(),
                    child: Some(couch_model::ChildSnapshot {
                        kind: "light".into(),
                        light: Some(couch_model::LightTraits {
                            dimmable: true,
                            mirek: None,
                            color: false,
                        }),
                        cover: None,
                        climate: None,
                    }),
                }),
            );
        config.validate().unwrap();
        let device = config
            .devices()
            .find(|(_, d)| d.id.as_str() == "desk")
            .map(|(_, d)| d)
            .unwrap();
        // Its kind declares `toggle`, so a key may switch it.
        assert!(config.can_toggle(device));
        // And a key pointed at the device opens its row, not a screen.
        let row = crate::lights::row_of_device(&config, &Id::new("living-room"), &Id::new("desk"))
            .unwrap();
        assert_eq!(
            open_device(&config, &Id::new("desk")),
            Dispatch::OpenRoom(Id::new("living-room"), row)
        );
        let hue = Arc::new(connections::HueFleet::default());
        let matter = connections::MatterFleet::default();
        let mut sent = Vec::new();
        let said = toggle_with(
            &config,
            &Id::new("desk"),
            &hue,
            &matter,
            &mut |connection, request, _| {
                sent.push((connection.to_owned(), request));
                Ok(couch_plugin::Response::Status {
                    status: serde_json::from_value(
                        serde_json::json!({"light":{"on":true,"brightness":40}}),
                    )
                    .unwrap(),
                })
            },
        )
        .unwrap();
        assert_eq!(said, "Desk lamp on");
        assert_eq!(
            sent,
            [(
                "bridge".to_string(),
                couch_plugin::Request::Command {
                    function: "toggle".into(),
                    phase: couch_model::KeyPhase::Tap,
                    resource: Some("lamp/1".into())
                }
            )]
        );
        // A busy connection is not a failure and says only the lamp's name.
        let quiet = toggle_with(&config, &Id::new("desk"), &hue, &matter, &mut |_, _, _| {
            Err(couch_plugin::Error::Busy.into())
        })
        .unwrap();
        assert_eq!(quiet, "Desk lamp");
        // A kind that does not declare `toggle` is not switched from a key,
        // and nothing is sent for it.
        let mut plain = config.clone();
        let couch_model::Provider::Plugin { children, .. } = &mut plain
            .connections
            .iter_mut()
            .find(|c| c.id.as_str() == "bridge")
            .unwrap()
            .provider
        else {
            panic!("a packaged connection")
        };
        children[0].capabilities.clear();
        let device = plain
            .devices()
            .find(|(_, d)| d.id.as_str() == "desk")
            .map(|(_, d)| d)
            .unwrap();
        assert!(!plain.can_toggle(device));
        // Its row is still where its brightness lives.
        assert!(matches!(
            open_device(&plain, &Id::new("desk")),
            Dispatch::OpenRoom(..)
        ));
        assert_eq!(
            toggle_with(&plain, &Id::new("desk"), &hue, &matter, &mut |_, _, _| {
                panic!("nothing may be sent")
            })
            .err()
            .as_deref(),
            Some("This device cannot be switched from a key")
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
