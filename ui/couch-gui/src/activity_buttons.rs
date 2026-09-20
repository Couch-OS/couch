//! Activity overrides: physical timing on the UI thread, device I/O on a worker.
use crate::{connections, keypad::Press, App};
use couch_model::commands::{Function as F, KeyPhase};
use couch_model::{
    buttons::{Binding, Button, Gesture},
    Action, Config, Integration,
};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
const HOLD: Duration = Duration::from_millis(600);
struct Pending {
    down: Press,
    at: Instant,
    fired: bool,
}
struct Request {
    generation: u64,
    at: Instant,
    config: Arc<Config>,
    action: Action,
    repeat: bool,
    /// The binding that matched, short or long: with `repeat`, how the key was
    /// pressed, for a package that asked to be told.
    gesture: Gesture,
    /// Which hold a repeat belongs to. Releasing the key starts a new one, and
    /// a repeat from a hold that has ended is never sent: the queue may still
    /// hold several, and a volume that keeps rising after the finger has left
    /// the key is the one failure this path must not have.
    hold: u64,
}
/// A volume level read back from a device after a volume or mute command,
/// for the volume card. `level` is 0..=100 where the device has such a scale;
/// `text` replaces the number when set (a dB reading, or "Muted").
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeReading {
    pub target: String,
    pub level: i32,
    pub text: String,
}
/// What a mapped command left behind: nothing, or a volume reading to show.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Outcome {
    pub volume: Option<VolumeReading>,
    /// What a power toggle decided, for the feedback card.
    pub notice: Option<Notice>,
    /// A held key's level was not read back, to keep the hold fast: read it
    /// once the device's lane goes quiet.
    pub settle: bool,
    /// The decibel level a read-back observed, in tenths, for the lane to
    /// predict a held key's steps from.
    pub observed_db: Option<i16>,
}
/// A line for the shared feedback card: "Power" over the device, "Off" beside.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Notice {
    pub caption: String,
    pub target: String,
    pub value: String,
}
/// A room row's Power key. Not a catalogue function: receivers and most TVs
/// offer power-on and power-off, not a toggle, so the press is resolved against
/// the device's observed state when it is sent (`power_toggle`).
pub(crate) const POWER_TOGGLE: &str = "power-toggle";
/// A held volume key's absolute write, `set-volume-db:<tenths>`. Like
/// `POWER_TOGGLE` it is made here, never configured: the lane turns a repeat
/// into it once it knows the device's level and declared scale.
pub(crate) const SET_VOLUME_DB: &str = "set-volume-db:";
const HOLD_UP_TENTHS: i16 = 10;
const HOLD_DOWN_TENTHS: i16 = 20;
/// The context prefix of a highlighted room row, as opposed to an activity id.
const ROW: &str = "row:";

/// The Kodi player call behind a transport key, and nothing for any other
/// function: a command Kodi does not have must never become one it does.
fn kodi_player_call(command: &F) -> Option<(&'static str, serde_json::Value)> {
    match command {
        F::PlayPause => Some(("Player.PlayPause", json!({}))),
        F::Stop => Some(("Player.Stop", json!({}))),
        F::Next => Some(("Player.GoTo", json!({"to":"next"}))),
        F::Previous => Some(("Player.GoTo", json!({"to":"previous"}))),
        _ => None,
    }
}

/// What the volume, mute and power keys do while this device's row is
/// highlighted in a room: the same commands an activity would map them to, for
/// whatever the device supports. Sonos rows keep their own controller
/// (`room_sonos`, which coalesces a held key into one write), and a switchable
/// row - a light, a cover - has no use for these keys.
pub(crate) fn row_bindings(config: &Config, device: &couch_model::Device) -> Vec<Binding> {
    if config.can_toggle(device)
        || matches!(
            config.resolve_integration(&device.integration),
            Some(Integration::Sonos { .. })
        )
    {
        return vec![];
    }
    let supports = |id: &str| F::parse(id).is_some_and(|f| f.supports_device(device, config));
    let bind = |button, command: &str| Binding {
        button,
        gesture: Gesture::Short,
        action: Some(Action::new(device.id.clone(), command)),
    };
    let mut bindings: Vec<Binding> = [
        (Button::VolumeUp, "volume-up"),
        (Button::VolumeDown, "volume-down"),
        (Button::Mute, "mute"),
    ]
    .into_iter()
    .filter(|(_, command)| supports(command))
    .map(|(button, command)| bind(button, command))
    .collect();
    if supports("toggle") || supports("power-on") && supports("power-off") {
        bindings.push(bind(Button::Power, POWER_TOGGLE));
    }
    bindings
}
/// What the main loop is told about a mapped press: a problem to toast, or a
/// reading to put on the volume card.
pub(crate) enum Feedback {
    Error(String),
    Volume(VolumeReading),
    Notice(Notice),
}
fn volume_reading(target: &str, level: Option<i64>, muted: bool) -> Option<VolumeReading> {
    Some(VolumeReading {
        target: target.to_owned(),
        level: level?.clamp(0, 100) as i32,
        text: if muted { "Muted".into() } else { String::new() },
    })
}
pub struct Controller {
    context: String,
    config: Arc<Config>,
    bindings: Vec<Binding>,
    pending: HashMap<u16, Pending>,
    replay: VecDeque<Press>,
    generation: Arc<AtomicU64>,
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<(u64, Feedback)>,
    // A press the worker queue had no room for. The key is consumed either
    // way, so without this the remote's primary input disappears in silence;
    // poll turns it into the same toast every other dispatch path raises.
    dropped: bool,
    // Power ends a running activity (main.rs); a highlighted row takes the key
    // only when there is none to end.
    activity_running: bool,
    // Bumped when a key is released; see `Request::hold`.
    hold: Arc<AtomicU64>,
    // The packaged device on the core control screen, which has no activity
    // and so no bindings of its own: its keys mean what they mean on its row.
    screen_device: Option<String>,
}
impl Controller {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(8);
        let (reply, out) = mpsc::sync_channel(8);
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let hold = Arc::new(AtomicU64::new(0));
        let holds = hold.clone();
        std::thread::spawn(move || worker(rx, reply, current, holds));
        Self {
            context: String::new(),
            config: Arc::new(Config::default()),
            bindings: vec![],
            pending: HashMap::new(),
            replay: VecDeque::new(),
            generation,
            tx,
            rx: out,
            dropped: false,
            activity_running: false,
            hold,
            screen_device: None,
        }
    }
    fn binding(&self, button: Button, gesture: Gesture) -> Option<&Binding> {
        if button == Button::Power && self.activity_running && self.context.starts_with(ROW) {
            return None;
        }
        self.bindings
            .iter()
            .find(|b| b.button == button && b.gesture == gesture)
    }
    fn fire(&mut self, button: Button, gesture: Gesture, repeat: bool) -> bool {
        let Some(binding) = self.binding(button, gesture) else {
            return false;
        };
        let Some(action) = binding.action.clone() else {
            return true;
        };
        if !repeat || couch_model::buttons::repeatable(&action.command) {
            let sent = self.tx.try_send(Request {
                generation: self.generation.load(Ordering::SeqCst),
                at: Instant::now(),
                config: self.config.clone(),
                action,
                repeat,
                gesture,
                hold: self.hold.load(Ordering::SeqCst),
            });
            // A held key repeats faster than a slow device answers. A repeat
            // there is no room for is the hold running at the device's pace,
            // not a lost press: only a fresh press is worth a toast.
            self.dropped |= sent.is_err() && !repeat;
        }
        true
    }
    pub fn next_replay(&mut self) -> Option<Press> {
        self.replay.pop_front()
    }
    pub fn handle(&mut self, app: &App, press: &Press) -> bool {
        self.sync_context(app);
        if self.context.is_empty()
            || app.get_pair_shown()
            || app.get_settings_shown()
            || app.get_keyboard_shown()
        {
            return false;
        }
        self.handle_press(press)
    }
    fn handle_press(&mut self, press: &Press) -> bool {
        let Some(button) = Button::from_evdev(press.code) else {
            return false;
        };
        if press.released {
            // The hold is over: whatever repeats are still queued are stale.
            self.hold.fetch_add(1, Ordering::SeqCst);
            if let Some(pending) = self.pending.remove(&press.code) {
                if !pending.fired && pending.at.elapsed() >= HOLD {
                    self.fire(button, Gesture::Long, false);
                    return true;
                }
                if !pending.fired && !self.fire(button, Gesture::Short, false) {
                    self.replay.push_back(pending.down);
                    self.replay.push_back(press.clone());
                }
                return true;
            }
            return self.binding(button, Gesture::Short).is_some();
        }
        if self.binding(button, Gesture::Long).is_some() {
            if !press.repeat {
                self.pending.entry(press.code).or_insert_with(|| Pending {
                    down: press.clone(),
                    at: Instant::now(),
                    fired: false,
                });
            }
            return true;
        }
        self.fire(button, Gesture::Short, press.repeat)
    }
    pub fn set_screen_device(&mut self, device: Option<String>) {
        self.screen_device = device;
    }
    fn sync_context(&mut self, app: &App) {
        self.activity_running = app.get_activity_running();
        let config = connections::config();
        let context = if app.get_player_shown() || app.get_tv_shown() {
            // A device's own screen (`device:<id>`) has no activity bindings to
            // read; the keys mean there what they mean on its row.
            let active = app.get_active_activity().to_string();
            match (active.strip_prefix("device:"), &self.screen_device) {
                (Some(id), _) => format!("{ROW}{id}"),
                (None, Some(id)) if active.is_empty() => format!("{ROW}{id}"),
                _ => active,
            }
        } else if app.get_light_shown() && !app.get_chooser_shown() {
            // The highlighted row of the open room, when it is a device these
            // keys mean something to.
            config
                .as_ref()
                .and_then(|config| {
                    let room = couch_model::Id::new(app.get_light_room_id().as_str());
                    let row = usize::try_from(app.get_light_index()).ok()?;
                    let device = crate::lights::device_at(config, &room, row)?;
                    (!row_bindings(config, device).is_empty())
                        .then(|| format!("{ROW}{}", device.id))
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.refresh(context, config);
    }
    fn refresh(&mut self, context: String, config: Option<Arc<Config>>) {
        let changed = config.as_ref().map_or(!self.bindings.is_empty(), |next| {
            !Arc::ptr_eq(next, &self.config)
        });
        if context != self.context || changed {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.pending.clear();
            self.replay.clear();
            self.bindings.clear();
            if let Some(config) = config {
                self.bindings = if let Some(id) = context.strip_prefix(ROW) {
                    config
                        .devices()
                        .find(|(_, d)| d.id.as_str() == id)
                        .map(|(_, d)| row_bindings(&config, d))
                        .unwrap_or_default()
                } else {
                    config
                        .activities
                        .iter()
                        .find(|a| a.id.as_str() == context)
                        .map(|a| {
                            a.buttons
                                .iter()
                                .filter(|b| {
                                    !(b.button == Button::Back && b.gesture == Gesture::Long)
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                };
                self.config = config;
            } else {
                self.config = Arc::new(Config::default());
            }
            self.context = context;
        }
    }
    pub fn poll(&mut self, app: &App) -> Option<Feedback> {
        self.sync_context(app);
        let due: Vec<_> = self
            .pending
            .iter_mut()
            .filter_map(|(&code, p)| {
                if !p.fired && p.at.elapsed() >= HOLD {
                    p.fired = true;
                    Button::from_evdev(code)
                } else {
                    None
                }
            })
            .collect();
        for button in due {
            self.fire(button, Gesture::Long, false);
        }
        self.feedback()
    }
    /// What the workers and `fire` left for the main loop. Separate from
    /// `poll` because it needs no App, and so can be tested without a panel.
    fn feedback(&mut self) -> Option<Feedback> {
        let generation = self.generation.load(Ordering::SeqCst);
        let latest = self
            .rx
            .try_iter()
            .filter(|(g, _)| *g == generation)
            .map(|(_, e)| e)
            .last();
        if std::mem::take(&mut self.dropped) {
            return Some(Feedback::Error("Still sending the last command".into()));
        }
        latest
    }
}
// An idle recv() would retain an AVR's scarce control socket forever after
// leaving an activity. Observe cancellation even when no new keys arrive.
fn worker(
    rx: mpsc::Receiver<Request>,
    reply: mpsc::SyncSender<(u64, Feedback)>,
    current: Arc<AtomicU64>,
    hold: Arc<AtomicU64>,
) {
    let mut lanes = HashMap::<String, mpsc::SyncSender<Request>>::new();
    let mut generation = current.load(Ordering::SeqCst);
    loop {
        let work = rx.recv_timeout(Duration::from_millis(100));
        let now = current.load(Ordering::SeqCst);
        if generation != now {
            lanes.clear();
            generation = now;
        }
        let r = match work {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return,
        };
        if r.generation != generation {
            continue;
        }
        let key = r
            .config
            .devices()
            .find(|(_, d)| d.id == r.action.device)
            .and_then(|(_, d)| match &d.integration {
                Integration::Connection { connection_id, .. } => Some(connection_id.to_string()),
                i => r
                    .config
                    .resolve_integration(i)
                    .and_then(|v| serde_json::to_string(&v).ok()),
            });
        let Some(key) = key else {
            let _ = reply.try_send((
                generation,
                Feedback::Error("Mapped device was removed".into()),
            ));
            continue;
        };
        let tx = lanes.entry(key).or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel(8);
            let reply = reply.clone();
            let current = current.clone();
            let hold = hold.clone();
            std::thread::spawn(move || connection_worker(rx, reply, current, hold));
            tx
        });
        let repeat = r.repeat;
        if tx.try_send(r).is_err() {
            // The same rule as `fire`: a surplus repeat is dropped quietly.
            if repeat {
                continue;
            }
            eprintln!("couch-gui: mapped connection queue full");
            let _ = reply.try_send((
                generation,
                Feedback::Error("Device command queue is full".into()),
            ));
        }
    }
}

fn connection_worker(
    rx: mpsc::Receiver<Request>,
    reply: mpsc::SyncSender<(u64, Feedback)>,
    current: Arc<AtomicU64>,
    hold: Arc<AtomicU64>,
) {
    let mut tv = HashMap::new();
    let mut streaming = HashMap::new();
    let mut sonos = HashMap::new();
    // Not cleared with the other caches on a generation change: the fabrics are
    // shared with the room list, which is still holding them open.
    let matter = connections::matter();
    let mut generation = current.load(Ordering::SeqCst);
    // A packaged device whose level is owed a read once its keys stop.
    let mut settle: Option<(Integration, String, Option<DbScale>)> = None;
    // Its last observed decibel level, in tenths: what a held volume key is
    // predicted from, the way a brightness hold moves its card before the
    // light has answered. Every real reading replaces it.
    let mut level: Option<i16> = None;
    loop {
        let request = rx.recv_timeout(Duration::from_millis(100));
        let now = current.load(Ordering::SeqCst);
        if now != generation {
            settle = None;
            level = None;
            tv.clear();
            streaming.clear();
            sonos.clear();
            generation = now;
        }
        let r = match request {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The lane has been quiet for a poll: a held volume key is
                // over. One read for the card, at the level it ended on.
                if let Some((integration, target, scale)) = settle.take() {
                    if let Some((reading, observed)) = plugin_level(&integration, &target, scale) {
                        level = observed;
                        let _ = reply.try_send((generation, Feedback::Volume(reading)));
                    }
                }
                continue;
            }
            Err(_) => return,
        };
        if r.generation != generation || r.at.elapsed() > Duration::from_millis(750) {
            continue;
        }
        if r.repeat && r.hold != hold.load(Ordering::SeqCst) {
            // The key was released while this waited: see `Request::hold`. A
            // level is still owed to the card once the lane is quiet.
            continue;
        }
        let mut r = r;
        let settles = r
            .config
            .devices()
            .find(|(_, d)| d.id == r.action.device)
            .and_then(|(_, d)| {
                let integration = d.network_integration(&r.config)?;
                let scale = DbScale::of(&integration);
                match integration {
                    // The whole integration, not just its connection: the read
                    // that settles the card has to name the same child the
                    // keys did.
                    plugin @ Integration::Plugin { .. } => Some((plugin, d.name.clone(), scale)),
                    _ => None,
                }
            });
        // Move the card now, from the last level seen, and let the readings
        // that follow correct it. Without a declared scale or a level yet
        // there is nothing to predict from, and the read-back shows the truth.
        if let (Some((_, target, Some(scale))), Some(known)) = (&settles, level) {
            let up = match r.action.command.as_str() {
                "volume-up" => Some(true),
                "volume-down" => Some(false),
                _ => None,
            };
            if let Some(up) = up {
                // A press is one of the receiver's own steps. A hold is a
                // request to travel: one absolute write per repeat, a larger
                // stride than a step, so the level moves at a useful pace
                // without a queue of half-decibel commands behind it.
                let predicted = if r.repeat {
                    let target = scale.held(known, up);
                    r.action =
                        Action::new(r.action.device.clone(), format!("{SET_VOLUME_DB}{target}"));
                    target
                } else {
                    scale.stepped(known, up)
                };
                level = Some(predicted);
                let _ = reply.try_send((
                    r.generation,
                    Feedback::Volume(db_reading(target, predicted, false, Some(*scale))),
                ));
            }
        }
        match execute_with_input(
            &r.config,
            &r.action,
            &mut tv,
            &mut streaming,
            &mut sonos,
            &matter,
            couch_model::buttons::key_phase(r.gesture, r.repeat),
            &|| {
                current.load(Ordering::SeqCst) == r.generation
                    && r.at.elapsed() <= Duration::from_millis(750)
            },
        ) {
            Err(error) => {
                let _ = reply.try_send((r.generation, Feedback::Error(error)));
            }
            Ok(Outcome {
                volume: Some(reading),
                observed_db,
                ..
            }) => {
                if settles.is_some() {
                    level = observed_db;
                }
                let _ = reply.try_send((r.generation, Feedback::Volume(reading)));
            }
            Ok(Outcome {
                notice: Some(notice),
                ..
            }) => {
                let _ = reply.try_send((r.generation, Feedback::Notice(notice)));
            }
            Ok(Outcome { settle: true, .. }) => settle = settles,
            Ok(_) => {}
        }
    }
}

/// Whether a failed Sonos command says anything about the session. A press
/// abandoned at the deadline and a word that is not a Sonos command are both
/// decided here rather than by the player, and dropping the cached client for
/// either would throw the session away exactly when presses are being missed.
fn session_is_suspect(error: &couch_sonos::Error) -> bool {
    !matches!(
        error,
        couch_sonos::Error::Cancelled | couch_sonos::Error::Command
    )
}

pub(crate) fn execute(
    config: &Config,
    action: &Action,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
    sonos: &mut HashMap<String, couch_sonos::Client>,
    matter: &connections::MatterFleet,
) -> Result<(), String> {
    execute_with_input(
        config,
        action,
        tv,
        streaming,
        sonos,
        matter,
        KeyPhase::Tap,
        &|| true,
    )
    .map(|_| ())
}

/// Why a transport did not take a key. `Unavailable` means it was not in a
/// position to try (no IR code for that key, the device's TV not on the
/// Bluetooth link, a network client that could not connect), so the next
/// transport in the device's order gets the key; `Command` means it tried and
/// failed, which ends the press: a failed IR write is never retried over the
/// network, and a network command that was refused is not re-sent by IR.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure {
    Unavailable(String),
    Command(String),
}
impl From<String> for Failure {
    fn from(e: String) -> Self {
        Failure::Command(e)
    }
}
impl From<&str> for Failure {
    fn from(e: &str) -> Self {
        Failure::Command(e.into())
    }
}
fn unreachable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(e.to_string())
}

/// Physical input preserves hold edges; other callers represent distinct presses.
/// `current` is rechecked after loading a codeset and opening the blaster.
/// `phase` is how the key was pressed (`buttons::key_phase`): a repeat is a
/// held key's edge for every transport, and a packaged device is sent the
/// phase itself, which the daemon's host passes on only to a package that
/// asked for it.
///
/// The key goes down the device's transport order (`Device::transport_order`:
/// the preferred transport first) and stops at the first that takes it. Which
/// transports a device has is configuration; whether one can take the key now
/// is decided here, per press.
pub(crate) fn execute_with_input(
    config: &Config,
    action: &Action,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
    sonos: &mut HashMap<String, couch_sonos::Client>,
    matter: &connections::MatterFleet,
    phase: KeyPhase,
    current: &dyn Fn() -> bool,
) -> Result<Outcome, String> {
    if !current() {
        return Ok(Outcome::default());
    }
    let repeat = phase == KeyPhase::Repeat;
    let device = config
        .devices()
        .find(|(_, d)| d.id == action.device)
        .map(|(_, d)| d)
        .ok_or("Mapped device was removed")?;
    if let Some(tenths) = action.command.strip_prefix(SET_VOLUME_DB) {
        let tenths: i16 = tenths.parse().map_err(|_| "Unsupported button function")?;
        let Some(integration @ Integration::Plugin { .. }) = device.network_integration(config)
        else {
            return Err("Unsupported button function".into());
        };
        // `ask_device` aims the frame at the child the device is; a device
        // that is the connection itself sends the bytes it always did.
        return match crate::tv::plugin::ask_device(
            &integration,
            couch_plugin::Request::action(couch_model::TypedAction::SetVolumeDb { tenths }),
        ) {
            // Part of a hold: the lane reads the level once it goes quiet.
            Ok(couch_plugin::Response::Ok) => Ok(Outcome {
                settle: true,
                ..Outcome::default()
            }),
            Ok(_) => Err("The integration returned an invalid response".into()),
            Err(failure) => Err(crate::tv::plugin::refusal(&failure)),
        };
    }
    if action.command == POWER_TOGGLE {
        return power_toggle(config, device, tv, streaming, sonos, matter, phase, current);
    }
    let command = F::parse(&action.command).ok_or("Unsupported button function")?;
    let order = device.transport_order(config);
    if order.is_empty() {
        return Err("Mapped connection was removed".into());
    }
    if !order
        .iter()
        .any(|t| command.supports_transport(device, config, *t))
    {
        return Err("Unsupported button function".into());
    }
    let mut skipped: Option<String> = None;
    for transport in order {
        if !command.supports_transport(device, config, transport) {
            continue;
        }
        let result = match transport {
            couch_model::Transport::Ir => {
                match try_device_ir(config, device.id.as_str(), &command, repeat, current) {
                    Ok(true) => Ok(Outcome::default()),
                    Ok(false) => Err(Failure::Unavailable(format!(
                        "No IR code assigned to {}",
                        command.id()
                    ))),
                    Err(e) => Err(Failure::Command(e)),
                }
            }
            couch_model::Transport::Bluetooth => send_bluetooth(device, &command),
            couch_model::Transport::Ip => send_network(
                config, device, &command, tv, streaming, sonos, matter, phase, current,
            ),
        };
        match result {
            Ok(outcome) => return Ok(outcome),
            Err(Failure::Unavailable(why)) => {
                println!(
                    "couch-gui: {} over {transport} unavailable for {}: {why}",
                    command.id(),
                    device.name
                );
                skipped = Some(why);
            }
            Err(Failure::Command(e)) => return Err(e),
        }
        if !current() {
            return Ok(Outcome::default());
        }
    }
    Err(skipped.unwrap_or_else(|| "Unsupported button function".into()))
}

/// A row's Power key. A device with a real toggle (an IR power code, webOS)
/// gets it; one with only power-on and power-off gets whichever its observed
/// state calls for. Observed, never assumed: a receiver that will not say
/// whether it is on is an error, not a guess that could switch it the wrong
/// way. Samsung and Apple TV report no power state on their remote channel,
/// so they take what their own TV screens send: off when reachable, wake when
/// not.
#[allow(clippy::too_many_arguments)]
fn power_toggle(
    config: &Config,
    device: &couch_model::Device,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
    sonos: &mut HashMap<String, couch_sonos::Client>,
    matter: &connections::MatterFleet,
    phase: KeyPhase,
    current: &dyn Fn() -> bool,
) -> Result<Outcome, String> {
    let name = device.name.clone();
    let notice = |value: &str| Outcome {
        notice: Some(Notice {
            caption: "Power".into(),
            target: name.clone(),
            value: value.into(),
        }),
        ..Outcome::default()
    };
    /// What to send, decided before anything is sent: reading a kept client's
    /// state and sending through the same caches cannot overlap.
    enum Plan {
        Toggle,
        Observed(Option<bool>),
        OffElseWake,
    }
    let plan = if F::Toggle.supports_device(device, config) {
        Plan::Toggle
    } else {
        match device.network_integration(config) {
            Some(integration @ Integration::Plugin { .. }) => {
                match crate::tv::plugin::ask_device(&integration, couch_plugin::Request::status()) {
                    Ok(couch_plugin::Response::Status { status }) => Plan::Observed(status.on),
                    Ok(_) => return Err("The integration returned an invalid status".into()),
                    Err(failure) => return Err(crate::tv::plugin::refusal(&failure)),
                }
            }
            Some(Integration::AndroidTv) => {
                // Its state arrives on the kept client; without one yet, a
                // first command opens it and the next press knows.
                let connection = match &device.integration {
                    Integration::Connection { connection_id, .. } => connection_id.as_str(),
                    _ => "",
                };
                Plan::Observed(
                    streaming
                        .get(&format!("androidtv:{connection}"))
                        .and_then(|c| c.status().ok())
                        .and_then(|status| status["on"].as_bool()),
                )
            }
            Some(Integration::Tizen | Integration::AppleTv) => Plan::OffElseWake,
            // Nothing can ask it: say why, in the words every other key uses.
            Some(integration) if integration.legacy_builtin().is_some() => {
                let row = integration.legacy_builtin().expect("checked by the guard");
                return Err(row.needs_package());
            }
            _ => Plan::Observed(None),
        }
    };
    let mut send = |command: &str| {
        execute_with_input(
            config,
            &Action::new(device.id.clone(), command),
            tv,
            streaming,
            sonos,
            matter,
            phase,
            current,
        )
    };
    match plan {
        Plan::Toggle => send("toggle").map(|_| notice("Toggled")),
        Plan::OffElseWake => match send("power-off") {
            Ok(_) => Ok(notice("Toggled")),
            Err(_) => send("power-on").map(|_| notice("Waking")),
        },
        Plan::Observed(Some(true)) => send("power-off").map(|_| notice("Off")),
        Plan::Observed(Some(false)) => send("power-on").map(|_| notice("On")),
        Plan::Observed(None) => Err(format!("{name} has not said whether it is on")),
    }
}

/// The remote is the HID peripheral: one datagram with the function's id to
/// the HID daemon, which turns it into a consumer-control report for the TV
/// on the link. Only when that TV is this device's: a bond that is not the
/// link right now (the TV is off, or another device holds the link) makes
/// the transport unavailable rather than sending a key to the wrong TV. A
/// migrated bond with no address takes whatever TV is on the link, as it
/// always did; so does a daemon that reports the link without an address.
fn send_bluetooth(device: &couch_model::Device, command: &F) -> Result<Outcome, Failure> {
    let bond = device
        .bluetooth
        .as_ref()
        .ok_or_else(|| Failure::Unavailable("No Bluetooth pairing".into()))?;
    let link = crate::system::bluetooth_link();
    let linked = link
        .link
        .as_ref()
        .is_some_and(|l| !bond.addressed() || l.address.is_empty() || l.address == bond.address);
    if !linked {
        return Err(Failure::Unavailable(format!(
            "{} is not connected over Bluetooth",
            device.name
        )));
    }
    crate::system::bluetooth_word(&command.id()).map_err(Failure::Command)?;
    Ok(Outcome::default())
}

/// The device's network integration. A client that cannot connect makes the
/// transport unavailable (the TV is asleep, the box is off) so a key can fall
/// through to infrared or Bluetooth; a command the connected device refused
/// is an error.
#[allow(clippy::too_many_arguments)]
fn send_network(
    config: &Config,
    device: &couch_model::Device,
    command: &F,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
    sonos: &mut HashMap<String, couch_sonos::Client>,
    matter: &connections::MatterFleet,
    phase: KeyPhase,
    current: &dyn Fn() -> bool,
) -> Result<Outcome, Failure> {
    let repeat = phase == KeyPhase::Repeat;
    let command = command.clone();
    // A volume or mute press on a device that can report its level gets the
    // level read back for the volume card; everything else reports nothing.
    let sound = matches!(
        command,
        F::VolumeUp | F::VolumeDown | F::Volume(_) | F::Mute | F::MuteOn | F::MuteOff
    );
    let name = device.name.clone();
    let integration = device
        .network_integration(config)
        .ok_or_else(|| Failure::Unavailable("This device has no network connection".into()))?;
    let connection = match &device.integration {
        Integration::Connection { connection_id, .. } => connection_id.as_str(),
        _ => "",
    };
    let scale = DbScale::of(&integration);
    match integration {
        // Connecting cost a TLS handshake and a GET /players/local/info before
        // the press could even be sent, inside the same 750 ms deadline the
        // receiver and the TVs beat by keeping their client. This one is kept
        // per host the same way.
        Integration::Sonos { host } => {
            if !sonos.contains_key(&host) {
                let address = host.parse().map_err(|_| "Sonos requires an IPv4 address")?;
                sonos.insert(
                    host.clone(),
                    couch_sonos::Client::connect(address).map_err(unreachable)?,
                );
            }
            let client = sonos.get(&host).expect("just inserted");
            let result = match command {
                // One absolute write: no preparatory read to go stale between.
                F::Volume(percent) => client.set_volume(percent),
                _ => client.command_if_current(&command.id(), current),
            };
            if result.as_ref().is_err_and(|e| session_is_suspect(e)) {
                sonos.remove(&host);
            }
            result.map_err(|e| e.to_string())?;
            let volume = if sound {
                sonos
                    .get(&host)
                    .and_then(|client| client.volume_state().ok())
                    .and_then(|(level, muted)| volume_reading(&name, Some(i64::from(level)), muted))
            } else {
                None
            };
            Ok(Outcome {
                volume,
                ..Outcome::default()
            })
        }
        Integration::AndroidTv | Integration::AppleTv | Integration::Tizen => {
            let kind = match integration {
                Integration::AppleTv => "appletv",
                Integration::Tizen => "tizen",
                _ => "androidtv",
            };
            if connection.is_empty() {
                return Err("This TV needs a named connection".into());
            }
            let settings = couch_control::StreamingConnection::load(&crate::connections::file(
                connection, kind,
            ))
            .map_err(|_| "Pair this TV in Connections first".to_string())?;
            if settings.kind() != kind {
                return Err("TV credentials have the wrong provider".into());
            }
            let key = format!("{kind}:{connection}");
            if !streaming
                .get(&key)
                .is_some_and(|client| client.matches(&settings))
            {
                streaming.insert(
                    key.clone(),
                    couch_control::StreamingTv::connect(&settings).map_err(unreachable)?,
                );
            }
            let result = streaming
                .get(&key)
                .unwrap()
                .command(&command.id())
                .map_err(|e| e.to_string());
            if result.is_err() {
                streaming.remove(&key);
            }
            result.map(|_| Outcome::default()).map_err(Failure::Command)
        }
        // A connection saved while its client was built in, not yet handed
        // to its package (couch-confd does that by itself once the package is
        // installed). Unavailable rather than failed: a device that also has
        // infrared codes or a Bluetooth bond still takes the key that way.
        integration if integration.legacy_builtin().is_some() => {
            let row = integration.legacy_builtin().expect("checked by the guard");
            Err(Failure::Unavailable(row.needs_package()))
        }
        Integration::Kodi { host, port } => {
            let c = couch_kodi::settings::Settings::load(&connections::file(connection, "kodi"))
                .ok()
                .filter(|s| s.host == host && s.http_control)
                .map(|s| couch_control::Kodi::settings(&s))
                .unwrap_or_else(|| couch_control::Kodi::tcp(&host, port))
                .with_timeout(Duration::from_secs(2));
            let result = match command {
                F::Ok => c.select(),
                F::Up | F::Down | F::Left | F::Right | F::Back | F::Home | F::Menu => {
                    let method = match command {
                        F::Up => "Input.Up",
                        F::Down => "Input.Down",
                        F::Left => "Input.Left",
                        F::Right => "Input.Right",
                        F::Back => "Input.Back",
                        F::Home => "Input.Home",
                        _ => "Input.ContextMenu",
                    };
                    c.call(method, json!({})).map(|_| ())
                }
                F::VolumeUp | F::VolumeDown => c
                    .call(
                        "Application.SetVolume",
                        json!({"volume":if command==F::VolumeUp{"increment"}else{"decrement"}}),
                    )
                    .map(|_| ()),
                F::Mute => c
                    .call("Application.SetMute", json!({"mute":"toggle"}))
                    .map(|_| ()),
                F::Volume(percent) => c.set_volume(i64::from(percent)).map(|_| ()),
                F::PlayPause | F::Stop | F::Next | F::Previous => {
                    let p = c
                        .playback()
                        .map_err(|e| e.to_string())?
                        .ok_or("Kodi has no active playback")?;
                    let (method, params) = kodi_player_call(&command).expect("matched above");
                    c.player_command(p.player, method, params).map(|_| ())
                }
                // Anything else has no Kodi call. It used to fall through to
                // "previous item", so a sequence step such as `on` aimed at a
                // Kodi box skipped back a track. Unavailable rather than
                // failed: infrared or Bluetooth may still carry the key.
                other => {
                    return Err(Failure::Unavailable(format!(
                        "Kodi has no {} command",
                        other.id()
                    )))
                }
            };
            result.map_err(|e| e.to_string())?;
            let volume = if sound {
                c.volume()
                    .ok()
                    .and_then(|v| volume_reading(&name, Some(v.volume), v.muted))
            } else {
                None
            };
            Ok(Outcome {
                volume,
                ..Outcome::default()
            })
        }
        Integration::WebOs => {
            if matches!(command, F::PowerOn | F::PowerOff | F::Toggle) {
                let path = connections::file(connection, "webos");
                let settings = couch_webos::Settings::load(&path).map_err(|e| e.to_string())?;
                let preference = couch_webos::power::PowerSettings::load(&path, &settings.url)?;
                if preference.method == couch_webos::power::Method::Ir {
                    return preference
                        .transmit(match command {
                            F::PowerOn => "power-on",
                            F::PowerOff => "power-off",
                            _ => "power",
                        })
                        .map(|_| Outcome::default())
                        .map_err(Failure::Command);
                }
                if command == F::Toggle {
                    return crate::tv::toggle_power(&settings, &path)
                        .map(|_| Outcome::default())
                        .map_err(Failure::Command);
                }
            }
            if command == F::PowerOn {
                let path = connections::file(connection, "webos");
                let settings = couch_webos::Settings::load(&path).map_err(|e| e.to_string())?;
                return crate::tv::wake_tv(&settings, &path)
                    .map(|_| Outcome::default())
                    .map_err(Failure::Command);
            }
            if !tv.contains_key(connection) {
                let settings = couch_webos::Settings::load(&connections::file(connection, "webos"))
                    .map_err(|e| e.to_string())?;
                tv.insert(
                    connection.into(),
                    couch_control::WebOs::connect(&settings).map_err(unreachable)?,
                );
            }
            let result = crate::tv::mapped_command(tv.get_mut(connection).unwrap(), &command);
            if result.is_err() {
                tv.remove(connection);
            }
            result.map_err(|e| e.to_string())?;
            let volume = if sound {
                tv.get_mut(connection)
                    .and_then(|c| c.volume().ok())
                    .and_then(|v| {
                        let v = if v["volumeStatus"].is_object() {
                            &v["volumeStatus"]
                        } else {
                            &v
                        };
                        volume_reading(
                            &name,
                            v["volume"].as_i64(),
                            v["muteStatus"] == true || v["muted"] == true,
                        )
                    })
            } else {
                None
            };
            Ok(Outcome {
                volume,
                ..Outcome::default()
            })
        }
        Integration::Hue { light_id } => {
            let (id, raw) = connections::split(&light_id);
            let c = couch_hue::settings::Settings::load(&connections::file(id, "hue"))
                .and_then(|s| s.client())
                .map_err(|e| e.to_string())?;
            if let F::Dim(percent) = command {
                return c
                    .command(raw, couch_hue::Command::Brightness(percent))
                    .map(|_| Outcome::default())
                    .map_err(|e| Failure::Command(e.to_string()));
            }
            let on = match command {
                F::On => true,
                F::Off => false,
                _ => !c
                    .control_state(raw)
                    .map_err(|e| e.to_string())?
                    .on
                    .ok_or("Hue light is unavailable")?,
            };
            c.set_power(raw, on)
                .map(|_| Outcome::default())
                .map_err(|e| Failure::Command(e.to_string()))
        }
        Integration::HomeAssistant { entity_id } => {
            let (c, raw) = connections::ha(&entity_id)?;
            // One entity domain per device kind, each with its own service set.
            let cover = match command {
                F::Open => Some(couch_ha::CoverCommand::Open),
                F::Close => Some(couch_ha::CoverCommand::Close),
                F::Stop => Some(couch_ha::CoverCommand::Stop),
                F::Position(percent) => Some(couch_ha::CoverCommand::Position(percent)),
                _ => None,
            };
            if let Some(cover) = cover {
                return c
                    .cover_command(&raw, cover)
                    .map(|_| Outcome::default())
                    .map_err(|e| Failure::Command(e.to_string()));
            }
            // Stepping the target reads it first: the increment, the limits and
            // whether the thermostat is in range mode are the entity's, not ours.
            let climate = match command {
                F::Mode(ref mode) => Some(couch_ha::ClimateCommand::HvacMode(mode.clone())),
                F::TemperatureUp | F::TemperatureDown => Some(
                    c.climate(&raw)
                        .and_then(|state| {
                            state.adjusted_target(
                                if command == F::TemperatureUp { 1 } else { -1 },
                                None,
                            )
                        })
                        .map_err(|e| e.to_string())?,
                ),
                _ => None,
            };
            if let Some(climate) = climate {
                return c
                    .climate_command(&raw, climate)
                    .map(|_| Outcome::default())
                    .map_err(|e| Failure::Command(e.to_string()));
            }
            if let F::Dim(percent) = command {
                return c
                    .command(&raw, couch_ha::Command::Brightness(percent))
                    .map(|_| Outcome::default())
                    .map_err(|e| Failure::Command(e.to_string()));
            }
            let on = match command {
                F::On => true,
                F::Off => false,
                _ => !c
                    .light(&raw)
                    .map_err(|e| e.to_string())?
                    .on
                    .ok_or("Home Assistant light is unavailable")?,
            };
            c.command(
                &raw,
                if on {
                    couch_ha::Command::On
                } else {
                    couch_ha::Command::Off
                },
            )
            .map(|_| Outcome::default())
            .map_err(|e| Failure::Command(e.to_string()))
        }
        // The fleet is the one the room list and the shortcut keys use, so a
        // mapped key reuses whatever CASE session those already opened.
        Integration::Matter { device } => match command {
            F::On => matter.power(&device, true),
            F::Off => matter.power(&device, false),
            F::Dim(percent) => matter.brightness(&device, percent),
            _ => matter.toggle_or_on(&device),
        }
        .map(|_| Outcome::default())
        .map_err(Failure::Command),
        Integration::Plugin { .. } => {
            // How the key was pressed goes with it, and which child of the
            // connection it is for. The daemon's host sends a phase only to a
            // protocol 3 package; every other package is sent the tap it
            // always was, byte for byte. A level on a child (`dim:30`,
            // `position:40`, `mode:heat`) goes as the plain command it is:
            // the one host gate turns it into the typed action the child's
            // kind declares.
            let result = crate::tv::plugin::ask_device(
                &integration,
                couch_plugin::Request::key(command.id(), phase),
            );
            match result {
                // The package reports its level only when asked, and asking
                // costs as much as the command did. A fresh press reads it back
                // for the volume card; a held key does not, and the lane reads
                // it once when the hold ends (`settle`).
                Ok(couch_plugin::Response::Ok) if sound && repeat => Ok(Outcome {
                    settle: true,
                    ..Outcome::default()
                }),
                Ok(couch_plugin::Response::Ok) => {
                    let level = sound
                        .then(|| plugin_level(&integration, &name, scale))
                        .flatten();
                    Ok(Outcome {
                        observed_db: level.as_ref().and_then(|(_, tenths)| *tenths),
                        volume: level.map(|(reading, _)| reading),
                        ..Outcome::default()
                    })
                }
                Ok(_) => Err(Failure::Command(
                    "The external integration returned an invalid command response".into(),
                )),
                Err(failure) => Err(plugin_failure(&failure)),
            }
        }
        _ => Err(Failure::Command(
            "This integration cannot send button commands yet".into(),
        )),
    }
}

/// Ask a packaged device for its level, for the volume card. The read is
/// aimed at the child the device is, exactly as the command was.
fn plugin_level(
    integration: &Integration,
    target: &str,
    scale: Option<DbScale>,
) -> Option<(VolumeReading, Option<i16>)> {
    match crate::tv::plugin::ask_device(integration, couch_plugin::Request::status()) {
        Ok(couch_plugin::Response::Status { status }) => {
            let tenths = match (status.muted, status.volume_db.as_ref()) {
                (Some(true), _) => None,
                (_, Some(couch_plugin::VolumeDb::Reading { tenths })) => Some(*tenths),
                _ => None,
            };
            plugin_volume_reading(target, &status, scale).map(|reading| (reading, tenths))
        }
        _ => None,
    }
}

/// A package's status as the volume card shows it: decibels for a receiver,
/// a percentage for anything that has one, and "Muted" over either.
fn plugin_volume_reading(
    target: &str,
    status: &couch_plugin::Status,
    scale: Option<DbScale>,
) -> Option<VolumeReading> {
    let muted = status.muted == Some(true);
    if let Some(percent) = status.volume {
        return volume_reading(target, Some(i64::from(percent)), muted);
    }
    Some(match status.volume_db.as_ref()? {
        couch_plugin::VolumeDb::Reading { tenths } => db_reading(target, *tenths, muted, scale),
        couch_plugin::VolumeDb::Minimum => VolumeReading {
            target: target.to_owned(),
            level: if scale.is_some() { 0 } else { -1 },
            text: if muted { "Muted" } else { "Minimum" }.into(),
        },
    })
}

/// The decibel range and key step a packaged receiver declares with its
/// set-volume action. Decibels have no natural percentage; the declared range
/// gives the card's bar one, and the step lets a held key move the card
/// before the receiver has been asked where it ended up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DbScale {
    min: i16,
    max: i16,
    step: i16,
}
impl DbScale {
    fn of(integration: &Integration) -> Option<Self> {
        let Integration::Plugin { actions, .. } = integration else {
            return None;
        };
        actions.iter().find_map(|action| match action {
            couch_model::PluginActionSchema::SetVolumeDb {
                min_tenths,
                max_tenths,
                step_tenths,
            } if max_tenths > min_tenths && *step_tenths > 0 => Some(Self {
                min: *min_tenths,
                max: *max_tenths,
                step: i16::try_from(*step_tenths).ok()?,
            }),
            _ => None,
        })
    }
    fn percent(self, tenths: i16) -> i32 {
        let span = i32::from(self.max) - i32::from(self.min);
        ((i32::from(tenths.clamp(self.min, self.max)) - i32::from(self.min)) * 100 + span / 2)
            / span
    }
    /// Where one repeat of a held volume key should leave the level: 1 dB up,
    /// 2 dB down, in whole declared steps. Down is the larger stride because
    /// getting quieter quickly is the safe direction.
    fn held(self, tenths: i16, up: bool) -> i16 {
        let stride = |tenths_wanted: i16| (tenths_wanted / self.step).max(1) * self.step;
        let next = if up {
            tenths.saturating_add(stride(HOLD_UP_TENTHS))
        } else {
            tenths.saturating_sub(stride(HOLD_DOWN_TENTHS))
        };
        next.clamp(self.min, self.max)
    }
    /// Where one more press of a volume key should leave the level.
    fn stepped(self, tenths: i16, up: bool) -> i16 {
        let next = if up {
            tenths.saturating_add(self.step)
        } else {
            tenths.saturating_sub(self.step)
        };
        next.clamp(self.min, self.max)
    }
}
fn db_reading(target: &str, tenths: i16, muted: bool, scale: Option<DbScale>) -> VolumeReading {
    VolumeReading {
        target: target.to_owned(),
        // A level of 0-100 draws the bar; -1 is a reading with no scale.
        level: scale.map_or(-1, |scale| scale.percent(tenths)),
        text: if muted {
            "Muted".into()
        } else {
            format!("{:.1} dB", f32::from(tenths) / 10.0)
        },
    }
}

/// Whether the next transport gets the key is the code's question alone: a
/// package's reason changes what is said, never what is done.
fn plugin_failure(failure: &couch_plugin::Failure) -> Failure {
    let message = crate::tv::plugin::refusal(failure);
    match failure.code {
        couch_plugin::Error::Transport
        | couch_plugin::Error::Timeout
        | couch_plugin::Error::Busy
        | couch_plugin::Error::Expired => Failure::Unavailable(message),
        _ => Failure::Command(message),
    }
}

/// Exact per-device assignment wins; a missing entry leaves the existing
/// network behavior intact. Never retry a failed IR write over the network.
pub(crate) fn try_device_ir(
    config: &Config,
    device_id: &str,
    command: &F,
    repeat: bool,
    current: &dyn Fn() -> bool,
) -> Result<bool, String> {
    let device = config
        .devices()
        .find(|(_, d)| d.id.as_str() == device_id)
        .map(|(_, d)| d)
        .ok_or("Device was removed")?;
    let Some(codeset) = device.effective_ir_codeset(config) else {
        return Ok(false);
    };
    if !current() {
        return Ok(true);
    }
    let codes =
        couch_ir::codeset::load(&crate::home::path("ir"), codeset).map_err(|e| e.to_string())?;
    ir_override_with(&codes, command, || {
        static TOGGLES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, bool>>> =
            std::sync::OnceLock::new();
        let mut states = TOGGLES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "IR command state unavailable")?;
        ir_send_with(
            &mut states,
            device_id,
            &codes,
            command,
            repeat,
            current,
            |message| {
                let mut blaster =
                    couch_ir::tx::Irtx::open("/dev/irtx").map_err(|e| e.to_string())?;
                if !current() {
                    return Ok(false);
                }
                couch_ir::tx::transmit(&mut blaster, message, 0).map_err(|e| e.to_string())?;
                Ok(true)
            },
        )?;
        Ok(())
    })
}
fn ir_override_with(
    codes: &couch_ir::codeset::Codeset,
    command: &F,
    send: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if codes.get(&command.id()).is_none() {
        return Ok(false);
    }
    send()?;
    Ok(true)
}

fn ir_send_with(
    states: &mut HashMap<String, bool>,
    device: &str,
    codes: &couch_ir::codeset::Codeset,
    command: &F,
    repeat: bool,
    current: &dyn Fn() -> bool,
    send: impl FnOnce(&couch_ir::proto::Message) -> Result<bool, String>,
) -> Result<(), String> {
    if !current() {
        return Ok(());
    }
    let toggle = states
        .get(device)
        .map(|last| if repeat { *last } else { !*last })
        .unwrap_or(false);
    let message = ir_message(codes, command, toggle)?;
    if !current() {
        return Ok(());
    }
    if send(&message)? {
        states.insert(device.into(), toggle);
    }
    Ok(())
}

fn ir_message(
    codes: &couch_ir::codeset::Codeset,
    command: &F,
    toggle: bool,
) -> Result<couch_ir::proto::Message, String> {
    codes
        .get(&command.id())
        .ok_or_else(|| format!("No IR code assigned to {}", command.id()))?
        .encode(toggle)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A room with a packaged receiver, a receiver saved while its client was
    /// built in and not yet handed to the package, a webOS TV, a Sonos speaker
    /// and a Hue light.
    fn room() -> Config {
        let mut config = Config::seed();
        let plugin = couch_model::Provider::Plugin {
            id: "denon".into(),
            label: "Denon AVR".into(),
            capabilities: ["power-on", "power-off", "volume-up", "volume-down", "mute"]
                .into_iter()
                .map(|id| couch_model::PluginCapability {
                    id: id.into(),
                    label: id.into(),
                })
                .collect(),
            supports_inputs: true,
            presentation: vec![],
            actions: vec![],
            children: vec![],
        };
        for (id, provider) in [
            ("avr-package", plugin),
            (
                "avr-legacy",
                couch_model::Provider::LegacyDenon {
                    host: "192.0.2.10".into(),
                    port: 23,
                },
            ),
            ("lg", couch_model::Provider::WebOs),
        ] {
            config.connections.push(couch_model::Connection {
                id: id.into(),
                name: id.into(),
                provider,
            });
        }
        let room = config.rooms.first_mut().unwrap();
        for id in ["avr-package", "avr-legacy", "lg"] {
            room.devices.push(
                couch_model::Device::new(
                    couch_model::Id::new(id),
                    id,
                    couch_model::DeviceKind::Speaker,
                )
                .with_integration(Integration::Connection {
                    connection_id: id.into(),
                    resource_id: String::new(),
                    child: None,
                }),
            );
        }
        config.validate().unwrap();
        config
    }
    fn device<'a>(config: &'a Config, id: &str) -> &'a couch_model::Device {
        config
            .devices()
            .find(|(_, d)| d.id.as_str() == id)
            .map(|(_, d)| d)
            .unwrap()
    }
    fn keys(bindings: &[Binding]) -> Vec<(Button, String)> {
        bindings
            .iter()
            .map(|b| (b.button, b.action.as_ref().unwrap().command.clone()))
            .collect()
    }

    #[test]
    fn a_receiver_or_tv_row_answers_volume_mute_and_power_and_other_rows_do_not() {
        let config = room();
        let expected = vec![
            (Button::VolumeUp, "volume-up".to_owned()),
            (Button::VolumeDown, "volume-down".to_owned()),
            (Button::Mute, "mute".to_owned()),
            (Button::Power, POWER_TOGGLE.to_owned()),
        ];
        for id in ["avr-package", "avr-legacy", "lg"] {
            let bindings = row_bindings(&config, device(&config, id));
            assert_eq!(keys(&bindings), expected, "{id}");
            assert!(bindings
                .iter()
                .all(|b| b.gesture == Gesture::Short
                    && b.action.as_ref().unwrap().device.as_str() == id));
        }
        // Sonos rows keep their own controller; a light has no use for the keys.
        for (_, d) in config.devices() {
            let sonos = matches!(
                config.resolve_integration(&d.integration),
                Some(Integration::Sonos { .. })
            );
            if sonos || config.can_toggle(d) {
                assert!(row_bindings(&config, d).is_empty(), "{}", d.name);
            }
        }
    }

    #[test]
    fn a_command_kodi_does_not_have_is_refused_and_never_becomes_previous_item() {
        // Only the four transport keys have a player call.
        for (function, method) in [
            (F::PlayPause, "Player.PlayPause"),
            (F::Stop, "Player.Stop"),
            (F::Next, "Player.GoTo"),
            (F::Previous, "Player.GoTo"),
        ] {
            assert_eq!(kodi_player_call(&function).unwrap().0, method);
        }
        assert_eq!(
            kodi_player_call(&F::Previous).unwrap().1,
            json!({"to":"previous"})
        );
        for function in [F::On, F::Off, F::PowerOn, F::PowerOff, F::Play, F::Pause] {
            assert!(kodi_player_call(&function).is_none(), "{}", function.id());
        }

        // Sent to a Kodi box, such a command is unavailable (so infrared or
        // Bluetooth may still carry it) and nothing reaches the box at all.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = Config::seed();
        config.connections.push(couch_model::Connection {
            id: "box".into(),
            name: "box".into(),
            provider: couch_model::Provider::Kodi {
                host: "127.0.0.1".into(),
                port,
            },
        });
        config.rooms.first_mut().unwrap().devices.push(
            couch_model::Device::new(
                couch_model::Id::new("box"),
                "box",
                couch_model::DeviceKind::Tv,
            )
            .with_integration(Integration::Connection {
                connection_id: "box".into(),
                resource_id: String::new(),
                child: None,
            }),
        );
        config.validate().unwrap();
        for function in [F::On, F::PowerOn, F::PowerOff] {
            let result = send_network(
                &config,
                device(&config, "box"),
                &function,
                &mut HashMap::new(),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &connections::MatterFleet::default(),
                KeyPhase::Tap,
                &|| true,
            );
            assert!(
                matches!(&result, Err(Failure::Unavailable(why)) if why.starts_with("Kodi has no ")),
                "{}",
                function.id()
            );
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "nothing may be sent to the box"
        );
    }

    #[test]
    fn a_declared_decibel_range_gives_the_bar_a_percentage_and_a_held_key_its_steps() {
        let config = room();
        // The fixture's package declares no set-volume action: no scale, so no
        // bar and nothing to predict from.
        let plain = device(&config, "avr-package")
            .network_integration(&config)
            .unwrap();
        assert_eq!(DbScale::of(&plain), None);
        let mut declared = plain;
        let Integration::Plugin { actions, .. } = &mut declared else {
            panic!("a packaged device")
        };
        actions.push(couch_model::PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        });
        let scale = DbScale::of(&declared).unwrap();
        assert_eq!((scale.percent(-800), scale.percent(180)), (0, 100));
        assert_eq!(scale.percent(-415), 39);
        // Out-of-range readings pin to the ends rather than overflow the bar.
        assert_eq!((scale.percent(-900), scale.percent(300)), (0, 100));
        assert_eq!(scale.stepped(-415, true), -410);
        assert_eq!(scale.stepped(-415, false), -420);
        assert_eq!(scale.stepped(180, true), 180);
        assert_eq!(scale.stepped(-800, false), -800);
        // A held key travels: 1 dB up, 2 dB down, clamped at the ends, and
        // never less than one declared step on a coarser scale.
        assert_eq!(scale.held(-415, true), -405);
        assert_eq!(scale.held(-415, false), -435);
        assert_eq!(scale.held(175, true), 180);
        assert_eq!(scale.held(-790, false), -800);
        let coarse = DbScale {
            min: -800,
            max: 180,
            step: 30,
        };
        assert_eq!(
            (coarse.held(-400, true), coarse.held(-400, false)),
            (-370, -430)
        );
        let predicted = db_reading("Theater AVR", scale.stepped(-415, true), false, Some(scale));
        assert_eq!((predicted.text.as_str(), predicted.level), ("-41.0 dB", 40));
        // A receiver still waiting for its package declares no scale either.
        assert_eq!(
            DbScale::of(
                &device(&config, "avr-legacy")
                    .network_integration(&config)
                    .unwrap()
            ),
            None
        );
    }

    #[test]
    fn power_on_a_row_yields_to_a_running_activity() {
        let config = Arc::new(room());
        let mut controller = Controller::new();
        controller.refresh(format!("{ROW}avr-legacy"), Some(config));
        assert!(controller.binding(Button::Power, Gesture::Short).is_some());
        assert!(controller
            .binding(Button::VolumeUp, Gesture::Short)
            .is_some());
        controller.activity_running = true;
        assert!(controller.binding(Button::Power, Gesture::Short).is_none());
        assert!(controller
            .binding(Button::VolumeUp, Gesture::Short)
            .is_some());
        // A row whose device is gone has no bindings rather than stale ones.
        controller.activity_running = false;
        controller.refresh(format!("{ROW}no-such-device"), Some(Arc::new(room())));
        assert!(controller.binding(Button::Power, Gesture::Short).is_none());
    }

    #[test]
    fn every_key_on_a_receiver_waiting_for_its_package_says_so() {
        let config = room();
        let matter = connections::MatterFleet::default();
        // Its row still takes the keys, so a press is answered and not lost.
        let bound = keys(&row_bindings(&config, device(&config, "avr-legacy")));
        assert_eq!(bound.len(), 4);
        for (_, command) in bound.into_iter().chain([
            (Button::Red, "input:SAT/CBL".to_owned()),
            (Button::Red, "power-on".to_owned()),
        ]) {
            let error = execute_with_input(
                &config,
                &Action::new("avr-legacy", command.as_str()),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &matter,
                KeyPhase::Tap,
                &|| true,
            )
            .unwrap_err();
            assert_eq!(error, "Needs the Denon package", "{command}");
        }
    }

    #[test]
    fn a_package_status_fills_the_volume_card_in_decibels_percent_or_muted() {
        let status = |json: serde_json::Value| -> couch_plugin::Status {
            serde_json::from_value(json).unwrap()
        };
        let card = |json| plugin_volume_reading("Theater AVR", &status(json), None);
        let db = card(serde_json::json!({"volume_db":{"kind":"reading","tenths":-415}})).unwrap();
        assert_eq!(
            (db.text.as_str(), db.level, db.target.as_str()),
            ("-41.5 dB", -1, "Theater AVR")
        );
        assert_eq!(
            card(serde_json::json!({"volume_db":{"kind":"minimum"}}))
                .unwrap()
                .text,
            "Minimum"
        );
        assert_eq!(
            card(serde_json::json!({"muted":true,"volume_db":{"kind":"reading","tenths":-300}}))
                .unwrap()
                .text,
            "Muted"
        );
        // With the range the package declares, the same readings fill the bar.
        let scale = Some(DbScale {
            min: -800,
            max: 180,
            step: 5,
        });
        let scaled = |json| plugin_volume_reading("Theater AVR", &status(json), scale).unwrap();
        let bar = scaled(serde_json::json!({"volume_db":{"kind":"reading","tenths":-415}}));
        assert_eq!((bar.text.as_str(), bar.level), ("-41.5 dB", 39));
        assert_eq!(
            scaled(serde_json::json!({"volume_db":{"kind":"minimum"}})).level,
            0
        );
        let percent = card(serde_json::json!({"volume":37})).unwrap();
        assert_eq!((percent.level, percent.text.as_str()), (37, ""));
        assert!(card(serde_json::json!({"on":true})).is_none());
    }
    #[test]
    fn supplemental_ir_routes_only_exact_assignments_and_never_falls_back_on_error() {
        let codes =
            couch_ir::codeset::Codeset::parse("fixture", "toggle nec 4 8\nvolume-up nec 4 2\n")
                .unwrap();
        assert_eq!(
            ir_override_with(&codes, &F::PowerOn, || panic!(
                "power-on must not use toggle"
            )),
            Ok(false)
        );
        assert_eq!(
            ir_override_with(&codes, &F::VolumeDown, || panic!(
                "unassigned must stay on network"
            )),
            Ok(false)
        );
        assert_eq!(ir_override_with(&codes, &F::VolumeUp, || Ok(())), Ok(true));
        assert_eq!(
            ir_override_with(&codes, &F::VolumeUp, || Err("TX failure".into())),
            Err("TX failure".into())
        );
    }
    #[test]
    fn infrared_discrete_power_never_falls_back_to_toggle() {
        let codes =
            couch_ir::codeset::Codeset::parse("fixture", "toggle nec 4 8\nvolume-up nec 4 2\n")
                .unwrap();
        assert!(ir_message(&codes, &F::Toggle, false).is_ok());
        assert!(ir_message(&codes, &F::VolumeUp, false).is_ok());
        assert!(ir_message(&codes, &F::PowerOn, false)
            .unwrap_err()
            .contains("No IR code assigned"));
        assert!(ir_message(&codes, &F::PowerOff, false).is_err());
    }

    #[test]
    fn infrared_actions_preserve_rc_toggle_and_exact_assignment() {
        let codes = couch_ir::codeset::Codeset::parse("fixture", "ok rc5 0 1\n").unwrap();
        let first = ir_message(&codes, &F::Ok, false).unwrap();
        let next = ir_message(&codes, &F::Ok, true).unwrap();
        assert_ne!(format!("{first:?}"), format!("{next:?}"));
        assert!(ir_message(&codes, &F::Back, false).is_err());
    }

    #[test]
    fn held_ir_presses_keep_toggle_and_only_successful_new_presses_advance_it() {
        let codes = couch_ir::codeset::Codeset::parse("fixture", "volume-up rc5 0 1").unwrap();
        let mut states = HashMap::new();
        let mut frames = Vec::new();
        for repeat in [false, true, true, false] {
            ir_send_with(
                &mut states,
                "tv",
                &codes,
                &F::VolumeUp,
                repeat,
                &|| true,
                |m| {
                    frames.push(m.frame.clone());
                    Ok(true)
                },
            )
            .unwrap();
        }
        assert_eq!(frames[0], frames[1]);
        assert_eq!(frames[1], frames[2]);
        assert_ne!(frames[2], frames[3]);
        let before = states.clone();
        assert!(ir_send_with(
            &mut states,
            "tv",
            &codes,
            &F::VolumeUp,
            false,
            &|| true,
            |_| Err("TX failed".into())
        )
        .is_err());
        assert_eq!(states, before);
        ir_send_with(
            &mut states,
            "tv",
            &codes,
            &F::VolumeUp,
            false,
            &|| true,
            |_| Ok(false),
        )
        .unwrap();
        assert_eq!(states, before);
        ir_send_with(
            &mut states,
            "tv",
            &codes,
            &F::VolumeUp,
            false,
            &|| false,
            |_| panic!("stale command sent"),
        )
        .unwrap();
        assert_eq!(states, before);
        ir_send_with(
            &mut states,
            "other",
            &codes,
            &F::VolumeUp,
            false,
            &|| true,
            |m| {
                assert_eq!(m.frame, frames[0]);
                Ok(true)
            },
        )
        .unwrap();
    }
    #[test]
    fn config_change_refreshes_active_bindings_and_invalidates_queued_work() {
        let (mut c, rx) = fixture();
        let mut config = Config::default();
        config.activities.push(couch_model::Activity {
            id: "watch".into(),
            name: "Watch".into(),
            room: "room".into(),
            source: None,
            setup: Default::default(),
            kind: Default::default(),
            steps: vec![],
            buttons: vec![binding(Gesture::Short, "ok")],
        });
        let original = Arc::new(config.clone());
        c.refresh("watch".into(), Some(original.clone()));
        c.handle_press(&press(353, false));
        let queued = rx.try_recv().unwrap();
        let old_generation = c.generation.load(Ordering::SeqCst);
        c.refresh("watch".into(), Some(original));
        assert_eq!(c.generation.load(Ordering::SeqCst), old_generation);
        c.pending.insert(
            353,
            Pending {
                down: press(353, false),
                at: Instant::now(),
                fired: false,
            },
        );
        config.activities[0].buttons[0].action = Some(Action::new("other-device", "home"));
        c.refresh("watch".into(), Some(Arc::new(config)));
        assert_ne!(queued.generation, c.generation.load(Ordering::SeqCst));
        assert!(c.pending.is_empty());
        c.handle_press(&press(353, false));
        assert_eq!(
            rx.try_recv().unwrap().action,
            Action::new("other-device", "home")
        );
        c.refresh("watch".into(), Some(Arc::new(Config::default())));
        assert!(c.bindings.is_empty());
    }
    #[test]
    fn physical_repeat_metadata_reaches_the_worker() {
        let (mut c, rx) = fixture();
        c.bindings = vec![Binding {
            button: Button::VolumeUp,
            gesture: Gesture::Short,
            action: Some(Action::new("tv", "volume-up")),
        }];
        let mut p = press(115, false);
        c.handle_press(&p);
        assert!(!rx.try_recv().unwrap().repeat);
        p.repeat = true;
        c.handle_press(&p);
        assert!(rx.try_recv().unwrap().repeat);
    }

    #[test]
    fn a_press_dropped_by_a_full_queue_is_reported() {
        let (mut c, rx) = fixture();
        c.bindings = vec![Binding {
            button: Button::VolumeUp,
            gesture: Gesture::Short,
            action: Some(Action::new("tv", "volume-up")),
        }];
        for _ in 0..8 {
            assert!(c.handle_press(&press(115, false)));
        }
        assert!(c.feedback().is_none());
        assert!(c.handle_press(&press(115, false)));
        assert!(
            matches!(c.feedback(), Some(Feedback::Error(m)) if m == "Still sending the last command")
        );
        assert!(c.feedback().is_none());
        assert_eq!(rx.try_iter().count(), 8);
    }

    #[test]
    fn releasing_a_key_ends_its_hold_so_queued_repeats_can_be_discarded() {
        let (mut c, rx) = fixture();
        c.bindings = vec![Binding {
            button: Button::VolumeUp,
            gesture: Gesture::Short,
            action: Some(Action::new("tv", "volume-up")),
        }];
        let mut held = press(115, false);
        c.handle_press(&held);
        held.repeat = true;
        c.handle_press(&held);
        c.handle_press(&held);
        let first: Vec<_> = rx.try_iter().map(|r| (r.repeat, r.hold)).collect();
        assert_eq!(first, [(false, 0), (true, 0), (true, 0)]);
        // Finger off the key: a repeat still queued carries the old hold and
        // no longer matches, which is how the lane knows not to send it.
        let mut up = press(115, false);
        up.released = true;
        c.handle_press(&up);
        assert_eq!(c.hold.load(Ordering::SeqCst), 1);
        c.handle_press(&press(115, false));
        assert_eq!(rx.try_recv().unwrap().hold, 1);
    }

    #[test]
    fn a_held_key_that_outruns_the_queue_is_not_an_error() {
        let (mut c, rx) = fixture();
        c.bindings = vec![Binding {
            button: Button::VolumeUp,
            gesture: Gesture::Short,
            action: Some(Action::new("tv", "volume-up")),
        }];
        // One press, then the hold's repeats arrive faster than anything drains.
        assert!(c.handle_press(&press(115, false)));
        let mut held = press(115, false);
        held.repeat = true;
        for _ in 0..20 {
            assert!(c.handle_press(&held), "the key is still consumed");
        }
        assert!(
            c.feedback().is_none(),
            "surplus repeats are dropped quietly"
        );
        assert_eq!(rx.try_iter().count(), 8);
        // A fresh press with no room is still worth telling the user about.
        for _ in 0..8 {
            c.handle_press(&held);
        }
        assert!(c.handle_press(&press(115, false)));
        assert!(
            matches!(c.feedback(), Some(Feedback::Error(m)) if m == "Still sending the last command")
        );
    }

    fn fixture() -> (Controller, mpsc::Receiver<Request>) {
        let (tx, rx) = mpsc::sync_channel(8);
        let (_, out) = mpsc::channel();
        (
            Controller {
                context: "watch".into(),
                config: Arc::new(Config::default()),
                bindings: vec![],
                pending: HashMap::new(),
                replay: VecDeque::new(),
                generation: Arc::new(AtomicU64::new(1)),
                tx,
                rx: out,
                dropped: false,
                activity_running: false,
                hold: Arc::new(AtomicU64::new(0)),
                screen_device: None,
            },
            rx,
        )
    }
    fn press(code: u16, released: bool) -> Press {
        Press {
            code,
            released,
            key: None,
            mic: None,
            menu: None,
            latency_us: 0,
            repeat: false,
        }
    }
    fn binding(gesture: Gesture, command: &str) -> Binding {
        Binding {
            button: Button::Ok,
            gesture,
            action: Some(Action::new("player", command)),
        }
    }
    #[test]
    fn short_and_long_are_mutually_exclusive_even_across_a_slow_frame() {
        let (mut c, rx) = fixture();
        c.bindings = vec![
            binding(Gesture::Short, "ok"),
            binding(Gesture::Long, "home"),
        ];
        assert!(c.handle_press(&press(353, false)));
        assert!(rx.try_recv().is_err());
        c.handle_press(&press(353, true));
        assert_eq!(rx.try_recv().unwrap().action.command, "ok");
        assert!(rx.try_recv().is_err());
        c.handle_press(&press(353, false));
        c.pending.get_mut(&353).unwrap().at = Instant::now() - HOLD;
        c.handle_press(&press(353, true));
        assert_eq!(rx.try_recv().unwrap().action.command, "home");
        assert!(rx.try_recv().is_err());
        c.handle_press(&press(353, false));
        c.pending.get_mut(&353).unwrap().fired = true;
        c.fire(Button::Ok, Gesture::Long, false);
        c.handle_press(&press(353, true));
        assert_eq!(rx.try_recv().unwrap().action.command, "home");
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn only_a_short_tap_replays_default_when_long_is_overridden() {
        let (mut c, rx) = fixture();
        c.bindings = vec![binding(Gesture::Long, "home")];
        c.handle_press(&press(353, false));
        c.handle_press(&press(353, true));
        assert_eq!(c.replay.len(), 2);
        assert!(rx.try_recv().is_err());
        c.replay.clear();
        c.handle_press(&press(353, false));
        c.pending.get_mut(&353).unwrap().at = Instant::now() - HOLD;
        c.handle_press(&press(353, true));
        assert!(c.replay.is_empty());
        assert_eq!(rx.try_recv().unwrap().action.command, "home");
    }
    #[test]
    fn disabled_buttons_and_nonrepeatable_actions_do_not_send_commands() {
        let (mut c, rx) = fixture();
        c.bindings = vec![Binding {
            button: Button::VolumeUp,
            gesture: Gesture::Short,
            action: None,
        }];
        assert!(c.handle_press(&press(115, false)));
        assert!(rx.try_recv().is_err());
        c.bindings[0].action = Some(Action::new("tv", "mute"));
        let mut p = press(115, false);
        p.repeat = true;
        c.handle_press(&p);
        assert!(rx.try_recv().is_err());
        p.repeat = false;
        c.handle_press(&p);
        assert_eq!(rx.try_recv().unwrap().action.command, "mute");
    }

    #[test]
    fn matter_devices_dispatch_instead_of_reporting_an_unsupported_integration() {
        let mut config = Config::default();
        config.rooms.push(couch_model::Room {
            id: "room".into(),
            name: "Room".into(),
            icon: None,
            devices: vec![couch_model::Device::new(
                "lamp".into(),
                "Lamp",
                couch_model::DeviceKind::Light,
            )
            .with_integration(Integration::Matter {
                device: "fabric/1/1".into(),
            })],
        });
        let matter = connections::MatterFleet::default();
        for command in ["on", "off", "toggle", "dim:30"] {
            let error = execute_with_input(
                &config,
                &Action::new("lamp", command),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &matter,
                KeyPhase::Tap,
                &|| true,
            )
            .unwrap_err();
            // No fabric on a test host, so the fleet refuses the connection.
            // The point is that the dispatch reaches Matter at all.
            assert_ne!(
                error, "This integration cannot send button commands yet",
                "{command}"
            );
            assert_eq!(error, "Matter connection was removed", "{command}");
        }
    }

    #[test]
    fn a_key_walks_the_transport_order_and_skips_a_bluetooth_tv_that_is_not_linked() {
        let mut config = Config::default();
        config.connections.push(couch_model::Connection {
            id: "lg".into(),
            name: "LG".into(),
            provider: couch_model::Provider::WebOs,
        });
        let bond = couch_model::DeviceBluetooth {
            address: "44:27:45:4E:33:25".into(),
            name: "LG".into(),
        };
        config.rooms.push(couch_model::Room {
            id: "room".into(),
            name: "Room".into(),
            icon: None,
            devices: vec![
                couch_model::Device {
                    bluetooth: Some(bond.clone()),
                    ..couch_model::Device::new(
                        "bt-only".into(),
                        "Bedroom TV",
                        couch_model::DeviceKind::Tv,
                    )
                },
                couch_model::Device {
                    bluetooth: Some(bond),
                    preferred_transport: Some(couch_model::Transport::Bluetooth),
                    ..couch_model::Device::new("lg".into(), "LG TV", couch_model::DeviceKind::Tv)
                        .with_integration(Integration::Connection {
                            connection_id: "lg".into(),
                            resource_id: String::new(),
                            child: None,
                        })
                },
            ],
        });
        let matter = connections::MatterFleet::default();
        let run = |device: &str, command: &str| {
            execute_with_input(
                &config,
                &Action::new(device, command),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &matter,
                KeyPhase::Tap,
                &|| true,
            )
            .unwrap_err()
        };
        // No HID daemon on a test host, so the bond is never the link: a
        // Bluetooth-only TV says so, and says nothing about other transports.
        assert_eq!(
            run("bt-only", "volume-up"),
            "Bedroom TV is not connected over Bluetooth"
        );
        // Power-on is not a Bluetooth key at all, and the device has nothing else.
        assert_eq!(run("bt-only", "power-on"), "Unsupported button function");
        // The LG prefers Bluetooth; with its TV not on the link the key falls
        // through to webOS, whose error (no credentials here) is what comes
        // back, not the Bluetooth one.
        let error = run("lg", "volume-up");
        assert!(!error.contains("Bluetooth"), "{error}");
        assert!(!error.is_empty());
        assert_eq!(
            Failure::from("x"),
            Failure::Command("x".into()),
            "a plain error ends the press"
        );
        assert!(matches!(unreachable("gone"), Failure::Unavailable(m) if m == "gone"));
    }

    fn said(code: couch_plugin::Error, text: &str) -> couch_plugin::Failure {
        couch_plugin::Failure {
            code,
            reason: Some(couch_plugin::Reason::Message { text: text.into() }),
        }
    }

    #[test]
    fn a_package_reason_changes_what_is_said_and_never_which_transport_is_tried() {
        use couch_plugin::Error as E;
        // Exactly the codes that let the next transport have the key, with a
        // reason or without: a package cannot talk its way into or out of the
        // infrared and Bluetooth fallback.
        for code in [
            E::Invalid,
            E::Unsupported,
            E::Incompatible,
            E::Protocol,
            E::Transport,
            E::Timeout,
            E::Busy,
            E::Expired,
            E::Rejected,
            E::Unpaired,
        ] {
            let falls_through = matches!(code, E::Transport | E::Timeout | E::Busy | E::Expired);
            // Without a reason the words are the ones every build has shown.
            let plain = plugin_failure(&code.into());
            let worded = plugin_failure(&said(code, "The TV is locked"));
            // The panel does no pairing of its own: `Unpaired` keeps its own
            // second line - the browser hint - never the package's, because
            // the two together are the most that fits the toast.
            let text = if code == E::Unpaired {
                format!("{code}\n{}", crate::tv::plugin::PAIRING_HINT)
            } else {
                format!("{code}\nThe TV is locked")
            };
            if falls_through {
                assert_eq!(plain, Failure::Unavailable(code.to_string()));
                assert_eq!(worded, Failure::Unavailable(text));
            } else if code == E::Unpaired {
                assert_eq!(plain, Failure::Command(text.clone()));
                assert_eq!(worded, Failure::Command(text));
            } else {
                assert_eq!(plain, Failure::Command(code.to_string()));
                assert_eq!(worded, Failure::Command(text));
            }
        }
        // An empty line is no line.
        assert_eq!(
            plugin_failure(&said(E::Rejected, "  ")),
            Failure::Command(E::Rejected.to_string())
        );
    }

    #[test]
    fn a_long_binding_is_remembered_so_the_worker_can_say_how_the_key_was_pressed() {
        use couch_model::buttons::key_phase;
        let (mut c, rx) = fixture();
        c.bindings = vec![
            Binding {
                button: Button::VolumeUp,
                gesture: Gesture::Short,
                action: Some(Action::new("tv", "volume-up")),
            },
            binding(Gesture::Long, "home"),
        ];
        let mut p = press(115, false);
        c.handle_press(&p);
        let tap = rx.try_recv().unwrap();
        assert_eq!(key_phase(tap.gesture, tap.repeat), KeyPhase::Tap);
        p.repeat = true;
        c.handle_press(&p);
        let held = rx.try_recv().unwrap();
        assert_eq!(key_phase(held.gesture, held.repeat), KeyPhase::Repeat);
        c.fire(Button::Ok, Gesture::Long, false);
        let long = rx.try_recv().unwrap();
        assert_eq!(long.action.command, "home");
        assert_eq!(key_phase(long.gesture, long.repeat), KeyPhase::LongPress);
    }

    /// What this panel writes to the daemon's socket for a packaged device,
    /// byte for byte, and what it says when the daemon relays a refusal. A tap
    /// is the frame every build has written (`local command` in couch-plugin's
    /// tests/golden/wire-00ab4da.tsv); only a held or long-pressed key carries
    /// a phase, which the daemon's host drops again for a protocol 1 or 2
    /// package (couch-confd's plugins.rs has the test with the Denon manifest).
    #[test]
    fn the_panel_sends_the_key_phase_to_the_daemon_and_shows_the_reason_it_relays() {
        const NAME: &str = "activity_buttons::tests::the_panel_sends_the_key_phase_to_the_daemon_and_shows_the_reason_it_relays";
        if std::env::var_os("COUCH_TEST_PANEL_SOCKET").is_none() {
            let home =
                std::env::temp_dir().join(format!("couch-panel-sock-{}", std::process::id()));
            std::fs::create_dir_all(&home).unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("COUCH_TEST_PANEL_SOCKET", "1")
                .env("COUCH_HOME_DIR", &home)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(home);
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use std::io::{Read, Write};
        let listener =
            std::os::unix::net::UnixListener::bind(crate::home::path("plugin.sock")).unwrap();
        let ok = r#"{"type":"ok"}"#;
        let status = r#"{"type":"status","status":{}}"#;
        // One reply per connection, in the order the presses below ask.
        let replies = [
            ok,
            status,
            ok,
            ok,
            status,
            r#"{"type":"error","code":"rejected","reason":{"kind":"message","text":"The TV is locked"}}"#,
            r#"{"type":"error","code":"timeout","reason":{"kind":"message","text":"Still starting up"}}"#,
            r#"{"type":"error","code":"rejected"}"#,
        ];
        let daemon = std::thread::spawn(move || {
            let mut seen = Vec::new();
            // Kept open to the end: macOS will not set a deadline on a socket
            // whose peer has gone, which the asking side does before reading.
            let mut open = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut frame = vec![0; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut frame).unwrap();
                seen.push(String::from_utf8(frame).unwrap());
                stream
                    .write_all(&(reply.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(reply.as_bytes()).unwrap();
                open.push(stream);
            }
            // Handed back, not dropped here: the last press may still be
            // reading its answer when this loop ends.
            (seen, open)
        });
        let config = room();
        let matter = connections::MatterFleet::default();
        let press = |command: &str, phase: KeyPhase| {
            execute_with_input(
                &config,
                &Action::new("avr-package", command),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &mut HashMap::new(),
                &matter,
                phase,
                &|| true,
            )
        };
        assert!(press("volume-up", KeyPhase::Tap).is_ok());
        // A held key is not read back: the lane does that once it goes quiet.
        assert!(press("volume-up", KeyPhase::Repeat).unwrap().settle);
        assert!(press("mute", KeyPhase::LongPress).is_ok());
        assert_eq!(
            press("power-on", KeyPhase::Tap).unwrap_err(),
            "The device refused the request\nThe TV is locked"
        );
        // No other transport on this device, so what made the network
        // unavailable is what is said.
        assert_eq!(
            press("power-on", KeyPhase::Tap).unwrap_err(),
            "The integration did not reply before the deadline\nStill starting up"
        );
        assert_eq!(
            press("power-on", KeyPhase::Tap).unwrap_err(),
            "The device refused the request"
        );
        let command = |function: &str, phase: &str| {
            format!(
                r#"{{"connection_id":"avr-package","request":{{"method":"command","function":"{function}"{phase}}}}}"#
            )
        };
        let read = r#"{"connection_id":"avr-package","request":{"method":"status"}}"#;
        let (seen, open) = daemon.join().unwrap();
        assert_eq!(
            seen,
            [
                command("volume-up", ""),
                read.into(),
                command("volume-up", r#","phase":"repeat""#),
                command("mute", r#","phase":"long_press""#),
                read.into(),
                command("power-on", ""),
                command("power-on", ""),
                command("power-on", ""),
            ]
        );
        drop(open);
    }

    /// A refusal with a package's line under it has to read well on the toast
    /// of the 480-pixel panel, at its longest too. `COUCH_CORE_SCREENSHOTS=<dir>`
    /// keeps the pictures.
    #[test]
    fn a_refusal_and_its_reason_render_on_the_toast() {
        const NAME: &str = "activity_buttons::tests::a_refusal_and_its_reason_render_on_the_toast";
        if std::env::var_os("COUCH_TEST_REASON_TOAST").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_REASON_TOAST", "1")
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
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        app.set_feedback_enabled(true);
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        // One buffer for every picture: the renderer repaints only what changed.
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        let mut above: Option<Vec<slint::Rgb8Pixel>> = None;
        let longest = "The receiver is updating its firmware and will not take commands until it has restarted, which can take as long as ten minutes. Try again after that, please. OK";
        assert_eq!(longest.len(), couch_plugin::Reason::MAX_TEXT);
        for (name, failure) in [
            (
                "reason-toast-1-code-only.png",
                couch_plugin::Error::Rejected.into(),
            ),
            (
                "reason-toast-2-message.png",
                said(couch_plugin::Error::Rejected, "The TV is locked"),
            ),
            (
                "reason-toast-3-unpaired.png",
                said(couch_plugin::Error::Unpaired, "Pair this TV again"),
            ),
            (
                "reason-toast-4-longest.png",
                said(couch_plugin::Error::Rejected, longest),
            ),
        ] {
            let message = match plugin_failure(&failure) {
                Failure::Command(message) | Failure::Unavailable(message) => message,
            };
            app.set_toast(message.as_str().into());
            for _ in 0..20 {
                slint::platform::update_timers_and_animations();
                std::thread::sleep(Duration::from_millis(16));
            }
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            let band = &pixels[720 * 480..760 * 480];
            assert!(band.iter().any(|p| *p != band[0]), "the toast drew nothing");
            // Two lines stay inside the bar one line has: the page above it
            // is the page the one-line toast left.
            let page = pixels[..680 * 480].to_vec();
            assert!(
                above.get_or_insert(page.clone()) == &page,
                "{name}: the toast spilled above its bar"
            );
            if let Some(dir) = std::env::var_os("COUCH_CORE_SCREENSHOTS") {
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
        }
        app.hide().unwrap();
    }

    /// `Unpaired` never shows the package's own line on the toast: Couch's
    /// sentence and [`crate::tv::plugin::PAIRING_HINT`] are the two lines the
    /// 72px bar keeps, the same pair whether or not the package sent a
    /// reason. `COUCH_CORE_SCREENSHOTS=<dir>` keeps the picture.
    #[test]
    fn an_unpaired_refusal_shows_the_browser_hint_not_the_package_reason() {
        const NAME: &str = "activity_buttons::tests::an_unpaired_refusal_shows_the_browser_hint_not_the_package_reason";
        if std::env::var_os("COUCH_TEST_PAIRING_TOAST").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("COUCH_TEST_PAIRING_TOAST", "1")
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
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        app.set_feedback_enabled(true);
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        let mut above: Option<Vec<slint::Rgb8Pixel>> = None;
        for (name, failure) in [
            (
                "reason-toast-5-unpaired-no-reason.png",
                couch_plugin::Error::Unpaired.into(),
            ),
            (
                "reason-toast-6-unpaired-with-reason.png",
                said(couch_plugin::Error::Unpaired, "Pair this TV again"),
            ),
        ] {
            let message = match plugin_failure(&failure) {
                Failure::Command(message) | Failure::Unavailable(message) => message,
            };
            // The assertion that matters: the package's line ("Pair this TV
            // again") never reaches the toast, with or without one.
            assert_eq!(
                message,
                format!(
                    "{}\n{}",
                    couch_plugin::Error::Unpaired,
                    crate::tv::plugin::PAIRING_HINT
                ),
                "{name}"
            );
            app.set_toast(message.as_str().into());
            for _ in 0..20 {
                slint::platform::update_timers_and_animations();
                std::thread::sleep(Duration::from_millis(16));
            }
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            let band = &pixels[720 * 480..760 * 480];
            assert!(band.iter().any(|p| *p != band[0]), "the toast drew nothing");
            // Two lines stay inside the bar one line has, the same check
            // `a_refusal_and_its_reason_render_on_the_toast` makes.
            let page = pixels[..680 * 480].to_vec();
            assert!(
                above.get_or_insert(page.clone()) == &page,
                "{name}: the toast spilled above its bar"
            );
            if let Some(dir) = std::env::var_os("COUCH_CORE_SCREENSHOTS") {
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
        }
        app.hide().unwrap();
    }

    #[test]
    fn an_abandoned_sonos_press_keeps_the_session_and_a_silent_player_loses_it() {
        use couch_sonos::Error as E;
        // The deadline and an unknown word are decided here, not by the
        // player; throwing the session away for those would reconnect on
        // exactly the presses the cache exists to save.
        assert!(!session_is_suspect(&E::Cancelled));
        assert!(!session_is_suspect(&E::Command));
        for error in [
            E::Transport,
            E::Response,
            E::Unsupported,
            E::Http(503),
            E::Api("ERROR_PLAYER_NOT_FOUND".into()),
            E::NotCoordinator {
                coordinator: "Kitchen".into(),
            },
        ] {
            assert!(session_is_suspect(&error), "{error}");
        }
    }
}
