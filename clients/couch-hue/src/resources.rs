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
        let mut result: Vec<_> = Self::parse_lights(all)?
            .into_iter()
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
                result.push(Resource {
                    state: Light {
                        entity_id: format!("room:{id}"),
                        name: name.into(),
                        on,
                        brightness_percent,
                        dimmable,
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
