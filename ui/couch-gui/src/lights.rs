//! Room light controls. Network requests run on one worker, never on Slint's thread.
use crate::{App, ChoiceItem};
use couch_ha::{Climate, Command, Cover, CoverCommand, Light};
use couch_model::{
    ChildComponent, ChildSnapshot, CoverState, CoverTraits, Id, Integration, LightState,
    LightTraits, TypedAction,
};
use couch_plugin::{Request, Response};
use slint::{Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub(crate) enum DeviceState {
    Light(Light),
    Cover(Cover),
    Climate(Climate),
}
impl DeviceState {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Light(s) => &s.entity_id,
            Self::Cover(s) => &s.entity_id,
            Self::Climate(s) => &s.entity_id,
        }
    }
    pub(crate) fn set_id(&mut self, id: String) {
        match self {
            Self::Light(s) => s.entity_id = id,
            Self::Cover(s) => s.entity_id = id,
            Self::Climate(s) => s.entity_id = id,
        }
    }
    fn active(&self) -> Option<bool> {
        match self {
            Self::Light(s) => s.on,
            Self::Cover(s) => s.state.as_deref().map(|s| s != "closed"),
            Self::Climate(s) => s.available.then(|| s.hvac_mode.as_deref() != Some("off")),
        }
    }
    fn description(&self) -> String {
        match self {
            Self::Light(s) => description(s),
            Self::Cover(s) => cover_description(s),
            Self::Climate(s) => {
                if !s.available {
                    return "Unavailable".into();
                }
                let unit = &s.temperature_unit;
                match (s.current_temperature, s.target_temperature) {
                    (Some(current), Some(target)) => {
                        format!("{current} {unit} · Target {target} {unit}")
                    }
                    (_, Some(target)) => format!("Target {target} {unit}"),
                    (Some(current), _) => format!(
                        "{current} {unit} · {}",
                        s.hvac_mode.as_deref().unwrap_or("Thermostat")
                    ),
                    _ => s.hvac_mode.clone().unwrap_or_else(|| "Thermostat".into()),
                }
            }
        }
    }
}
fn ha_domain(id: &str) -> &str {
    // A packaged child's row id carries the package's own spelling of the
    // child, which may well look like an entity id (a Home Assistant package
    // would list `cover.blind`). It is not one: only a Home Assistant row
    // has a domain.
    if id.starts_with(PLUGIN_PREFIX) {
        return "";
    }
    crate::connections::split(id)
        .1
        .split_once('.')
        .map(|(domain, _)| domain)
        .unwrap_or("")
}
/// Row ids of packaged children: the connection and the child, which cannot
/// collide because a connection id never contains a slash.
const PLUGIN_PREFIX: &str = "plugin:";
/// A status read is one of many in a round, made one after another on the one
/// worker, so it waits far less than a command does.
const STATUS_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long one write may be out before the control screen says so.
///
/// The bar and the read-out move on the press, so a write that is answered in
/// the usual 100-300 ms needs no label at all; a packaged lamp under a held key
/// chains writes and would otherwise read "Updating…" for the whole hold. Past
/// this, something is wrong enough to be worth saying.
const UPDATING_AFTER: Duration = Duration::from_millis(1500);
/// A command, and the read that follows a package's bare `Ok`.
fn write_timeout() -> Duration {
    couch_plugin::REQUEST_TIMEOUT + Duration::from_secs(1)
}
/// What every packaged request in this module goes through: the connection,
/// the frame, and how long to wait. The tests put a closure here in place of
/// the daemon's socket.
pub(crate) type Ask<'a> =
    &'a mut dyn FnMut(&str, Request, Duration) -> Result<Response, couch_plugin::Failure>;

pub(crate) fn plugin_socket(
    connection: &str,
    request: Request,
    timeout: Duration,
) -> Result<Response, couch_plugin::Failure> {
    crate::tv::plugin::ask_within(connection, request, timeout)
}

/// A row that is one child of a packaged connection: which child to name, and
/// what this particular one can do while its package is not running.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginRow {
    connection: String,
    resource: String,
    /// Drawn and driven as a blind rather than as a lamp.
    cover: bool,
    snapshot: ChildSnapshot,
}
impl PluginRow {
    fn id(&self) -> String {
        format!("{PLUGIN_PREFIX}{}/{}", self.connection, self.resource)
    }
    /// What one status reading says about this child, in the shape the rows
    /// already use. A reading that carries nothing about this child is no
    /// reading at all: the row says "Unavailable" rather than inventing an
    /// off state for it.
    fn state(&self, name: &str, status: &couch_plugin::Status) -> Option<DeviceState> {
        if self.cover {
            let cover = status.cover.as_ref()?;
            Some(DeviceState::Cover(cover_row(
                name,
                self.snapshot.cover.as_ref(),
                cover,
            )))
        } else {
            let light = status.light.as_ref()?;
            Some(DeviceState::Light(light_row(
                name,
                self.snapshot.light.as_ref(),
                light,
            )))
        }
    }
    /// The typed action a level on this row means.
    fn level(&self, percent: u8) -> TypedAction {
        if self.cover {
            TypedAction::SetCover { position: percent }
        } else {
            TypedAction::SetLight {
                on: None,
                brightness: Some(percent),
                mirek: None,
                xy: None,
            }
        }
    }
}

/// A package's lamp, as `couch_ha::Light`: the shape `brightness_step`,
/// `description` and the row already speak. Nothing is inferred - a lamp that
/// has not said whether it is on stays unknown, and the level it would return
/// to is kept while it is off, as the domain type has it.
fn light_row(name: &str, traits: Option<&LightTraits>, state: &LightState) -> Light {
    // A range the child never declared is no range: a reading of its own
    // colour temperature is ignored rather than tuned against nothing.
    let mirek_range = traits.and_then(|traits| traits.mirek);
    Light {
        entity_id: String::new(),
        name: name.to_owned(),
        on: state.on,
        brightness_percent: state.brightness,
        dimmable: traits.is_some_and(|traits| traits.dimmable),
        mirek: mirek_range
            .and_then(|(cool, warm)| state.mirek.filter(|mirek| (cool..=warm).contains(mirek))),
        mirek_range,
    }
}

/// A package's blind, as `couch_ha::Cover`. A child reports where it is, not
/// which way it is travelling, so a blind in motion reads as its last known
/// end state until it settles.
fn cover_row(name: &str, traits: Option<&CoverTraits>, state: &CoverState) -> Cover {
    Cover {
        entity_id: String::new(),
        name: name.to_owned(),
        state: state
            .open
            .map(|open| if open { "open" } else { "closed" }.to_owned()),
        position_percent: state.position,
        can_open: true,
        can_close: true,
        can_set_position: traits.is_some_and(|traits| traits.position),
        can_stop: traits.is_some_and(|traits| traits.stop),
    }
}

/// Which built-in control a packaged child is drawn with.
///
/// The connection's declared kind decides. A kind the connection no longer
/// lists (a rollback that has not healed yet) falls back on what the device
/// saved about itself, so a lamp keeps its row instead of turning into a
/// device with a screen; a child that saved nothing at all is treated as a
/// light, which shows "Unavailable" and opens nothing.
fn child_component(
    config: &couch_model::Config,
    integration: &Integration,
    child: &ChildSnapshot,
) -> ChildComponent {
    if let Some(kind) = config.device_child_kind(integration) {
        return kind.component;
    }
    if child.cover.is_some() {
        ChildComponent::Cover
    } else if child.climate.is_some() {
        ChildComponent::Climate
    } else {
        ChildComponent::Light
    }
}

/// The room row a packaged child is drawn as, for a light or a blind. A
/// thermostat child is not one: it keeps its `device:` row and the packaged
/// control screen until Home Assistant is packaged.
pub(crate) fn plugin_row(
    config: &couch_model::Config,
    device: &couch_model::Device,
) -> Option<PluginRow> {
    let resolved = config.resolve_integration(&device.integration)?;
    let Integration::Plugin {
        connection_id,
        resource_id,
        child: Some(child),
        ..
    } = &resolved
    else {
        return None;
    };
    let cover = match child_component(config, &device.integration, child) {
        ChildComponent::Light => false,
        ChildComponent::Cover => true,
        ChildComponent::Climate | ChildComponent::Scene => return None,
    };
    Some(PluginRow {
        connection: connection_id.to_string(),
        resource: resource_id.clone(),
        cover,
        snapshot: child.clone(),
    })
}
/// What a row is before any reading: a packaged child declared it, a Home
/// Assistant row has its domain, and everything else is drawn as a lamp until
/// its reading says otherwise. Only the screen uses it, and only while the
/// device has not answered.
fn declared(entry: &Entry) -> crate::light_screen::Declared {
    use crate::light_screen::Declared;
    if let Some(row) = &entry.plugin {
        return Declared {
            cover: row.cover,
            can_stop: row.snapshot.cover.is_some_and(|traits| traits.stop),
            dimmable: row.snapshot.light.is_some_and(|traits| traits.dimmable),
            mirek: row.snapshot.light.and_then(|traits| traits.mirek),
        };
    }
    Declared {
        cover: ha_domain(&entry.id) == "cover",
        ..Declared::default()
    }
}
/// Short-lived observations speed up navigation, never authorize commands.
#[derive(Default)]
struct StateCache(HashMap<String, (Instant, DeviceState)>);
impl StateCache {
    fn get(&self, id: &str) -> Option<DeviceState> {
        self.0
            .get(id)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(5))
            .map(|(_, s)| s.clone())
    }
    fn put(&mut self, state: DeviceState) {
        self.0
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(5));
        self.0
            .insert(state.id().to_owned(), (Instant::now(), state));
    }
}
#[derive(Clone)]
struct Entry {
    icon: couch_model::Icon,
    name: String,
    id: String,
    state: Option<DeviceState>,
    hue: bool,
    matter: bool,
    /// One child of a packaged connection, driven over the panel socket.
    plugin: Option<PluginRow>,
    /// A Sonos speaker: the physical keys drive it from this list.
    media: bool,
    /// An activity pinned above the devices: its glyph index and the
    /// "source · room" caption the area page's strip shows for it.
    activity: Option<(i32, String)>,
    /// This round's status read named this row `Unpaired`: the panel does no
    /// pairing of its own, so the row's detail says so instead of the
    /// generic "Unavailable" a fresh read leaves for a child the read simply
    /// has not answered yet.
    unpaired: bool,
}
enum Operation {
    IrCheck(Id, String, bool, Arc<couch_model::Config>, Input),
    List,
    Toggle(String),
    Brightness(String, u8),
    /// Colour temperature, in mirek, from the light's own control screen.
    Mirek(String, u16),
    /// A blind's own button: "open", "close" or "stop".
    Cover(String, &'static str),
}
enum Answer {
    IrChecked(bool, Input),
    List(Vec<Entry>),
    State(DeviceState),
    /// The daemon was busy with this connection (a browser listing its
    /// children, say). Nothing was sent; the level goes back on the queue and
    /// the user is told nothing, because nothing is wrong.
    Busy,
}
enum Input {
    PhysicalPick(usize, bool),
    PhysicalLevel(usize, i32, bool),
    /// The Power key on a highlighted row. It switches the row whatever else
    /// is going on, so it runs through the same IR check OK does.
    PhysicalPower(usize, bool),
    Open(Id),
    Pick(usize),
    Power(usize),
    Brightness(usize, i32),
    Back,
    /// Something the open control screen asked for: the name the screen sends
    /// and its index (light.slint).
    Screen(String, i32),
}
pub struct Controller {
    input: Rc<RefCell<VecDeque<Input>>>,
    ir_pending: VecDeque<Operation>,
    physical_repeat: Rc<std::cell::Cell<bool>>,
    active: Arc<std::sync::atomic::AtomicU64>,
    tx: mpsc::SyncSender<(u64, Id, Operation)>,
    rx: mpsc::Receiver<(u64, Result<Answer, String>)>,
    generation: u64,
    room: Option<Id>,
    entries: Vec<Entry>,
    cache: StateCache,
    hue: Arc<crate::connections::HueFleet>,
    busy: Option<String>,
    /// When the operation `busy` names went out. The control screen says
    /// nothing while a write is answered promptly - the bar and the read-out
    /// have already moved - and only calls it "Updating…" once one has been
    /// out for `UPDATING_AFTER`.
    busy_since: Option<Instant>,
    refreshing: bool,
    last_refresh: Instant,
    brightness_pending: VecDeque<(String, u8)>,
    position_targets: HashMap<String, (Instant, u8)>,
    brightness_flight: Option<(String, u8)>,
    brightness_until: Option<Instant>,
    last_brightness_send: Instant,
    /// Colour temperature, queued and in flight exactly as a level is: only
    /// the latest target per row is ever sent.
    mirek_pending: VecDeque<(String, u16)>,
    mirek_flight: Option<(String, u16)>,
    /// The row whose control screen is open, and which of a blind's three
    /// buttons is highlighted there. Neither bar is ever highlighted: each has
    /// a key of its own, so there is nothing to select between.
    screen: Option<String>,
    screen_button: i32,
    /// What the screen was last drawn from, so a poll that changed nothing
    /// does not touch a property (docs/slint-notes.md). The blind's highlight
    /// and the "Updating…" flag are part of it: neither is in the view, and a
    /// press that moved only one of them used to compare equal here and never
    /// reach the panel.
    last_screen: Option<(crate::light_screen::View, i32, bool)>,
    /// A sentence for the main loop's toast, raised by a row press.
    notice: Option<String>,
}
/// What a row says, and OK on it toasts, when its device's built-in client has
/// left the OS and the connection has not been handed to the package yet: the
/// sentence the device's volume and power keys give too (`activity_buttons`).
pub(crate) fn needs_package(config: &couch_model::Config, device_id: &str) -> Option<String> {
    let (_, device) = config.devices().find(|(_, d)| d.id.as_str() == device_id)?;
    let row = config
        .resolve_integration(&device.integration)?
        .legacy_builtin()?;
    Some(row.needs_package())
}
/// The second line of a device row that opens a screen.
fn device_row_detail(config: Option<&couch_model::Config>, entry_id: &str) -> String {
    config
        .and_then(|c| needs_package(c, entry_id.trim_start_matches("device:")))
        .unwrap_or_else(|| "Press OK for controls".into())
}
fn configured(room: &Id) -> Result<Vec<Entry>, String> {
    let config = crate::connections::config().ok_or("Cannot read your rooms")?;
    configured_in(&config, room)
}
/// The activity rows pinned above the devices in a room's list, the way the
/// area page keeps its activity strip above its rooms.
pub(crate) fn activity_rows(config: &couch_model::Config, room: &Id) -> usize {
    config.activities.iter().filter(|a| &a.room == room).count()
}
/// The configured device on a row of the room list, if the row is a device:
/// rows follow the room's own device order after the pinned activities.
pub(crate) fn device_at<'a>(
    config: &'a couch_model::Config,
    room: &Id,
    row: usize,
) -> Option<&'a couch_model::Device> {
    let index = row.checked_sub(activity_rows(config, room))?;
    config.room(room)?.devices.get(index)
}
/// The row a device sits on in its room's list.
pub(crate) fn row_of_device(config: &couch_model::Config, room: &Id, device: &Id) -> Option<usize> {
    let index = config
        .room(room)?
        .devices
        .iter()
        .position(|d| &d.id == device)?;
    Some(activity_rows(config, room) + index)
}
/// The caption the area page's activity strip shows: the source device and
/// the room, joined the way home.rs joins them.
fn activity_caption(config: &couch_model::Config, activity: &couch_model::Activity) -> String {
    let source = activity
        .source
        .as_ref()
        .and_then(|id| {
            config
                .devices()
                .find(|(_, d)| &d.id == id)
                .map(|(_, d)| d.name.as_str())
        })
        .unwrap_or("Choose a source");
    let place = config
        .room(&activity.room)
        .map(|r| r.name.as_str())
        .unwrap_or("");
    format!("{source} · {place}")
}
fn configured_in(config: &couch_model::Config, room: &Id) -> Result<Vec<Entry>, String> {
    let room = config
        .room(room)
        .ok_or("This room was removed; return home to reload")?;
    let mut entries: Vec<Entry> = config
        .activities
        .iter()
        .filter(|a| a.room == room.id)
        .map(|a| Entry {
            icon: couch_model::Icon::Tv,
            name: a.name.clone(),
            id: format!("activity:{}", a.id),
            state: None,
            hue: false,
            matter: false,
            plugin: None,
            media: false,
            activity: Some((a.kind.glyph_index(), activity_caption(config, a))),
            unpaired: false,
        })
        .collect();
    entries.extend(room.devices.iter().filter_map(|d| {
        let integration = config.resolve_integration(&d.integration);
        // A light or a blind behind a package is a room row like any other:
        // the same slider, the same optimistic level, the same description.
        // Only where its readings come from is different.
        if let Some(row) = plugin_row(config, d) {
            return Some(Entry {
                name: d.name.clone(),
                icon: d.effective_icon(),
                id: row.id(),
                state: None,
                hue: false,
                matter: false,
                plugin: Some(row),
                media: false,
                activity: None,
                unpaired: false,
            });
        }
        match integration.as_ref() {
            Some(Integration::HomeAssistant { entity_id })
                if matches!(ha_domain(entity_id), "light" | "cover" | "climate") =>
            {
                Some(Entry {
                    name: d.name.clone(),
                    icon: d.effective_icon(),
                    id: entity_id.clone(),
                    state: None,
                    hue: false,
                    matter: false,
                    plugin: None,
                    media: false,
                    activity: None,
                    unpaired: false,
                })
            }
            Some(Integration::Hue { light_id }) if !light_id.starts_with("scene:") => Some(Entry {
                name: d.name.clone(),
                icon: d.effective_icon(),
                id: format!("hue:{light_id}"),
                state: None,
                hue: true,
                matter: false,
                plugin: None,
                media: false,
                activity: None,
                unpaired: false,
            }),
            Some(Integration::Matter { device }) => Some(Entry {
                name: d.name.clone(),
                icon: d.effective_icon(),
                id: format!("matter:{device}"),
                state: None,
                hue: false,
                matter: true,
                plugin: None,
                media: false,
                activity: None,
                unpaired: false,
            }),
            _ => Some(Entry {
                name: d.name.clone(),
                icon: d.effective_icon(),
                id: format!("device:{}", d.id),
                state: None,
                hue: false,
                matter: false,
                plugin: None,
                media: matches!(integration, Some(Integration::Sonos { .. })),
                activity: None,
                unpaired: false,
            }),
        }
    }));
    Ok(entries)
}
fn toggle_command(state: &Light) -> Result<Command, String> {
    match state.on {
        Some(true) => Ok(Command::Off),
        Some(false) => Ok(Command::On),
        None => Err("This light is unavailable".into()),
    }
}

/// What OK on a packaged row sends: the child's own `toggle`, which the
/// package decides the meaning of. Unlike Home Assistant, nothing is read
/// first: a child that cannot toggle refuses the command, and a refusal is
/// better than a guess made from a stale reading.
fn toggle_request(row: &PluginRow) -> Request {
    Request::command("toggle").at(&row.resource)
}

/// Switch one packaged row from a shortcut key: the same request OK on the row
/// sends. `None` means the connection was busy - the press was not lost, there
/// is simply nothing to say about it yet.
pub(crate) fn plugin_toggle(
    row: &PluginRow,
    name: &str,
    ask: Ask,
) -> Result<Option<DeviceState>, String> {
    match write_plugin_row(row, name, toggle_request(row), ask)? {
        Answer::State(state) => Ok(Some(state)),
        _ => Ok(None),
    }
}

/// One status read for every packaged row in the room, in order, on this one
/// worker.
///
/// A connection that fails to answer takes the rest of its rows with it for
/// this round: waiting 1.5 s per row on a bridge that is not there would hold
/// the list up for as long as the room is large. Those rows keep their place
/// and say "Unavailable"; the next round asks again. A child the package no
/// longer knows refuses the read and is shown the same way - never removed,
/// because the device is still configured.
fn read_plugin_rows(entries: &mut [Entry], ask: Ask) {
    let mut silent: Vec<String> = Vec::new();
    for e in entries.iter_mut() {
        let Some(row) = e.plugin.clone() else {
            continue;
        };
        if silent.contains(&row.connection) {
            continue;
        }
        match ask(
            &row.connection,
            Request::status().at(&row.resource),
            STATUS_TIMEOUT,
        ) {
            Ok(Response::Status { status }) => {
                e.state = row.state(&e.name, &status).map(|mut state| {
                    state.set_id(e.id.clone());
                    state
                });
            }
            Ok(_) => {}
            Err(failure) => {
                if matches!(
                    failure.code,
                    couch_plugin::Error::Transport | couch_plugin::Error::Timeout
                ) {
                    silent.push(row.connection.clone());
                }
                // The panel does no pairing of its own: the row says so
                // instead of the generic "Unavailable" a read that simply
                // has not answered yet leaves.
                e.unpaired = failure.code == couch_plugin::Error::Unpaired;
            }
        }
    }
}

/// A command or a typed action on one packaged row, and the state it left the
/// child in.
///
/// A write may be acknowledged with that state, which saves a round trip and
/// cannot disagree with what was just written. A package that answers a bare
/// `Ok` is asked once, straight after.
fn write_plugin_row(
    row: &PluginRow,
    name: &str,
    request: Request,
    ask: Ask,
) -> Result<Answer, String> {
    let answered = |status: &couch_plugin::Status| {
        row.state(name, status)
            .map(|mut state| {
                state.set_id(row.id());
                Answer::State(state)
            })
            .ok_or_else(|| "The integration did not report this device".to_string())
    };
    match ask(&row.connection, request, write_timeout()) {
        Ok(Response::Status { status }) => answered(&status),
        Ok(Response::Ok) => match ask(
            &row.connection,
            Request::status().at(&row.resource),
            STATUS_TIMEOUT,
        ) {
            Ok(Response::Status { status }) => answered(&status),
            Ok(_) => Err("The integration returned an invalid status".into()),
            Err(failure) if failure.code == couch_plugin::Error::Busy => Ok(Answer::Busy),
            Err(failure) => Err(crate::tv::plugin::refusal(&failure)),
        },
        Ok(_) => Err("The integration returned an invalid response".into()),
        Err(failure) if failure.code == couch_plugin::Error::Busy => Ok(Answer::Busy),
        Err(failure) => Err(crate::tv::plugin::refusal(&failure)),
    }
}
fn perform(
    room: &Id,
    operation: Operation,
    hue: &crate::connections::HueFleet,
    matter: &crate::connections::MatterFleet,
    ask: Ask,
    current: &dyn Fn() -> bool,
) -> Result<Answer, String> {
    let mut entries = configured(room)?;
    match operation {
        Operation::IrCheck(device, function, repeat, config, resume) => {
            if !config
                .room(room)
                .is_some_and(|r| r.devices.iter().any(|d| d.id == device))
            {
                return Err("Device was removed from room".into());
            }
            let live = || {
                current() && crate::connections::config().is_some_and(|c| Arc::ptr_eq(&c, &config))
            };
            let handled = crate::activity_buttons::try_device_ir(
                &config,
                device.as_str(),
                &couch_model::commands::Function::parse(&function)
                    .ok_or("Unsupported IR function")?,
                repeat,
                &live,
            )?;
            Ok(Answer::IrChecked(handled, resume))
        }
        Operation::List => {
            let ha_states = if entries
                .iter()
                .any(|e| !e.hue && !e.matter && e.plugin.is_none() && !e.id.starts_with("device:"))
            {
                crate::connections::ha_room_states()
            } else {
                Vec::new()
            };
            let hue_states = if entries.iter().any(|e| e.hue) {
                hue.lights()
                    .unwrap_or_default()
                    .into_iter()
                    .map(DeviceState::Light)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let matter_states = if entries.iter().any(|e| e.matter) {
                matter
                    .lights()
                    .into_iter()
                    .map(DeviceState::Light)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for e in &mut entries {
                if e.plugin.is_some() {
                    continue;
                }
                let states = if e.hue {
                    &hue_states
                } else if e.matter {
                    &matter_states
                } else {
                    &ha_states
                };
                let id =
                    e.id.strip_prefix("hue:")
                        .or_else(|| e.id.strip_prefix("matter:"))
                        .unwrap_or(&e.id);
                e.state = states.iter().find(|s| s.id() == id).cloned().map(|mut s| {
                    s.set_id(e.id.clone());
                    s
                });
            }
            read_plugin_rows(&mut entries, ask);
            Ok(Answer::List(entries))
        }
        Operation::Brightness(id, percent) => {
            let Some(entry) = entries.iter().find(|e| e.id == id) else {
                return Err("This device was removed from the room".into());
            };
            if let Some(row) = entry.plugin.clone() {
                let name = entry.name.clone();
                let action = Request::action(row.level(percent)).at(&row.resource);
                return write_plugin_row(&row, &name, action, ask);
            }
            let mut state = if let Some(raw) = id.strip_prefix("hue:") {
                DeviceState::Light(hue.brightness(raw, percent).map_err(|e| e.to_string())?)
            } else if let Some(raw) = id.strip_prefix("matter:") {
                DeviceState::Light(matter.brightness(raw, percent)?)
            } else if id.starts_with("device:") {
                return Err("This device does not support brightness".into());
            } else {
                let (c, raw) = crate::connections::ha(&id)?;
                if ha_domain(&id) == "cover" {
                    c.cover_command(&raw, CoverCommand::Position(percent))
                        .map_err(|e| e.to_string())?;
                    DeviceState::Cover(c.cover(&raw).map_err(|e| e.to_string())?)
                } else {
                    c.command(&raw, Command::Brightness(percent))
                        .map_err(|e| e.to_string())?;
                    DeviceState::Light(c.light(&raw).map_err(|e| e.to_string())?)
                }
            };
            state.set_id(id);
            Ok(Answer::State(state))
        }
        Operation::Toggle(id) => {
            let Some(entry) = entries.iter().find(|e| e.id == id) else {
                return Err("This device was removed from the room".into());
            };
            if let Some(row) = entry.plugin.clone() {
                let name = entry.name.clone();
                return write_plugin_row(&row, &name, toggle_request(&row), ask);
            }
            // Hue uses its push-maintained cache; HA still reads before toggling.
            let mut state = if let Some(raw) = id.strip_prefix("hue:") {
                let started = Instant::now();
                let result = hue.toggle(raw).map_err(|e| e.to_string());
                println!(
                    "couch-gui: Hue toggle acknowledged in {} ms (success={})",
                    started.elapsed().as_millis(),
                    result.is_ok()
                );
                DeviceState::Light(result?)
            } else if let Some(raw) = id.strip_prefix("matter:") {
                // Matter reads before toggling, like Home Assistant: no push cache yet.
                DeviceState::Light(matter.toggle(raw)?)
            } else if id.starts_with("device:") {
                return Err("Controls for this device are not available yet".into());
            } else {
                let (c, raw) = crate::connections::ha(&id)?;
                if ha_domain(&id) == "cover" {
                    c.cover_command(&raw, CoverCommand::Toggle)
                        .map_err(|e| e.to_string())?;
                    DeviceState::Cover(c.cover(&raw).map_err(|e| e.to_string())?)
                } else {
                    let state = c.light(&raw).map_err(|e| e.to_string())?;
                    c.command(&raw, toggle_command(&state)?)
                        .map_err(|e| e.to_string())?;
                    DeviceState::Light(c.light(&raw).map_err(|e| e.to_string())?)
                }
            };
            state.set_id(id);
            Ok(Answer::State(state))
        }
        Operation::Mirek(id, mirek) => {
            let Some(entry) = entries.iter().find(|e| e.id == id) else {
                return Err("This device was removed from the room".into());
            };
            if let Some(row) = entry.plugin.clone() {
                let name = entry.name.clone();
                let action = Request::action(TypedAction::SetLight {
                    on: None,
                    brightness: None,
                    mirek: Some(mirek),
                    xy: None,
                })
                .at(&row.resource);
                return write_plugin_row(&row, &name, action, ask);
            }
            let mut state = if let Some(raw) = id.strip_prefix("hue:") {
                DeviceState::Light(hue.mirek(raw, mirek)?)
            } else if id.starts_with("matter:") || id.starts_with("device:") {
                // The Matter controller reads and writes on/off and level;
                // nothing here can colour a Matter lamp yet.
                return Err("This light does not support colour temperature".into());
            } else {
                let (c, raw) = crate::connections::ha(&id)?;
                c.command(&raw, Command::Mirek(mirek))
                    .map_err(|e| e.to_string())?;
                DeviceState::Light(c.light(&raw).map_err(|e| e.to_string())?)
            };
            state.set_id(id);
            Ok(Answer::State(state))
        }
        Operation::Cover(id, command) => {
            let Some(entry) = entries.iter().find(|e| e.id == id) else {
                return Err("This device was removed from the room".into());
            };
            if let Some(row) = entry.plugin.clone() {
                let name = entry.name.clone();
                // A packaged blind's kind declares open, close and stop as
                // capabilities, so each is the child's own command.
                let request = Request::command(command).at(&row.resource);
                return write_plugin_row(&row, &name, request, ask);
            }
            if id.starts_with("hue:") || id.starts_with("matter:") || id.starts_with("device:") {
                return Err("This device does not open and close".into());
            }
            let (c, raw) = crate::connections::ha(&id)?;
            c.cover_command(
                &raw,
                match command {
                    "open" => CoverCommand::Open,
                    "close" => CoverCommand::Close,
                    _ => CoverCommand::Stop,
                },
            )
            .map_err(|e| e.to_string())?;
            let mut state = DeviceState::Cover(c.cover(&raw).map_err(|e| e.to_string())?);
            state.set_id(id);
            Ok(Answer::State(state))
        }
    }
}

impl Controller {
    pub fn install(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let physical_repeat = Rc::new(std::cell::Cell::new(false));
        let repeat = physical_repeat.clone();
        let q = input.clone();
        app.on_light_activate(move |i| {
            if i >= 0 {
                q.borrow_mut()
                    .push_back(Input::PhysicalPick(i as usize, repeat.get()));
            }
        });
        let q = input.clone();
        let repeat = physical_repeat.clone();
        app.on_light_brightness(move |i, delta| {
            if i >= 0 {
                q.borrow_mut()
                    .push_back(Input::PhysicalLevel(i as usize, delta, repeat.get()));
            }
        });
        let q = input.clone();
        app.on_light_back(move || q.borrow_mut().push_back(Input::Back));
        let q = input.clone();
        app.on_light_screen_action(move |name, index| {
            q.borrow_mut()
                .push_back(Input::Screen(name.to_string(), index))
        });
        let (tx, requests) = mpsc::sync_channel::<(u64, Id, Operation)>(1);
        let (events, rx) = mpsc::channel();
        let hue = Arc::new(crate::connections::HueFleet::default());
        let worker_hue = hue.clone();
        // Matter has no push cache and nothing the GUI thread asks for directly.
        // The fleet is shared with the shortcut keys and mapped buttons so one
        // node never carries two CASE sessions; it opens fabrics on first use.
        let worker_matter = crate::connections::matter();
        let active = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_active = active.clone();
        std::thread::spawn(move || {
            let _ = worker_hue.lights(); // Warm the cache without delaying GUI startup.
            while let Ok((generation, room, op)) = requests.recv() {
                let _ = events.send((
                    generation,
                    perform(
                        &room,
                        op,
                        &worker_hue,
                        &worker_matter,
                        &mut plugin_socket,
                        &|| worker_active.load(std::sync::atomic::Ordering::SeqCst) == generation,
                    ),
                ));
            }
        });
        Self {
            ir_pending: VecDeque::new(),
            physical_repeat,
            active,
            input,
            tx,
            rx,
            generation: 0,
            room: None,
            entries: Vec::new(),
            cache: StateCache::default(),
            hue,
            busy: None,
            busy_since: None,
            refreshing: false,
            last_refresh: Instant::now(),
            brightness_pending: VecDeque::new(),
            position_targets: HashMap::new(),
            brightness_flight: None,
            brightness_until: None,
            last_brightness_send: Instant::now() - Duration::from_secs(1),
            mirek_pending: VecDeque::new(),
            mirek_flight: None,
            screen: None,
            screen_button: 0,
            last_screen: None,
            notice: None,
        }
    }
    pub fn hue_live(&self) -> Arc<crate::connections::HueFleet> {
        self.hue.clone()
    }
    pub fn wake(&mut self) {
        self.hue.reset();
        self.cache.0.retain(|id, _| !id.starts_with("hue:"));
        self.last_refresh = Instant::now() - Duration::from_secs(5);
    }
    pub fn opener(&self) -> impl Fn(Id) + 'static {
        let input = self.input.clone();
        move |room| input.borrow_mut().push_back(Input::Open(room))
    }
    fn row(&self, e: &Entry) -> ChoiceItem {
        let config = crate::connections::config();
        let detail = if !e.hue && self.busy.as_deref() == Some(&e.id) {
            "Updating…".into()
        } else if let Some((_, caption)) = &e.activity {
            caption.clone()
        } else if e.id.starts_with("device:") {
            device_row_detail(config.as_deref(), &e.id)
        } else {
            e.state
                .as_ref()
                .map(DeviceState::description)
                .unwrap_or_else(|| {
                    if e.unpaired {
                        "Needs pairing".into()
                    } else if self.refreshing {
                        "Checking status…".into()
                    } else {
                        "Unavailable".into()
                    }
                })
        };
        ChoiceItem {
            icon: crate::icons::image(e.icon),
            title: e.name.clone().into(),
            detail: detail.into(),
            light: !e.id.starts_with("device:") && e.activity.is_none(),
            media: e.media,
            active: e.state.as_ref().is_some_and(|s| s.active() == Some(true)),
            power_known: e.state.as_ref().is_some_and(|s| s.active().is_some()),
            activity: e.activity.is_some(),
            kind: e.activity.as_ref().map_or(0, |(kind, _)| *kind),
            controls: row_opens_a_screen(config.as_deref(), e),
        }
    }
    fn update_rows(&self, app: &App, reset: bool) {
        if !reset {
            let model = app.get_light_items();
            if let Some(rows) = model.as_any().downcast_ref::<VecModel<ChoiceItem>>() {
                if rows.row_count() == self.entries.len() {
                    for (i, e) in self.entries.iter().enumerate() {
                        rows.set_row_data(i, self.row(e));
                    }
                    return;
                }
            }
        }
        app.set_light_items(ModelRc::new(VecModel::from(
            self.entries.iter().map(|e| self.row(e)).collect::<Vec<_>>(),
        )));
    }
    fn refresh(&mut self) {
        let Some(room) = self.room.clone() else {
            return;
        };
        self.last_refresh = Instant::now();
        self.refreshing = self
            .tx
            .try_send((self.generation, room, Operation::List))
            .is_ok();
    }
    pub fn clear_brightness(&mut self, app: &App) {
        self.brightness_pending.clear();
        self.mirek_pending.clear();
        self.ir_pending.clear();
        self.position_targets.clear();
        self.brightness_flight = None;
        self.mirek_flight = None;
        self.brightness_until = None;
        app.set_brightness_shown(false);
    }
    pub fn physical_input(&self, repeat: bool, dispatch: impl FnOnce()) {
        let previous = self.physical_repeat.replace(repeat);
        dispatch();
        self.physical_repeat.set(previous);
    }
    fn intercept_ir(&mut self, i: usize, function: &str, repeat: bool, resume: Input) {
        let device = self.room.as_ref().and_then(|room| {
            crate::connections::config().and_then(|c| {
                device_at(&c, room, i)
                    .filter(|d| d.effective_ir_codeset(&c).is_some())
                    .map(|d| (d.id.clone(), c.clone()))
            })
        });
        if let Some((device, config)) = device {
            if self.ir_pending.len() < 32 {
                self.ir_pending.push_back(Operation::IrCheck(
                    device,
                    function.into(),
                    repeat,
                    config,
                    resume,
                ));
            }
        } else {
            self.input.borrow_mut().push_front(resume);
        }
    }
    fn send_ir(&mut self) {
        if !self.input.borrow().is_empty() || self.busy.is_some() || self.refreshing {
            return;
        }
        let Some(room) = self.room.clone() else {
            return;
        };
        let Some(operation) = self.ir_pending.pop_front() else {
            return;
        };
        let next = self.generation + 1;
        self.active.store(next, std::sync::atomic::Ordering::SeqCst);
        match self.tx.try_send((next, room, operation)) {
            Ok(()) => {
                self.generation = next;
                self.claim("ir-command".into());
            }
            Err(error) => {
                self.active
                    .store(self.generation, std::sync::atomic::Ordering::SeqCst);
                let (mpsc::TrySendError::Full(work) | mpsc::TrySendError::Disconnected(work)) =
                    error;
                self.ir_pending.push_front(work.2);
            }
        }
    }
    fn device_resource(&self, i: usize) -> Option<String> {
        self.room.as_ref().and_then(|room| {
            crate::connections::config()
                .and_then(|c| device_at(&c, room, i).map(|d| format!("device:{}", d.id)))
        })
    }
    fn adjust_brightness(&mut self, app: &App, i: usize, delta: i32) {
        let Some(entry) = self.entries.get(i) else {
            return;
        };
        if ha_domain(&entry.id) == "climate" {
            app.invoke_thermostat_room_adjust(
                self.device_resource(i)
                    .unwrap_or_else(|| entry.id.clone())
                    .into(),
                entry.name.as_str().into(),
                delta.signum(),
            );
            return;
        }
        let Some(state) = &entry.state else {
            app.set_light_detail("Checking this device’s status. Try again in a moment.".into());
            return;
        };
        let target = self.pending_level(&entry.id);
        let result = match state {
            DeviceState::Light(s) => brightness_step(s, target, delta),
            DeviceState::Cover(s) => cover_step(s, target, delta),
            DeviceState::Climate(_) => return,
        };
        match result {
            Ok(percent) => {
                queue_level(&mut self.brightness_pending, entry.id.clone(), percent);
                if matches!(state, DeviceState::Cover(_)) {
                    self.position_targets
                        .insert(entry.id.clone(), (Instant::now(), percent));
                }
                // The control screen already shows the level in full, so the
                // card that stands in for it over the room list stays down.
                if self.screen.is_none() {
                    app.set_brightness_target(entry.name.clone().into());
                    app.set_brightness_label(
                        if matches!(state, DeviceState::Cover(_)) {
                            "Open position"
                        } else {
                            "Brightness"
                        }
                        .into(),
                    );
                    app.set_light_brightness_percent(percent as i32);
                    app.set_feedback_enabled(true);
                    app.set_brightness_shown(true);
                    self.brightness_until = Some(Instant::now() + Duration::from_secs(1));
                }
                app.set_light_detail("".into());
                app.set_light_screen_detail("".into());
            }
            Err(error) => {
                app.set_light_detail(error.into());
                app.set_light_screen_detail(error.into());
            }
        }
    }
    /// Take the one operation slot, and remember when it went out.
    fn claim(&mut self, id: String) {
        self.busy = Some(id);
        self.busy_since = Some(Instant::now());
    }
    /// Give it back: whatever was out has answered, or the room has changed
    /// under it.
    fn release(&mut self) {
        self.busy = None;
        self.busy_since = None;
    }
    /// The next queued adjustment: a level first, then a colour temperature.
    /// One goes out at a time, and only the latest target per row is ever in
    /// the queue, so a held key never builds a backlog of commands.
    fn send_brightness(&mut self) {
        if self.busy.is_some()
            || self.refreshing
            || self.last_brightness_send.elapsed() < Duration::from_millis(100)
        {
            return;
        }
        let Some(room) = self.room.clone() else {
            return;
        };
        let level = self.brightness_pending.front().cloned();
        let colour = self.mirek_pending.front().cloned();
        let operation = match (&level, &colour) {
            (Some((id, percent)), _) => Operation::Brightness(id.clone(), *percent),
            (None, Some((id, mirek))) => Operation::Mirek(id.clone(), *mirek),
            (None, None) => return,
        };
        let id = match (&level, &colour) {
            (Some((id, _)), _) | (None, Some((id, _))) => id.clone(),
            (None, None) => return,
        };
        let generation = self.generation + 1;
        if self.tx.try_send((generation, room, operation)).is_ok() {
            self.generation = generation;
            self.active
                .store(self.generation, std::sync::atomic::Ordering::SeqCst);
            self.claim(id);
            if let Some(level) = level {
                self.brightness_flight = Some(level);
                self.brightness_pending.pop_front();
            } else if let Some(colour) = colour {
                self.mirek_flight = Some(colour);
                self.mirek_pending.pop_front();
            }
            self.last_brightness_send = Instant::now();
        }
    }
    /// Switch one row: what OK does on a row with nothing but on and off
    /// behind it, and what the Power key does on any of them.
    fn toggle_row(&mut self, app: &App, id: String) {
        self.position_targets.remove(&id);
        let generation = self.generation + 1;
        if self
            .tx
            .try_send((
                generation,
                self.room.clone().unwrap(),
                Operation::Toggle(id.clone()),
            ))
            .is_ok()
        {
            self.generation = generation;
            self.active
                .store(self.generation, std::sync::atomic::Ordering::SeqCst);
            self.claim(id);
            self.refreshing = false;
            app.set_light_detail("".into());
            self.update_rows(app, false);
        } else {
            app.set_light_detail("Connection busy. Press OK again in a moment.".into());
        }
    }
    /// The level a row is being driven towards, or its last endpoint while a
    /// blind travels: what the next press steps from, and what the screen
    /// shows before the device has answered.
    fn pending_level(&self, id: &str) -> Option<u8> {
        self.brightness_pending
            .iter()
            .find(|(pending, _)| pending == id)
            .or_else(|| self.brightness_flight.as_ref().filter(|(p, _)| p == id))
            .map(|(_, percent)| *percent)
            .or_else(|| {
                // Cover reports describe physical motion, not the requested
                // endpoint. Keep quick presses relative to our last endpoint
                // while it travels.
                self.position_targets
                    .get(id)
                    .filter(|(at, _)| at.elapsed() < Duration::from_secs(10))
                    .map(|(_, target)| *target)
            })
    }
    /// The same, for colour temperature.
    fn pending_mirek(&self, id: &str) -> Option<u16> {
        self.mirek_pending
            .iter()
            .find(|(pending, _)| pending == id)
            .or_else(|| self.mirek_flight.as_ref().filter(|(p, _)| p == id))
            .map(|(_, mirek)| *mirek)
    }
    /// Where a row's readings come from, for the screen's second line.
    fn source_line(&self, entry: &Entry) -> String {
        let config = crate::connections::config();
        let room = self
            .room
            .as_ref()
            .and_then(|room| config.as_ref().and_then(|c| c.room(room)))
            .map(|room| room.name.clone())
            .unwrap_or_default();
        let source = if entry.hue {
            "Philips Hue".to_string()
        } else if entry.matter {
            "Matter".to_string()
        } else if let Some(row) = &entry.plugin {
            config
                .as_ref()
                .and_then(|c| {
                    c.connections
                        .iter()
                        .find(|c| c.id.as_str() == row.connection)
                })
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "Integration".into())
        } else {
            "Home Assistant".to_string()
        };
        if room.is_empty() {
            source
        } else {
            format!("{room} · {source}")
        }
    }
    /// Whether the open screen is a blind: one bar with three buttons under
    /// it, rather than one or two bars.
    fn screen_cover(&self, entry: &Entry) -> bool {
        matches!(entry.state, Some(DeviceState::Cover(_))) || declared(entry).cover
    }
    /// Whether the open screen has a colour temperature at all. The channel
    /// keys ask this before anything else: on a lamp that only dims they do
    /// nothing, rather than saying so on every press of a held key.
    fn screen_tunable(&self, entry: &Entry) -> bool {
        match entry.state.as_ref() {
            Some(DeviceState::Light(light)) => light.mirek_range.is_some(),
            Some(_) => false,
            None => {
                let declared = declared(entry);
                !declared.cover && declared.mirek.is_some()
            }
        }
    }
    fn open_screen(&mut self, app: &App, row: usize) {
        let Some(entry) = self.entries.get(row) else {
            return;
        };
        self.screen = Some(entry.id.clone());
        // A blind opens with Open highlighted, so OK always has a button to
        // press; a lamp has no highlight at all and ignores the field.
        self.screen_button = 0;
        self.last_screen = None;
        app.set_light_screen_detail("".into());
        app.set_brightness_shown(false);
        self.render_screen(app);
        app.set_light_screen_shown(true);
        app.invoke_focus_light_screen();
    }
    /// Back from the screen returns to the room with the same row highlighted;
    /// Home leaves the room altogether, the way every other device screen does.
    fn close_screen(&mut self, app: &App, home: bool) {
        self.screen = None;
        self.last_screen = None;
        app.set_light_screen_shown(false);
        app.set_light_screen_detail("".into());
        if home {
            app.invoke_light_back();
        } else if app.get_light_shown() {
            app.invoke_focus_light();
        } else {
            app.invoke_focus_home();
        }
    }
    /// The row the open screen belongs to, by index, or nothing because it has
    /// left the room.
    fn screen_row(&self) -> Option<usize> {
        let id = self.screen.as_ref()?;
        self.entries.iter().position(|entry| &entry.id == id)
    }
    fn screen_action(&mut self, app: &App, name: &str, index: i32) {
        let Some(row) = self.screen_row() else {
            self.close_screen(app, false);
            return;
        };
        match name {
            "close" => return self.close_screen(app, false),
            "home" => return self.close_screen(app, true),
            // With no bar to select, left and right have only a blind's three
            // buttons to walk. A lamp has nothing for them.
            "button" => {
                if self
                    .entries
                    .get(row)
                    .is_some_and(|entry| self.screen_cover(entry))
                {
                    self.screen_button = (self.screen_button + index).clamp(0, 2);
                }
            }
            // The volume keys and up and down are both the level bar, and the
            // channel keys the colour temperature. A lamp with no colour
            // temperature simply has nothing for the channel keys.
            "level" => self.adjust_brightness(app, row, index.signum() * 5),
            "warmth" => {
                if self
                    .entries
                    .get(row)
                    .is_some_and(|entry| self.screen_tunable(entry))
                {
                    self.adjust_mirek(app, row, index);
                }
            }
            "toggle" => {
                if self.busy.is_none() && self.brightness_pending.is_empty() {
                    if let Some(id) = self.entries.get(row).map(|entry| entry.id.clone()) {
                        self.toggle_row(app, id);
                    }
                }
            }
            "cover" => {
                self.screen_button = index.clamp(0, 2);
                self.send_cover(app, row, index);
            }
            _ => {}
        }
        self.render_screen(app);
    }
    /// A blind's own button. Stop is only sent where the blind said it has one.
    fn send_cover(&mut self, app: &App, row: usize, button: i32) {
        let Some(entry) = self.entries.get(row) else {
            return;
        };
        let command = match button {
            0 => "open",
            1 => "stop",
            _ => "close",
        };
        if command == "stop"
            && !matches!(&entry.state, Some(DeviceState::Cover(cover)) if cover.can_stop)
            && !entry
                .plugin
                .as_ref()
                .is_some_and(|row| row.snapshot.cover.is_some_and(|traits| traits.stop))
        {
            app.set_light_screen_detail("This blind cannot be stopped part way.".into());
            return;
        }
        let (id, room) = (entry.id.clone(), self.room.clone());
        let Some(room) = room else {
            return;
        };
        if self.busy.is_some() {
            app.set_light_screen_detail("Still sending the last one.".into());
            return;
        }
        self.position_targets.remove(&id);
        let generation = self.generation + 1;
        if self
            .tx
            .try_send((generation, room, Operation::Cover(id.clone(), command)))
            .is_ok()
        {
            self.generation = generation;
            self.active
                .store(self.generation, std::sync::atomic::Ordering::SeqCst);
            self.claim(id);
            self.refreshing = false;
            app.set_light_screen_detail("".into());
        }
    }
    fn adjust_mirek(&mut self, app: &App, row: usize, delta: i32) {
        let Some(entry) = self.entries.get(row) else {
            return;
        };
        let Some(DeviceState::Light(light)) = entry.state.as_ref() else {
            app.set_light_screen_detail(
                "Checking this light's status. Try again in a moment.".into(),
            );
            return;
        };
        let target = self.pending_mirek(&entry.id);
        match crate::light_screen::mirek_step(light, target, delta) {
            Ok(mirek) => {
                let id = entry.id.clone();
                queue_level(&mut self.mirek_pending, id, mirek);
                app.set_light_screen_detail("".into());
            }
            Err(error) => app.set_light_screen_detail(error.into()),
        }
    }
    /// Whether this row's screen should own up to a write that is taking its
    /// time. A press moves the bar and the read-out at once, so while writes
    /// are answered promptly there is nothing to say; a packaged lamp chaining
    /// 150 ms writes under a held key would otherwise read "Updating…" for the
    /// whole hold, which is the one thing the screen must not do.
    fn screen_updating(&self, id: &str, now: Instant) -> bool {
        self.busy.as_deref() == Some(id)
            && self
                .busy_since
                .is_some_and(|at| now.saturating_duration_since(at) >= UPDATING_AFTER)
    }
    fn render_screen(&mut self, app: &App) {
        self.render_screen_at(app, Instant::now());
    }
    /// The clock is a parameter so that the `UPDATING_AFTER` rule can be
    /// tested without waiting for it.
    fn render_screen_at(&mut self, app: &App, now: Instant) {
        let Some(id) = self.screen.clone() else {
            return;
        };
        let Some(entry) = self.entries.iter().find(|entry| entry.id == id) else {
            return;
        };
        let mut view = crate::light_screen::view(
            &entry.name,
            &self.source_line(entry),
            declared(entry),
            entry.state.as_ref(),
            self.pending_level(&id),
            self.pending_mirek(&id),
        );
        // A refusal from the last press outlives one reading; the view's own
        // sentence is only shown when there is nothing to say about a press.
        let detail = app.get_light_screen_detail();
        if !detail.is_empty() {
            view.detail = detail.to_string();
        }
        // Both the blind's highlight and the "Updating…" flag are part of the
        // key: a press that moved only the highlight, and a write that crossed
        // `UPDATING_AFTER` while nothing else changed, both have to reach the
        // panel.
        let drawn = (view, self.screen_button, self.screen_updating(&id, now));
        if self.last_screen.as_ref() == Some(&drawn) {
            return;
        }
        let view = &drawn.0;
        app.set_light_screen_title(view.title.as_str().into());
        app.set_light_screen_room(view.room.as_str().into());
        app.set_light_screen_state(view.state.as_str().into());
        app.set_light_screen_active(view.active);
        app.set_light_screen_level_label(view.level_label.as_str().into());
        app.set_light_screen_level(view.level.as_str().into());
        app.set_light_screen_level_percent(view.level_percent);
        app.set_light_screen_level_known(view.level_known);
        app.set_light_screen_adjustable(view.adjustable);
        app.set_light_screen_cover(view.cover);
        app.set_light_screen_can_stop(view.can_stop);
        app.set_light_screen_tunable(view.tunable);
        app.set_light_screen_kelvin(view.kelvin.as_str().into());
        app.set_light_screen_mirek_percent(view.mirek_percent);
        app.set_light_screen_mirek_known(view.mirek_known);
        app.set_light_screen_detail(view.detail.as_str().into());
        app.set_light_screen_hint(view.hint.as_str().into());
        app.set_light_screen_button(self.screen_button);
        app.set_light_screen_pending(drawn.2);
        self.last_screen = Some(drawn);
    }
    /// The row on this index the Power key switches. An activity row, a
    /// device with a screen of its own and a thermostat are not: their Power
    /// key is a mapped binding (`activity_buttons`) or nothing at all.
    fn power_row(&self, row: usize) -> Option<&Entry> {
        self.entries.get(row).filter(|entry| {
            entry.activity.is_none()
                && !entry.id.starts_with("device:")
                && ha_domain(&entry.id) != "climate"
        })
    }
    /// Take the Power key for the highlighted row of the open room. `false`
    /// means nothing here wants it, and the key loop looks elsewhere.
    pub fn power_press(&self, app: &App) -> bool {
        // The screen is over the room, so it is what the key belongs to.
        if self.screen.is_some() {
            let Some(row) = self.screen_row() else {
                return false;
            };
            self.input
                .borrow_mut()
                .push_back(Input::PhysicalPower(row, false));
            return true;
        }
        if !app.get_light_shown()
            || app.get_chooser_shown()
            || app.get_settings_shown()
            || app.get_keyboard_shown()
        {
            return false;
        }
        let Some(row) = usize::try_from(app.get_light_index())
            .ok()
            .filter(|row| self.power_row(*row).is_some())
        else {
            return false;
        };
        self.input
            .borrow_mut()
            .push_back(Input::PhysicalPower(row, false));
        true
    }
    fn open_room(&mut self, app: &App, room: Id) {
        self.clear_brightness(app);
        let started = Instant::now();
        self.generation += 1;
        self.active
            .store(self.generation, std::sync::atomic::Ordering::SeqCst);
        self.room = Some(room.clone());
        self.release();
        let title = crate::connections::config()
            .and_then(|c| c.room(&room).map(|r| r.name.clone()))
            .unwrap_or_else(|| "Room".into());
        app.set_light_title(title.into());
        app.set_light_room_id(room.as_str().into());
        let scene_names = crate::connections::config()
            .map(|c| {
                c.scenes
                    .iter()
                    .filter(|s| s.rooms.contains(&room))
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let scenes = scene_names.len();
        app.set_light_scene_label(if scenes == 1 {
            scene_names[0].clone().into()
        } else {
            format!("{scenes} scenes").into()
        });
        app.set_light_scene_count(scenes as i32);
        app.set_light_detail("".into());
        app.set_light_shown(true);
        match configured(&room) {
            Ok(mut entries) => {
                for e in &mut entries {
                    e.state = self.cache.get(&e.id);
                }
                self.entries = entries;
                self.refresh();
                if self.entries.is_empty() {
                    app.set_light_detail("Add devices to this room in the web editor.".into());
                }
            }
            Err(error) => {
                self.entries.clear();
                self.refreshing = false;
                app.set_light_detail(error.into());
            }
        }
        self.update_rows(app, true);
        app.invoke_focus_light();
        println!(
            "couch-gui: room list ready in {} us ({} devices)",
            started.elapsed().as_micros(),
            self.entries.len()
        );
    }
    pub fn navigation_pending(&self) -> bool {
        self.input
            .borrow()
            .iter()
            .any(|input| matches!(input, Input::Open(_) | Input::Back))
    }
    /// The toast a row press asked for, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }
    pub fn poll(&mut self, app: &App) {
        if self
            .brightness_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.brightness_until = None;
            app.set_brightness_shown(false);
        }
        if app.get_pair_shown() {
            self.input.borrow_mut().clear();
            self.clear_brightness(app);
            return;
        }
        loop {
            let Some(input) = self.input.borrow_mut().pop_front() else {
                break;
            };
            match input {
                Input::PhysicalLevel(i, delta, repeat) => self.intercept_ir(
                    i,
                    if delta > 0 {
                        "volume-up"
                    } else {
                        "volume-down"
                    },
                    repeat,
                    Input::Brightness(i, delta),
                ),
                Input::PhysicalPick(i, repeat) => {
                    if self
                        .entries
                        .get(i)
                        .is_some_and(|e| e.hue || matches!(ha_domain(&e.id), "light" | "cover"))
                    {
                        self.intercept_ir(i, "toggle", repeat, Input::Pick(i));
                    } else {
                        self.input.borrow_mut().push_front(Input::Pick(i));
                    }
                }
                Input::PhysicalPower(i, repeat) => {
                    if self
                        .entries
                        .get(i)
                        .is_some_and(|e| e.hue || matches!(ha_domain(&e.id), "light" | "cover"))
                    {
                        self.intercept_ir(i, "toggle", repeat, Input::Power(i));
                    } else {
                        self.input.borrow_mut().push_front(Input::Power(i));
                    }
                }
                Input::Power(i) => {
                    if self.room.is_none()
                        || self.busy.is_some()
                        || !self.brightness_pending.is_empty()
                    {
                        continue;
                    }
                    let Some(id) = self.power_row(i).map(|e| e.id.clone()) else {
                        continue;
                    };
                    self.toggle_row(app, id);
                }
                Input::Open(room) => self.open_room(app, room),
                Input::Back => {
                    self.clear_brightness(app);
                    self.screen = None;
                    self.last_screen = None;
                    app.set_light_screen_shown(false);
                    self.generation += 1;
                    self.active
                        .store(self.generation, std::sync::atomic::Ordering::SeqCst);
                    self.room = None;
                    self.release();
                    self.refreshing = false;
                    app.set_light_shown(false);
                    app.invoke_focus_home();
                }
                Input::Brightness(i, delta) => {
                    if self.room.is_some() {
                        self.adjust_brightness(app, i, delta);
                    }
                }
                Input::Pick(i) => {
                    if self.room.is_none()
                        || self.busy.is_some()
                        || !self.brightness_pending.is_empty()
                    {
                        continue;
                    }
                    let Some(e) = self.entries.get(i) else {
                        continue;
                    };
                    if ha_domain(&e.id) == "climate" {
                        let (id, name) = (
                            self.device_resource(i).unwrap_or_else(|| e.id.clone()),
                            e.name.clone(),
                        );
                        self.clear_brightness(app);
                        app.invoke_open_thermostat(id.into(), name.into());
                        continue;
                    }
                    if let Some(id) = e.id.strip_prefix("activity:") {
                        app.invoke_open_activity(id.into());
                        continue;
                    }
                    if e.id.starts_with("device:") {
                        let cfg = crate::connections::config();
                        let camera = cfg.as_ref().is_some_and(|c| {
                            c.devices()
                                .find(|(_, d)| d.id.as_str() == e.id.trim_start_matches("device:"))
                                .and_then(|(_, d)| c.resolve_integration(&d.integration))
                                .is_some_and(|i| matches!(i, Integration::UnifiProtect { .. }))
                        });
                        if camera {
                            app.invoke_open_camera(e.id.as_str().into(), e.name.as_str().into());
                            continue;
                        }
                        let player = cfg.as_ref().is_some_and(|c| {
                            c.devices()
                                .find(|(_, d)| d.id.as_str() == e.id.trim_start_matches("device:"))
                                .map(|(_, d)| d)
                                .and_then(|d| c.resolve_integration(&d.integration))
                                .is_some_and(|i| crate::shortcuts::opens_player(&i))
                        });
                        if player {
                            app.invoke_open_activity(e.id.as_str().into());
                            continue;
                        }
                        if let Some(_connection) = cfg
                            .as_ref()
                            .and_then(|c| tv_connection(c, e.id.trim_start_matches("device:")))
                        {
                            app.set_active_activity("".into());
                            app.invoke_open_tv(e.id.as_str().into(), e.name.as_str().into());
                            continue;
                        }
                        if let Some(message) = cfg
                            .as_ref()
                            .and_then(|c| needs_package(c, e.id.trim_start_matches("device:")))
                        {
                            self.notice = Some(message);
                            continue;
                        }
                        app.set_light_detail(
                            "Controls for this device are not available yet.".into(),
                        );
                        continue;
                    }
                    // OK on a row with more than on and off behind it opens
                    // its screen; anything else keeps the toggle it had,
                    // because an empty screen would be worse than a switch.
                    if opens_screen(e) {
                        self.open_screen(app, i);
                        continue;
                    }
                    let id = e.id.clone();
                    self.toggle_row(app, id);
                }
                Input::Screen(name, index) => self.screen_action(app, &name, index),
            }
        }
        while let Ok((generation, result)) = self.rx.try_recv() {
            if generation != self.generation || self.room.is_none() {
                continue;
            }
            self.last_refresh = Instant::now();
            self.refreshing = false;
            self.release();
            let brightness = self.brightness_flight.take();
            let colour = self.mirek_flight.take();
            match result {
                Ok(Answer::IrChecked(handled, resume)) => {
                    if !handled {
                        self.input.borrow_mut().push_front(resume);
                    }
                    app.set_light_detail("".into());
                }
                Ok(Answer::List(entries)) => {
                    let reset = self.entries.len() != entries.len()
                        || self.entries.iter().zip(&entries).any(|(a, b)| a.id != b.id);
                    for e in &entries {
                        if let Some(s) = &e.state {
                            self.cache.put(s.clone());
                        } else {
                            self.cache.0.remove(&e.id);
                        }
                    }
                    self.entries = entries;
                    self.update_rows(app, reset);
                }
                Ok(Answer::Busy) => {
                    // Nothing was sent and nothing is wrong: put the level
                    // back at the head of the queue unless a newer one for the
                    // same row is already waiting, and say nothing.
                    if let Some((id, percent)) = brightness {
                        requeue_level(&mut self.brightness_pending, id, percent);
                    }
                    if let Some((id, mirek)) = colour {
                        requeue_level(&mut self.mirek_pending, id, mirek);
                    }
                    // Without this the next poll would resend immediately and
                    // spin against a connection that is busy for a moment.
                    self.last_brightness_send = Instant::now();
                    // The row said "Updating…" while the request was out.
                    self.update_rows(app, false);
                }
                Ok(Answer::State(s)) => {
                    self.cache.put(s.clone());
                    for e in &mut self.entries {
                        if e.id == s.id() {
                            e.state = Some(s.clone());
                        }
                    }
                    self.update_rows(app, false);
                    app.set_light_detail("".into());
                }
                Err(error) => {
                    if let Some((id, _)) = brightness {
                        self.position_targets.remove(&id);
                        self.brightness_pending
                            .retain(|(pending, _)| pending != &id);
                        app.set_brightness_shown(false);
                    }
                    if let Some((id, _)) = colour {
                        self.mirek_pending.retain(|(pending, _)| pending != &id);
                    }
                    app.set_light_screen_detail(error.as_str().into());
                    app.set_light_detail(error.into());
                    self.update_rows(app, false);
                }
            }
        }
        self.send_ir();
        self.send_brightness();
        // The screen is another view of the row behind it: every reading that
        // moves the row moves the screen too, and a row that has left the room
        // takes its screen with it.
        if self.screen.is_some() {
            if self.screen_row().is_some() {
                self.render_screen(app);
            } else {
                self.close_screen(app, false);
            }
        }
        if self.room.is_some()
            && self.brightness_pending.is_empty()
            && self.ir_pending.is_empty()
            && self.busy.is_none()
            && !self.refreshing
            && self.last_refresh.elapsed()
                > Duration::from_millis(if self.entries.iter().all(|e| e.hue) {
                    500
                } else {
                    5000
                })
        {
            self.refresh();
        }
    }
}
// Retain only the latest unsent target per device, preserving device order.
// One queue shape for both the level and the colour temperature: a held key
// must never build a backlog of commands, whichever it is moving.
fn queue_level<T>(queue: &mut VecDeque<(String, T)>, id: String, value: T) {
    if let Some((_, target)) = queue.iter_mut().find(|(pending, _)| pending == &id) {
        *target = value;
    } else {
        queue.push_back((id, value));
    }
}
/// Put an unsent target back at the head of the queue. A newer one for the
/// same row is already waiting there and wins: only the latest is ever sent.
fn requeue_level<T>(queue: &mut VecDeque<(String, T)>, id: String, value: T) {
    if !queue.iter().any(|(pending, _)| pending == &id) {
        queue.push_front((id, value));
    }
}
fn brightness_step(light: &Light, target: Option<u8>, delta: i32) -> Result<u8, &'static str> {
    if light.on.is_none() {
        return Err("This light is unavailable.");
    }
    if !light.dimmable {
        return Err("This light does not support brightness.");
    }
    let current = target
        .or_else(|| {
            if light.on == Some(false) {
                Some(0)
            } else {
                light.brightness_percent
            }
        })
        .ok_or("Checking brightness. Try again in a moment.")?;
    Ok((current as i32 + delta.clamp(-100, 100)).clamp(0, 100) as u8)
}
fn cover_step(cover: &Cover, target: Option<u8>, delta: i32) -> Result<u8, &'static str> {
    if cover.state.is_none() {
        return Err("This blind is unavailable.");
    }
    if !cover.can_set_position {
        return Err("This blind does not support position control.");
    }
    let current = target
        .or(cover.position_percent)
        .ok_or("This blind has not reported its position.")?;
    Ok((current as i32 + delta.clamp(-100, 100)).clamp(0, 100) as u8)
}
pub(crate) fn cover_description(cover: &Cover) -> String {
    let text = match cover.state.as_deref() {
        Some("open") => "Open",
        Some("closed") => "Closed",
        Some("opening") => "Opening",
        Some("closing") => "Closing",
        _ => return "Unavailable".into(),
    };
    match cover.position_percent {
        Some(position) => format!("{text} · {position}% open"),
        None => text.into(),
    }
}
pub(crate) fn description(light: &Light) -> String {
    match light.on {
        None => "Unavailable".into(),
        Some(false) => "Off".into(),
        Some(true) => light
            .brightness_percent
            .map(|p| format!("On · {p}%"))
            .unwrap_or_else(|| "On".into()),
    }
}
/// Whether OK on this row opens a screen of any kind, which is what the
/// chevron on it promises: its own light or blind screen, or the device screen
/// a `device:` row has always had. A device still waiting for its package has
/// no screen to open and says so in words instead.
fn row_opens_a_screen(config: Option<&couch_model::Config>, entry: &Entry) -> bool {
    if entry.activity.is_some() {
        return false;
    }
    if let Some(device) = entry.id.strip_prefix("device:") {
        return config.is_none_or(|config| needs_package(config, device).is_none());
    }
    opens_screen(entry)
}
/// Whether OK on this row opens its control screen rather than switching it.
///
/// A packaged child declares what it can do, so this is known before any
/// reading; everything else is decided by the reading it has, and a row that
/// has not answered keeps the toggle OK has always been - opening a screen on
/// a guess would be worse than the switch it replaced.
fn opens_screen(entry: &Entry) -> bool {
    if let Some(row) = &entry.plugin {
        return if row.cover {
            row.snapshot
                .cover
                .is_some_and(|traits| traits.position || traits.stop)
        } else {
            row.snapshot
                .light
                .is_some_and(|traits| traits.dimmable || traits.mirek.is_some())
        };
    }
    entry
        .state
        .as_ref()
        .is_some_and(crate::light_screen::has_controls)
}
/// Whether a packaged device gets the generic packaged control screen.
///
/// A device that is the connection does, and so does a thermostat child until
/// Home Assistant is packaged. A light or a blind child never does: it is a
/// room row, where its level already lives.
pub(crate) fn opens_packaged_screen(
    config: &couch_model::Config,
    device: &couch_model::Device,
) -> bool {
    plugin_row(config, device).is_none()
}
// Resolve the selected device's own connection; never select the first Android
// TV when multiple TVs are configured.
pub(crate) fn tv_connection(config: &couch_model::Config, device_id: &str) -> Option<String> {
    let (_, device) = config.devices().find(|(_, d)| d.id.as_str() == device_id)?;
    let integration = config.resolve_integration(&device.integration);
    if device.network_integration(config).is_none() {
        if device.effective_ir_codeset(config).is_some() {
            return Some(format!("ir:{device_id}"));
        }
        if device.bluetooth.is_some() {
            return Some(format!("bt:{device_id}"));
        }
    }
    let provider = match integration? {
        Integration::Sonos { .. } => return Some(format!("sonos:{device_id}")),
        // A packaged device: the core control screen, filled from what the
        // package declares (tv_plugin.rs). Not a packaged light or blind:
        // those are room rows, and a row has no screen behind it.
        Integration::Plugin { .. } if opens_packaged_screen(config, device) => {
            return Some(format!("plugin:{device_id}"))
        }
        Integration::Plugin { .. } => return None,
        Integration::WebOs => couch_model::Provider::WebOs,
        Integration::AndroidTv => couch_model::Provider::AndroidTv,
        Integration::AppleTv => couch_model::Provider::AppleTv,
        Integration::Tizen => couch_model::Provider::Tizen,
        _ => return None,
    };
    match &device.integration {
        Integration::Connection { connection_id, .. } => Some(connection_id.to_string()),
        _ => config
            .connections
            .iter()
            .find(|c| c.provider == provider)
            .map(|c| c.id.to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_receiver_waiting_for_its_package_says_so_instead_of_opening_anything() {
        let config:couch_model::Config=serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"receiver","name":"Receiver","provider":{"kind":"denon","host":"avr.invalid","port":23}},
                {"id":"tv","name":"TV","provider":{"kind":"apple-tv"}}],
            "rooms":[{"id":"r","name":"Room","devices":[
                {"id":"avr","name":"Theater AVR","kind":"speaker","integration":{"via":"connection","connection_id":"receiver"}},
                {"id":"inline","name":"Old AVR","kind":"speaker","integration":{"via":"denon","host":"192.0.2.7","port":23}},
                {"id":"tv","name":"TV","kind":"tv","integration":{"via":"connection","connection_id":"tv"}}]}]})).unwrap();
        config.validate().unwrap();
        assert_eq!(
            needs_package(&config, "avr").as_deref(),
            Some("Needs the Denon package")
        );
        assert_eq!(
            needs_package(&config, "inline").as_deref(),
            Some("Needs the Denon package")
        );
        // The row says it before anyone presses OK.
        assert_eq!(
            device_row_detail(Some(&config), "device:avr"),
            "Needs the Denon package"
        );
        assert_eq!(
            device_row_detail(Some(&config), "device:tv"),
            "Press OK for controls"
        );
        assert_eq!(
            device_row_detail(None, "device:avr"),
            "Press OK for controls"
        );
        assert_eq!(needs_package(&config, "tv"), None);
        assert_eq!(needs_package(&config, "missing"), None);
        // It still has a row, and no screen of its own to open - so it makes
        // no promise of one either: no chevron beside "Needs the Denon
        // package", where the TV beside it has one.
        let entries = configured_in(&config, &Id::new("r")).unwrap();
        assert!(entries.iter().any(|e| e.id == "device:avr"));
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), row_opens_a_screen(Some(&config), e)))
                .collect::<Vec<_>>(),
            [
                ("device:avr", false),
                ("device:inline", false),
                ("device:tv", true)
            ]
        );
        assert_eq!(tv_connection(&config, "avr"), None);
    }
    /// The sentence has to fit the toast on the 480-pixel panel, under a real
    /// room list. `COUCH_LEGACY_SCREENSHOTS=<dir>` keeps the picture.
    #[test]
    fn the_needs_package_toast_renders_over_the_room_list() {
        const NAME: &str = "lights::tests::the_needs_package_toast_renders_over_the_room_list";
        if std::env::var_os("COUCH_TEST_LEGACY_TOAST").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_LEGACY_TOAST", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{platform::WindowEvent, ComponentHandle};
        let config:couch_model::Config=serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"receiver","name":"Receiver","provider":{"kind":"denon","host":"avr.invalid","port":23}}],
            "rooms":[{"id":"r","name":"Den","devices":[
                {"id":"avr","name":"Theater AVR","kind":"speaker","integration":{"via":"connection","connection_id":"receiver"}}]}]})).unwrap();
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let entries = configured_in(&config, &Id::new("r")).unwrap();
        app.set_light_title("Den".into());
        app.set_light_items(slint::ModelRc::new(slint::VecModel::from(
            entries
                .iter()
                .map(|e| ChoiceItem {
                    icon: crate::icons::image(e.icon),
                    title: e.name.clone().into(),
                    detail: device_row_detail(Some(&config), &e.id).into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        app.set_light_shown(true);
        app.set_feedback_enabled(true);
        // What OK on that row raises (`poll` hands it to the main loop's toast).
        let message = needs_package(&config, "avr").unwrap();
        app.set_toast(message.as_str().into());
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(16));
        }
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        // The bar is up: its band is not the page background all the way across.
        let band = &pixels[720 * 480..760 * 480];
        assert!(band.iter().any(|p| *p != band[0]), "the toast drew nothing");
        if let Some(dir) = std::env::var_os("COUCH_LEGACY_SCREENSHOTS") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::Path::new(&dir).join("legacy-needs-package-toast.png"),
                &bytes,
                480,
                800,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        app.hide().unwrap();
    }
    #[test]
    fn sonos_rows_are_the_only_media_rows() {
        let config:couch_model::Config=serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"s","name":"S","provider":{"kind":"sonos","host":"192.0.2.9"}},{"id":"tv","name":"TV","provider":{"kind":"apple-tv"}}],
            "rooms":[{"id":"r","name":"Room","devices":[
                {"id":"speaker","name":"Speaker","kind":"speaker","integration":{"via":"connection","connection_id":"s"}},
                {"id":"tv","name":"TV","kind":"tv","integration":{"via":"connection","connection_id":"tv"}},
                {"id":"lamp","name":"Lamp","kind":"light","integration":{"via":"hue","light_id":"1"}}]}]})).unwrap();
        let entries = configured_in(&config, &Id::new("r")).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), e.media))
                .collect::<Vec<_>>(),
            [
                ("device:speaker", true),
                ("device:tv", false),
                ("hue:1", false)
            ]
        );
    }
    #[test]
    fn activities_are_pinned_above_the_devices_and_rows_still_find_their_device() {
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "rooms":[{"id":"r","name":"Room","devices":[
                {"id":"lamp","name":"Lamp","kind":"light","integration":{"via":"hue","light_id":"1"}},
                {"id":"tv","name":"TV","kind":"tv"}]}],
            "activities":[{"id":"watch","name":"Watch","room":"r","kind":"video","source":"tv"},{"id":"elsewhere","name":"Elsewhere","room":"other"}]})).unwrap();
        let room = Id::new("r");
        let entries = configured_in(&config, &room).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["activity:watch", "hue:1", "device:tv"]
        );
        // The pinned row carries what the area page's strip shows for the
        // activity: its glyph and "source · room"; devices carry nothing.
        assert_eq!(entries[0].activity, Some((1, "TV · Room".to_string())));
        assert!(entries[1].activity.is_none() && entries[2].activity.is_none());
        assert_eq!(activity_rows(&config, &room), 1);
        assert!(device_at(&config, &room, 0).is_none());
        assert_eq!(
            device_at(&config, &room, 1).map(|d| d.id.as_str()),
            Some("lamp")
        );
        assert_eq!(
            device_at(&config, &room, 2).map(|d| d.id.as_str()),
            Some("tv")
        );
        assert!(device_at(&config, &room, 3).is_none());
        assert_eq!(row_of_device(&config, &room, &Id::new("tv")), Some(2));
        assert_eq!(row_of_device(&config, &room, &Id::new("ghost")), None);
    }
    #[test]
    fn selected_ir_device_keeps_per_device_target() {
        let config:couch_model::Config=serde_json::from_value(serde_json::json!({"schema_version":1,"connections":[{"id":"ir","name":"IR","provider":{"kind":"ir"}}],"rooms":[{"id":"r","name":"Room","devices":[{"id":"tv-a","name":"A","kind":"tv","integration":{"via":"connection","connection_id":"ir","resource_id":"a"}},{"id":"tv-b","name":"B","kind":"tv","integration":{"via":"connection","connection_id":"ir","resource_id":"b"}}]}]})).unwrap();
        assert_eq!(tv_connection(&config, "tv-a").as_deref(), Some("ir:tv-a"));
        assert_eq!(tv_connection(&config, "tv-b").as_deref(), Some("ir:tv-b"));
    }
    #[test]
    fn selected_apple_tv_uses_its_own_connection() {
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({
            "schema_version":1,
            "connections":[
                {"id":"first","name":"First TV","provider":{"kind":"apple-tv"}},
                {"id":"second","name":"Second TV","provider":{"kind":"apple-tv"}}
            ],
            "rooms":[{"id":"office","name":"Office","devices":[
                {"id":"tv","name":"Apple TV","kind":"tv","integration":{"via":"connection","connection_id":"second","resource_id":""}}
            ]}]
        })).unwrap();
        assert_eq!(tv_connection(&config, "tv").as_deref(), Some("second"));
    }
    #[test]
    fn selected_android_tv_uses_its_own_connection() {
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({
            "schema_version":1,
            "connections":[
                {"id":"first","name":"First TV","provider":{"kind":"android-tv"}},
                {"id":"second","name":"Second TV","provider":{"kind":"android-tv"}}
            ],
            "rooms":[{"id":"office","name":"Office","devices":[
                {"id":"tv","name":"Android TV","kind":"tv","integration":{"via":"connection","connection_id":"second","resource_id":""}}
            ]}]
        })).unwrap();
        assert_eq!(tv_connection(&config, "tv").as_deref(), Some("second"));
        assert_eq!(tv_connection(&config, "missing"), None);
        let mut removed = config;
        removed.connections.pop();
        assert_eq!(tv_connection(&removed, "tv"), None);
    }
    #[test]
    fn brightness_steps_use_pending_targets_clamp_and_reject_unsupported_lights() {
        let mut light = Light {
            entity_id: "light.test".into(),
            name: "Test".into(),
            on: Some(true),
            brightness_percent: Some(50),
            dimmable: true,
            mirek: None,
            mirek_range: None,
        };
        assert_eq!(brightness_step(&light, None, 5), Ok(55));
        assert_eq!(brightness_step(&light, Some(55), 5), Ok(60));
        assert_eq!(brightness_step(&light, Some(98), 5), Ok(100));
        assert_eq!(brightness_step(&light, Some(2), -5), Ok(0));
        light.on = Some(false);
        assert_eq!(brightness_step(&light, None, 5), Ok(5));
        light.on = None;
        assert!(brightness_step(&light, Some(50), 5).is_err());
        light.on = Some(true);
        light.dimmable = false;
        assert!(brightness_step(&light, None, 5).is_err());
        light.dimmable = true;
        light.brightness_percent = None;
        assert!(brightness_step(&light, None, 5).is_err());
    }
    #[test]
    fn rapid_dimming_keeps_latest_target_without_dropping_other_lights() {
        let mut queue = VecDeque::new();
        queue_level(&mut queue, "one".into(), 55);
        queue_level(&mut queue, "two".into(), 25);
        queue_level(&mut queue, "one".into(), 60);
        queue_level(&mut queue, "one".into(), 65);
        assert_eq!(
            queue.into_iter().collect::<Vec<_>>(),
            vec![("one".into(), 65), ("two".into(), 25)]
        );
    }
    #[test]
    fn toggle_uses_live_state_and_rejects_unavailable() {
        let mut state = Light {
            entity_id: "light.test".into(),
            name: "Test".into(),
            on: Some(false),
            brightness_percent: None,
            dimmable: true,
            mirek: None,
            mirek_range: None,
        };
        assert!(matches!(toggle_command(&state), Ok(Command::On)));
        state.on = Some(true);
        assert!(matches!(toggle_command(&state), Ok(Command::Off)));
        state.on = None;
        assert!(toggle_command(&state).is_err());
    }
    fn blind() -> Cover {
        Cover {
            entity_id: "cover.office".into(),
            name: "Office".into(),
            state: Some("open".into()),
            position_percent: Some(50),
            can_open: true,
            can_close: true,
            can_set_position: true,
            can_stop: true,
        }
    }
    #[test]
    fn blind_steps_use_latest_target_without_inventing_unknown_position() {
        let mut cover = blind();
        assert_eq!(cover_step(&cover, None, 5), Ok(55));
        assert_eq!(cover_step(&cover, Some(55), 5), Ok(60));
        assert_eq!(cover_step(&cover, Some(98), 5), Ok(100));
        assert_eq!(cover_step(&cover, Some(2), -5), Ok(0));
        cover.position_percent = None;
        assert!(cover_step(&cover, None, 5).is_err());
        cover.state = None;
        assert!(cover_step(&cover, Some(50), 5).is_err());
        cover.state = Some("open".into());
        cover.can_set_position = false;
        assert!(cover_step(&cover, Some(50), 5).is_err());
    }
    #[test]
    fn blind_status_describes_motion_and_open_percentage() {
        let mut cover = blind();
        assert_eq!(cover_description(&cover), "Open · 50% open");
        cover.state = Some("closing".into());
        assert_eq!(cover_description(&cover), "Closing · 50% open");
        cover.state = None;
        assert_eq!(cover_description(&cover), "Unavailable");
    }
    #[test]
    fn scoped_ha_entities_keep_domains_and_separate_state_caches() {
        assert_eq!(ha_domain("upstairs/climate.office"), "climate");
        assert_eq!(ha_domain("cover.office"), "cover");
        let mut cache = StateCache::default();
        let mut a = DeviceState::Cover(blind());
        a.set_id("a/cover.office".into());
        let mut b = DeviceState::Cover(blind());
        b.set_id("b/cover.office".into());
        cache.put(a);
        cache.put(b);
        assert!(cache.get("a/cover.office").is_some());
        assert!(cache.get("b/cover.office").is_some());
        assert!(cache.get("cover.office").is_none());
    }
    /// Protocol 3 is unreleased and the panel is never built with its preview
    /// switched on, so no connection can declare children and no device can be
    /// saved as one: nothing here changes what a user sees today.
    #[test]
    fn the_panel_is_never_built_with_the_protocol_3_preview() {
        assert_eq!(
            couch_plugin::accepted_protocol_version(),
            couch_plugin::PROTOCOL_VERSION
        );
    }

    /// A bridge that offers children, and a receiver that is a connection of
    /// its own: the two shapes a packaged connection has.
    fn packaged() -> couch_model::Config {
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[
                {"id":"bridge","name":"Hue bridge","provider":{"kind":"plugin","id":"hue","label":"Philips Hue",
                    "children":[
                        {"kind":"light","label":"Light","device_kind":"light","component":"light",
                         "capabilities":[{"id":"toggle","label":"Toggle"}],"actions":[{"action":"set_light"}]},
                        {"kind":"blind","label":"Blind","device_kind":"blind","component":"cover",
                         "capabilities":[{"id":"toggle","label":"Toggle"}],"actions":[{"action":"set_cover"}]},
                        {"kind":"thermostat","label":"Thermostat","device_kind":"thermostat","component":"climate",
                         "actions":[{"action":"set_climate"}]},
                        {"kind":"scene","label":"Scene","device_kind":"other","component":"scene",
                         "capabilities":[{"id":"on","label":"On"}]}]}},
                {"id":"receiver","name":"Theater AVR","provider":{"kind":"plugin","id":"denon","label":"Denon AVR",
                    "supports_inputs":true,
                    "capabilities":[{"id":"power-on","label":"Main zone on"},{"id":"power-off","label":"Main zone off"}]}}],
            "rooms":[{"id":"living-room","name":"Living room","devices":[
                {"id":"desk","name":"Desk lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge",
                    "resource_id":"lamp/1","child":{"kind":"light","light":{"dimmable":true,"mirek":[153,500]}}}},
                {"id":"reading","name":"Reading lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge",
                    "resource_id":"lamp/2","child":{"kind":"light","light":{"dimmable":true}}}},
                {"id":"blind","name":"Blind","kind":"blind","integration":{"via":"connection","connection_id":"bridge",
                    "resource_id":"cover/1","child":{"kind":"blind","cover":{"position":true,"stop":true}}}},
                {"id":"heat","name":"Heating","kind":"thermostat","integration":{"via":"connection","connection_id":"bridge",
                    "resource_id":"climate/1","child":{"kind":"thermostat","climate":{
                        "min_tenths":70,"max_tenths":300,"step_tenths":5,"modes":["off","heat"]}}}},
                {"id":"avr","name":"Theater AVR","kind":"speaker",
                 "integration":{"via":"connection","connection_id":"receiver","resource_id":""}}]}],
            "scenes":[{"id":"relax","name":"Relax","rooms":["living-room"],
                "resource":{"connection_id":"bridge","resource_id":"scene/1","kind":"scene"}}]})).unwrap();
        config.validate().unwrap();
        config
    }

    fn reading(json: serde_json::Value) -> couch_plugin::Status {
        serde_json::from_value(json).unwrap()
    }

    /// The shape of the room list with packaged children in it: lamps and
    /// blinds are rows like any other, a thermostat keeps the screen it had,
    /// a scene is not a device at all, and a receiver is untouched.
    #[test]
    fn packaged_lights_and_blinds_are_room_rows_and_nothing_else_moves() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            [
                "plugin:bridge/lamp/1",
                "plugin:bridge/lamp/2",
                "plugin:bridge/cover/1",
                "device:heat",
                "device:avr"
            ]
        );
        // The two lamps and the blind carry what it takes to drive them; the
        // thermostat and the receiver carry nothing.
        let row = |i: usize| entries[i].plugin.clone();
        assert_eq!(
            row(0).map(|r| (r.connection, r.resource, r.cover)),
            Some(("bridge".into(), "lamp/1".into(), false))
        );
        assert_eq!(
            row(2).map(|r| (r.connection, r.resource, r.cover)),
            Some(("bridge".into(), "cover/1".into(), true))
        );
        assert!(row(3).is_none() && row(4).is_none());
        // A lamp is drawn as a light row, never as a device with a screen.
        assert!(entries[0].state.is_none() && !entries[0].hue && !entries[0].matter);
        // Nothing here opens the packaged control screen but the thermostat
        // and the receiver.
        assert_eq!(tv_connection(&config, "desk"), None);
        assert_eq!(tv_connection(&config, "blind"), None);
        assert_eq!(
            tv_connection(&config, "heat").as_deref(),
            Some("plugin:heat")
        );
        assert_eq!(tv_connection(&config, "avr").as_deref(), Some("plugin:avr"));
        // The room's Scenes button counts the package's scene beside any Hue
        // one, because a scene is listed by its rooms alone.
        assert_eq!(
            config
                .scenes
                .iter()
                .filter(|s| s.rooms.contains(&Id::new("living-room")))
                .count(),
            1
        );
    }

    /// A kind the connection has stopped declaring (a rollback that has not
    /// healed): the saved traits still say what the device is, so the lamp
    /// keeps its row rather than turning into a screen.
    #[test]
    fn a_child_whose_kind_has_gone_keeps_the_row_its_own_traits_describe() {
        let mut config = packaged();
        let couch_model::Provider::Plugin { children, .. } = &mut config
            .connections
            .iter_mut()
            .find(|c| c.id.as_str() == "bridge")
            .unwrap()
            .provider
        else {
            panic!("a packaged connection")
        };
        children.clear();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            [
                "plugin:bridge/lamp/1",
                "plugin:bridge/lamp/2",
                "plugin:bridge/cover/1",
                "device:heat",
                "device:avr"
            ]
        );
        assert!(entries[2].plugin.as_ref().is_some_and(|r| r.cover));
        assert_eq!(tv_connection(&config, "desk"), None);
    }

    /// Readings become the shapes the rows already speak, and an unknown
    /// stays unknown: a lamp that has not said whether it is on is not off.
    #[test]
    fn a_reading_never_turns_an_unknown_into_an_off() {
        let dimmable = LightTraits {
            dimmable: true,
            mirek: Some((153, 500)),
            color: false,
        };
        let plug = LightTraits::default();
        for (traits, state, on, percent, dims) in [
            (
                Some(&dimmable),
                serde_json::json!({"on":true,"brightness":40}),
                Some(true),
                Some(40),
                true,
            ),
            (
                Some(&dimmable),
                serde_json::json!({"on":false,"brightness":40}),
                Some(false),
                Some(40),
                true,
            ),
            (Some(&dimmable), serde_json::json!({}), None, None, true),
            (
                Some(&plug),
                serde_json::json!({"on":true}),
                Some(true),
                None,
                false,
            ),
            (
                None,
                serde_json::json!({"on":true}),
                Some(true),
                None,
                false,
            ),
        ] {
            let state: LightState = serde_json::from_value(state).unwrap();
            let light = light_row("Desk", traits, &state);
            assert_eq!(
                (light.on, light.brightness_percent, light.dimmable),
                (on, percent, dims),
                "{state:?}"
            );
        }
        // Unknown reads as "Unavailable", and the slider refuses to invent a
        // level for it.
        let unknown = light_row("Desk", Some(&dimmable), &LightState::default());
        assert_eq!(description(&unknown), "Unavailable");
        assert!(brightness_step(&unknown, None, 5).is_err());
        // An off lamp keeps the level it would return to, and dims from zero.
        let off = light_row(
            "Desk",
            Some(&dimmable),
            &serde_json::from_value(serde_json::json!({"on":false,"brightness":40})).unwrap(),
        );
        assert_eq!(description(&off), "Off");
        assert_eq!(brightness_step(&off, None, 5), Ok(5));
        // A lamp that cannot be dimmed says so rather than sending a level.
        let plain = light_row(
            "Plug",
            Some(&plug),
            &serde_json::from_value(serde_json::json!({"on":true})).unwrap(),
        );
        assert_eq!(description(&plain), "On");
        assert!(brightness_step(&plain, None, 5).is_err());

        let full = CoverTraits {
            position: true,
            stop: true,
        };
        for (traits, state, shown, steps) in [
            (
                Some(&full),
                serde_json::json!({"open":true,"position":60}),
                "Open · 60% open",
                true,
            ),
            (
                Some(&full),
                serde_json::json!({"open":false,"position":0}),
                "Closed · 0% open",
                true,
            ),
            (Some(&full), serde_json::json!({}), "Unavailable", false),
            (
                Some(&CoverTraits::default()),
                serde_json::json!({"open":true,"position":60}),
                "Open · 60% open",
                false,
            ),
            (None, serde_json::json!({"open":true}), "Open", false),
        ] {
            let state: CoverState = serde_json::from_value(state).unwrap();
            let cover = cover_row("Blind", traits, &state);
            assert_eq!(cover_description(&cover), shown, "{state:?}");
            assert_eq!(cover_step(&cover, None, 5).is_ok(), steps, "{state:?}");
        }
    }

    /// A status that says nothing about this child is no reading at all: the
    /// row says "Unavailable" rather than showing a lamp as off.
    #[test]
    fn a_status_without_this_childs_state_is_not_a_reading() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let lamp = entries[0].plugin.clone().unwrap();
        let blind = entries[2].plugin.clone().unwrap();
        assert!(lamp
            .state("Desk", &reading(serde_json::json!({})))
            .is_none());
        assert!(lamp
            .state("Desk", &reading(serde_json::json!({"cover":{"open":true}})))
            .is_none());
        assert!(blind
            .state("Blind", &reading(serde_json::json!({"light":{"on":true}})))
            .is_none());
        assert!(matches!(
            lamp.state("Desk", &reading(serde_json::json!({"light":{"on":true}}))),
            Some(DeviceState::Light(_))
        ));
        assert!(matches!(
            blind.state(
                "Blind",
                &reading(serde_json::json!({"cover":{"open":true}}))
            ),
            Some(DeviceState::Cover(_))
        ));
    }

    /// Every packaged row is read on its own, in order, on the one worker.
    /// The first connection that cannot be reached takes the rest of its rows
    /// with it for this round; another connection is still read, and no row is
    /// ever removed.
    #[test]
    fn a_round_reads_each_row_once_and_gives_up_on_a_silent_connection() {
        let mut config = packaged();
        // A second bridge, so one going quiet cannot silence the other.
        let (bridge, devices) = {
            let bridge = config
                .connections
                .iter()
                .find(|c| c.id.as_str() == "bridge")
                .unwrap()
                .clone();
            let devices = config.rooms[0].devices.clone();
            (bridge, devices)
        };
        config.connections.push(couch_model::Connection {
            id: "hall".into(),
            name: "Hall bridge".into(),
            provider: bridge.provider.clone(),
        });
        let mut hall = devices[0].clone();
        hall.id = "hall-lamp".into();
        hall.name = "Hall lamp".into();
        hall.integration = couch_model::Integration::Connection {
            connection_id: "hall".into(),
            resource_id: "lamp/9".into(),
            child: match &devices[0].integration {
                couch_model::Integration::Connection { child, .. } => child.clone(),
                _ => panic!("a child"),
            },
        };
        config.rooms[0].devices.push(hall);
        config.validate().unwrap();
        let mut entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let mut asked: Vec<(String, Option<String>, Duration)> = Vec::new();
        read_plugin_rows(&mut entries, &mut |connection, request, timeout| {
            asked.push((
                connection.to_owned(),
                request.resource().map(str::to_owned),
                timeout,
            ));
            match request.resource() {
                // The first lamp answers; the blind would, but the bridge has
                // already gone quiet by then.
                Some("lamp/1") => Ok(Response::Status {
                    status: reading(serde_json::json!({"light":{"on":true,"brightness":40}})),
                }),
                Some("lamp/2") => Err(couch_plugin::Error::Timeout.into()),
                Some("lamp/9") => Ok(Response::Status {
                    status: reading(serde_json::json!({"light":{"on":false,"brightness":10}})),
                }),
                _ => panic!("nothing else may be asked: {request:?}"),
            }
        });
        assert_eq!(
            asked,
            [
                ("bridge".to_string(), Some("lamp/1".into()), STATUS_TIMEOUT),
                ("bridge".to_string(), Some("lamp/2".into()), STATUS_TIMEOUT),
                ("hall".to_string(), Some("lamp/9".into()), STATUS_TIMEOUT),
            ]
        );
        assert_eq!(STATUS_TIMEOUT, Duration::from_millis(1500));
        // The rows are all still there; the ones that went unasked simply have
        // no reading, which is what "Unavailable" is drawn from.
        assert_eq!(
            entries
                .iter()
                .map(|e| (
                    e.id.as_str(),
                    e.state.as_ref().map(DeviceState::description)
                ))
                .collect::<Vec<_>>(),
            [
                ("plugin:bridge/lamp/1", Some("On · 40%".to_string())),
                ("plugin:bridge/lamp/2", None),
                ("plugin:bridge/cover/1", None),
                ("device:heat", None),
                ("device:avr", None),
                ("plugin:hall/lamp/9", Some("Off".to_string())),
            ]
        );
        // The reading is filed under the row's own id, so the state cache and
        // the row find each other.
        assert_eq!(
            entries[0].state.as_ref().map(DeviceState::id),
            Some("plugin:bridge/lamp/1")
        );
    }

    /// A child the package no longer knows refuses the read. The row stays:
    /// the device is still configured, it just cannot be reached.
    #[test]
    fn a_child_the_package_forgot_is_unavailable_and_never_removed() {
        let config = packaged();
        let mut entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let mut asked = 0;
        read_plugin_rows(&mut entries, &mut |_, _, _| {
            asked += 1;
            Err(couch_plugin::Error::Unsupported.into())
        });
        // Unsupported is not a connection failure: every row is still asked.
        assert_eq!(asked, 3);
        assert_eq!(entries.len(), 5);
        assert!(entries.iter().all(|e| e.state.is_none()));
    }

    /// `Unpaired` is not a connection failure either - every row is still
    /// asked - but it is the one refusal a plugin row remembers, so its
    /// detail can say "Needs pairing" instead of the generic "Unavailable".
    #[test]
    fn a_status_read_that_answers_unpaired_marks_only_that_row() {
        let config = packaged();
        let mut entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let mut asked = 0;
        read_plugin_rows(&mut entries, &mut |_, request, _| {
            asked += 1;
            match request.resource() {
                Some("lamp/2") => Err(couch_plugin::Error::Unpaired.into()),
                _ => Err(couch_plugin::Error::Unsupported.into()),
            }
        });
        assert_eq!(asked, 3);
        assert!(entries.iter().all(|e| e.state.is_none()));
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), e.unpaired))
                .collect::<Vec<_>>(),
            [
                ("plugin:bridge/lamp/1", false),
                ("plugin:bridge/lamp/2", true),
                ("plugin:bridge/cover/1", false),
                ("device:heat", false),
                ("device:avr", false),
            ]
        );
    }

    /// A level is written as the typed action the child's kind declares, and
    /// the state the write acknowledged is what the row then shows.
    #[test]
    fn a_level_uses_the_state_the_write_acknowledged() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let lamp = entries[0].plugin.clone().unwrap();
        let blind = entries[2].plugin.clone().unwrap();
        let mut sent = Vec::new();
        let answer = write_plugin_row(
            &lamp,
            "Desk lamp",
            Request::action(lamp.level(40)).at(&lamp.resource),
            &mut |connection, request, _| {
                sent.push((connection.to_owned(), request));
                Ok(Response::Status {
                    status: reading(serde_json::json!({"light":{"on":true,"brightness":40}})),
                })
            },
        )
        .unwrap();
        assert_eq!(
            sent,
            [(
                "bridge".to_string(),
                Request::Action {
                    action: TypedAction::SetLight {
                        on: None,
                        brightness: Some(40),
                        mirek: None,
                        xy: None
                    },
                    resource: Some("lamp/1".into())
                }
            )]
        );
        let Answer::State(state) = answer else {
            panic!("the acknowledged state")
        };
        assert_eq!(state.id(), "plugin:bridge/lamp/1");
        assert_eq!(state.description(), "On · 40%");
        // A blind's level is the other typed action, at its own child.
        assert_eq!(blind.level(40), TypedAction::SetCover { position: 40 });
        // And OK on the row is the child's own toggle, never a read first.
        assert_eq!(
            toggle_request(&lamp),
            Request::Command {
                function: "toggle".into(),
                phase: couch_model::KeyPhase::Tap,
                resource: Some("lamp/1".into())
            }
        );
    }

    /// A package that only says `Ok` is asked once, straight after, and that
    /// read is the short one.
    #[test]
    fn a_bare_ok_is_followed_by_one_read() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let lamp = entries[0].plugin.clone().unwrap();
        let mut sent: Vec<(Request, Duration)> = Vec::new();
        let answer = write_plugin_row(
            &lamp,
            "Desk lamp",
            toggle_request(&lamp),
            &mut |_, request, timeout| {
                sent.push((request.clone(), timeout));
                match request {
                    Request::Command { .. } => Ok(Response::Ok),
                    Request::Status { .. } => Ok(Response::Status {
                        status: reading(serde_json::json!({"light":{"on":true,"brightness":80}})),
                    }),
                    other => panic!("{other:?}"),
                }
            },
        )
        .unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].1, write_timeout());
        assert_eq!(sent[1], (Request::status().at("lamp/1"), STATUS_TIMEOUT));
        assert!(matches!(answer, Answer::State(state) if state.description() == "On · 80%"));
        // A reply that says nothing about this child is reported, not shown as
        // an off lamp.
        let quiet = write_plugin_row(&lamp, "Desk lamp", toggle_request(&lamp), &mut |_, _, _| {
            Ok(Response::Status {
                status: reading(serde_json::json!({"on":true})),
            })
        });
        assert_eq!(
            quiet.err().as_deref(),
            Some("The integration did not report this device")
        );
    }

    /// A busy connection is not a failure: nothing was sent, the level goes
    /// back on the queue, and the user is told nothing.
    #[test]
    fn a_busy_connection_keeps_only_the_latest_target_and_says_nothing() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let lamp = entries[0].plugin.clone().unwrap();
        let answer = write_plugin_row(
            &lamp,
            "Desk lamp",
            Request::action(lamp.level(40)).at(&lamp.resource),
            &mut |_, _, _| Err(couch_plugin::Error::Busy.into()),
        )
        .unwrap();
        assert!(matches!(answer, Answer::Busy));
        // The in-flight level goes back at the head of the queue, ahead of
        // another light's, and in front of nothing newer for itself.
        let mut queue = VecDeque::new();
        queue_level(&mut queue, "two".into(), 25);
        requeue_level(&mut queue, "one".into(), 40);
        assert_eq!(
            queue.iter().cloned().collect::<Vec<_>>(),
            [("one".to_string(), 40), ("two".to_string(), 25)]
        );
        // A newer level for the same row is already waiting: it wins, and the
        // one that came back is dropped.
        requeue_level(&mut queue, "one".into(), 10);
        assert_eq!(
            queue.into_iter().collect::<Vec<_>>(),
            [("one".to_string(), 40), ("two".to_string(), 25)]
        );
        // A refusal that is not busy is still a refusal.
        let refused =
            write_plugin_row(&lamp, "Desk lamp", toggle_request(&lamp), &mut |_, _, _| {
                Err(couch_plugin::Error::Rejected.into())
            });
        assert_eq!(
            refused.err().as_deref(),
            Some("The device refused the request")
        );
    }

    /// A packaged child's name is the package's own, and may well read like a
    /// Home Assistant entity id. It is not one: nothing here treats it as a
    /// domain, so a `climate.` child never opens the thermostat screen.
    #[test]
    fn a_child_name_is_never_read_as_a_home_assistant_domain() {
        assert_eq!(ha_domain("plugin:bridge/climate.living"), "");
        assert_eq!(ha_domain("plugin:bridge/cover.blind"), "");
        assert_eq!(ha_domain("bridge/climate.living"), "climate");
        assert_eq!(ha_domain("cover.office"), "cover");
    }

    /// One rendered room, drawn the way the panel draws it: the controller's
    /// own rows, on the 480-pixel screen. `COUCH_ROOM_SCREENSHOTS=<dir>`
    /// keeps the picture.
    fn draw_room(
        config: &couch_model::Config,
        room: &str,
        fill: impl Fn(&mut Vec<Entry>),
        name: &str,
    ) -> Vec<String> {
        use slint::{platform::WindowEvent, ComponentHandle};
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let controller = Controller::install(&app);
        let mut entries = configured_in(config, &Id::new(room)).unwrap();
        fill(&mut entries);
        let mut controller = controller;
        controller.entries = entries;
        app.set_light_title(
            config
                .room(&Id::new(room))
                .map(|r| r.name.as_str())
                .unwrap_or("Room")
                .into(),
        );
        // The Scenes button, filled the way `open_room` fills it.
        let scenes: Vec<String> = config
            .scenes
            .iter()
            .filter(|s| s.rooms.contains(&Id::new(room)))
            .map(|s| s.name.clone())
            .collect();
        app.set_light_scene_label(if scenes.len() == 1 {
            scenes[0].clone().into()
        } else {
            format!("{} scenes", scenes.len()).into()
        });
        app.set_light_scene_count(scenes.len() as i32);
        app.set_light_shown(true);
        app.set_feedback_enabled(true);
        controller.update_rows(&app, true);
        // What the rows say, read back from the model the screen is drawn
        // from, so the sentences asserted are the ones in the picture. A
        // trailing chevron is the row's hint that OK opens a screen.
        let shown: Vec<String> = app
            .get_light_items()
            .iter()
            .map(|row| {
                format!(
                    "{} - {}{}",
                    row.title,
                    row.detail,
                    if row.controls { " ›" } else { "" }
                )
            })
            .collect();
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        app.invoke_focus_light();
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(16));
        }
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        if let Some(dir) = std::env::var_os("COUCH_ROOM_SCREENSHOTS") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::Path::new(&dir).join(name),
                &bytes,
                480,
                800,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        app.hide().unwrap();
        shown
    }

    /// A room with nothing in it but the built-in Hue rows. The sentences are
    /// the ones they have always been; what is new is the chevron on the rows
    /// whose OK now opens a screen, which never replaces the state text.
    #[test]
    fn a_room_of_built_in_hue_lights_is_drawn_as_it_always_was() {
        const NAME: &str = "lights::tests::a_room_of_built_in_hue_lights_is_drawn_as_it_always_was";
        if std::env::var_os("COUCH_TEST_HUE_ROOM").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_HUE_ROOM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"bridge","name":"Hue bridge","provider":{"kind":"hue"}}],
            "rooms":[{"id":"living-room","name":"Living room","devices":[
                {"id":"desk","name":"Desk lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge","resource_id":"11111111-1111-4111-8111-111111111111"}},
                {"id":"reading","name":"Reading lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge","resource_id":"22222222-2222-4222-8222-222222222222"}},
                {"id":"corner","name":"Corner lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge","resource_id":"33333333-3333-4333-8333-333333333333"}}]}]}))
            .unwrap();
        config.validate().unwrap();
        let lamp = |id: &str, name: &str, on: Option<bool>, percent: Option<u8>| {
            DeviceState::Light(Light {
                entity_id: id.to_owned(),
                name: name.to_owned(),
                on,
                brightness_percent: percent,
                dimmable: true,
                mirek: None,
                mirek_range: None,
            })
        };
        let shown = draw_room(
            &config,
            "living-room",
            |entries| {
                entries[0].state = Some(lamp(
                    &entries[0].id.clone(),
                    "Desk lamp",
                    Some(true),
                    Some(40),
                ));
                entries[1].state = Some(lamp(
                    &entries[1].id.clone(),
                    "Reading lamp",
                    Some(false),
                    Some(70),
                ));
                // The third said nothing: unknown, never an inferred off.
                entries[2].state = None;
            },
            "room-built-in-hue.png",
        );
        assert_eq!(
            shown,
            [
                // Two lamps that dim: OK opens their screen, and the state
                // text stays where it was.
                "Desk lamp - On · 40% ›",
                "Reading lamp - Off ›",
                // The third has not answered, so OK is still the switch it
                // has always been, and there is nothing to promise.
                "Corner lamp - Unavailable"
            ]
        );
    }

    /// The same room list with packaged children in it: two lamps and a
    /// blind behind one package, drawn as ordinary rows, beside a thermostat
    /// and a receiver that keep their screens.
    #[test]
    fn a_room_draws_packaged_lamps_and_a_blind_as_ordinary_rows() {
        const NAME: &str =
            "lights::tests::a_room_draws_packaged_lamps_and_a_blind_as_ordinary_rows";
        if std::env::var_os("COUCH_TEST_PACKAGED_ROOM").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_PACKAGED_ROOM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config = packaged();
        let shown = draw_room(
            &config,
            "living-room",
            |entries| {
                for (i, status) in [
                    (
                        0,
                        Some(serde_json::json!({"light":{"on":true,"brightness":40}})),
                    ),
                    // The second lamp went unread: its bridge had already
                    // failed to answer this round.
                    (1, None),
                    (
                        2,
                        Some(serde_json::json!({"cover":{"open":true,"position":60}})),
                    ),
                ] {
                    let row = entries[i].plugin.clone().unwrap();
                    let id = entries[i].id.clone();
                    let name = entries[i].name.clone();
                    entries[i].state = status.map(|status| {
                        let mut state = row.state(&name, &reading(status)).unwrap();
                        state.set_id(id);
                        state
                    });
                }
            },
            "room-packaged-children.png",
        );
        assert_eq!(
            shown,
            [
                "Desk lamp - On · 40% ›",
                // The bridge had already failed to answer this round. A
                // packaged child still declares what it can do, so the row
                // keeps its promise even with nothing to report.
                "Reading lamp - Unavailable ›",
                "Blind - Open · 60% open ›",
                // The thermostat and the receiver keep their device rows and
                // the packaged screen behind them.
                "Heating - Press OK for controls ›",
                "Theater AVR - Press OK for controls ›"
            ]
        );
    }

    /// The panel does no pairing of its own (T3): a packaged row this
    /// round's read named `Unpaired` says so, not the generic "Unavailable"
    /// a read that simply has not answered yet leaves the other rows with.
    #[test]
    fn a_row_told_it_needs_pairing_says_so() {
        const NAME: &str = "lights::tests::a_row_told_it_needs_pairing_says_so";
        if std::env::var_os("COUCH_TEST_UNPAIRED_ROOM").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_UNPAIRED_ROOM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config = packaged();
        let shown = draw_room(
            &config,
            "living-room",
            |entries| {
                entries[1].unpaired = true;
            },
            "room-packaged-unpaired.png",
        );
        assert_eq!(
            shown,
            [
                "Desk lamp - Unavailable ›",
                "Reading lamp - Needs pairing ›",
                "Blind - Unavailable ›",
                // The thermostat and the receiver keep their device rows and
                // the packaged screen behind them.
                "Heating - Press OK for controls ›",
                "Theater AVR - Press OK for controls ›"
            ]
        );
    }

    /// Which rows the Power key switches, and which it leaves to their own
    /// bindings. Guarded like the other panel tests: one Slint platform per
    /// process.
    #[test]
    fn the_power_key_switches_light_rows_and_leaves_the_rest_alone() {
        const NAME: &str =
            "lights::tests::the_power_key_switches_light_rows_and_leaves_the_rest_alone";
        if std::env::var_os("COUCH_TEST_POWER_ROWS").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_POWER_ROWS", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let mut config = packaged();
        // An activity pinned above the devices, so the row it adds is part of
        // what the key has to see past.
        config.activities.push(
            serde_json::from_value(
                serde_json::json!({"id":"movie","name":"Movie night","room":"living-room"}),
            )
            .unwrap(),
        );
        config.validate().unwrap();
        let _window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let mut controller = Controller::install(&app);
        let room = Id::new("living-room");
        controller.entries = configured_in(&config, &room).unwrap();
        controller.room = Some(room);
        app.set_light_shown(true);
        assert_eq!(
            controller
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            [
                "activity:movie",
                "plugin:bridge/lamp/1",
                "plugin:bridge/lamp/2",
                "plugin:bridge/cover/1",
                "device:heat",
                "device:avr"
            ]
        );
        // The two lamps and the blind take it; the pinned activity, the
        // thermostat and the receiver do not - a receiver's Power is a mapped
        // binding (`activity_buttons::row_bindings`), and the other two have
        // nothing for it at all.
        for (row, takes) in [
            (0, false),
            (1, true),
            (2, true),
            (3, true),
            (4, false),
            (5, false),
            (6, false),
        ] {
            app.set_light_index(row);
            controller.input.borrow_mut().clear();
            assert_eq!(controller.power_press(&app), takes, "row {row}");
            assert_eq!(controller.input.borrow().len(), usize::from(takes));
        }
        // Off the room list the key is nobody's here: the chooser, the
        // settings sheet and the keyboard each own their own keys.
        app.set_light_index(1);
        for close in [
            |a: &crate::App| a.set_light_shown(false),
            |a: &crate::App| a.set_chooser_shown(true),
            |a: &crate::App| a.set_settings_shown(true),
            |a: &crate::App| a.set_keyboard_shown(true),
        ] {
            app.set_light_shown(true);
            app.set_chooser_shown(false);
            app.set_settings_shown(false);
            app.set_keyboard_shown(false);
            close(&app);
            assert!(!controller.power_press(&app));
        }
    }

    /// Which rows OK opens a screen on, and which keep the switch they were.
    #[test]
    fn ok_opens_a_screen_only_where_there_is_something_to_adjust() {
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        // A packaged child says what it can do in its own declaration, so it
        // is decided before any reading has arrived.
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), opens_screen(e)))
                .collect::<Vec<_>>(),
            [
                // dimmable and tunable
                ("plugin:bridge/lamp/1", true),
                // dimmable only
                ("plugin:bridge/lamp/2", true),
                // position and stop
                ("plugin:bridge/cover/1", true),
                // a thermostat and a receiver keep their own screens
                ("device:heat", false),
                ("device:avr", false),
            ]
        );
        // A child that only switches keeps OK as a toggle: an empty screen
        // would be worse than the switch it replaced.
        let mut plug = entries[0].clone();
        plug.plugin.as_mut().unwrap().snapshot.light = Some(LightTraits::default());
        assert!(!opens_screen(&plug));
        let mut tilt = entries[2].clone();
        tilt.plugin.as_mut().unwrap().snapshot.cover = Some(CoverTraits::default());
        assert!(!opens_screen(&tilt));

        // Everything else is decided by the reading. A row that has not
        // answered yet keeps the toggle it has always had.
        let mut hue = Entry {
            icon: couch_model::Icon::Lamp,
            name: "Hue lamp".into(),
            id: "hue:1".into(),
            state: None,
            hue: true,
            matter: false,
            plugin: None,
            media: false,
            activity: None,
            unpaired: false,
        };
        assert!(!opens_screen(&hue));
        let lamp = |dimmable, mirek_range| {
            Some(DeviceState::Light(Light {
                entity_id: "hue:1".into(),
                name: "Hue lamp".into(),
                on: Some(true),
                brightness_percent: Some(40),
                dimmable,
                mirek: None,
                mirek_range,
            }))
        };
        hue.state = lamp(true, None);
        assert!(opens_screen(&hue));
        hue.state = lamp(false, Some((153, 500)));
        assert!(opens_screen(&hue));
        hue.state = lamp(false, None);
        assert!(!opens_screen(&hue));
    }

    /// A colour temperature is queued, sent and requeued exactly as a level
    /// is: only the latest target per row is ever in flight, and a package
    /// that is busy for a moment costs the user nothing and says nothing.
    #[test]
    fn a_colour_temperature_keeps_the_latest_target_and_a_busy_package_says_nothing() {
        let mut queue: VecDeque<(String, u16)> = VecDeque::new();
        queue_level(&mut queue, "one".into(), 300);
        queue_level(&mut queue, "two".into(), 250);
        queue_level(&mut queue, "one".into(), 280);
        assert_eq!(
            queue.iter().cloned().collect::<Vec<_>>(),
            [("one".to_string(), 280), ("two".to_string(), 250)]
        );
        // The one that came back goes in front, and loses to anything newer
        // for the same row that is already waiting.
        requeue_level(&mut queue, "three".into(), 400);
        requeue_level(&mut queue, "one".into(), 999);
        assert_eq!(
            queue.into_iter().collect::<Vec<_>>(),
            [
                ("three".to_string(), 400),
                ("one".to_string(), 280),
                ("two".to_string(), 250)
            ]
        );

        // The write itself: the typed action the child's kind declares, at
        // that child, and a busy connection is not a failure.
        let config = packaged();
        let entries = configured_in(&config, &Id::new("living-room")).unwrap();
        let lamp = entries[0].plugin.clone().unwrap();
        let mut sent = Vec::new();
        let answer = write_plugin_row(
            &lamp,
            "Desk lamp",
            Request::action(TypedAction::SetLight {
                on: None,
                brightness: None,
                mirek: Some(336),
                xy: None,
            })
            .at(&lamp.resource),
            &mut |connection, request, _| {
                sent.push((connection.to_owned(), request));
                Err(couch_plugin::Error::Busy.into())
            },
        )
        .unwrap();
        assert!(matches!(answer, Answer::Busy));
        assert_eq!(
            sent,
            [(
                "bridge".to_string(),
                Request::Action {
                    action: TypedAction::SetLight {
                        on: None,
                        brightness: None,
                        mirek: Some(336),
                        xy: None
                    },
                    resource: Some("lamp/1".into())
                }
            )]
        );

        // And the reading that comes back carries the colour temperature the
        // child declared a range for, and nothing it did not.
        let traits = LightTraits {
            dimmable: true,
            mirek: Some((153, 500)),
            color: false,
        };
        let state: LightState =
            serde_json::from_value(serde_json::json!({"on":true,"brightness":40,"mirek":370}))
                .unwrap();
        let read = light_row("Desk lamp", Some(&traits), &state);
        assert_eq!(
            (read.mirek, read.mirek_range),
            (Some(370), Some((153, 500)))
        );
        // A mirek outside the declared range is not a reading of it.
        let odd: LightState =
            serde_json::from_value(serde_json::json!({"on":true,"mirek":600})).unwrap();
        assert_eq!(light_row("Desk lamp", Some(&traits), &odd).mirek, None);
        // A lamp that declared no range is never offered one.
        let plain = LightTraits {
            dimmable: true,
            mirek: None,
            color: false,
        };
        let no_range = light_row("Desk lamp", Some(&plain), &state);
        assert_eq!((no_range.mirek, no_range.mirek_range), (None, None));
    }

    /// The control screen behind a row, drawn the way the panel draws it.
    /// `COUCH_ROOM_SCREENSHOTS=<dir>` keeps the picture.
    fn draw_screen(
        window: &Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
        app: &crate::App,
        config: &couch_model::Config,
        room: &str,
        // The row OK opens, and how many times right is pressed once the
        // screen is up, so a picture can show a blind with a different one of
        // its three buttons highlighted.
        open: (usize, usize),
        fill: impl Fn(&mut Vec<Entry>),
        name: &str,
    ) -> Vec<String> {
        use slint::ComponentHandle;
        let mut controller = Controller::install(app);
        let mut entries = configured_in(config, &Id::new(room)).unwrap();
        fill(&mut entries);
        controller.entries = entries;
        controller.room = Some(Id::new(room));
        app.set_light_title(
            config
                .room(&Id::new(room))
                .map(|r| r.name.as_str())
                .unwrap_or("Room")
                .into(),
        );
        app.set_light_shown(true);
        app.set_feedback_enabled(true);
        controller.update_rows(app, true);
        app.set_light_index(open.0 as i32);
        controller.open_screen(app, open.0);
        for _ in 0..open.1 {
            controller.screen_action(app, "button", 1);
        }
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(16));
        }
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        // The page drew something other than the flat background.
        assert!(
            pixels.iter().any(|p| *p != pixels[0]),
            "{name}: the screen drew nothing"
        );
        if let Some(dir) = std::env::var_os("COUCH_ROOM_SCREENSHOTS") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::Path::new(&dir).join(name),
                &bytes,
                480,
                800,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        let _ = app.show();
        vec![
            app.get_light_screen_title().to_string(),
            app.get_light_screen_room().to_string(),
            app.get_light_screen_state().to_string(),
            app.get_light_screen_level_label().to_string(),
            app.get_light_screen_level().to_string(),
            app.get_light_screen_kelvin().to_string(),
            app.get_light_screen_detail().to_string(),
            app.get_light_screen_hint().to_string(),
        ]
    }

    /// Every shape the one screen has to take: a lamp that dims and tunes, one
    /// that only dims, one that is not answering, and a blind.
    #[test]
    fn the_light_screen_is_one_screen_for_every_shape_of_device() {
        const NAME: &str =
            "lights::tests::the_light_screen_is_one_screen_for_every_shape_of_device";
        if std::env::var_os("COUCH_TEST_LIGHT_SCREEN").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_LIGHT_SCREEN", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{platform::WindowEvent, ComponentHandle};
        let config = packaged();
        // The screen names the room and the connection its readings come
        // from, both of which it reads from the live snapshot.
        let path = std::env::temp_dir().join(format!(
            "couch-light-screen-{}-{}.json",
            std::process::id(),
            NAME.len()
        ));
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        crate::config_snapshot::start(path.clone());
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        let state = |entries: &mut Vec<Entry>, i: usize, status: serde_json::Value| {
            let row = entries[i].plugin.clone().unwrap();
            let id = entries[i].id.clone();
            let name = entries[i].name.clone();
            let mut state = row.state(&name, &reading(status)).unwrap();
            state.set_id(id);
            entries[i].state = Some(state);
        };

        // A lamp that dims and tunes: on at 40 %, 2700 K.
        assert_eq!(
            draw_screen(
                &window,
                &app,
                &config,
                "living-room",
                (0, 0),
                |entries| state(
                    entries,
                    0,
                    serde_json::json!({"light":{"on":true,"brightness":40,"mirek":370}})
                ),
                "light-screen-1-dimmable-and-tunable.png",
            ),
            [
                "Desk lamp",
                "Living room · Hue bridge",
                "On · 40%",
                "BRIGHTNESS",
                "40%",
                "2700 K",
                "",
                "Vol: brightness · Ch: warmth · Power: on/off"
            ]
        );
        // A lamp that only dims: no colour temperature is offered at all.
        assert_eq!(
            draw_screen(
                &window,
                &app,
                &config,
                "living-room",
                (1, 0),
                |entries| state(
                    entries,
                    1,
                    serde_json::json!({"light":{"on":true,"brightness":70}})
                ),
                "light-screen-2-dimmable-only.png",
            ),
            [
                "Reading lamp",
                "Living room · Hue bridge",
                "On · 70%",
                "BRIGHTNESS",
                "70%",
                "—",
                "",
                "Vol: brightness · Power: on/off · Back: room"
            ]
        );
        // One that is not answering: no level, no colour, no invented off.
        assert_eq!(
            draw_screen(
                &window,
                &app,
                &config,
                "living-room",
                (1, 0),
                |_| {},
                "light-screen-3-unavailable.png",
            ),
            [
                "Reading lamp",
                "Living room · Hue bridge",
                "Unavailable",
                "BRIGHTNESS",
                "—",
                "—",
                "",
                "Vol: brightness · Power: on/off · Back: room"
            ]
        );
        // A blind: the same screen with a position on it and its own buttons.
        assert_eq!(
            draw_screen(
                &window,
                &app,
                &config,
                "living-room",
                (2, 0),
                |entries| state(
                    entries,
                    2,
                    serde_json::json!({"cover":{"open":true,"position":60}})
                ),
                "light-screen-4-blind.png",
            ),
            [
                "Blind",
                "Living room · Hue bridge",
                "Open · 60% open",
                "OPEN POSITION",
                "60%",
                "—",
                "",
                "Vol: position · Power: open/close · Back: room"
            ]
        );
        // The same blind with right pressed once: Stop is highlighted, which
        // is all left and right have left to move now that no bar is selected.
        assert_eq!(
            draw_screen(
                &window,
                &app,
                &config,
                "living-room",
                (2, 1),
                |entries| state(
                    entries,
                    2,
                    serde_json::json!({"cover":{"open":true,"position":60}})
                ),
                "light-screen-5-blind-stop-highlighted.png",
            ),
            [
                "Blind",
                "Living room · Hue bridge",
                "Open · 60% open",
                "OPEN POSITION",
                "60%",
                "—",
                "",
                "Vol: position · Power: open/close · Back: room"
            ]
        );
        app.hide().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// The keys on the open screen, which no longer has a highlight to move.
    ///
    /// Volume and up and down are the level bar, channel is the colour
    /// temperature, and neither bar is ever selected - there is nothing to
    /// select between, because each has a key of its own. Left and right have
    /// only a blind's three buttons to walk. Everything goes through the same
    /// optimistic queue the rows use, so a held key leaves one target behind,
    /// not a press-by-press backlog.
    #[test]
    fn volume_is_the_level_channel_is_the_warmth_and_no_bar_is_ever_selected() {
        const NAME: &str =
            "lights::tests::volume_is_the_level_channel_is_the_warmth_and_no_bar_is_ever_selected";
        if std::env::var_os("COUCH_TEST_SCREEN_KEYS").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_SCREEN_KEYS", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config = packaged();
        let _window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let mut controller = Controller::install(&app);
        let room = Id::new("living-room");
        let mut entries = configured_in(&config, &room).unwrap();
        let read = |entries: &mut Vec<Entry>, i: usize, status: serde_json::Value| {
            let row = entries[i].plugin.clone().unwrap();
            let id = entries[i].id.clone();
            let name = entries[i].name.clone();
            let mut state = row.state(&name, &reading(status)).unwrap();
            state.set_id(id);
            entries[i].state = Some(state);
        };
        read(
            &mut entries,
            0,
            serde_json::json!({"light":{"on":true,"brightness":40,"mirek":370}}),
        );
        read(
            &mut entries,
            1,
            serde_json::json!({"light":{"on":true,"brightness":70}}),
        );
        read(
            &mut entries,
            2,
            serde_json::json!({"cover":{"open":true,"position":60}}),
        );
        controller.entries = entries;
        controller.room = Some(room);
        app.set_light_shown(true);
        let levels = |c: &Controller| c.brightness_pending.iter().cloned().collect::<Vec<_>>();
        let mireks = |c: &Controller| c.mirek_pending.iter().cloned().collect::<Vec<_>>();

        // The lamp that dims and tunes.
        controller.open_screen(&app, 0);
        let lamp = "plugin:bridge/lamp/1".to_string();
        // Volume, and up and down, are the same bar and the same step the rows
        // use. A held key coalesces: one target, the last.
        controller.screen_action(&app, "level", 1);
        assert_eq!(levels(&controller), [(lamp.clone(), 45)]);
        controller.screen_action(&app, "level", 1);
        controller.screen_action(&app, "level", 1);
        assert_eq!(levels(&controller), [(lamp.clone(), 55)]);
        controller.screen_action(&app, "level", -1);
        assert_eq!(levels(&controller), [(lamp.clone(), 50)]);
        // The state line follows the target, so it never contradicts the bar
        // while the write is out.
        assert_eq!(app.get_light_screen_state(), "On · 50%");
        assert_eq!(app.get_light_screen_level(), "50%");

        // Channel is the colour temperature, a twentieth of the lamp's
        // 153..500 range a press, and it too keeps only the latest target.
        controller.screen_action(&app, "warmth", 1);
        assert_eq!(mireks(&controller), [(lamp.clone(), 353)]);
        controller.screen_action(&app, "warmth", 1);
        assert_eq!(mireks(&controller), [(lamp.clone(), 336)]);
        controller.screen_action(&app, "warmth", -1);
        assert_eq!(mireks(&controller), [(lamp.clone(), 353)]);
        // Held to either end, it stops at the lamp's own limits.
        for _ in 0..40 {
            controller.screen_action(&app, "warmth", 1);
        }
        assert_eq!(mireks(&controller), [(lamp.clone(), 153)]);
        for _ in 0..40 {
            controller.screen_action(&app, "warmth", -1);
        }
        assert_eq!(mireks(&controller), [(lamp.clone(), 500)]);
        assert_eq!(app.get_light_screen_detail(), "");

        // Left and right have nothing to move on a lamp: no highlight exists,
        // and neither key adjusts anything by accident.
        for index in [1, -1, 1, 1] {
            controller.screen_action(&app, "button", index);
        }
        assert_eq!(controller.screen_button, 0);
        assert_eq!(levels(&controller), [(lamp.clone(), 50)]);
        assert_eq!(mireks(&controller), [(lamp, 500)]);

        // A lamp that only dims has no colour temperature, so the channel keys
        // do nothing at all - not even a sentence saying so.
        controller.brightness_pending.clear();
        controller.mirek_pending.clear();
        controller.open_screen(&app, 1);
        let plain = "plugin:bridge/lamp/2".to_string();
        controller.screen_action(&app, "warmth", 1);
        controller.screen_action(&app, "warmth", -1);
        assert!(mireks(&controller).is_empty());
        assert_eq!(app.get_light_screen_detail(), "");
        controller.screen_action(&app, "level", 1);
        assert_eq!(levels(&controller), [(plain, 75)]);

        // A blind: volume is its open position, and left and right walk its
        // three buttons and go no further. Open is highlighted to begin with,
        // so OK always has a button to press.
        controller.brightness_pending.clear();
        controller.mirek_pending.clear();
        controller.open_screen(&app, 2);
        let blind = "plugin:bridge/cover/1".to_string();
        assert_eq!(controller.screen_button, 0);
        controller.screen_action(&app, "level", 1);
        assert_eq!(levels(&controller), [(blind.clone(), 65)]);
        assert_eq!(app.get_light_screen_state(), "Open · 65% open");
        controller.screen_action(&app, "warmth", 1);
        assert!(mireks(&controller).is_empty());
        for (index, button) in [(1, 1), (1, 2), (1, 2), (-1, 1), (-1, 0), (-1, 0)] {
            controller.screen_action(&app, "button", index);
            assert_eq!(controller.screen_button, button);
            assert_eq!(app.get_light_screen_button(), button);
        }
        // The bar keeps moving whichever button is highlighted.
        controller.screen_action(&app, "button", 1);
        controller.screen_action(&app, "level", 1);
        assert_eq!(levels(&controller), [(blind, 70)]);
        assert_eq!(controller.screen_button, 1);
    }

    /// "Updating…" is for a write that has stopped answering, not for one in
    /// the ordinary run of things.
    ///
    /// A packaged lamp answers each write in about 150 ms and chains them
    /// under a held key; the bar and the read-out have already moved, so the
    /// state line has nothing to add and must keep saying what the lamp is.
    #[test]
    fn the_state_line_only_says_updating_once_a_write_has_stopped_answering() {
        const NAME: &str =
            "lights::tests::the_state_line_only_says_updating_once_a_write_has_stopped_answering";
        if std::env::var_os("COUCH_TEST_SCREEN_UPDATING").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_SCREEN_UPDATING", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config = packaged();
        let _window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let mut controller = Controller::install(&app);
        let room = Id::new("living-room");
        let mut entries = configured_in(&config, &room).unwrap();
        let row = entries[0].plugin.clone().unwrap();
        let id = entries[0].id.clone();
        let name = entries[0].name.clone();
        let mut state = row
            .state(
                &name,
                &reading(serde_json::json!({"light":{"on":true,"brightness":40,"mirek":370}})),
            )
            .unwrap();
        state.set_id(id.clone());
        entries[0].state = Some(state);
        controller.entries = entries;
        controller.room = Some(room);
        app.set_light_shown(true);
        controller.open_screen(&app, 0);
        assert!(!app.get_light_screen_pending());

        // A press, and the write it sends: the bar and the state line have
        // both already moved, and neither says "Updating…".
        controller.screen_action(&app, "level", 1);
        controller.send_brightness();
        assert_eq!(controller.busy.as_deref(), Some(id.as_str()));
        assert_eq!(app.get_light_screen_state(), "On · 45%");
        assert!(!app.get_light_screen_pending());
        let sent = controller.busy_since.unwrap();

        // A second and a half later it still has not answered, and the label
        // arrives with nothing else on screen having changed - which is what
        // the redraw key had to be widened for.
        controller.render_screen_at(&app, sent + Duration::from_millis(1499));
        assert!(!app.get_light_screen_pending());
        controller.render_screen_at(&app, sent + Duration::from_millis(1500));
        assert!(app.get_light_screen_pending());
        // The state line itself is untouched: the screen swaps the text, so
        // what it swaps back to has to stay right.
        assert_eq!(app.get_light_screen_state(), "On · 45%");

        // The answer takes the label away again, just as quietly.
        controller.release();
        controller.render_screen_at(&app, sent + Duration::from_millis(1600));
        assert!(!app.get_light_screen_pending());

        // A write out for another row is not this screen's business.
        controller.claim("plugin:bridge/lamp/2".into());
        let other = controller.busy_since.unwrap();
        controller.render_screen_at(&app, other + Duration::from_secs(9));
        assert!(!app.get_light_screen_pending());
    }

    /// The whole way in and out: OK on a row opens its screen, Power still
    /// switches the device from there, and Back returns to the room with the
    /// same row highlighted.
    #[test]
    fn the_screen_opens_on_ok_and_back_returns_to_the_same_row() {
        const NAME: &str = "lights::tests::the_screen_opens_on_ok_and_back_returns_to_the_same_row";
        if std::env::var_os("COUCH_TEST_SCREEN_ROUND_TRIP").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_SCREEN_ROUND_TRIP", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let config = packaged();
        let path = std::env::temp_dir().join(format!(
            "couch-light-round-trip-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        crate::config_snapshot::start(path.clone());
        let _window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let mut controller = Controller::install(&app);
        controller.open_room(&app, Id::new("living-room"));
        // The blind, two rows down: the row the user is standing on has to be
        // the row they come back to.
        app.set_light_index(2);
        app.invoke_light_activate(2);
        controller.poll(&app);
        assert!(app.get_light_screen_shown());
        assert_eq!(app.get_light_screen_title(), "Blind");
        assert!(app.get_light_screen_cover());
        // Power on the open screen is the screen's device, not the room's.
        assert!(controller.power_press(&app));
        controller.input.borrow_mut().clear();
        // Back: the room again, with the blind still highlighted.
        app.invoke_light_screen_action("close".into(), 0);
        controller.poll(&app);
        assert!(!app.get_light_screen_shown());
        assert!(app.get_light_shown());
        assert_eq!(app.get_light_index(), 2);

        // The receiver two rows further on keeps the screen it had: OK there
        // opens the packaged control screen, not this one.
        app.set_light_index(4);
        app.invoke_light_activate(4);
        controller.poll(&app);
        assert!(!app.get_light_screen_shown());
        let _ = std::fs::remove_file(&path);
    }

    /// The sentence that teaches the new hold has to read well on the toast of
    /// the 480-pixel panel, under a real room list with a pinned activity on
    /// it. `COUCH_ROOM_SCREENSHOTS=<dir>` keeps the picture.
    #[test]
    fn the_hold_power_hint_renders_over_the_room_list() {
        const NAME: &str = "lights::tests::the_hold_power_hint_renders_over_the_room_list";
        if std::env::var_os("COUCH_TEST_POWER_TOAST").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_POWER_TOAST", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{platform::WindowEvent, ComponentHandle};
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[{"id":"bridge","name":"Hue bridge","provider":{"kind":"hue"}}],
            "rooms":[{"id":"den","name":"Den","devices":[
                {"id":"desk","name":"Desk lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge","resource_id":"11111111-1111-4111-8111-111111111111"}},
                {"id":"corner","name":"Corner lamp","kind":"light","integration":{"via":"connection","connection_id":"bridge","resource_id":"22222222-2222-4222-8222-222222222222"}}]}],
            "activities":[{"id":"movie","name":"Movie night","room":"den","kind":"video"}]})).unwrap();
        config.validate().unwrap();
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let controller = Controller::install(&app);
        let mut entries = configured_in(&config, &Id::new("den")).unwrap();
        let lamp = |id: &str, on: Option<bool>, percent: Option<u8>| {
            Some(DeviceState::Light(Light {
                entity_id: id.to_owned(),
                name: String::new(),
                on,
                brightness_percent: percent,
                dimmable: true,
                mirek: None,
                mirek_range: None,
            }))
        };
        entries[1].state = lamp(&entries[1].id.clone(), Some(true), Some(40));
        entries[2].state = lamp(&entries[2].id.clone(), Some(false), Some(70));
        let mut controller = controller;
        controller.entries = entries;
        app.set_light_title("Den".into());
        app.set_light_scene_label("0 scenes".into());
        app.set_light_shown(true);
        app.set_feedback_enabled(true);
        controller.update_rows(&app, true);
        // The activity is running and the highlighted row is the pinned
        // activity itself, which has nothing for Power: this is the one case
        // that raises the hint.
        app.set_light_index(0);
        app.set_activity_running(true);
        app.set_active_activity("movie".into());
        let hint = crate::hold_to_end(Some(&config), "movie");
        assert_eq!(hint, "Hold Power to end Movie night");
        app.set_toast(hint.as_str().into());
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        app.invoke_focus_light();
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(16));
        }
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        // The bar is up: its band is not the page background all the way across.
        let band = &pixels[720 * 480..760 * 480];
        assert!(band.iter().any(|p| *p != band[0]), "the toast drew nothing");
        if let Some(dir) = std::env::var_os("COUCH_ROOM_SCREENSHOTS") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::Path::new(&dir).join("room-hold-power-hint.png"),
                &bytes,
                480,
                800,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        app.hide().unwrap();
    }

    #[test]
    fn navigation_cache_expires() {
        let mut c = StateCache::default();
        c.put(DeviceState::Light(Light {
            entity_id: "light.test".into(),
            name: "Test".into(),
            on: Some(true),
            brightness_percent: None,
            dimmable: true,
            mirek: None,
            mirek_range: None,
        }));
        assert!(c.get("light.test").is_some());
        c.0.get_mut("light.test").unwrap().0 = Instant::now() - Duration::from_secs(6);
        assert!(c.get("light.test").is_none());
    }
}
