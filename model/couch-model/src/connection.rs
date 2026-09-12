//! Named connections are shared by devices; credentials stay on the remote.
use crate::{Config, Id, Integration};
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: Id,
    pub name: String,
    pub provider: Provider,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Provider {
    CoreElec { host: String, port: u16 },
    Sonos { host: String },
    Kodi { host: String, port: u16 },
    Denon { host: String, port: u16 },
    HomeAssistant,
    Hue,
    WebOs,
    AndroidTv,
    AppleTv,
    UnifiProtect,
    Ir,
}
impl Provider {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Kodi { .. } => "kodi",
            Self::CoreElec { .. } => "core-elec",
            Self::Sonos { .. } => "sonos",
            Self::Denon { .. } => "denon",
            Self::HomeAssistant => "home-assistant",
            Self::Hue => "hue",
            Self::WebOs => "web-os",
            Self::AndroidTv => "android-tv",
            Self::AppleTv => "apple-tv",
            Self::UnifiProtect => "unifi-protect",
            Self::Ir => "ir",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Kodi { .. } => "Kodi",
            Self::CoreElec { .. } => "CoreELEC",
            Self::Sonos { .. } => "Sonos",
            Self::Denon { .. } => "Denon AVR",
            Self::HomeAssistant => "Home Assistant",
            Self::Hue => "Philips Hue",
            Self::WebOs => "LG webOS",
            Self::AndroidTv => "Android / Google TV",
            Self::AppleTv => "Apple TV",
            Self::UnifiProtect => "UniFi Protect",
            Self::Ir => "Infrared",
        }
    }
}
impl Config {
    pub fn connection(&self, id: &Id) -> Option<&Connection> {
        self.connections.iter().find(|c| &c.id == id)
    }
    /// Legacy inline integrations remain readable. New devices refer to a
    /// connection, so changing a Kodi address updates every referring device.
    /// Resolved Hue/HA IDs are runtime cache keys (connection/resource); strip
    /// the connection prefix before calling the upstream API. Stored IDs stay raw.
    pub fn resolve_integration(&self, integration: &Integration) -> Option<Integration> {
        let Integration::Connection {
            connection_id,
            resource_id,
        } = integration
        else {
            let legacy = match integration {Integration::Hue{light_id}=>Some(("hue",light_id)),Integration::HomeAssistant{entity_id}=>Some(("home-assistant",entity_id)),_=>None};
            if let Some((kind,resource))=legacy {
                if let Some(c)=self.connections.iter().find(|c|c.provider.kind()==kind) {
                    return self.resolve_integration(&Integration::Connection{connection_id:c.id.clone(),resource_id:resource.clone()});
                }
            }
            return Some(integration.clone());
        };
        Some(match &self.connection(connection_id)?.provider {
            Provider::Sonos { host } => Integration::Sonos { host: host.clone() },
            Provider::Kodi { host, port } | Provider::CoreElec { host, port } => Integration::Kodi {
                host: host.clone(),
                port: *port,
            },
            Provider::Denon {host,port} => Integration::Denon {host:host.clone(),port:*port},
            Provider::HomeAssistant => Integration::HomeAssistant {
                entity_id: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::Hue => Integration::Hue {
                light_id: alloc::format!("{connection_id}/{resource_id}"),
            },
            Provider::WebOs => Integration::WebOs,
            Provider::AndroidTv => Integration::AndroidTv,
            Provider::AppleTv => Integration::AppleTv,
            Provider::UnifiProtect => Integration::UnifiProtect { camera_id: alloc::format!("{connection_id}/{resource_id}") },
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
    fn hue_scene_room_assignments_validate_and_survive_round_trip() {
        let mut c = Config::default();
        c.connections.push(Connection {
            id: "hue".into(),
            name: "Hue".into(),
            provider: Provider::Hue,
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
        config.connections.push(Connection {id:"lg".into(),name:"LG TV".into(),provider:Provider::WebOs});
        let integration = Integration::Connection {connection_id:"lg".into(),resource_id:String::new()};
        config.rooms.push(Room {id:"office".into(),name:"Office".into(),icon:None,devices:vec![Device::new("tv".into(),"LG TV",DeviceKind::Tv).with_integration(integration.clone())]});
        assert!(config.validate().is_ok());
        assert_eq!(config.resolve_integration(&integration),Some(Integration::WebOs));
        let saved=serde_json::to_string(&config).unwrap();
        assert!(saved.contains("web-os"));
        assert_eq!(serde_json::from_str::<Config>(&saved).unwrap(),config);
        config.connections.push(Connection {id:"second".into(),name:"Second TV".into(),provider:Provider::WebOs});
        assert!(config.validate().is_ok());
    }
    #[test]
    fn identical_resources_on_different_servers_remain_distinct() {
        let mut c=Config::default();
        for (id,provider) in [("ha-a",Provider::HomeAssistant),("ha-b",Provider::HomeAssistant),("hue-a",Provider::Hue),("hue-b",Provider::Hue)] {
            c.connections.push(Connection{id:id.into(),name:id.into(),provider});
        }
        assert!(c.validate().is_ok());
        for (a,b,resource) in [("ha-a","ha-b","light.same"),("hue-a","hue-b","00000000-0000-0000-0000-000000000001")] {
            let left=c.resolve_integration(&Integration::Connection{connection_id:a.into(),resource_id:resource.into()});
            let right=c.resolve_integration(&Integration::Connection{connection_id:b.into(),resource_id:resource.into()});
            assert_ne!(left,right);
        }
        c.connections[0].id="../escape".into();assert!(c.validate().is_err());
    }
    #[test]
    fn built_in_ir_is_shared_but_each_device_keeps_its_own_codeset() {
        let mut c=Config::default();
        c.connections.push(Connection{id:"ir".into(),name:"Built-in IR".into(),provider:Provider::Ir});
        c.rooms.push(Room{id:"room".into(),name:"Room".into(),icon:None,devices:vec![
            Device::new("tv".into(),"TV",DeviceKind::Tv).with_integration(Integration::Connection{connection_id:"ir".into(),resource_id:"lg-tv".into()}),
            Device::new("amp".into(),"Amplifier",DeviceKind::Speaker).with_integration(Integration::Connection{connection_id:"ir".into(),resource_id:"denon".into()})]});
        assert!(c.validate().is_ok());
        c.connections.push(Connection{id:"another-ir".into(),name:"Duplicate blaster".into(),provider:Provider::Ir});assert!(c.validate().is_err());
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
            })
            .unwrap();
        assert!(matches!(ce, Integration::Kodi { port: 9090, .. }));
        assert!(crate::commands::Function::Ok.supports(&ce));
        let sonos = c
            .resolve_integration(&Integration::Connection {
                connection_id: "speaker".into(),
                resource_id: String::new(),
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
        ] {
            config.connections.push(Connection {
                id: name.into(),
                name: name.into(),
                provider,
            });
            let saved = Integration::Connection {
                connection_id: name.into(),
                resource_id: String::new(),
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
    }
}

#[cfg(test)]
mod protect_tests {
    use super::*;
    use crate::{Device,DeviceKind,Room};
    use alloc::vec;
    #[test]
    fn protect_camera_resources_validate_and_resolve_without_credentials(){
        let mut config=Config::default();
        config.connections.push(Connection{id:"protect".into(),name:"Cameras".into(),provider:Provider::UnifiProtect});
        config.rooms.push(Room{id:"entry".into(),name:"Entry".into(),icon:None,devices:vec![Device::new("front".into(),"Front door",DeviceKind::Camera).with_integration(Integration::Connection{connection_id:"protect".into(),resource_id:"camera-123".into()})]});
        assert!(config.validate().is_ok());
        assert_eq!(config.resolve_integration(&config.rooms[0].devices[0].integration),Some(Integration::UnifiProtect{camera_id:"protect/camera-123".into()}));
        config.rooms[0].devices[0].kind=DeviceKind::Light;assert!(config.validate().is_err());
        config.rooms[0].devices[0].kind=DeviceKind::Camera;
        config.rooms[0].devices[0].integration=Integration::Connection{connection_id:"protect".into(),resource_id:"../other".into()};assert!(config.validate().is_err());
    }
}
