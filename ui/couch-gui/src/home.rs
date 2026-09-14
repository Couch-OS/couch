//! Saved house configuration projected into the device's existing room rows.
use crate::{LiveActivity, RoomRow, SceneCell};
use couch_model::{Config, Id};
use std::path::PathBuf;
pub struct Area {
    /// The configured area, or `None` for the synthetic ALL ROOMS page.
    pub id: Option<Id>,
    pub name: String,
    /// What the shortcut and color keys reach on this page.
    pub shortcuts: Vec<couch_model::Shortcut>,
    pub activities: Vec<LiveActivity>,
    pub activity_ids: Vec<Id>,
    pub rooms: Vec<RoomRow>,
    pub scenes: Vec<SceneCell>,
    pub room_ids: Vec<Id>,
    pub scene_ids: Vec<Id>,
}
pub fn path(file: &str) -> PathBuf {
    if let Some(root) = std::env::var_os("COUCH_HOME_DIR") {
        return PathBuf::from(root).join(file);
    }
    let root = if std::path::Path::new("/mnt/alpine/opt/couch").is_dir() {
        "/mnt/alpine/opt/couch"
    } else {
        "/opt/couch"
    };
    PathBuf::from(root).join(file)
}
pub fn read(previous: &str) -> Option<(String, Vec<Area>, [u8; 3])> {
    let snapshot=crate::config_snapshot::current()?;
    let raw=snapshot.serial.to_string();
    if raw==previous{return None}
    let config=&snapshot.config;
    Some((raw, project(config), config.appearance.rgb()?))
}
pub(crate) fn project(config: &Config) -> Vec<Area> {
    let make = |id: Option<Id>, name: String, ids: Vec<Id>, scene_ids: Vec<Id>, activity_ids: Vec<Id>, shortcuts: Vec<couch_model::Shortcut>| {
        let rooms: Vec<_> = ids.iter().filter_map(|id| config.room(id)).collect();
        Area {
            id,
            name,
            shortcuts,
            activities: activity_ids.iter().filter_map(|id| config.activities.iter().find(|a| &a.id==id)).map(|a|LiveActivity {
                kind:a.kind.glyph_index(),title:a.name.as_str().into(),source:a.source.as_ref().and_then(|id|config.devices().find(|(_,d)|&d.id==id).map(|(_,d)|d.name.as_str())).unwrap_or("Choose a source").into(),
                place:config.room(&a.room).map(|r|r.name.as_str()).unwrap_or("").into(),
            }).collect(),
            activity_ids,
            scenes: scene_ids
                .iter()
                .filter_map(|id| config.scene(id))
                .map(|s| SceneCell {
                    name: s.name.as_str().into(),
                    active: false,
                })
                .collect(),
            scene_ids,
            room_ids: rooms.iter().map(|r| r.id.clone()).collect(),
            rooms: rooms
                .iter()
                .map(|r| RoomRow {
                    name: r.name.as_str().into(),
                    devices: r.device_summary().into(),
                    detail: r.device_detail().into(),
                    active_count: 0,
                    power_state: -1,
                    icon: crate::icons::image(r.effective_icon()),
                    status_known: false,
                    idle: true,
                    offline: false,
                    dimmed: false,
                    glyph: match r.effective_icon() {
                        couch_model::Icon::Sofa => 0,
                        couch_model::Icon::Bed => 1,
                        couch_model::Icon::CookingPot => 2,
                        couch_model::Icon::BookOpen => 3,
                        couch_model::Icon::DoorOpen => 4,
                        couch_model::Icon::Car => 5,
                        couch_model::Icon::Trees => 6,
                        couch_model::Icon::Lamp => 7,
                        couch_model::Icon::Tv => 8,
                        _ => 9,
                    },
                })
                .collect(),
        }
    };
    // All rooms remain reachable while the user is arranging their screens.
    let mut areas: Vec<_> = config
        .areas
        .iter()
        .map(|a| make(Some(a.id.clone()), a.name.clone(), a.rooms.clone(), a.scenes.clone(), a.activities.clone(), a.shortcuts.clone()))
        .collect();
    areas.push(make(
        None,
        "ALL ROOMS".into(),
        config.rooms.iter().map(|r| r.id.clone()).collect(),
        config.scenes.iter().map(|s| s.id.clone()).collect(),
        config.activities.iter().map(|a| a.id.clone()).collect(),
        Vec::new(),
    ));

    areas
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_icons_distinguish_on_off_and_unknown_devices() {
        assert_eq!(room_power([Some(false), Some(false)].into_iter()), 0);
        assert_eq!(room_power([Some(false), Some(true)].into_iter()), 1);
        assert_eq!(room_power([None, Some(true)].into_iter()), 1);
        assert_eq!(room_power([None, Some(false)].into_iter()), -1);
        assert_eq!(room_power([None].into_iter()), -1);
        assert_eq!(room_power([].into_iter()), -1);
    }
    #[test]
    fn the_state_plan_resolves_every_device_once_and_names_the_integrations() {
        let config: Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"h","name":"Hue","provider":{"kind":"hue"}},
                {"id":"a","name":"HA","provider":{"kind":"home-assistant"}}],
            "rooms":[{"id":"r","name":"Room","devices":[
                {"id":"l","name":"Lamp","kind":"light","integration":{"via":"connection","connection_id":"h","resource_id":"3"}},
                {"id":"s","name":"Strip","kind":"light","integration":{"via":"connection","connection_id":"a","resource_id":"light.strip"}},
                {"id":"t","name":"TV","kind":"tv"}]}]}))
        .unwrap();
        let plan = Plan::build(&config);
        assert!(plan.has_hue && plan.has_ha);
        assert_eq!(plan.rooms.len(), 1);
        assert_eq!(plan.rooms[0].0, config.rooms[0].id);
        assert_eq!(
            plan.rooms[0].1,
            vec![
                Source::Hue("h/3".into()),
                Source::Ha("a/light.strip".into()),
                Source::Unknown
            ]
        );
        let mut empty = config.clone();
        empty.rooms[0].devices.clear();
        let plan = Plan::build(&empty);
        assert!(!plan.has_hue && !plan.has_ha);
    }
    #[test]
    fn every_room_is_reachable_without_configured_areas() {
        let mut config = Config::seed();
        config.areas.clear();
        let areas = project(&config);
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].room_ids.len(), config.rooms.len());
        assert_eq!(
            areas[0].room_ids,
            config
                .rooms
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn configured_screens_keep_their_room_order() {
        let mut config = Config::seed();
        config.areas[0].rooms.reverse();
        let areas = project(&config);
        assert_eq!(areas[0].name, config.areas[0].name);
        assert_eq!(areas[0].id.as_ref(), Some(&config.areas[0].id));
        assert!(areas.last().unwrap().id.is_none());
        assert_eq!(areas[0].room_ids, config.areas[0].rooms);
        assert_eq!(areas.last().unwrap().room_ids.len(), config.rooms.len());
    }
}

/// Set both focus/text accent and its recessed tint from the saved RGB value.
pub fn apply_accent(app: &crate::App, rgb: [u8; 3]) {
    app.set_accent(slint::Color::from_rgb_u8(rgb[0], rgb[1], rgb[2]));
    let bg = [21u16, 19, 15];
    let tint = std::array::from_fn::<_, 3, _>(|i| ((rgb[i] as u16 * 15 + bg[i] * 85) / 100) as u8);
    app.set_accent_background(slint::Color::from_rgb_u8(tint[0], tint[1], tint[2]));
}

// One background observer for all rooms, sharing Hue's existing push cache.
// Unsupported devices stay unknown; an off light cannot prove an entire room off.
fn room_power(states: impl Iterator<Item = Option<bool>>) -> i32 {
    let mut any = false;
    let mut unknown = false;
    for state in states {
        any = true;
        match state {
            Some(true) => return 1,
            Some(false) => {}
            None => unknown = true,
        }
    }
    if any && !unknown {
        0
    } else {
        -1
    }
}
/// Where one device's on/off is read from. Anything else stays unknown: an
/// unreadable device must not let the rest of a room speak for it.
#[derive(Debug, PartialEq)]
enum Source {
    Hue(String),
    Ha(String),
    Unknown,
}
/// What a state pass needs from the configuration, derived once per snapshot
/// rather than on every pass: whether the house has either integration at all,
/// and each room's devices already resolved to their state key.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    has_hue: bool,
    has_ha: bool,
    rooms: Vec<(Id, Vec<Source>)>,
}
impl Plan {
    fn build(config: &Config) -> Self {
        let mut plan = Plan::default();
        for room in &config.rooms {
            let sources = room
                .devices
                .iter()
                .map(|d| match config.resolve_integration(&d.integration) {
                    Some(couch_model::Integration::Hue { light_id }) => {
                        plan.has_hue = true;
                        Source::Hue(light_id)
                    }
                    Some(couch_model::Integration::HomeAssistant { entity_id }) => {
                        plan.has_ha = true;
                        Source::Ha(entity_id)
                    }
                    _ => Source::Unknown,
                })
                .collect();
            plan.rooms.push((room.id.clone(), sources));
        }
        plan
    }
}
pub struct RoomMonitor {
    rx: std::sync::mpsc::Receiver<std::collections::HashMap<Id, i32>>,
    latest: std::collections::HashMap<Id, i32>,
    /// The area the rows were last written for; a page change has to write
    /// them again even when no new map arrived.
    shown: usize,
}
impl RoomMonitor {
    pub fn new(hue: std::sync::Arc<crate::connections::HueFleet>) -> Self {
        use std::{
            collections::HashMap,
            time::{Duration, Instant},
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut ha_states = HashMap::new();
            let mut ha_at = Instant::now() - Duration::from_secs(5);
            // The snapshot the plan was built from. The watcher re-reads and
            // validates config.json when its stat changes; this loop is a
            // state poll and must not read or parse the file at all.
            let (mut seen, mut plan) = (0, Plan::default());
            loop {
                let mut powers = HashMap::new();
                if let Some(snapshot) = crate::config_snapshot::current() {
                    if snapshot.serial != seen {
                        seen = snapshot.serial;
                        plan = Plan::build(&snapshot.config);
                    }
                    if plan.has_ha && ha_at.elapsed() >= Duration::from_secs(5) {
                        ha_states = crate::connections::ha_lights()
                            .into_iter()
                            .map(|s| (s.entity_id, s.on))
                            .collect();
                        ha_at = Instant::now();
                    }
                    let hue_states: HashMap<_, _> = if plan.has_hue {
                        hue.lights()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|s| (s.entity_id, s.on))
                            .collect()
                    } else {
                        HashMap::new()
                    };
                    for (room, sources) in &plan.rooms {
                        powers.insert(
                            room.clone(),
                            room_power(sources.iter().map(|s| match s {
                                Source::Hue(id) => hue_states.get(id).copied().flatten(),
                                Source::Ha(id) => ha_states.get(id).copied().flatten(),
                                Source::Unknown => None,
                            })),
                        );
                    }
                }
                match tx.try_send(powers) {
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
                    _ => {}
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        Self {
            rx,
            latest: Default::default(),
            shown: usize::MAX,
        }
    }
    pub fn poll(&mut self, app: &crate::App, areas: &mut [Area], current: usize) {
        use slint::Model;
        let mut arrived = false;
        while let Ok(latest) = self.rx.try_recv() {
            self.latest = latest;
            arrived = true;
        }
        // Walking every area's every room costs the same whether or not
        // anything moved, and this runs at the main loop's rate.
        if !arrived && current == self.shown {
            return;
        }
        self.shown = current;
        let model = app.get_rooms();
        let rows = model.as_any().downcast_ref::<slint::VecModel<RoomRow>>();
        for (area_index, area) in areas.iter_mut().enumerate() {
            for (row_index, (id, row)) in area.room_ids.iter().zip(&mut area.rooms).enumerate() {
                let power = self.latest.get(id).copied().unwrap_or(-1);
                if row.power_state != power {
                    row.power_state = power;
                    if area_index == current {
                        if let Some(rows) = rows {
                            rows.set_row_data(row_index, row.clone());
                        }
                    }
                }
            }
        }
    }
}
