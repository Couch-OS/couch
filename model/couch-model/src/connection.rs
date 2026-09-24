//! Named connections are shared by devices; credentials stay on the remote.
use crate::{Config, Id, Integration};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: Id,
    pub name: String,
    pub provider: Provider,
}
/// One command an external integration deliberately exposes to button maps.
///
/// This small public snapshot is stored with the connection. It lets the
/// remote keep rendering and validating an existing setup when the package is
/// temporarily missing, without putting executable code or private settings
/// in the home configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCapability {
    pub id: String,
    pub label: String,
}

/// Native Couch controls an external integration may compose. These are data,
/// not package-supplied UI code; both browser and panel render them with their
/// own built-in components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginComponent {
    CommandGroup {
        title: String,
        commands: Vec<String>,
    },
    StatusText {
        label: String,
        field: PluginStatusField,
    },
    Toggle {
        label: String,
        state: PluginStatusField,
        on: String,
        off: String,
    },
    VolumeDbControl {
        label: String,
    },
    InputSelector {
        label: String,
    },
    /// Protocol 3 (unreleased): the connection is itself one lamp, blind or
    /// thermostat, drawn with the built-in control and driven by the matching
    /// typed action. A connection with many of them declares `children`
    /// instead.
    Light {
        label: String,
    },
    Cover {
        label: String,
    },
    Climate {
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatusField {
    On,
    Playing,
    Muted,
    Volume,
    VolumeDb,
    Input,
    Title,
}

impl PluginStatusField {
    pub fn is_boolean(self) -> bool {
        matches!(self, Self::On | Self::Playing | Self::Muted)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Provider {
    CoreElec {
        host: String,
        port: u16,
    },
    Sonos {
        host: String,
    },
    Kodi {
        host: String,
        port: u16,
    },
    /// A Denon connection saved while the receiver client was built into the
    /// OS. The client now ships as the `denon` package, so nothing drives this
    /// variant: it exists to read an older file, keep its rooms and activities
    /// valid, and hand the address to the package
    /// ([`crate::LEGACY_BUILTINS`]). New connections never take this shape.
    #[serde(rename = "denon")]
    LegacyDenon {
        host: String,
        port: u16,
    },
    HomeAssistant,
    /// A Hue connection saved while the bridge client was built into the OS.
    /// The `hue` package takes it over through [`crate::LEGACY_BUILTINS`].
    #[serde(rename = "hue")]
    LegacyHue,
    /// An LG TV connection saved while the webOS client was built into the
    /// OS. The `webos` package takes it over through [`crate::LEGACY_BUILTINS`].
    #[serde(rename = "web-os")]
    LegacyWebOs,
    AndroidTv,
    AppleTv,
    Tizen,
    /// The remote as a Bluetooth HID peripheral; TVs pair to it.
    BluetoothTv,
    UnifiProtect,
    /// A Matter fabric this remote administers; devices are commissioned
    /// onto it with a pairing code and identified by node ID and endpoint.
    Matter,
    /// A separately installed integration package. Metadata is copied from its
    /// validated manifest whenever the connection is created or updated.
    Plugin {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<PluginCapability>,
        #[serde(default, skip_serializing_if = "core::ops::Not::not")]
        supports_inputs: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presentation: Vec<PluginComponent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<crate::PluginActionSchema>,
        /// Protocol 3 (unreleased): the kinds of child this connection offers
        /// (a bridge's lights, rooms and scenes). Empty for a package whose
        /// connection is the device, and then never written.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        children: Vec<crate::PluginChildKind>,
    },
    Ir,
}
impl Provider {
    pub fn kind(&self) -> &str {
        match self {
            Self::Kodi { .. } => "kodi",
            Self::CoreElec { .. } => "core-elec",
            Self::Sonos { .. } => "sonos",
            Self::LegacyDenon { .. } => "denon",
            Self::HomeAssistant => "home-assistant",
            Self::LegacyHue => "hue",
            Self::LegacyWebOs => "web-os",
            Self::AndroidTv => "android-tv",
            Self::AppleTv => "apple-tv",
            Self::Tizen => "tizen",
            Self::BluetoothTv => "bluetooth-tv",
            Self::UnifiProtect => "unifi-protect",
            Self::Matter => "matter",
            Self::Plugin { .. } => "plugin",
            Self::Ir => "ir",
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Kodi { .. } => "Kodi",
            Self::CoreElec { .. } => "CoreELEC",
            Self::Sonos { .. } => "Sonos",
            Self::LegacyDenon { .. } => "Denon AVR",
            Self::HomeAssistant => "Home Assistant",
            Self::LegacyHue => "Philips Hue",
            Self::LegacyWebOs => "LG webOS",
            Self::AndroidTv => "Android / Google TV",
            Self::AppleTv => "Apple TV",
            Self::Tizen => "Samsung Tizen",
            Self::BluetoothTv => "Bluetooth TV",
            Self::UnifiProtect => "UniFi Protect",
            Self::Matter => "Matter",
            Self::Plugin { label, .. } => label,
            Self::Ir => "Infrared",
        }
    }
}
impl Config {
    pub fn connection(&self, id: &Id) -> Option<&Connection> {
        self.connections.iter().find(|c| &c.id == id)
    }
    /// The kind a packaged connection declares under this name.
    pub fn child_kind(&self, connection: &Id, kind: &str) -> Option<&crate::PluginChildKind> {
        match &self.connection(connection)?.provider {
            Provider::Plugin { children, .. } => children.iter().find(|k| k.kind == kind),
            _ => None,
        }
    }
    /// The declared kind of a device that is a child, in either saved form.
    pub fn device_child_kind(&self, integration: &Integration) -> Option<&crate::PluginChildKind> {
        match integration {
            Integration::Connection {
                connection_id,
                child: Some(child),
                ..
            }
            | Integration::Plugin {
                connection_id,
                child: Some(child),
                ..
            } => self.child_kind(connection_id, &child.kind),
            _ => None,
        }
    }
    /// Legacy inline integrations remain readable. New devices refer to a
    /// connection, so changing a Kodi address updates every referring device.
    /// Resolved Hue/HA IDs are runtime cache keys (connection/resource); strip
    /// the connection prefix before calling the upstream API. Stored IDs stay raw.
    ///
    /// A device that is a child of a packaged connection resolves to what its
    /// *kind* can do, never to what the connection can: the kind's commands
    /// and typed actions, no inputs, no screen of its own, and the saved
    /// snapshot. A kind the package no longer declares resolves to nothing it
    /// can be told.
    pub fn resolve_integration(&self, integration: &Integration) -> Option<Integration> {
        let Integration::Connection {
            connection_id,
            resource_id,
            child,
        } = integration
        else {
            let legacy = match integration {
                Integration::Hue { light_id } => Some(("hue", light_id)),
                Integration::HomeAssistant { entity_id } => Some(("home-assistant", entity_id)),
                _ => None,
            };
            if let Some((kind, resource)) = legacy {
                if let Some(c) = self.connections.iter().find(|c| c.provider.kind() == kind) {
                    return self.resolve_integration(&Integration::Connection {
                        connection_id: c.id.clone(),
                        resource_id: resource.clone(),
                        child: None,
                    });
                }
            }
            return Some(integration.clone());
        };
        Some(match &self.connection(connection_id)?.provider {
            Provider::Sonos { host } => Integration::Sonos { host: host.clone() },
            Provider::Kodi { host, port } | Provider::CoreElec { host, port } => {
                Integration::Kodi {
                    host: host.clone(),
                    port: *port,
                }
            }
            Provider::LegacyDenon { host, port } => Integration::LegacyDenon {
                host: host.clone(),
                port: *port,
            },
            Provider::HomeAssistant => Integration::HomeAssistant {
                entity_id: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::LegacyHue => Integration::Hue {
                light_id: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::LegacyWebOs => Integration::WebOs,
            Provider::AndroidTv => Integration::AndroidTv,
            Provider::AppleTv => Integration::AppleTv,
            Provider::Tizen => Integration::Tizen,
            Provider::BluetoothTv => Integration::BluetoothTv,
            Provider::UnifiProtect => Integration::UnifiProtect {
                camera_id: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::Matter => Integration::Matter {
                device: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::Plugin { id, children, .. } if child.is_some() => {
                let kind = child
                    .as_ref()
                    .and_then(|child| children.iter().find(|kind| kind.kind == child.kind));
                Integration::Plugin {
                    id: id.clone(),
                    connection_id: connection_id.clone(),
                    resource_id: resource_id.clone(),
                    capabilities: kind.map(|k| k.capabilities.clone()).unwrap_or_default(),
                    supports_inputs: false,
                    presentation: Vec::new(),
                    actions: kind.map(|k| k.actions.clone()).unwrap_or_default(),
                    child: child.clone(),
                }
            }
            Provider::Plugin {
                id,
                capabilities,
                supports_inputs,
                presentation,
                actions,
                ..
            } => Integration::Plugin {
                id: id.clone(),
                connection_id: connection_id.clone(),
                resource_id: resource_id.clone(),
                capabilities: capabilities.clone(),
                supports_inputs: *supports_inputs,
                presentation: presentation.clone(),
                actions: actions.clone(),
                child: None,
            },
            Provider::Ir => Integration::Ir {
                codeset: resource_id.clone(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Device, DeviceKind, Room};
    use alloc::vec;
    #[test]
    fn external_provider_round_trips_and_resolves_without_an_installed_package() {
        let provider = Provider::Plugin {
            id: "sample-avr".into(),
            label: "Sample AVR".into(),
            capabilities: vec![PluginCapability {
                id: "volume-up".into(),
                label: "Volume up".into(),
            }],
            supports_inputs: true,
            actions: vec![],
            presentation: vec![PluginComponent::CommandGroup {
                title: "Volume".into(),
                commands: vec!["volume-up".into()],
            }],
            children: vec![],
        };
        let mut config = Config::default();
        config.connections.push(Connection {
            id: "receiver".into(),
            name: "Receiver".into(),
            provider: provider.clone(),
        });
        let stored = Integration::Connection {
            connection_id: "receiver".into(),
            resource_id: "zone-main".into(),
            child: None,
        };
        let saved = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.connections[0].provider, provider);
        assert_eq!(
            restored.resolve_integration(&stored),
            Some(Integration::Plugin {
                id: "sample-avr".into(),
                connection_id: "receiver".into(),
                resource_id: "zone-main".into(),
                capabilities: vec![PluginCapability {
                    id: "volume-up".into(),
                    label: "Volume up".into(),
                }],
                supports_inputs: true,
                actions: vec![],
                presentation: vec![PluginComponent::CommandGroup {
                    title: "Volume".into(),
                    commands: vec!["volume-up".into()],
                }],
                child: None,
            })
        );
        assert!(restored.validate().is_ok());
    }
    #[test]
    fn external_presentations_reject_unsafe_component_combinations() {
        let connection = |presentation| Connection {
            id: "receiver".into(),
            name: "Receiver".into(),
            provider: Provider::Plugin {
                id: "sample-avr".into(),
                label: "Sample AVR".into(),
                capabilities: vec![
                    PluginCapability {
                        id: "power-on".into(),
                        label: "Power on".into(),
                    },
                    PluginCapability {
                        id: "power-off".into(),
                        label: "Power off".into(),
                    },
                ],
                supports_inputs: false,
                presentation,
                actions: vec![],
                children: vec![],
            },
        };
        for presentation in [
            vec![PluginComponent::CommandGroup {
                title: "Power".into(),
                commands: vec!["power-on".into(), "power-on".into()],
            }],
            vec![PluginComponent::Toggle {
                label: "Volume".into(),
                state: PluginStatusField::Volume,
                on: "power-on".into(),
                off: "power-off".into(),
            }],
            vec![PluginComponent::InputSelector {
                label: "Source".into(),
            }],
        ] {
            let mut config = Config::default();
            config.connections.push(connection(presentation));
            assert!(config.validate().is_err());
        }
    }
    #[test]
    fn hue_scene_room_assignments_validate_and_survive_round_trip() {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "hue".into(),
            name: "Hue".into(),
            provider: Provider::LegacyHue,
        });
        c.rooms.push(Room {
            id: "office".into(),
            name: "Office".into(),
            icon: None,
            devices: vec![],
        });
        c.scenes.push(crate::Scene {
            id: "relax".into(),
            name: "Relax".into(),
            icon: None,
            steps: vec![],
            rooms: vec!["office".into()],
            hue: Some(crate::HueScene {
                connection_id: "hue".into(),
                scene_id: "00000000-0000-0000-0000-000000000001".into(),
            }),
            resource: None,
        });
        assert!(c.validate().is_ok());
        let saved = serde_json::to_string(&c).unwrap();
        let restored: Config = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, c);
        c.connections.clear();
        assert!(c.validate().is_err());
        let mut c = restored;
        c.remove_room(&"office".into());
        assert!(c.scenes[0].rooms.is_empty());
        assert!(c.validate().is_ok());
    }
    #[test]
    fn shared_connection_updates_devices_and_cannot_be_deleted_in_use() {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "player".into(),
            name: "Player".into(),
            provider: Provider::Kodi {
                host: "old.local".into(),
                port: 9090,
            },
        });
        let integration = Integration::Connection {
            connection_id: "player".into(),
            resource_id: String::new(),
            child: None,
        };
        let mut room = Room {
            id: "room".into(),
            name: "Room".into(),
            icon: None,
            devices: vec![],
        };
        room.devices = vec![Device::new("tv".into(), "Player", DeviceKind::MediaPlayer)
            .with_integration(integration.clone())];
        c.rooms.push(room);
        assert!(c.validate().is_ok());
        c.connections[0].provider = Provider::Kodi {
            host: "new.local".into(),
            port: 9090,
        };
        assert_eq!(
            c.resolve_integration(&integration),
            Some(Integration::Kodi {
                host: "new.local".into(),
                port: 9090
            })
        );
        c.connections.clear();
        assert!(c.validate().is_err());
    }
    #[test]
    fn webos_connection_round_trips_and_resolves_room_tv() {
        let mut config = Config::default();
        config.connections.push(Connection {
            id: "lg".into(),
            name: "LG TV".into(),
            provider: Provider::LegacyWebOs,
        });
        let integration = Integration::Connection {
            connection_id: "lg".into(),
            resource_id: String::new(),
            child: None,
        };
        config.rooms.push(Room {
            id: "office".into(),
            name: "Office".into(),
            icon: None,
            devices: vec![Device::new("tv".into(), "LG TV", DeviceKind::Tv)
                .with_integration(integration.clone())],
        });
        assert!(config.validate().is_ok());
        assert_eq!(
            config.resolve_integration(&integration),
            Some(Integration::WebOs)
        );
        let saved = serde_json::to_string(&config).unwrap();
        assert!(saved.contains("web-os"));
        assert_eq!(serde_json::from_str::<Config>(&saved).unwrap(), config);
        config.connections.push(Connection {
            id: "second".into(),
            name: "Second TV".into(),
            provider: Provider::LegacyWebOs,
        });
        assert!(config.validate().is_ok());
    }
    #[test]
    fn identical_resources_on_different_servers_remain_distinct() {
        let mut c = Config::default();
        for (id, provider) in [
            ("ha-a", Provider::HomeAssistant),
            ("ha-b", Provider::HomeAssistant),
            ("hue-a", Provider::LegacyHue),
            ("hue-b", Provider::LegacyHue),
        ] {
            c.connections.push(Connection {
                id: id.into(),
                name: id.into(),
                provider,
            });
        }
        assert!(c.validate().is_ok());
        for (a, b, resource) in [
            ("ha-a", "ha-b", "light.same"),
            ("hue-a", "hue-b", "00000000-0000-0000-0000-000000000001"),
        ] {
            let left = c.resolve_integration(&Integration::Connection {
                connection_id: a.into(),
                resource_id: resource.into(),
                child: None,
            });
            let right = c.resolve_integration(&Integration::Connection {
                connection_id: b.into(),
                resource_id: resource.into(),
                child: None,
            });
            assert_ne!(left, right);
        }
        c.connections[0].id = "../escape".into();
        assert!(c.validate().is_err());
    }
    #[test]
    fn built_in_ir_is_shared_but_each_device_keeps_its_own_codeset() {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "ir".into(),
            name: "Built-in IR".into(),
            provider: Provider::Ir,
        });
        c.rooms.push(Room {
            id: "room".into(),
            name: "Room".into(),
            icon: None,
            devices: vec![
                Device::new("tv".into(), "TV", DeviceKind::Tv).with_integration(
                    Integration::Connection {
                        connection_id: "ir".into(),
                        resource_id: "lg-tv".into(),
                        child: None,
                    },
                ),
                Device::new("amp".into(), "Amplifier", DeviceKind::Speaker).with_integration(
                    Integration::Connection {
                        connection_id: "ir".into(),
                        resource_id: "denon".into(),
                        child: None,
                    },
                ),
            ],
        });
        assert!(c.validate().is_ok());
        c.connections.push(Connection {
            id: "another-ir".into(),
            name: "Duplicate blaster".into(),
            provider: Provider::Ir,
        });
        assert!(c.validate().is_err());
    }
    #[test]
    fn coreelec_reuses_kodi_and_sonos_capabilities_are_bounded() {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "ce".into(),
            name: "CoreELEC".into(),
            provider: Provider::CoreElec {
                host: "192.0.2.1".into(),
                port: 9090,
            },
        });
        c.connections.push(Connection {
            id: "speaker".into(),
            name: "Sonos".into(),
            provider: Provider::Sonos {
                host: "192.0.2.2".into(),
            },
        });
        let ce = c
            .resolve_integration(&Integration::Connection {
                connection_id: "ce".into(),
                resource_id: String::new(),
                child: None,
            })
            .unwrap();
        assert!(matches!(ce, Integration::Kodi { port: 9090, .. }));
        assert!(crate::commands::Function::Ok.supports(&ce));
        let sonos = c
            .resolve_integration(&Integration::Connection {
                connection_id: "speaker".into(),
                resource_id: String::new(),
                child: None,
            })
            .unwrap();
        assert!(crate::commands::Function::Play.supports(&sonos));
        assert!(crate::commands::Function::VolumeUp.supports(&sonos));
        assert!(!crate::commands::Function::PowerOff.supports(&sonos));
        assert!(!crate::commands::Function::Ok.supports(&sonos));
        assert!(c.validate().is_ok());
        let bytes = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&bytes).unwrap(), c);
        c.connections[1].provider = Provider::Sonos {
            host: "speaker.local".into(),
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn old_config_remains_readable_without_connections() {
        let c: Config = serde_json::from_str(r#"{"schema_version":1,"rooms":[]}"#).unwrap();
        assert!(c.connections.is_empty());
    }
    #[test]
    fn streaming_tv_connections_resolve_and_do_not_invent_capabilities() {
        let mut config = Config::default();
        for (name, provider, integration) in [
            ("android", Provider::AndroidTv, Integration::AndroidTv),
            ("apple", Provider::AppleTv, Integration::AppleTv),
            ("samsung", Provider::Tizen, Integration::Tizen),
        ] {
            config.connections.push(Connection {
                id: name.into(),
                name: name.into(),
                provider,
            });
            let saved = Integration::Connection {
                connection_id: name.into(),
                resource_id: String::new(),
                child: None,
            };
            assert_eq!(
                config.resolve_integration(&saved),
                Some(integration.clone())
            );
            assert!(crate::commands::Function::Ok.supports(&integration));
        }
        assert!(config.validate().is_ok());
        assert_eq!(
            serde_json::from_str::<Config>(&serde_json::to_string(&config).unwrap()).unwrap(),
            config
        );
        assert!(!crate::commands::Function::Mute.supports(&Integration::AppleTv));
        assert!(crate::commands::Function::Mute.supports(&Integration::AndroidTv));
        // Samsung offers fixed source keys and app IDs, but no next/previous.
        let tizen = Integration::Tizen;
        assert!(crate::commands::Function::parse("input:hdmi2")
            .unwrap()
            .supports(&tizen));
        assert!(!crate::commands::Function::parse("input:HDMI_2")
            .unwrap()
            .supports(&tizen));
        assert!(crate::commands::Function::parse("app:111299001912")
            .unwrap()
            .supports(&tizen));
        assert!(crate::commands::Function::PowerOn.supports(&tizen));
        assert!(!crate::commands::Function::Next.supports(&tizen));
        assert!(!crate::commands::Function::PlayPause.supports(&tizen));
        assert!(serde_json::to_string(&config)
            .unwrap()
            .contains("\"tizen\""));
    }
}

#[cfg(test)]
mod protect_tests {
    use super::*;
    use crate::{Device, DeviceKind, Room};
    use alloc::vec;
    #[test]
    fn protect_camera_resources_validate_and_resolve_without_credentials() {
        let mut config = Config::default();
        config.connections.push(Connection {
            id: "protect".into(),
            name: "Cameras".into(),
            provider: Provider::UnifiProtect,
        });
        config.rooms.push(Room {
            id: "entry".into(),
            name: "Entry".into(),
            icon: None,
            devices: vec![
                Device::new("front".into(), "Front door", DeviceKind::Camera).with_integration(
                    Integration::Connection {
                        connection_id: "protect".into(),
                        resource_id: "camera-123".into(),
                        child: None,
                    },
                ),
            ],
        });
        assert!(config.validate().is_ok());
        assert_eq!(
            config.resolve_integration(&config.rooms[0].devices[0].integration),
            Some(Integration::UnifiProtect {
                camera_id: "protect/camera-123".into()
            })
        );
        config.rooms[0].devices[0].kind = DeviceKind::Light;
        assert!(config.validate().is_err());
        config.rooms[0].devices[0].kind = DeviceKind::Camera;
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: "protect".into(),
            resource_id: "../other".into(),
            child: None,
        };
        assert!(config.validate().is_err());
    }
}

#[cfg(test)]
mod matter_tests {
    use super::*;
    use crate::{Device, DeviceKind, Room};
    use alloc::vec;
    #[test]
    fn matter_devices_resolve_to_node_and_endpoint_and_reject_bad_resources() {
        let mut config = Config::default();
        config.connections.push(Connection {
            id: "matter".into(),
            name: "Matter".into(),
            provider: Provider::Matter,
        });
        config.rooms.push(Room {
            id: "den".into(),
            name: "Den".into(),
            icon: None,
            devices: vec![
                Device::new("lamp".into(), "Lamp", DeviceKind::Light).with_integration(
                    Integration::Connection {
                        connection_id: "matter".into(),
                        resource_id: "7/1".into(),
                        child: None,
                    },
                ),
            ],
        });
        assert!(config.validate().is_ok());
        let resolved = config.resolve_integration(&config.rooms[0].devices[0].integration);
        assert_eq!(
            resolved,
            Some(Integration::Matter {
                device: "matter/7/1".into()
            })
        );
        assert_eq!(resolved.as_ref().map(Integration::via), Some("matter"));
        assert!(crate::commands::Function::Toggle.supports(resolved.as_ref().unwrap()));
        assert!(!crate::commands::Function::VolumeUp.supports(resolved.as_ref().unwrap()));
        assert_eq!(Provider::Matter.kind(), "matter");
        assert_eq!(
            serde_json::to_value(&Provider::Matter).unwrap(),
            serde_json::json!({"kind":"matter"})
        );
        for bad in [
            "",
            "7",
            "7/0",
            "0/1",
            "7/1/2",
            "a/1",
            "7/65536",
            "123456789012345678901/1",
        ] {
            config.rooms[0].devices[0].integration = Integration::Connection {
                connection_id: "matter".into(),
                resource_id: bad.into(),
                child: None,
            };
            assert!(config.validate().is_err(), "{bad:?} should be rejected");
        }
    }
}
