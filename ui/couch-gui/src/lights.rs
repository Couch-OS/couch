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
    Light {
        entity_id: String::new(),
        name: name.to_owned(),
        on: state.on,
        brightness_percent: state.brightness,
        dimmable: traits.is_some_and(|traits| traits.dimmable),
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
}
enum Operation {
    IrCheck(Id, String, bool, Arc<couch_model::Config>, Input),
    List,
    Toggle(String),
    Brightness(String, u8),
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
    Open(Id),
    Pick(usize),
    Brightness(usize, i32),
    Back,
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
    refreshing: bool,
    last_refresh: Instant,
    brightness_pending: VecDeque<(String, u8)>,
    position_targets: HashMap<String, (Instant, u8)>,
    brightness_flight: Option<(String, u8)>,
    brightness_until: Option<Instant>,
    last_brightness_send: Instant,
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
            refreshing: false,
            last_refresh: Instant::now(),
            brightness_pending: VecDeque::new(),
            position_targets: HashMap::new(),
            brightness_flight: None,
            brightness_until: None,
            last_brightness_send: Instant::now() - Duration::from_secs(1),
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
        let detail = if !e.hue && self.busy.as_deref() == Some(&e.id) {
            "Updating…".into()
        } else if let Some((_, caption)) = &e.activity {
            caption.clone()
        } else if e.id.starts_with("device:") {
            device_row_detail(crate::connections::config().as_deref(), &e.id)
        } else {
            e.state
                .as_ref()
                .map(DeviceState::description)
                .unwrap_or_else(|| {
                    if self.refreshing {
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
        self.ir_pending.clear();
        self.position_targets.clear();
        self.brightness_flight = None;
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
                self.busy = Some("ir-command".into());
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
        let target = self
            .brightness_pending
            .iter()
            .find(|(id, _)| id == &entry.id)
            .or_else(|| {
                self.brightness_flight
                    .as_ref()
                    .filter(|(id, _)| id == &entry.id)
            })
            .map(|(_, p)| *p)
            .or_else(|| {
                // Cover reports describe physical motion, not the requested endpoint.
                // Keep quick presses relative to our last endpoint while it travels.
                self.position_targets
                    .get(&entry.id)
                    .filter(|(at, _)| at.elapsed() < Duration::from_secs(10))
                    .map(|(_, target)| *target)
            });
        let result = match state {
            DeviceState::Light(s) => brightness_step(s, target, delta),
            DeviceState::Cover(s) => cover_step(s, target, delta),
            DeviceState::Climate(_) => return,
        };
        match result {
            Ok(percent) => {
                queue_brightness(&mut self.brightness_pending, entry.id.clone(), percent);
                if matches!(state, DeviceState::Cover(_)) {
                    self.position_targets
                        .insert(entry.id.clone(), (Instant::now(), percent));
                }
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
                app.set_light_detail("".into());
                self.brightness_until = Some(Instant::now() + Duration::from_secs(1));
            }
            Err(error) => {
                app.set_light_detail(error.into());
            }
        }
    }
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
        let Some((id, percent)) = self.brightness_pending.front().cloned() else {
            return;
        };
        let generation = self.generation + 1;
        if self
            .tx
            .try_send((generation, room, Operation::Brightness(id.clone(), percent)))
            .is_ok()
        {
            self.generation = generation;
            self.active
                .store(self.generation, std::sync::atomic::Ordering::SeqCst);
            self.busy = Some(id.clone());
            self.brightness_flight = Some((id, percent));
            self.brightness_pending.pop_front();
            self.last_brightness_send = Instant::now();
        }
    }
    fn open_room(&mut self, app: &App, room: Id) {
        self.clear_brightness(app);
        let started = Instant::now();
        self.generation += 1;
        self.active
            .store(self.generation, std::sync::atomic::Ordering::SeqCst);
        self.room = Some(room.clone());
        self.busy = None;
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
                Input::Open(room) => self.open_room(app, room),
                Input::Back => {
                    self.clear_brightness(app);
                    self.generation += 1;
                    self.active
                        .store(self.generation, std::sync::atomic::Ordering::SeqCst);
                    self.room = None;
                    self.busy = None;
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
                    let id = e.id.clone();
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
                        self.busy = Some(id);
                        self.refreshing = false;
                        app.set_light_detail("".into());
                        self.update_rows(app, false);
                    } else {
                        app.set_light_detail("Connection busy. Press OK again in a moment.".into());
                    }
                }
            }
        }
        while let Ok((generation, result)) = self.rx.try_recv() {
            if generation != self.generation || self.room.is_none() {
                continue;
            }
            self.last_refresh = Instant::now();
            self.refreshing = false;
            self.busy = None;
            let brightness = self.brightness_flight.take();
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
                        requeue_brightness(&mut self.brightness_pending, id, percent);
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
                    app.set_light_detail(error.into());
                    self.update_rows(app, false);
                }
            }
        }
        self.send_ir();
        self.send_brightness();
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
// Retain only the latest unsent level per device, preserving device order.
fn queue_brightness(queue: &mut VecDeque<(String, u8)>, id: String, percent: u8) {
    if let Some((_, target)) = queue.iter_mut().find(|(pending, _)| pending == &id) {
        *target = percent;
    } else {
        queue.push_back((id, percent));
    }
}
/// Put an unsent level back at the head of the queue. A newer level for the
/// same row is already waiting there and wins: only the latest target is ever
/// sent.
fn requeue_brightness(queue: &mut VecDeque<(String, u8)>, id: String, percent: u8) {
    if !queue.iter().any(|(pending, _)| pending == &id) {
        queue.push_front((id, percent));
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
fn cover_description(cover: &Cover) -> String {
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
fn description(light: &Light) -> String {
    match light.on {
        None => "Unavailable".into(),
        Some(false) => "Off".into(),
        Some(true) => light
            .brightness_percent
            .map(|p| format!("On · {p}%"))
            .unwrap_or_else(|| "On".into()),
    }
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
        // It still has a row, and no screen of its own to open.
        let entries = configured_in(&config, &Id::new("r")).unwrap();
        assert!(entries.iter().any(|e| e.id == "device:avr"));
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
        queue_brightness(&mut queue, "one".into(), 55);
        queue_brightness(&mut queue, "two".into(), 25);
        queue_brightness(&mut queue, "one".into(), 60);
        queue_brightness(&mut queue, "one".into(), 65);
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
        queue_brightness(&mut queue, "two".into(), 25);
        requeue_brightness(&mut queue, "one".into(), 40);
        assert_eq!(
            queue.iter().cloned().collect::<Vec<_>>(),
            [("one".to_string(), 40), ("two".to_string(), 25)]
        );
        // A newer level for the same row is already waiting: it wins, and the
        // one that came back is dropped.
        requeue_brightness(&mut queue, "one".into(), 10);
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
        // from, so the sentences asserted are the ones in the picture.
        let shown: Vec<String> = app
            .get_light_items()
            .iter()
            .map(|row| format!("{} - {}", row.title, row.detail))
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

    /// A room with nothing in it but the built-in Hue rows: the picture that
    /// must not move when packaged children exist. Run on the commit before
    /// this change and on it, the two files are byte for byte the same.
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
                "Desk lamp - On · 40%",
                "Reading lamp - Off",
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
                "Desk lamp - On · 40%",
                // The bridge had already failed to answer this round.
                "Reading lamp - Unavailable",
                "Blind - Open · 60% open",
                // The thermostat and the receiver keep their device rows and
                // the packaged screen behind them.
                "Heating - Press OK for controls",
                "Theater AVR - Press OK for controls"
            ]
        );
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
        }));
        assert!(c.get("light.test").is_some());
        c.0.get_mut("light.test").unwrap().0 = Instant::now() - Duration::from_secs(6);
        assert!(c.get("light.test").is_none());
    }
}
