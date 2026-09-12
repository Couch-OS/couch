//! Room light controls. Network requests run on one worker, never on Slint's thread.
use crate::{App, ChoiceItem};
use couch_ha::{Climate, Command, Cover, CoverCommand, Light};
use couch_model::{Id, Integration};
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
    crate::connections::split(id)
        .1
        .split_once('.')
        .map(|(domain, _)| domain)
        .unwrap_or("")
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
}
fn configured(room: &Id) -> Result<Vec<Entry>, String> {
    let config = crate::connections::config().ok_or("Cannot read your rooms")?;
    let room = config
        .room(room)
        .ok_or("This room was removed; return home to reload")?;
    let mut entries: Vec<Entry> = room
        .devices
        .iter()
        .filter_map(
            |d| match config.resolve_integration(&d.integration).as_ref() {
                Some(Integration::HomeAssistant { entity_id })
                    if matches!(ha_domain(entity_id), "light" | "cover" | "climate") =>
                {
                    Some(Entry {
                        name: d.name.clone(),
                        icon: d.effective_icon(),
                        id: entity_id.clone(),
                        state: None,
                        hue: false,
                    })
                }
                Some(Integration::Hue { light_id }) if !light_id.starts_with("scene:") => {
                    Some(Entry {
                        name: d.name.clone(),
                        icon: d.effective_icon(),
                        id: format!("hue:{light_id}"),
                        state: None,
                        hue: true,
                    })
                }
                _ => Some(Entry {
                    name: d.name.clone(),
                    icon: d.effective_icon(),
                    id: format!("device:{}", d.id),
                    state: None,
                    hue: false,
                }),
            },
        )
        .collect();
    entries.extend(
        config
            .activities
            .iter()
            .filter(|a| a.room == room.id)
            .map(|a| Entry {
                icon: couch_model::Icon::Tv,
                name: a.name.clone(),
                id: format!("activity:{}", a.id),
                state: None,
                hue: false,
            }),
    );
    Ok(entries)
}
fn toggle_command(state: &Light) -> Result<Command, String> {
    match state.on {
        Some(true) => Ok(Command::Off),
        Some(false) => Ok(Command::On),
        None => Err("This light is unavailable".into()),
    }
}
fn perform(
    room: &Id,
    operation: Operation,
    hue: &crate::connections::HueFleet,
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
                .any(|e| !e.hue && !e.id.starts_with("device:"))
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
            for e in &mut entries {
                let states = if e.hue { &hue_states } else { &ha_states };
                let id = e.id.strip_prefix("hue:").unwrap_or(&e.id);
                e.state = states.iter().find(|s| s.id() == id).cloned().map(|mut s| {
                    s.set_id(e.id.clone());
                    s
                });
            }
            Ok(Answer::List(entries))
        }
        Operation::Brightness(id, percent) => {
            if !entries.iter().any(|e| e.id == id) {
                return Err("This device was removed from the room".into());
            }
            let mut state = if let Some(raw) = id.strip_prefix("hue:") {
                DeviceState::Light(hue.brightness(raw, percent).map_err(|e| e.to_string())?)
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
            if !entries.iter().any(|e| e.id == id) {
                return Err("This device was removed from the room".into());
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
        let active = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_active = active.clone();
        std::thread::spawn(move || {
            let _ = worker_hue.lights(); // Warm the cache without delaying GUI startup.
            while let Ok((generation, room, op)) = requests.recv() {
                let _ = events.send((
                    generation,
                    perform(&room, op, &worker_hue, &|| {
                        worker_active.load(std::sync::atomic::Ordering::SeqCst) == generation
                    }),
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
        } else if e.id.starts_with("activity:") {
            "Open activity".into()
        } else if e.id.starts_with("device:") {
            "Press OK for controls".into()
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
            light: !e.id.starts_with("device:"),
            active: e.state.as_ref().is_some_and(|s| s.active() == Some(true)),
            power_known: e.state.as_ref().is_some_and(|s| s.active().is_some()),
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
                c.room(room)
                    .and_then(|r| r.devices.get(i))
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
            crate::connections::config().and_then(|c| {
                c.room(room)
                    .and_then(|r| r.devices.get(i))
                    .map(|d| format!("device:{}", d.id))
            })
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
                        let camera = cfg.as_ref().is_some_and(|c|c.devices().find(|(_,d)|d.id.as_str()==e.id.trim_start_matches("device:")).and_then(|(_,d)|c.resolve_integration(&d.integration)).is_some_and(|i|matches!(i,Integration::UnifiProtect{..})));
                        if camera {app.invoke_open_camera(e.id.as_str().into(),e.name.as_str().into());continue;}
                        let kodi = cfg.as_ref().is_some_and(|c| {
                            c.devices()
                                .find(|(_, d)| d.id.as_str() == e.id.trim_start_matches("device:"))
                                .map(|(_, d)| d)
                                .and_then(|d| c.resolve_integration(&d.integration))
                                .is_some_and(|i| matches!(i, Integration::Kodi { .. }))
                        });
                        if kodi {
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
// Resolve the selected device's own connection; never select the first Android
// TV when multiple TVs are configured.
fn tv_connection(config: &couch_model::Config, device_id: &str) -> Option<String> {
    let (_, device) = config.devices().find(|(_, d)| d.id.as_str() == device_id)?;
    let integration = config.resolve_integration(&device.integration);
    if device.effective_ir_codeset(config).is_some()
        && matches!(
            integration,
            Some(Integration::None | Integration::Ir { .. }) | None
        )
    {
        return Some(format!("ir:{device_id}"));
    }
    let provider = match integration? {
        Integration::Sonos {..} => return Some(format!("sonos:{device_id}")),
        Integration::WebOs => couch_model::Provider::WebOs,
        Integration::AndroidTv => couch_model::Provider::AndroidTv,
        Integration::AppleTv => couch_model::Provider::AppleTv,
        Integration::Ir { .. } => return Some(format!("ir:{device_id}")),
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
