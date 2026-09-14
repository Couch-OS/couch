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
        assert!(plan.has_hue);
        assert_eq!(plan.ha_ids, vec!["a/light.strip".to_string()]);
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
        assert!(!plan.has_hue && plan.ha_ids.is_empty());
    }
    fn on(bright: Option<u8>) -> Reading {
        Reading { on: Some(true), bright, lost: false }
    }
    fn off() -> Reading {
        Reading { on: Some(false), bright: Some(0), lost: false }
    }
    /// What the hub's status column says, and the four ways it can say
    /// nothing rather than guess.
    #[test]
    fn the_status_column_counts_only_devices_that_answered() {
        // Two lights on out of three readable devices, one of them bright.
        let lit = room_state(&[on(Some(80)), on(Some(20)), off()]);
        assert_eq!((lit.power, lit.on, lit.known), (1, 2, true));
        assert!(!lit.dimmed && !lit.offline);
        // Known and nothing on: IDLE, and the icon disc goes to its off tint.
        let idle = room_state(&[off(), off()]);
        assert_eq!((idle.power, idle.on, idle.known), (0, 0, true));
        assert!(!idle.offline);
        // An unreadable device is excluded from the count, and leaves the
        // tri-state icon unknown rather than claiming the room is off.
        let mixed = room_state(&[Reading::default(), off()]);
        assert_eq!((mixed.power, mixed.on, mixed.known), (-1, 0, true));
        // No device in the room reads at all: no status column.
        let silent = room_state(&[Reading::default(), Reading::default()]);
        assert_eq!((silent.power, silent.on, silent.known), (-1, 0, false));
        assert!(!silent.offline);
        assert_eq!(room_state(&[]), RoomState::default());
    }
    #[test]
    fn dimmed_needs_a_reported_level_and_every_lit_light_below_half() {
        assert!(room_state(&[on(Some(20)), on(Some(49)), off()]).dimmed);
        assert!(!room_state(&[on(Some(20)), on(Some(50))]).dimmed);
        // A lit light with no level reported is not evidence of a dim room,
        // and an off light's zero must not drag the room into it.
        assert!(!room_state(&[on(None), on(None)]).dimmed);
        assert!(room_state(&[on(Some(10)), on(None)]).dimmed);
        assert!(!room_state(&[off(), off()]).dimmed);
    }
    /// OFFLINE is the bridge failing, not the room being off, and it only
    /// speaks when nothing else in the room can.
    #[test]
    fn offline_is_reserved_for_a_connection_that_failed_its_last_fetch() {
        let down = Reading { on: None, bright: None, lost: true };
        let gone = room_state(&[down, Reading::default()]);
        assert!(gone.offline && gone.known);
        assert_eq!((gone.power, gone.on), (-1, 0));
        // A second bridge still answering: the count it gives is true, so the
        // card shows it rather than declaring the whole room unreachable.
        let partial = room_state(&[down, on(None)]);
        assert!(!partial.offline && partial.known);
        assert_eq!(partial.on, 1);
        assert!(!room_state(&[down, off()]).offline);
        assert!(lost(&["a".to_string()], "a/light.strip"));
        assert!(!lost(&["a".to_string()], "b/light.strip"));
        // A legacy unprefixed entity belongs to the unnamed connection.
        assert!(lost(&[String::new()], "light.strip"));
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
    /// The hub's fallback when there is no configuration at all: the panel
    /// says so, and every index the shell keeps stays valid behind it.
    #[test]
    fn an_empty_configuration_still_projects_one_reachable_area() {
        let areas = project(&Config::default());
        assert_eq!(areas.len(), 1);
        assert!(areas[0].id.is_none());
        assert!(areas[0].rooms.is_empty());
        assert!(areas[0].scenes.is_empty());
        assert!(areas[0].activities.is_empty());
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
/// unreadable device must not let the rest of a room speak for it. A Sonos
/// player reached through Home Assistant is a `media_player` entity and reads
/// like any other; a directly configured one would need a poll of its own,
/// which this loop deliberately does not make.
#[derive(Debug, PartialEq)]
enum Source {
    Hue(String),
    Ha(String),
    Unknown,
}
/// What a state pass needs from the configuration, derived once per snapshot
/// rather than on every pass: whether the house has either integration at all,
/// each room's devices already resolved to their state key, and the exact HA
/// entities to ask for so the pass never reads the whole house.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    has_hue: bool,
    ha_ids: Vec<String>,
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
                        if !plan.ha_ids.contains(&entity_id) {
                            plan.ha_ids.push(entity_id.clone());
                        }
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
/// One device's contribution to its room's card. `on` is None when nothing
/// readable answered for it; `bright` is only ever a light's percentage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Reading {
    on: Option<bool>,
    bright: Option<u8>,
    /// The connection serving this device failed its last fetch, so "off"
    /// would be a guess.
    lost: bool,
}
/// Everything one room card shows beyond its name and device list.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RoomState {
    power: i32,
    on: i32,
    known: bool,
    dimmed: bool,
    offline: bool,
}
impl Default for RoomState {
    /// Nothing has answered yet: the row the projection starts from.
    fn default() -> Self {
        Self { power: -1, on: 0, known: false, dimmed: false, offline: false }
    }
}
/// The status column derived from one room's readings. A device nothing
/// answered for is excluded, never counted off, so a single unreadable TV
/// cannot claim its room is idle. OFFLINE is reserved for the case where a
/// room has devices behind a failed connection and nothing else to report:
/// with one bridge down and another answering, the count is still true.
fn room_state(readings: &[Reading]) -> RoomState {
    let on = readings.iter().filter(|r| r.on == Some(true)).count() as i32;
    let known = readings.iter().any(|r| r.on.is_some());
    let offline = !known && readings.iter().any(|r| r.lost);
    // Only lights report brightness, and only while they are on. No level at
    // all is not evidence of a dim room.
    let mut levels = readings
        .iter()
        .filter(|r| r.on == Some(true))
        .filter_map(|r| r.bright)
        .peekable();
    RoomState {
        power: room_power(readings.iter().map(|r| r.on)),
        on,
        known: known || offline,
        dimmed: on > 0 && levels.peek().is_some() && levels.all(|b| b < 50),
        offline,
    }
}
/// A device is lost rather than off when its connection failed its last
/// fetch. State keys carry the connection ID that resolved them.
fn lost(failed: &[String], key: &str) -> bool {
    failed.iter().any(|id| id == crate::connections::split(key).0)
}
pub struct RoomMonitor {
    rx: std::sync::mpsc::Receiver<std::collections::HashMap<Id, RoomState>>,
    latest: std::collections::HashMap<Id, RoomState>,
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
            let mut ha_failed: Vec<String> = Vec::new();
            let mut ha_at = Instant::now() - Duration::from_secs(5);
            // The snapshot the plan was built from. The watcher re-reads and
            // validates config.json when its stat changes; this loop is a
            // state poll and must not read or parse the file at all.
            let (mut seen, mut plan) = (0, Plan::default());
            loop {
                let mut states = HashMap::new();
                if let Some(snapshot) = crate::config_snapshot::current() {
                    if snapshot.serial != seen {
                        seen = snapshot.serial;
                        plan = Plan::build(&snapshot.config);
                    }
                    if !plan.ha_ids.is_empty() && ha_at.elapsed() >= Duration::from_secs(5) {
                        let (power, failed) = crate::connections::ha_power(&plan.ha_ids);
                        ha_states = power
                            .into_iter()
                            .map(|s| (s.entity_id, (s.on, s.brightness_percent)))
                            .collect();
                        ha_failed = failed;
                        ha_at = Instant::now();
                    }
                    let (hue_lights, hue_failed) = if plan.has_hue {
                        hue.states()
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    let hue_states: HashMap<_, _> = hue_lights
                        .into_iter()
                        .map(|s| (s.entity_id, (s.on, s.brightness_percent)))
                        .collect();
                    let read = |from: &HashMap<String, (Option<bool>, Option<u8>)>,
                                failed: &[String],
                                id: &str| {
                        let (on, bright) = from.get(id).copied().unwrap_or_default();
                        Reading { on, bright, lost: lost(failed, id) }
                    };
                    for (room, sources) in &plan.rooms {
                        let readings: Vec<_> = sources
                            .iter()
                            .map(|s| match s {
                                Source::Hue(id) => read(&hue_states, &hue_failed, id),
                                Source::Ha(id) => read(&ha_states, &ha_failed, id),
                                Source::Unknown => Reading::default(),
                            })
                            .collect();
                        states.insert(room.clone(), room_state(&readings));
                    }
                }
                match tx.try_send(states) {
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
                let s = self.latest.get(id).copied().unwrap_or_default();
                // A room nothing answered for keeps the recessed style it has
                // today, so idle is simply "nothing on".
                let next = (s.power, s.on, s.known, s.on == 0, s.offline, s.dimmed);
                let shown = (row.power_state, row.active_count, row.status_known, row.idle, row.offline, row.dimmed);
                if shown != next {
                    (row.power_state, row.active_count, row.status_known, row.idle, row.offline, row.dimmed) = next;
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
