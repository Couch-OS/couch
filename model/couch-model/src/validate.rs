//! What has to be true of a config before anything is allowed to store it.
//!
//! The daemon runs this on every write and refuses the whole document if it
//! fails, which is the only reason readers elsewhere in this crate can be
//! relaxed about dangling ids. Every check here exists because breaking it
//! would produce a house that renders wrong rather than an error: two rooms
//! with the same id means one of them is unreachable, and an area pointing at a
//! room that was deleted means a gap in the list with no explanation.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::{Config, Id, SCHEMA_VERSION};

/// One thing wrong, named well enough to put in front of a user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// A dotted path into the document: `areas[1].rooms[0]`.
    pub at: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationError {
    pub problems: Vec<Problem>,
}

impl core::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, p) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}: {}", p.at, p.message)?;
        }
        Ok(())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ValidationError {}

impl Config {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut problems = Vec::new();
        self.validate_app_shortcuts(&mut problems);
        if self.appearance.rgb().is_none() { problems.push(Problem{at:"appearance.accent".into(),message:"Use a color in #RRGGBB format".into()}); }

        if self.schema_version > SCHEMA_VERSION {
            problems.push(Problem {
                at: "schema_version".to_string(),
                message: alloc::format!(
                    "written by a newer build (schema {}, this understands {})",
                    self.schema_version,
                    SCHEMA_VERSION
                ),
            });
        }

        // Ids and names, per collection. A blank name is worth rejecting: the
        // hub renders it as an empty row that cannot be told apart from a
        // rendering bug, and there is no way to select it to fix it.
        let mut seen: Vec<&Id> = Vec::new();
        for (i, a) in self.areas.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "areas", i, &a.id, &a.name);
        }
        seen.clear();
        for (i, r) in self.rooms.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "rooms", i, &r.id, &r.name);
        }
        seen.clear();
        for (i, s) in self.scenes.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "scenes", i, &s.id, &s.name);
        }
        seen.clear();
        for (i, a) in self.activities.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "activities", i, &a.id, &a.name);
        }

        // Device ids are unique across the home, not per room: an Action names
        // a device and nothing else, so a duplicate id makes a scene ambiguous.
        let mut device_ids: Vec<&Id> = Vec::new();
        for (ri, room) in self.rooms.iter().enumerate() {
            for (di, d) in room.devices.iter().enumerate() {
                let at = alloc::format!("rooms[{ri}].devices[{di}]");
                if d.ir.as_ref().is_some_and(|ir| !crate::DeviceIr::valid_codeset(&ir.codeset)) {
                    problems.push(Problem { at: alloc::format!("{at}.ir"), message: "Choose a lowercase IR codeset ID with letters, numbers, hyphens or underscores (maximum 64 characters)".into() });
                }
                if d.id.is_empty() {
                    problems.push(Problem { at: at.clone(), message: "blank id".to_string() });
                }
                if d.name.trim().is_empty() {
                    problems.push(Problem { at: at.clone(), message: "blank name".to_string() });
                }
                if device_ids.contains(&&d.id) {
                    problems.push(Problem {
                        at,
                        message: alloc::format!("duplicate device id \"{}\"", d.id),
                    });
                } else {
                    device_ids.push(&d.id);
                }
            }
        }

        let mut connection_ids = Vec::new();
        for (i,c) in self.connections.iter().enumerate() {
            check_entity(&mut problems, &mut connection_ids, "connections", i, &c.id, &c.name);
            if let crate::Provider::Kodi{host,port}|crate::Provider::CoreElec{host,port}|crate::Provider::Denon{host,port}=&c.provider {
                if host.trim().is_empty() || *port==0 { problems.push(Problem{at:alloc::format!("connections[{i}]"),message:"Connection needs an address and a TCP port from 1 to 65535".into()}); }
            }
            if let crate::Provider::Sonos { host } = &c.provider {
                if host.parse::<core::net::Ipv4Addr>().is_err() {
                    problems.push(Problem { at: alloc::format!("connections[{i}]"), message: "Sonos needs an IPv4 address".into() });
                }
            }
            if c.provider==crate::Provider::Ir && self.connections[..i].iter().any(|old|old.provider==crate::Provider::Ir) { problems.push(Problem{at:alloc::format!("connections[{i}]"),message:"Use the built-in IR connection and configure a separate codeset on each device".into()}); }
            if c.id.as_str().len()>128 || !c.id.as_str().bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-' || b==b'_') { problems.push(Problem{at:alloc::format!("connections[{i}]"),message:"Connection IDs must be safe alphanumeric identifiers".into()}); }
        }
        for (room,device) in self.devices() {
            if let crate::Integration::Sonos { host } = &device.integration {
                if host.parse::<core::net::Ipv4Addr>().is_err() {
                    problems.push(Problem {
                        at: alloc::format!("rooms.{}.devices.{}", room.id, device.id),
                        message: "Sonos needs an IPv4 address".into(),
                    });
                }
            }
            if let crate::Integration::Connection{connection_id,resource_id}=&device.integration {
                let at=alloc::format!("rooms.{}.devices.{}",room.id,device.id);
                match self.connection(connection_id) {
                    None=>problems.push(Problem{at,message:"This device refers to a missing connection; remove its devices before deleting the connection".into()}),
                    Some(c)=>{
                        let valid=match c.provider {
                            crate::Provider::Kodi{..}|crate::Provider::CoreElec{..}|crate::Provider::Sonos{..}|crate::Provider::Denon{..}|crate::Provider::WebOs|crate::Provider::AndroidTv|crate::Provider::AppleTv=>resource_id.is_empty(),
                            crate::Provider::UnifiProtect=>device.kind==crate::DeviceKind::Camera && !resource_id.is_empty() && resource_id.len()<=128 && resource_id.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-' || b==b'_'),
                            crate::Provider::HomeAssistant=>valid_ha_resource(resource_id, device.kind),
                            crate::Provider::Hue=>{ let id=resource_id.strip_prefix("room:").unwrap_or(resource_id); id.len()==36 && id.bytes().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i){b==b'-'}else{b.is_ascii_hexdigit()}) },
                            crate::Provider::Ir=>!resource_id.is_empty() && resource_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'),
                        };
                        if !valid {problems.push(Problem{at,message:"Choose a valid device from this connection".into()});}
                    }
                }
            }
        }

        for (i, scene) in self.scenes.iter().enumerate() {
            for room in &scene.rooms { if self.room(room).is_none() { problems.push(Problem{at:alloc::format!("scenes[{i}].rooms"),message:"Choose an existing room".into()}); } }
            if let Some(hue) = &scene.hue {
                let id=&hue.scene_id;
                let valid=self.connection(&hue.connection_id).is_some_and(|c|c.provider==crate::Provider::Hue)
                    && id.len()==36 && id.bytes().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i){b==b'-'}else{b.is_ascii_hexdigit()}) && scene.steps.is_empty();
                if !valid { problems.push(Problem{at:alloc::format!("scenes[{i}].hue"),message:"Choose a Hue connection and scene; bridge scenes cannot include device steps".into()}); }
            }
        }
        // References.
        for (i, area) in self.areas.iter().enumerate() {
            for (j, id) in area.rooms.iter().enumerate() {
                if self.room(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].rooms[{j}]"),
                        message: alloc::format!("no room \"{id}\""),
                    });
                }
                if area.rooms[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].rooms[{j}]"),
                        message: alloc::format!("room \"{id}\" listed twice"),
                    });
                }
            }
            for (j, id) in area.scenes.iter().enumerate() {
                if self.scene(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].scenes[{j}]"),
                        message: alloc::format!("no scene \"{id}\""),
                    });
                }
                if area.scenes[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].scenes[{j}]"),
                        message: alloc::format!("scene \"{id}\" listed twice"),
                    });
                }
            }
            for (j, id) in area.activities.iter().enumerate() {
                if self.activity(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].activities[{j}]"),
                        message: alloc::format!("no activity \"{id}\""),
                    });
                }
                if area.activities[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].activities[{j}]"),
                        message: alloc::format!("activity \"{id}\" listed twice"),
                    });
                }
            }
        }


        for (i, scene) in self.scenes.iter().enumerate() {
            for (j, step) in scene.steps.iter().enumerate() {
                if !device_ids.contains(&&step.device) {
                    problems.push(Problem {
                        at: alloc::format!("scenes[{i}].steps[{j}]"),
                        message: alloc::format!("no device \"{}\"", step.device),
                    });
                }
            }
        }

        for (i, act) in self.activities.iter().enumerate() {
            if let Err(message) = act.setup.validate(self) {
                problems.push(Problem { at: alloc::format!("activities[{i}].setup"), message: message.into() });
            }
            if self.room(&act.room).is_none() {
                problems.push(Problem {
                    at: alloc::format!("activities[{i}].room"),
                    message: alloc::format!("no room \"{}\"", act.room),
                });
            }
            if let Some(src) = &act.source {
                if !device_ids.contains(&src) {
                    problems.push(Problem {
                        at: alloc::format!("activities[{i}].source"),
                        message: alloc::format!("no device \"{src}\""),
                    });
                }
            }
            for (j, binding) in act.buttons.iter().enumerate() {
                let valid = !act.buttons[..j].iter().any(|b| b.button == binding.button && b.gesture == binding.gesture)
                    && (binding.gesture == crate::buttons::Gesture::Short || binding.button.supports_long())
                    && binding.action.as_ref().map_or(true, |action| self.devices().find(|(_, d)| d.id == action.device)
                        .is_some_and(|(_, d)| crate::commands::Function::parse(&action.command).is_some_and(|f|f.supports_device(d,self))));
                if !valid { problems.push(Problem {at:alloc::format!("activities[{i}].buttons[{j}]"),message:"Choose one mapping per button and a supported device function".into()}); }
            }
            for (j, step) in act.steps.iter().enumerate() {
                if !device_ids.contains(&&step.device) {
                    problems.push(Problem {
                        at: alloc::format!("activities[{i}].steps[{j}]"),
                        message: alloc::format!("no device \"{}\"", step.device),
                    });
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { problems })
        }
    }
}

fn check_entity<'a>(
    problems: &mut Vec<Problem>,
    seen: &mut Vec<&'a Id>,
    collection: &str,
    index: usize,
    id: &'a Id,
    name: &str,
) {
    let at = alloc::format!("{collection}[{index}]");
    if id.is_empty() {
        problems.push(Problem { at: at.clone(), message: "blank id".to_string() });
    }
    if name.trim().is_empty() {
        problems.push(Problem { at: at.clone(), message: "blank name".to_string() });
    }
    if seen.contains(&id) {
        problems.push(Problem {
            at,
            message: alloc::format!("duplicate id \"{id}\""),
        });
    } else {
        seen.push(id);
    }
}

fn valid_ha_resource(id: &str, kind: crate::DeviceKind) -> bool {
    let Some((domain, name)) = id.split_once('.') else { return false; };
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && matches!((domain, kind),
            ("light", crate::DeviceKind::Light)
            | ("cover", crate::DeviceKind::Blind)
            | ("climate", crate::DeviceKind::Thermostat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Area, Device, DeviceKind, Room};
    use alloc::vec;

    fn room(id: &str, name: &str) -> Room {
        Room { id: Id::new(id), name: name.to_string(), icon: None, devices: vec![] }
    }

    #[test]
    fn direct_sonos_integrations_require_literal_ipv4_addresses() {
        for (host, valid) in [("192.0.2.1", true), ("speaker.local", false), ("::1", false), ("", false), ("256.1.1.1", false)] {
            let mut room = room("living", "Living room");
            let mut device = Device::new(Id::new("speaker"), "Sonos", DeviceKind::Speaker);
            device.integration = crate::Integration::Sonos { host: host.into() };
            room.devices.push(device);
            let cfg = Config { rooms: vec![room], ..Config::default() };
            if valid {
                assert!(cfg.validate().is_ok(), "{host}");
            } else {
                let error = cfg.validate().unwrap_err();
                assert!(error.problems.iter().any(|p| p.at == "rooms.living.devices.speaker" && p.message == "Sonos needs an IPv4 address"));
            }
        }
    }

    #[test]
    fn dangling_area_room_is_rejected() {
        let cfg = Config {
            areas: vec![Area {
                id: Id::new("a"),
                name: "A".to_string(),
                icon: None,
                rooms: vec![Id::new("nowhere")],
                scenes: vec![],
                activities: vec![],
            }],
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems[0].at, "areas[0].rooms[0]");
    }

    #[test]
    fn dangling_area_activity_is_rejected() {
        let cfg = Config {
            areas: vec![Area {
                id: Id::new("a"),
                name: "A".to_string(),
                icon: None,
                rooms: vec![],
                scenes: vec![],
                activities: vec![Id::new("nothing")],
            }],
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems[0].at, "areas[0].activities[0]");
    }


    #[test]
    fn duplicate_device_ids_across_rooms_are_rejected() {
        let mut a = room("a", "A");
        let mut b = room("b", "B");
        a.devices.push(Device::new(Id::new("dup"), "One", DeviceKind::Light));
        b.devices.push(Device::new(Id::new("dup"), "Two", DeviceKind::Light));
        let cfg = Config { rooms: vec![a, b], ..Config::default() };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_scene_step_must_name_a_real_device() {
        let cfg = Config {
            scenes: vec![crate::Scene {
                hue: None,
                rooms: vec![],
                id: Id::new("s"),
                name: "S".to_string(),
                icon: None,
                steps: vec![Action::new(Id::new("ghost"), "on")],
            }],
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let cfg = Config { schema_version: SCHEMA_VERSION + 1, ..Config::default() };
        assert!(cfg.validate().is_err());
    }
    #[test]
    fn ha_resources_require_supported_domains_and_matching_kinds() {
        for (entity, kind) in [("light.office", DeviceKind::Light), ("cover.office", DeviceKind::Blind), ("climate.office", DeviceKind::Thermostat)] {
            assert!(valid_ha_resource(entity, kind));
        }
        for entity in ["cover.", "cover.a/b", "cover.a.b", "switch.office", "cover.Office"] {
            assert!(!valid_ha_resource(entity, DeviceKind::Blind));
        }
        assert!(!valid_ha_resource("cover.office", DeviceKind::Light));
        assert!(!valid_ha_resource("climate.office", DeviceKind::Blind));
    }

}
