//! Bridge rooms are grouped-light controls; scenes are recall actions, not switches.
use crate::{valid_id, Error, Hue, Light, Result};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Serialize)]
pub struct Resource {
    #[serde(flatten)]
    pub state: Light,
    pub resource_kind: String,
    pub room_name: String,
}
/// How close a room's lamps have to be for it to have one colour temperature:
/// lamps of different models land a few mirek apart on the same request.
///
/// The packaged Hue integration derives a room's white the same way
/// (couch-integration-hue, `src/catalog.rs`); the two have to agree, because a
/// bridge can be driven through either.
const SAME_WHITE: u16 = 12;

/// The colour temperatures every tunable lamp in a room can reach: the part
/// their ranges share. A room with no tunable lamp, or whose lamps share
/// nothing, has no range and is offered no colour temperature.
fn room_range(lamps: &[&Light]) -> Option<(u16, u16)> {
    let mut ranges = lamps.iter().filter_map(|lamp| lamp.mirek_range);
    let first = ranges.next()?;
    let (cool, warm) = ranges.fold(first, |(cool, warm), (c, w)| (cool.max(c), warm.min(w)));
    (cool <= warm).then_some((cool, warm))
}

/// The room's colour temperature, when its lamps agree on one.
///
/// Only lamps that are on have a say, and a lamp showing a colour reports no
/// mirek at all, so it takes no part. Lamps further apart than `SAME_WHITE`
/// are not showing one white between them, and the room has no value - which
/// is not a problem: the first Channel press gives it one.
fn room_mirek(lamps: &[&Light], range: Option<(u16, u16)>) -> Option<u16> {
    let (cool, warm) = range?;
    let lit: Vec<u16> = lamps
        .iter()
        .filter(|lamp| lamp.on == Some(true))
        .filter_map(|lamp| lamp.mirek)
        .collect();
    let (coolest, warmest) = (*lit.iter().min()?, *lit.iter().max()?);
    if warmest - coolest > SAME_WHITE {
        return None;
    }
    let mean = lit.iter().map(|m| u32::from(*m)).sum::<u32>() / lit.len() as u32;
    Some((mean as u16).clamp(cool, warm))
}

/// The lamps a room holds.
///
/// A room's `children` are *device* rids and a lamp's `owner.rid` is its
/// device, so the two are joined through the device. One lamp a device, which
/// is what the packaged integration does too: a multi-head fixture speaks with
/// one voice rather than out-voting the rest of the room.
fn room_lamps<'a>(all: &[Value], room: &Value, lights: &'a [Light]) -> Vec<&'a Light> {
    room["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|child| child["rid"].as_str())
        .filter_map(|device| {
            let lamp = all
                .iter()
                .find(|v| v["type"] == "light" && v["owner"]["rid"].as_str() == Some(device))?;
            let id = lamp["id"].as_str()?;
            lights.iter().find(|light| light.entity_id == id)
        })
        .collect()
}

pub fn valid_control(id: &str) -> bool {
    valid_id(
        id.strip_prefix("room:")
            .or_else(|| id.strip_prefix("scene:"))
            .unwrap_or(id),
    )
}
impl Hue {
    pub fn resources(&self) -> Result<Vec<Resource>> {
        Self::parse_resources(&self.raw_resources()?)
    }
    pub fn control_states(&self) -> Result<Vec<Light>> {
        Ok(self.resources()?.into_iter().map(|r| r.state).collect())
    }
    pub fn control_state(&self, id: &str) -> Result<Light> {
        if !valid_control(id) {
            return Err(Error::Configuration);
        }
        self.control_states()?
            .into_iter()
            .find(|s| s.entity_id == id)
            .ok_or(Error::Unavailable)
    }
    pub fn recall_scene(&self, id: &str) -> Result<()> {
        self.write_resource("scene", id, json!({"recall":{"action":"active"}}))
    }
    fn parse_resources(all: &[Value]) -> Result<Vec<Resource>> {
        let lights = Self::parse_lights(all)?;
        let mut result: Vec<_> = lights
            .iter()
            .cloned()
            .map(|state| Resource {
                state,
                resource_kind: "light".into(),
                room_name: String::new(),
            })
            .collect();
        for room in all.iter().filter(|v| v["type"] == "room") {
            let name = room["metadata"]["name"].as_str().unwrap_or("Hue room");
            let services = room["services"].as_array().cloned().unwrap_or_default();
            let group = services
                .iter()
                .find(|s| s["rtype"] == "grouped_light")
                .and_then(|s| s["rid"].as_str());
            if let Some(id) = group.filter(|id| valid_id(id)) {
                let state = all
                    .iter()
                    .find(|s| s["type"] == "grouped_light" && s["id"] == id);
                let on = state.and_then(|s| s["on"]["on"].as_bool());
                let dimmable = state.is_some_and(|s| s["dimming"].is_object());
                let brightness_percent = match on {
                    Some(false) | Some(true) => state
                        .and_then(|s| s["dimming"]["brightness"].as_f64())
                        .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
                        .map(|p| p.round() as u8),
                    None => None,
                };
                // A grouped light takes a colour temperature and never
                // reports one, nor a range: both come from the room's lamps.
                let lamps = room_lamps(all, room, &lights);
                let range = room_range(&lamps);
                result.push(Resource {
                    state: Light {
                        entity_id: format!("room:{id}"),
                        name: name.into(),
                        on,
                        brightness_percent,
                        dimmable,
                        mirek: on.and(room_mirek(&lamps, range)),
                        mirek_range: range,
                    },
                    resource_kind: "room".into(),
                    room_name: name.into(),
                });
            }
        }
        for scene in all.iter().filter(|v| v["type"] == "scene") {
            let id = scene["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or(Error::Response)?;
            let room_name = all
                .iter()
                .find(|r| {
                    (r["type"] == "room" || r["type"] == "zone") && r["id"] == scene["group"]["rid"]
                })
                .and_then(|r| r["metadata"]["name"].as_str())
                .unwrap_or("");
            result.push(Resource {
                state: Light {
                    entity_id: format!("scene:{id}"),
                    name: scene["metadata"]["name"]
                        .as_str()
                        .unwrap_or("Hue scene")
                        .into(),
                    on: None,
                    brightness_percent: None,
                    dimmable: false,
                    mirek: None,
                    mirek_range: None,
                },
                resource_kind: "scene".into(),
                room_name: room_name.into(),
            });
        }
        result.sort_by(|a, b| {
            a.state
                .name
                .cmp(&b.state.name)
                .then(a.state.entity_id.cmp(&b.state.entity_id))
        });
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_brightness_comes_from_its_grouped_light_service() {
        let id = "00000000-0000-0000-0000-000000000001";
        for (on, dimming, expected, supported) in [
            (json!(true), json!({"brightness":42.4}), Some(42), true),
            (json!(false), json!({"brightness":42.4}), Some(42), true),
            (Value::Null, json!({"brightness":42.4}), None, true),
            (json!(true), json!({"brightness":101}), None, true),
            (json!(true), Value::Null, None, false),
        ] {
            let resources = Hue::parse_resources(&[
                json!({"id":"room","type":"room","metadata":{"name":"Server Room"},
                    "services":[{"rtype":"grouped_light","rid":id}]}),
                json!({"id":id,"type":"grouped_light","on":{"on":on},"dimming":dimming}),
            ])
            .unwrap();
            assert_eq!(resources[0].state.entity_id, format!("room:{id}"));
            assert_eq!(resources[0].state.brightness_percent, expected);
            assert_eq!(resources[0].state.dimmable, supported);
        }
    }
    #[test]
    fn grouped_power_and_scene_recall_use_distinct_resources() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let id = "00000000-0000-0000-0000-000000000001";
        let worker = std::thread::spawn(move || {
            for (kind, body) in [
                ("grouped_light", json!({"on":{"on":false}})),
                ("scene", json!({"recall":{"action":"active"}})),
            ] {
                let mut request = server
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method(), &tiny_http::Method::Put);
                assert_eq!(request.url(), format!("/clip/v2/resource/{kind}/{id}"));
                let mut text = String::new();
                request.as_reader().read_to_string(&mut text).unwrap();
                assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), body);
                request
                    .respond(tiny_http::Response::from_string(
                        json!({"errors":[],"data":[{"rid":id,"rtype":kind}]}).to_string(),
                    ))
                    .unwrap();
            }
        });
        let client = Hue {
            base: format!("http://{address}"),
            key: "fixture".into(),
            agent: ureq::Agent::new_with_defaults(),
        };
        client.set_power(&format!("room:{id}"), false).unwrap();
        client.recall_scene(id).unwrap();
        assert!(client.set_power(&format!("scene:{id}"), true).is_err());
        worker.join().unwrap();
    }
    fn tunable(range: Option<(u16, u16)>, on: Option<bool>, mirek: Option<u16>) -> Light {
        Light {
            entity_id: "lamp".into(),
            name: "Lamp".into(),
            on,
            brightness_percent: None,
            dimmable: true,
            mirek,
            mirek_range: range,
        }
    }

    /// The rules the packaged Hue integration uses for the same job
    /// (couch-integration-hue, `src/catalog.rs`): a bridge can be driven
    /// through either, so the two have to say the same thing about a room.
    #[test]
    fn a_rooms_white_is_the_one_its_lamps_share_and_agree_on() {
        let wide = tunable(Some((153, 500)), Some(true), Some(370));
        let narrow = tunable(Some((200, 454)), Some(true), Some(366));
        // The range is the part every tunable lamp can reach.
        assert_eq!(room_range(&[&wide, &narrow]), Some((200, 454)));
        assert_eq!(room_range(&[&wide]), Some((153, 500)));
        // A lamp with no range of its own is not a tunable member, and a room
        // with none at all is offered nothing.
        let plain = tunable(None, Some(true), None);
        assert_eq!(room_range(&[&wide, &plain]), Some((153, 500)));
        assert_eq!(room_range(&[&plain, &plain]), None);
        assert_eq!(room_range(&[]), None);
        // Ranges that share nothing are not a range.
        let warm_only = tunable(Some((400, 500)), Some(true), None);
        let cool_only = tunable(Some((153, 250)), Some(true), None);
        assert_eq!(room_range(&[&warm_only, &cool_only]), None);

        // Lamps within `SAME_WHITE` of each other are showing one white: the
        // room's value is their mean, clamped to the shared range.
        let range = Some((153, 454));
        assert_eq!(room_mirek(&[&wide, &narrow], range), Some(368));
        // Further apart than that, the room has no one colour temperature.
        let far = tunable(Some((153, 500)), Some(true), Some(300));
        assert_eq!(room_mirek(&[&wide, &far], range), None);
        // A lamp that is off, and a lamp showing a colour (which reports no
        // mirek at all), take no part; with none left there is no value.
        let off = tunable(Some((153, 500)), Some(false), Some(370));
        let colour = tunable(Some((153, 500)), Some(true), None);
        assert_eq!(room_mirek(&[&off, &colour], range), None);
        assert_eq!(room_mirek(&[&wide, &colour, &off], range), Some(370));
        // The mean is clamped into the shared range, never outside it.
        let hot = tunable(Some((153, 500)), Some(true), Some(500));
        assert_eq!(room_mirek(&[&hot], Some((153, 454))), Some(454));
        // No range means no value, whatever the lamps say.
        assert_eq!(room_mirek(&[&wide], None), None);
    }

    /// End to end from one bridge read: the room carries the range and the
    /// value its lamps give it, joined through the device its children name.
    #[test]
    fn a_room_takes_its_colour_temperature_from_the_lamps_it_holds() {
        let group = "00000000-0000-0000-0000-000000000001";
        let (one, two) = (
            "00000000-0000-0000-0000-00000000000a",
            "00000000-0000-0000-0000-00000000000b",
        );
        let lamp = |id: &str, device: &str, cool: u64, warm: u64, mirek: u64| {
            json!({"id":id,"type":"light","owner":{"rid":device,"rtype":"device"},
                "metadata":{"name":id},"on":{"on":true},"dimming":{"brightness":50},
                "color_temperature":{"mirek":mirek,"mirek_valid":true,
                    "mirek_schema":{"mirek_minimum":cool,"mirek_maximum":warm}}})
        };
        let connectivity = |device: &str| {
            json!({"id":device,"type":"zigbee_connectivity",
                "owner":{"rid":device,"rtype":"device"},"status":"connected"})
        };
        let bridge = |children: Value| {
            vec![
                lamp(one, "d1", 153, 500, 370),
                lamp(two, "d2", 200, 454, 366),
                connectivity("d1"),
                connectivity("d2"),
                json!({"id":"r0","type":"room","metadata":{"name":"Living room"},
                    "children":children,
                    "services":[{"rtype":"grouped_light","rid":group}]}),
                json!({"id":group,"type":"grouped_light","on":{"on":true},
                    "dimming":{"brightness":60}}),
            ]
        };
        let room_of = |all: &[Value]| {
            Hue::parse_resources(all)
                .unwrap()
                .into_iter()
                .find(|r| r.resource_kind == "room")
                .unwrap()
                .state
        };
        let both = room_of(&bridge(
            json!([{"rid":"d1","rtype":"device"},{"rid":"d2","rtype":"device"}]),
        ));
        assert_eq!(both.entity_id, format!("room:{group}"));
        assert_eq!(both.mirek_range, Some((200, 454)));
        assert_eq!(both.mirek, Some(368));
        // One lamp only: its own range, its own reading.
        let alone = room_of(&bridge(json!([{"rid":"d1","rtype":"device"}])));
        assert_eq!(alone.mirek_range, Some((153, 500)));
        assert_eq!(alone.mirek, Some(370));
        // A room that lists no lamps is offered no colour temperature, and one
        // whose grouped light is unreachable reports no value.
        let empty = room_of(&bridge(json!([])));
        assert_eq!((empty.mirek_range, empty.mirek), (None, None));
        let mut unreachable = bridge(json!([{"rid":"d1","rtype":"device"}]));
        unreachable[5] = json!({"id":group,"type":"grouped_light","dimming":{"brightness":60}});
        let dark = room_of(&unreachable);
        assert_eq!(dark.on, None);
        assert_eq!(dark.mirek_range, Some((153, 500)));
        assert_eq!(dark.mirek, None);
    }

    #[test]
    fn rooms_use_group_service_and_scenes_keep_room_context() {
        let id = "00000000-0000-0000-0000-000000000001";
        let resources=Hue::parse_resources(&[
            json!({"id":"room","type":"room","metadata":{"name":"Office"},"services":[{"rtype":"grouped_light","rid":id}]}),
            json!({"id":id,"type":"grouped_light","on":{"on":true}}),
            json!({"id":id,"type":"scene","metadata":{"name":"Relax"},"group":{"rid":"room"}})
        ]).unwrap();
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].state.entity_id, format!("room:{id}"));
        assert_eq!(resources[0].state.on, Some(true));
        assert_eq!(resources[1].room_name, "Office");
        assert_eq!(resources[1].state.on, None);
        assert!(!valid_control("scene:../room"));
        assert!(!valid_control(&format!("room:scene:{id}")));
    }
}
