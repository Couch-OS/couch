//! Connection-scoped credentials and state. Resource IDs may repeat on different servers.
use crate::home;
use couch_model::{Config, Provider};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub fn config() -> Option<Arc<Config>> {
    crate::config_snapshot::current().map(|s| s.config.clone())
}

pub fn file(id: &str, prefix: &str) -> PathBuf {
    if id.is_empty() {
        home::path(&format!("{prefix}-connection.json"))
    } else {
        home::path(&format!("connections/{id}/{prefix}-connection.json"))
    }
}
pub fn split(resource: &str) -> (&str, &str) {
    resource.split_once('/').unwrap_or(("", resource))
}
fn valid(id: &str, provider: Provider) -> bool {
    if id.is_empty() {
        return true;
    }
    config().is_some_and(|c| {
        c.connections
            .iter()
            .any(|v| v.id.as_str() == id && v.provider == provider)
    })
}
fn ids(provider: Provider) -> Vec<String> {
    let ids: Vec<_> = config()
        .map(|c| {
            c.connections
                .iter()
                .filter(|c| c.provider == provider)
                .map(|c| c.id.to_string())
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        vec![String::new()]
    } else {
        ids
    }
}
pub fn ha(resource: &str) -> Result<(couch_ha::HomeAssistant, String), String> {
    let (id, raw) = split(resource);
    if !valid(id, Provider::HomeAssistant) {
        return Err("Home Assistant connection was removed".into());
    }
    let client = couch_ha::settings::Settings::load(&file(id, "ha"))
        .and_then(|s| s.client())
        .map_err(|e| e.to_string())?;
    Ok((client, raw.into()))
}
/// Where speech goes: the first saved Home Assistant connection's host, port
/// and token, or why there is none. The token is read from the connection's
/// own file, never from the config the web UI edits.
pub fn ha_assist() -> Result<crate::mic::Endpoint, String> {
    let ids = ids(Provider::HomeAssistant);
    let Some(id) = ids.first() else {
        return Err("Add a Home Assistant connection in the web UI to use voice".into());
    };
    if config().is_some_and(|c| {
        !c.connections
            .iter()
            .any(|v| v.provider == Provider::HomeAssistant)
    }) {
        return Err("Add a Home Assistant connection in the web UI to use voice".into());
    }
    let settings = couch_ha::settings::Settings::load(&file(id, "ha"))
        .map_err(|_| "The Home Assistant connection has no saved URL and token yet".to_string())?;
    crate::mic::Endpoint::from_url(&settings.url, &settings.token)
}
/// Room-row state for exactly the entities the hub names, one `/api/states`
/// read per connection, plus the connections whose read failed: a room behind
/// a Home Assistant that is down is offline, which is not the same as off.
pub fn ha_power(wanted: &[String]) -> (Vec<couch_ha::Power>, Vec<String>) {
    use std::collections::BTreeSet;
    let (mut states, mut failed) = (Vec::new(), Vec::new());
    for id in ids(Provider::HomeAssistant) {
        let mine: BTreeSet<String> = wanted
            .iter()
            .filter(|w| split(w).0 == id)
            .map(|w| split(w).1.to_owned())
            .collect();
        if mine.is_empty() {
            continue;
        }
        let read = couch_ha::settings::Settings::load(&file(&id, "ha"))
            .and_then(|s| s.client())
            .and_then(|c| c.power(&mine));
        match read {
            Ok(list) => states.extend(list.into_iter().map(|mut p| {
                if !id.is_empty() {
                    p.entity_id = format!("{id}/{}", p.entity_id);
                }
                p
            })),
            Err(_) => failed.push(id),
        }
    }
    (states, failed)
}
/// Fetch all room-supported HA entities once per connection. Preserve the
/// connection prefix because entity IDs are only unique within one server.
pub fn ha_room_states() -> Vec<crate::lights::DeviceState> {
    use crate::lights::DeviceState;
    let mut result = Vec::new();
    for id in ids(Provider::HomeAssistant) {
        let Ok(client) = couch_ha::settings::Settings::load(&file(&id, "ha"))
            .and_then(|settings| settings.client())
        else {
            continue;
        };
        let Ok(entities) = client.entities() else {
            continue;
        };
        let mut states = Vec::new();
        states.extend(entities.lights.into_iter().map(DeviceState::Light));
        states.extend(entities.covers.into_iter().map(DeviceState::Cover));
        states.extend(entities.climates.into_iter().map(DeviceState::Climate));
        for mut state in states {
            if !id.is_empty() {
                state.set_id(format!("{id}/{}", state.id()));
            }
            result.push(state);
        }
    }
    result
}
#[derive(Default)]
pub struct HueFleet {
    clients: Mutex<HashMap<String, Arc<couch_hue::live::Live>>>,
}
impl HueFleet {
    fn get(&self, id: &str) -> Result<Arc<couch_hue::live::Live>, String> {
        if !valid(id, Provider::Hue) {
            return Err("Hue connection was removed".into());
        }
        let mut clients = self.clients.lock().unwrap();
        Ok(clients
            .entry(id.into())
            .or_insert_with(|| Arc::new(couch_hue::live::Live::new(file(id, "hue"))))
            .clone())
    }
    /// Also which bridges failed their last read, for the same reason
    /// `ha_power` reports them: an unreachable bridge is not an off room.
    pub fn states(&self) -> (Vec<couch_ha::Light>, Vec<String>) {
        let ids = ids(Provider::Hue);
        self.clients
            .lock()
            .unwrap()
            .retain(|id, _| ids.contains(id));
        let (mut lights, mut failed) = (vec![], vec![]);
        for id in ids {
            let Ok(list) = self.get(&id).and_then(|c| c.lights().map_err(|e| e.to_string())) else {
                failed.push(id);
                continue;
            };
            for mut light in list {
                if !id.is_empty() {
                    light.entity_id = format!("{id}/{}", light.entity_id);
                }
                lights.push(light);
            }
        }
        (lights, failed)
    }
    pub fn lights(&self) -> Result<Vec<couch_ha::Light>, String> {
        Ok(self.states().0)
    }
    pub fn toggle(&self, resource: &str) -> Result<couch_ha::Light, String> {
        let (id, raw) = split(resource);
        self.get(id)?.toggle(raw).map_err(|e| e.to_string())
    }
    pub fn brightness(&self, resource: &str, level: u8) -> Result<couch_ha::Light, String> {
        let (id, raw) = split(resource);
        self.get(id)?
            .brightness(raw, level)
            .map_err(|e| e.to_string())
    }
    pub fn reset(&self) {
        for client in self.clients.lock().unwrap().values() {
            client.reset();
        }
    }
}

/// Matter fabrics, one controller per connection, opened on first use and kept
/// so light reads reuse the bound sockets. Lights carry `<connection>/<node>/<endpoint>`
/// like Hue resources, so the room list needs no new state shape.
#[derive(Default)]
pub struct MatterFleet {
    controllers: Mutex<HashMap<String, Arc<couch_matter::Controller>>>,
}
impl MatterFleet {
    fn get(&self, id: &str) -> Result<Arc<couch_matter::Controller>, String> {
        if !valid(id, Provider::Matter) {
            return Err("Matter connection was removed".into());
        }
        let mut controllers = self.controllers.lock().unwrap();
        if let Some(c) = controllers.get(id) {
            return Ok(c.clone());
        }
        let dir = file(id, "matter").with_file_name("matter");
        let controller = Arc::new(couch_matter::Controller::open(&dir).map_err(|e| e.to_string())?);
        controllers.insert(id.into(), controller.clone());
        Ok(controller)
    }
    fn light(id: &str, light: couch_matter::Light) -> couch_ha::Light {
        couch_ha::Light {
            entity_id: format!("{id}/{}", light.entity_id),
            name: light.name,
            on: light.on,
            brightness_percent: light.brightness_percent,
            dimmable: light.dimmable,
        }
    }
    pub fn lights(&self) -> Vec<couch_ha::Light> {
        let ids = ids(Provider::Matter);
        self.controllers
            .lock()
            .unwrap()
            .retain(|id, _| ids.contains(id));
        let mut lights = vec![];
        for id in ids {
            if let Ok(controller) = self.get(&id) {
                lights.extend(controller.lights().into_iter().map(|l| Self::light(&id, l)));
            }
        }
        lights
    }
    pub fn toggle(&self, resource: &str) -> Result<couch_ha::Light, String> {
        let (id, raw) = split(resource);
        let controller = self.get(id)?;
        let state = controller.light(raw).map_err(|e| e.to_string())?;
        let command = match state.on {
            Some(true) => couch_matter::Command::Off,
            Some(false) => couch_matter::Command::On,
            None => return Err("This device is unavailable".into()),
        };
        controller
            .command(raw, command)
            .map(|l| Self::light(id, l))
            .map_err(|e| e.to_string())
    }
    pub fn brightness(&self, resource: &str, percent: u8) -> Result<couch_ha::Light, String> {
        let (id, raw) = split(resource);
        self.get(id)?
            .command(raw, couch_matter::Command::Brightness(percent))
            .map(|l| Self::light(id, l))
            .map_err(|e| e.to_string())
    }
}
