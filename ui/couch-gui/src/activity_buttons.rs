//! Activity overrides: physical timing on the UI thread, device I/O on a worker.
use crate::{connections, keypad::Press, App};
use couch_model::commands::Function as F;
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
}
pub struct Controller {
    context: String,
    config: Arc<Config>,
    bindings: Vec<Binding>,
    pending: HashMap<u16, Pending>,
    replay: VecDeque<Press>,
    generation: Arc<AtomicU64>,
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<(u64, String)>,
}
impl Controller {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(8);
        let (reply, out) = mpsc::sync_channel(8);
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::spawn(move || worker(rx, reply, current));
        Self {
            context: String::new(),
            config: Arc::new(Config::default()),
            bindings: vec![],
            pending: HashMap::new(),
            replay: VecDeque::new(),
            generation,
            tx,
            rx: out,
        }
    }
    fn binding(&self, button: Button, gesture: Gesture) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.button == button && b.gesture == gesture)
    }
    fn fire(&self, button: Button, gesture: Gesture, repeat: bool) -> bool {
        let Some(binding) = self.binding(button, gesture) else {
            return false;
        };
        if let Some(action) = &binding.action {
            if !repeat || couch_model::buttons::repeatable(&action.command) {
                let _ = self.tx.try_send(Request {
                    generation: self.generation.load(Ordering::SeqCst),
                    at: Instant::now(),
                    config: self.config.clone(),
                    action: action.clone(),
                    repeat,
                });
            }
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
    fn sync_context(&mut self, app: &App) {
        let context = if app.get_player_shown() || app.get_tv_shown() {
            app.get_active_activity().to_string()
        } else {
            String::new()
        };
        self.refresh(context, connections::config());
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
                self.bindings = config
                    .activities
                    .iter()
                    .find(|a| a.id.as_str() == context)
                    .map(|a| {
                        a.buttons
                            .iter()
                            .filter(|b| !(b.button == Button::Back && b.gesture == Gesture::Long))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                self.config = config;
            } else {
                self.config = Arc::new(Config::default());
            }
            self.context = context;
        }
    }
    pub fn poll(&mut self, app: &App) -> Option<String> {
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
        let generation = self.generation.load(Ordering::SeqCst);
        self.rx
            .try_iter()
            .filter(|(g, _)| *g == generation)
            .map(|(_, e)| e)
            .last()
    }
}
// An idle recv() would retain an AVR's scarce control socket forever after
// leaving an activity. Observe cancellation even when no new keys arrive.
fn worker(
    rx: mpsc::Receiver<Request>,
    reply: mpsc::SyncSender<(u64, String)>,
    current: Arc<AtomicU64>,
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
            let _ = reply.try_send((generation, "Mapped device was removed".into()));
            continue;
        };
        let tx = lanes.entry(key).or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel(8);
            let reply = reply.clone();
            let current = current.clone();
            std::thread::spawn(move || connection_worker(rx, reply, current));
            tx
        });
        if tx.try_send(r).is_err() {
            eprintln!("couch-gui: mapped connection queue full");
            let _ = reply.try_send((generation, "Device command queue is full".into()));
        }
    }
}

fn connection_worker(
    rx: mpsc::Receiver<Request>,
    reply: mpsc::SyncSender<(u64, String)>,
    current: Arc<AtomicU64>,
) {
    let mut denon = HashMap::new();
    let mut tv = HashMap::new();
    let mut streaming = HashMap::new();
    let mut generation = current.load(Ordering::SeqCst);
    loop {
        let request = rx.recv_timeout(Duration::from_millis(100));
        let now = current.load(Ordering::SeqCst);
        if now != generation {
            denon.clear();
            tv.clear();
            streaming.clear();
            generation = now;
        }
        let r = match request {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return,
        };
        if r.generation != generation || r.at.elapsed() > Duration::from_millis(750) {
            continue;
        }
        if let Err(error) = execute_with_input(
            &r.config,
            &r.action,
            &mut denon,
            &mut tv,
            &mut streaming,
            r.repeat,
            &|| {
                current.load(Ordering::SeqCst) == r.generation
                    && r.at.elapsed() <= Duration::from_millis(750)
            },
        ) {
            let _ = reply.try_send((r.generation, error));
        }
    }
}

pub(crate) fn execute(
    config: &Config,
    action: &Action,
    denon: &mut HashMap<String, couch_control::Denon>,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
) -> Result<(), String> {
    execute_with_input(config, action, denon, tv, streaming, false, &|| true)
}

/// Physical input preserves hold edges; other callers represent distinct presses.
/// `current` is rechecked after loading a codeset and opening the blaster.
pub(crate) fn execute_with_input(
    config: &Config,
    action: &Action,
    denon: &mut HashMap<String, couch_control::Denon>,
    tv: &mut HashMap<String, couch_control::WebOs>,
    streaming: &mut HashMap<String, couch_control::StreamingTv>,
    repeat: bool,
    current: &dyn Fn() -> bool,
) -> Result<(), String> {
    if !current() {
        return Ok(());
    }
    let device = config
        .devices()
        .find(|(_, d)| d.id == action.device)
        .map(|(_, d)| d)
        .ok_or("Mapped device was removed")?;
    let command = F::parse(&action.command).ok_or("Unsupported button function")?;
    if try_device_ir(config, device.id.as_str(), &command, repeat, current)? {
        return Ok(());
    }
    let integration = config
        .resolve_integration(&device.integration)
        .ok_or("Mapped connection was removed")?;
    if !command.supports(&integration) {
        return Err("Unsupported button function".into());
    }
    let connection = match &device.integration {
        Integration::Connection { connection_id, .. } => connection_id.as_str(),
        _ => "",
    };
    match integration {
        Integration::Sonos { host } => {
            let client=couch_sonos::Client::connect(host.parse().map_err(|_|"Sonos requires an IPv4 address")?).map_err(|e|e.to_string())?;
            client.command_if_current(&command.id(),current).map_err(|e|e.to_string())
        }
        Integration::Ir { .. } => Err(format!("No IR code assigned to {}", command.id())),
        Integration::AndroidTv | Integration::AppleTv => {
            let kind = if matches!(integration, Integration::AppleTv) {
                "appletv"
            } else {
                "androidtv"
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
                    couch_control::StreamingTv::connect(&settings).map_err(|e| e.to_string())?,
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
            result
        }
        Integration::Denon { host, port } => {
            let key = format!("{host}:{port}");
            if !denon.contains_key(&key) {
                denon.insert(
                    key.clone(),
                    couch_control::Denon::connect(&couch_denon::Settings { host, port })
                        .map_err(|e| e.to_string())?,
                );
            }
            let c = denon.get_mut(&key).unwrap();
            let result = (|| {
                use couch_denon::Command as C;
                if command == F::Mute {
                    return c.toggle_mute().map(|_| ());
                }
                let cmd = match command {
                    F::PowerOn => C::Power(true),
                    F::PowerOff => C::Power(false),
                    F::VolumeUp => C::VolumeUp,
                    F::VolumeDown => C::VolumeDown,
                    F::MuteOn => C::Mute(true),
                    F::MuteOff => C::Mute(false),
                    F::Input(ref id) => C::Input(id.clone()),

                    _ => return Err(couch_control::Error::Protocol),
                };
                c.command(cmd).map(|_| ())
            })();
            if result.is_err() {
                denon.remove(&key);
            }
            result.map_err(|e| e.to_string())
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
                _ => {
                    let p = c
                        .playback()
                        .map_err(|e| e.to_string())?
                        .ok_or("Kodi has no active playback")?;
                    let (method, params) = match command {
                        F::PlayPause => ("Player.PlayPause", json!({})),
                        F::Stop => ("Player.Stop", json!({})),
                        F::Next => ("Player.GoTo", json!({"to":"next"})),
                        _ => ("Player.GoTo", json!({"to":"previous"})),
                    };
                    c.player_command(p.player, method, params).map(|_| ())
                }
            };
            result.map_err(|e| e.to_string())
        }
        Integration::WebOs => {
            if matches!(command, F::PowerOn | F::PowerOff) {
                let path = connections::file(connection, "webos");
                let settings = couch_webos::Settings::load(&path).map_err(|e| e.to_string())?;
                let preference = couch_webos::power::PowerSettings::load(&path, &settings.url)?;
                if preference.method == couch_webos::power::Method::Ir {
                    return preference.transmit(if command == F::PowerOn {
                        "power-on"
                    } else {
                        "power-off"
                    });
                }
            }
            if command == F::PowerOn {
                let path = connections::file(connection, "webos");
                let settings = couch_webos::Settings::load(&path).map_err(|e| e.to_string())?;
                return crate::tv::wake_tv(&settings, &path);
            }
            if !tv.contains_key(connection) {
                let settings = couch_webos::Settings::load(&connections::file(connection, "webos"))
                    .map_err(|e| e.to_string())?;
                tv.insert(
                    connection.into(),
                    couch_control::WebOs::connect(&settings).map_err(|e| e.to_string())?,
                );
            }
            let result = crate::tv::mapped_command(tv.get_mut(connection).unwrap(), &command);
            if result.is_err() {
                tv.remove(connection);
            }
            result.map_err(|e| e.to_string())
        }
        Integration::Hue { light_id } => {
            let (id, raw) = connections::split(&light_id);
            let c = couch_hue::settings::Settings::load(&connections::file(id, "hue"))
                .and_then(|s| s.client())
                .map_err(|e| e.to_string())?;
            let on = match command {
                F::On => true,
                F::Off => false,
                _ => !c
                    .control_state(raw)
                    .map_err(|e| e.to_string())?
                    .on
                    .ok_or("Hue light is unavailable")?,
            };
            c.set_power(raw, on).map_err(|e| e.to_string())
        }
        Integration::HomeAssistant { entity_id } => {
            let (c, raw) = connections::ha(&entity_id)?;
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
            .map_err(|e| e.to_string())
        }
        _ => Err("This integration cannot send button commands yet".into()),
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
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    #[test]
    fn leaving_an_activity_releases_the_avr_socket_without_another_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (observed, events) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut line = Vec::new();
            let mut b = [0];
            loop {
                match socket.read(&mut b).unwrap() {
                    0 => {
                        observed.send("closed").unwrap();
                        break;
                    }
                    _ => {
                        if b[0] == b'\r' {
                            let q = String::from_utf8(std::mem::take(&mut line)).unwrap();
                            assert!(q == "MVUP" || q == "MV?");
                            socket.write_all(b"MV275\r").unwrap();
                            if q == "MV?" {
                                observed.send("command").unwrap();
                            }
                        } else {
                            line.push(b[0]);
                        }
                    }
                }
            }
        });
        let mut config = Config::default();
        config.rooms.push(couch_model::Room {
            id: "room".into(),
            name: "Room".into(),
            icon: None,
            devices: vec![couch_model::Device::new(
                "avr".into(),
                "AVR",
                couch_model::DeviceKind::Speaker,
            )
            .with_integration(Integration::Denon {
                host: "127.0.0.1".into(),
                port,
            })],
        });
        let (tx, rx) = mpsc::sync_channel(8);
        let (reply, _out) = mpsc::sync_channel(8);
        let current = Arc::new(AtomicU64::new(1));
        let shared = current.clone();
        let thread = std::thread::spawn(move || worker(rx, reply, shared));
        tx.send(Request {
            generation: 1,
            at: Instant::now(),
            config: Arc::new(config),
            action: Action::new("avr", "volume-up"),
            repeat: false,
        })
        .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(3)).unwrap(),
            "command"
        );
        current.store(2, Ordering::SeqCst);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "closed"
        );
        drop(tx);
        thread.join().unwrap();
        server.join().unwrap();
    }
}
