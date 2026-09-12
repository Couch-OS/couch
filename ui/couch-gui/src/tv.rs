//! TV control: one background owner, bounded input, no network on the UI thread.
#[path = "tv_android.rs"]
mod android;
#[path = "tv_apple.rs"]
mod apple;
#[path = "tv_ir.rs"]
mod infrared;
#[path = "tv_media.rs"]
mod media;
#[path = "tv_sonos.rs"]
mod sonos;
use crate::{home, App, TvChoice};
use couch_control::WebOs as Client;
use couch_webos::{Button, Playback, Settings};
use serde_json::json;
use slint::{ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
enum Command {
    Key(Button),
    Volume(bool),
    Channel(bool),
    Mute(bool),
    ToggleMute,
    Power,
    Wake,
    Play(bool),
    Retry,
    Next(bool),
    Stop,
    Rewind(bool),
    Input(String),
    App(String),
    Sound(String),
    IrFunction(String),
}
fn command(name: &str) -> Option<Command> {
    if let Some(id) = name.strip_prefix("ir:") {
        return Some(Command::IrFunction(id.into()));
    }
    if let Some(id) = name.strip_prefix("input:") {
        return Some(Command::Input(id.into()));
    }
    if let Some(id) = name.strip_prefix("app:") {
        return Some(Command::App(id.into()));
    }
    if let Some(id) = name.strip_prefix("sound:") {
        return ["tv_speaker", "external_arc"]
            .contains(&id)
            .then(|| Command::Sound(id.into()));
    }

    Some(match name {
        "power" => Command::Power,
        "wake" => Command::Wake,
        "toggle-mute" => Command::ToggleMute,
        "red" => Command::Key(Button::Red),
        "green" => Command::Key(Button::Green),
        "blue" => Command::Key(Button::Blue),
        "yellow" => Command::Key(Button::Yellow),
        "up" => Command::Key(Button::Up),
        "down" => Command::Key(Button::Down),
        "left" => Command::Key(Button::Left),
        "right" => Command::Key(Button::Right),
        "ok" => Command::Key(Button::Enter),
        "back" => Command::Key(Button::Back),
        "home" => Command::Key(Button::Home),
        "menu" => Command::Key(Button::Menu),
        "volume-up" => Command::Volume(true),
        "volume-down" => Command::Volume(false),
        "channel-up" => Command::Channel(true),
        "channel-down" => Command::Channel(false),
        "mute" => Command::Mute(true),
        "unmute" => Command::Mute(false),
        "play" => Command::Play(true),
        "pause" => Command::Play(false),
        "retry" => Command::Retry,
        "next" => Command::Next(true),
        "previous" => Command::Next(false),
        "stop" => Command::Stop,
        "rewind" => Command::Rewind(false),
        "forward" => Command::Rewind(true),
        _ => return None,
    })
}
fn execute(c: &mut Client, action: &Command) -> couch_control::Result<()> {
    match action {
        Command::Key(key) => c.button(*key),
        Command::Volume(true) => c.volume_up(),
        Command::Volume(false) => c.volume_down(),
        Command::Channel(up) => c.channel(*up),
        Command::Mute(on) => c.mute(*on),
        Command::Power => c.power_off(),
        Command::ToggleMute => c.toggle_mute(),
        Command::Play(play) => c.playback(if *play {
            Playback::Play
        } else {
            Playback::Pause
        }),
        Command::Retry => Ok(()),
        Command::Next(_) | Command::Wake | Command::IrFunction(_) => {
            Err(couch_control::Error::Rejected)
        }
        Command::Stop => c.playback(Playback::Stop),
        Command::Rewind(forward) => c.playback(if *forward {
            Playback::FastForward
        } else {
            Playback::Rewind
        }),
        Command::Input(id) => c.select_input(id),
        Command::App(id) => c.launch_app(id),
        Command::Sound(output) => c
            .request(
                "ssap://com.webos.service.apiadapter/audio/changeSoundOutput",
                json!({"output":output}),
            )
            .map(|_| ()),
    }
}
fn volume(c: &mut Client) -> couch_control::Result<String> {
    let v = c.volume()?;
    let v = if v["volumeStatus"].is_object() {
        &v["volumeStatus"]
    } else {
        &v
    };
    Ok(format!(
        "Volume {}{}",
        v["volume"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
        if v["muteStatus"] == true || v["muted"] == true {
            " · Muted"
        } else {
            ""
        }
    ))
}
// Bind the learned wake address to this exact pairing endpoint. It is private
// device state, not part of exported room configuration.
fn remember_wake(settings: &Settings, credentials: &std::path::Path) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let Ok(address) = settings.address() else {
        return;
    };
    let Ok(arp) = std::fs::read_to_string("/proc/net/arp") else {
        return;
    };
    let Some(mac) = arp
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 6 && fields[0] == address.to_string() && fields[2] == "0x2")
                .then(|| fields[3].to_string())
        })
        .next()
    else {
        return;
    };
    if couch_webos::magic_packet(&mac).is_err() || mac == "00:00:00:00:00:00" {
        return;
    }
    let data = serde_json::json!({"url":settings.url,"mac":mac}).to_string();
    let file = credentials.with_file_name("webos-wake.json");
    if std::fs::read_to_string(&file).ok().as_deref() == Some(&data) {
        return;
    }
    let tmp = file.with_extension("new");
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
}
pub(crate) fn wake_tv(settings: &Settings, credentials: &std::path::Path) -> Result<(), String> {
    let saved = std::fs::read(credentials.with_file_name("webos-wake.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .ok_or("Connect while the TV is on once to learn its wake address")?;
    if saved["url"].as_str() != Some(settings.url.as_str()) {
        return Err("Connect to this TV while it is on once to learn its wake address".into());
    }
    couch_webos::wake(
        saved["mac"].as_str().ok_or("Missing TV wake address")?,
        std::net::Ipv4Addr::BROADCAST,
    )
    .map_err(|e| e.to_string())
}
fn power(
    client: &mut Option<Client>,
    active: &AtomicU64,
    generation: u64,
    credentials: &std::path::Path,
) -> Result<String, String> {
    let settings = Settings::load(credentials).map_err(|e| e.to_string())?;
    let preference = couch_webos::power::PowerSettings::load(credentials, &settings.url)?;
    if preference.method == couch_webos::power::Method::Ir {
        if active.load(Ordering::SeqCst) != generation {
            return Ok(String::new());
        }
        preference.transmit("power")?;
        return Ok("IR power toggle sent".into());
    }
    if client.is_none() {
        match Client::connect(&settings) {
            Ok(c) => *client = Some(c),
            Err(couch_control::Error::Transport | couch_control::Error::Timeout) => {
                if active.load(Ordering::SeqCst) != generation {
                    return Ok(String::new());
                }
                wake_tv(&settings, credentials)?;
                return Ok("Wake requested…".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    if active.load(Ordering::SeqCst) != generation {
        return Ok(String::new());
    }
    remember_wake(&settings, credentials);
    let status = client
        .as_mut()
        .unwrap()
        .power_state()
        .map_err(|e| e.to_string())?;
    let state = status["state"]
        .as_str()
        .ok_or("TV did not report its power state")?;
    if active.load(Ordering::SeqCst) != generation {
        return Ok(String::new());
    }
    if state != "Active" {
        wake_tv(&settings, credentials)?;
        *client = None;
        return Ok("Wake requested…".into());
    }
    // A failed power-off write is ambiguous: never follow it with a wake packet.
    client
        .as_mut()
        .unwrap()
        .power_off()
        .map_err(|e| e.to_string())?;
    *client = None;
    Ok("TV powered off · Press Power to wake".into())
}
struct Work {
    connection: String,
    device: Option<String>,
    generation: u64,
    action: Command,
    at: Instant,
    repeat: bool,
    config: Option<Arc<couch_model::Config>>,
}
#[derive(Default, Clone)]
struct Details {
    sources: Vec<(String, String)>,
    source: String,
    sound: String,
    picture: String,
    choices: Vec<(String, String, String)>,
    settings_app: Option<String>,
}
fn details(c: &mut Client) -> Details {
    let inputs = c.inputs().unwrap_or_default();
    let foreground = c.foreground_app().unwrap_or_default();
    let apps = c.apps().unwrap_or_default();
    let app = foreground["appId"].as_str().unwrap_or("");
    let devices = inputs["devices"].as_array().cloned().unwrap_or_default();
    let launches = apps["launchPoints"].as_array().cloned().unwrap_or_default();
    let source = devices
        .iter()
        .find(|d| d["appId"] == app)
        .and_then(|d| d["label"].as_str())
        .or_else(|| {
            launches
                .iter()
                .find(|d| d["id"] == app)
                .and_then(|d| d["title"].as_str())
        })
        .or(foreground["appName"].as_str())
        .unwrap_or(if app.is_empty() {
            "Source unavailable"
        } else {
            app
        })
        .to_string();
    let mut choices = Vec::new();
    let mut sources = Vec::new();
    for d in &devices {
        if let (Some(id), Some(label)) = (d["appId"].as_str(), d["label"].as_str()) {
            sources.push((id.into(), label.into()));
        }
    }
    for d in &launches {
        if let (Some(id), Some(label)) = (d["id"].as_str(), d["title"].as_str()) {
            if !sources.iter().any(|(key, _)| key == id) {
                sources.push((id.into(), label.into()));
            }
        }
    }
    for d in devices {
        if let Some(id) = d["id"].as_str() {
            choices.push((
                format!("input:{id}"),
                d["label"].as_str().unwrap_or(id).into(),
                if d["connected"] == true {
                    "Connected"
                } else {
                    "No signal reported"
                }
                .into(),
            ));
        }
    }
    let mut settings_app = None;
    for d in launches {
        if let (Some(id), Some(title)) = (d["id"].as_str(), d["title"].as_str()) {
            if id == "com.palm.app.settings" || id == "com.webos.app.settings" {
                settings_app = Some(id.into());
            }
            choices.push((format!("app:{id}"), title.into(), "App".into()));
        }
    }
    let sound = c
        .request(
            "ssap://com.webos.service.apiadapter/audio/getSoundOutput",
            json!({}),
        )
        .ok()
        .and_then(|v| v["soundOutput"].as_str().map(str::to_string))
        .unwrap_or_default();
    let picture = c
        .request(
            "ssap://settings/getSystemSettings",
            json!({"category":"picture","keys":["pictureMode"]}),
        )
        .ok()
        .and_then(|v| v["settings"]["pictureMode"].as_str().map(str::to_string))
        .unwrap_or_default();
    Details {
        sources,
        source,
        sound,
        picture,
        choices,
        settings_app,
    }
}
struct Event {
    details: Option<Details>,
    generation: u64,
    status: Result<String, String>,
}
fn worker(rx: mpsc::Receiver<Work>, tx: mpsc::SyncSender<Event>, active: Arc<AtomicU64>) {
    let mut credentials = home::path("webos-connection.json");
    let mut client = None;
    let mut android_client = None;
    let mut android_mode = false;
    let mut apple_mode = false;
    let mut generation = 0;
    let mut refreshed = Instant::now();
    let mut waking: Option<Instant> = None;
    let mut view: Option<Details> = None;
    loop {
        let work = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(w) => Some(w),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        let current = active.load(Ordering::SeqCst);
        if current != generation {
            client = None;
            android_client = None;
            android_mode = false;
            apple_mode = false;
            generation = current;
            waking = None;
            view = None;
        }
        if current != 0
            && (android_mode || apple_mode)
            && refreshed.elapsed() >= Duration::from_secs(4)
        {
            if let Some(c) = android_client.as_ref() {
                match if apple_mode {
                    apple::refresh(c, generation)
                } else {
                    android::refresh(c, generation)
                } {
                    Ok(event) => {
                        let _ = tx.try_send(event);
                    }
                    Err(error) => {
                        android_client = None;
                        let _ = tx.try_send(Event {
                            generation,
                            details: None,
                            status: Err(error),
                        });
                    }
                }
            }
            refreshed = Instant::now();
        }
        if let Some(w) = work {
            if w.generation != current
                || current == 0
                || (!matches!(w.action, Command::Retry)
                    && w.at.elapsed() > Duration::from_millis(750))
            {
                continue;
            }
            if !w.connection.starts_with("ir:") {
                if let (Some(device), Some(config), Ok(Some(function))) =
                    (&w.device, &w.config, infrared::function(&w.action))
                {
                    let current = || {
                        infrared::request_current(
                            &w,
                            &active,
                            crate::connections::config().as_ref(),
                        )
                    };
                    let result = crate::activity_buttons::try_device_ir(
                        config,
                        device,
                        &couch_model::commands::Function::parse(&function).unwrap(),
                        w.repeat,
                        &current,
                    );
                    match result {
                        Ok(false) => {}
                        result => {
                            let status = result.map(|_| String::new());
                            let _ = tx.try_send(Event {
                                generation,
                                details: None,
                                status,
                            });
                            continue;
                        }
                    }
                }
            }
            if w.connection.starts_with("sonos:") {
                match sonos::run(&w,&active) {
                    Ok(Some(event)) => { let _=tx.try_send(event); },
                    Ok(None) => {},
                    Err(error) => { let _=tx.try_send(Event{generation,details:None,status:Err(error)}); },
                }
                continue;
            }
            if w.connection.starts_with("ir:") {
                let result = infrared::run(&w, &active);
                match result {
                    Ok(Some(event)) => {
                        let _ = tx.try_send(event);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = tx.try_send(Event {
                            generation,
                            details: None,
                            status: Err(error),
                        });
                    }
                }
                continue;
            }
            let provider = crate::connections::config().and_then(|c| {
                c.connection(&couch_model::Id::new(&w.connection))
                    .map(|c| c.provider.clone())
            });
            android_mode = provider == Some(couch_model::Provider::AndroidTv);
            apple_mode = provider == Some(couch_model::Provider::AppleTv);
            if android_mode || apple_mode {
                let result = if apple_mode {
                    apple::run(&mut android_client, &w, &active)
                } else {
                    android::run(&mut android_client, &w, &active)
                };
                match result {
                    Ok(Some(event)) => {
                        let _ = tx.try_send(event);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        android_client = None;
                        let _ = tx.try_send(Event {
                            generation,
                            details: None,
                            status: Err(error),
                        });
                    }
                }
                if matches!(w.action, Command::Retry) {
                    refreshed = Instant::now();
                }
                continue;
            }
            credentials = crate::connections::file(&w.connection, "webos");
            if !w.connection.is_empty() && provider != Some(couch_model::Provider::WebOs) {
                continue;
            }
            if matches!(w.action, Command::Power) {
                waking = None;
                let result = power(&mut client, &active, generation, &credentials);
                if result.as_deref() == Ok("Wake requested…") {
                    waking = Some(Instant::now() + Duration::from_secs(30));
                }
                if result.is_err() {
                    client = None;
                }
                let _ = tx.try_send(Event {
                    details: None,
                    generation,
                    status: result,
                });
                refreshed = Instant::now();
                continue;
            }
            let result = (|| {
                if matches!(w.action, Command::Retry) || client.is_none() {
                    let settings = Settings::load(&credentials)?;
                    let mut connected = Client::connect(&settings)?;
                    connected.prepare_input()?;
                    remember_wake(&settings, &credentials);
                    client = Some(connected);
                }
                // Opening or closing another screen cancels queued keys, including
                // one that was waiting for connection establishment.
                if active.load(Ordering::SeqCst) != generation
                    || (!matches!(w.action, Command::Retry)
                        && w.at.elapsed() > Duration::from_millis(750))
                {
                    return Ok(String::new());
                }
                let c = client.as_mut().unwrap();
                execute(c, &w.action)?;
                if matches!(
                    w.action,
                    Command::Volume(_) | Command::Mute(_) | Command::ToggleMute | Command::Retry
                ) {
                    volume(c)
                } else {
                    Ok(String::new())
                }
            })();
            if let Err(ref error) = result {
                if !matches!(error, couch_control::Error::Rejected) {
                    client = None;
                }
            }
            let _ = tx.try_send(Event {
                details: None,
                generation,
                status: result.map_err(|e| e.to_string()),
            });
            if matches!(
                w.action,
                Command::Retry | Command::Input(_) | Command::App(_) | Command::Sound(_)
            ) {
                if let Some(c) = client.as_mut() {
                    let fresh = details(c);
                    view = Some(fresh.clone());
                    let _ = tx.try_send(Event {
                        generation,
                        status: Ok(String::new()),
                        details: Some(fresh),
                    });
                }
            }
            refreshed = Instant::now();
        } else if current != 0 && waking.is_some() && refreshed.elapsed() > Duration::from_secs(2) {
            let connected = Settings::load(&credentials)
                .map_err(couch_control::Error::from)
                .and_then(|s| Client::connect(&s))
                .and_then(|mut c| {
                    if c.power_state()?["state"] == "Active" {
                        Ok(c)
                    } else {
                        Err(couch_control::Error::Timeout)
                    }
                });
            if let Ok(mut c) = connected {
                let result = volume(&mut c).map_err(|e| e.to_string());
                client = Some(c);
                waking = None;
                let _ = tx.try_send(Event {
                    details: None,
                    generation,
                    status: result,
                });
            } else if waking.is_some_and(|until| Instant::now() >= until) {
                waking = None;
                let _ = tx.try_send(Event {
                    details: None,
                    generation,
                    status: Err(
                        "TV did not wake. Enable network/mobile power-on in the LG TV settings"
                            .into(),
                    ),
                });
            }
            refreshed = Instant::now();
        } else if current != 0 && refreshed.elapsed() > Duration::from_secs(5) {
            if let Some(c) = client.as_mut() {
                let result = volume(c);
                if result.is_ok() {
                    if let Some(v) = view.as_mut() {
                        let foreground = c.foreground_app().ok();
                        let app_id = foreground.as_ref().and_then(|f| f["appId"].as_str());
                        v.source = app_id
                            .map(|id| {
                                v.sources
                                    .iter()
                                    .find(|(key, _)| key == id)
                                    .map(|(_, name)| name.as_str())
                                    .unwrap_or(id)
                            })
                            .unwrap_or("Source unavailable")
                            .into();
                        v.sound = c
                            .request(
                                "ssap://com.webos.service.apiadapter/audio/getSoundOutput",
                                json!({}),
                            )
                            .ok()
                            .and_then(|s| s["soundOutput"].as_str().map(str::to_string))
                            .unwrap_or_default();
                        let _ = tx.try_send(Event {
                            generation,
                            status: Ok(String::new()),
                            details: Some(v.clone()),
                        });
                    }
                }
                if result.is_err() {
                    client = None;
                }
                let _ = tx.try_send(Event {
                    details: None,
                    generation,
                    status: result.map_err(|e| e.to_string()),
                });
            }
            refreshed = Instant::now();
        }
    }
}
// UI-thread presentation only. Cached values never enter Work or client state.
#[derive(Clone, PartialEq, Eq)]
struct ViewKey {
    connection: String,
    device: String,
    name: String,
    activity: String,
    config: u64,
}
struct ViewCache<T>(std::collections::VecDeque<(ViewKey, Instant, T)>);
impl<T> Default for ViewCache<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}
impl<T: Clone> ViewCache<T> {
    fn put(&mut self, key: ViewKey, at: Instant, value: T) {
        self.0.retain(|(k, stamp, _)| {
            k != &key && k.config == key.config && stamp.elapsed() < Duration::from_secs(30)
        });
        self.0.push_back((key, at, value));
        while self.0.len() > 3 {
            self.0.pop_front();
        }
    }
    fn get(&mut self, key: &ViewKey) -> Option<(Instant, T)> {
        self.0
            .retain(|(k, at, _)| k.config == key.config && at.elapsed() < Duration::from_secs(30));
        self.0
            .iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, at, v)| (*at, v.clone()))
    }
    fn remove(&mut self, key: &ViewKey) {
        self.0.retain(|(k, _, _)| k != key);
    }
}
#[derive(Clone)]
struct ViewPresentation {
    source: slint::SharedString,
    sound: slint::SharedString,
    picture: slint::SharedString,
    status: slint::SharedString,
}
impl ViewPresentation {
    fn capture(app: &App) -> Self {
        Self {
            source: app.get_tv_source(),
            sound: app.get_tv_sound(),
            picture: app.get_tv_picture(),
            status: app.get_tv_status(),
        }
    }
    fn apply(&self, app: &App) {
        app.set_tv_source(self.source.clone());
        app.set_tv_sound(self.sound.clone());
        app.set_tv_picture(self.picture.clone());
        app.set_tv_status(self.status.clone());
    }
}
fn resolve_target(
    config: Option<&couch_model::Config>,
    requested: &str,
) -> Result<(String, Option<String>), String> {
    let Some(id) = requested.strip_prefix("device:") else {
        return Ok((
            requested.into(),
            requested.strip_prefix("ir:").map(str::to_owned),
        ));
    };
    let config = config.ok_or("Configuration unavailable")?;
    let device = config
        .devices()
        .find(|(_, d)| d.id.as_str() == id)
        .map(|(_, d)| d)
        .ok_or("Device was removed")?;
    let integration = config.resolve_integration(&device.integration);
    if matches!(
        integration,
        Some(couch_model::Integration::None | couch_model::Integration::Ir { .. }) | None
    ) && device.effective_ir_codeset(config).is_some()
    {
        return Ok((format!("ir:{id}"), Some(id.into())));
    }
    if matches!(integration,Some(couch_model::Integration::Sonos{..})) {
        return Ok((format!("sonos:{id}"),Some(id.into())));
    }
    let provider = match integration {
        Some(couch_model::Integration::AndroidTv) => couch_model::Provider::AndroidTv,
        Some(couch_model::Integration::AppleTv) => couch_model::Provider::AppleTv,
        Some(couch_model::Integration::WebOs) => couch_model::Provider::WebOs,
        _ => return Err("This device does not have TV controls".into()),
    };
    let connection = match &device.integration {
        couch_model::Integration::Connection { connection_id, .. } => connection_id.to_string(),
        _ => config
            .connections
            .iter()
            .find(|c| c.provider == provider)
            .map(|c| c.id.to_string())
            .unwrap_or_default(),
    };
    Ok((connection, Some(id.into())))
}
pub struct Controller {
    media: media::Controller,
    view_cache: ViewCache<ViewPresentation>,
    view_key: Option<ViewKey>,
    view_at: Option<Instant>,
    choices: Vec<(String, String, String)>,
    settings_app: Option<String>,
    connection: String,
    device: Option<String>,
    input: Rc<RefCell<Vec<(String, bool, Option<Arc<couch_model::Config>>)>>>,
    physical_repeat: Rc<Cell<bool>>,
    tx: mpsc::SyncSender<Work>,
    rx: mpsc::Receiver<Event>,
    active: Arc<AtomicU64>,
    generation: u64,
}
impl Controller {
    pub fn new(app: &App) -> Self {
        let input = Rc::new(RefCell::new(Vec::new()));
        let q = input.clone();
        app.on_open_tv(move |id, name| {
            q.borrow_mut().push((
                format!("open:{id}/{name}"),
                false,
                crate::connections::config(),
            ))
        });
        let q = input.clone();
        let physical_repeat = Rc::new(Cell::new(false));
        let repeat = physical_repeat.clone();
        app.on_tv_action(move |action| {
            q.borrow_mut().push((
                action.to_string(),
                repeat.get(),
                crate::connections::config(),
            ))
        });
        let (tx, requests) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::sync_channel(16);
        let active = Arc::new(AtomicU64::new(0));
        let current = active.clone();
        std::thread::spawn(move || worker(requests, events, current));
        Self {
            media: media::Controller::new(),
            view_cache: ViewCache::default(),
            view_key: None,
            view_at: None,
            choices: Vec::new(),
            settings_app: None,
            connection: String::new(),
            device: None,
            input,
            physical_repeat,
            tx,
            rx,
            active,
            generation: 0,
        }
    }
    /// Slint synthesizes releases after each press; preserve physical repeat
    /// metadata only during this dispatch, without affecting touch callbacks.
    pub fn physical_input(&self, repeat: bool, dispatch: impl FnOnce()) {
        let previous = self.physical_repeat.replace(repeat);
        dispatch();
        self.physical_repeat.set(previous);
    }
    pub fn navigation_pending(&self) -> bool {
        self.input
            .borrow()
            .iter()
            .any(|(action, _, _)| action.starts_with("open:") || action == "close")
    }
    fn save_view(&mut self, app: &App) {
        if let (Some(key), Some(at)) = (&self.view_key, self.view_at) {
            if app.get_tv_error().is_empty() {
                self.view_cache
                    .put(key.clone(), at, ViewPresentation::capture(app));
            }
        }
    }
    pub fn poll(&mut self, app: &App) {
        if app.get_tv_shown()
            && self
                .view_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(30))
        {
            self.view_at = None;
            if let Some(key) = &self.view_key {
                self.view_cache.remove(key);
            }
            app.set_tv_source("Checking…".into());
            app.set_tv_sound("Checking…".into());
            app.set_tv_picture("Checking…".into());
            app.set_tv_status(if app.get_tv_sonos() {"Checking Sonos status…"} else {"Checking TV status…"}.into());
            if app.get_tv_sonos() {
                let _=self.tx.try_send(Work{connection:self.connection.clone(),device:self.device.clone(),generation:self.generation,action:Command::Retry,at:Instant::now(),repeat:false,config:crate::connections::config()});
            }
        }
        let inputs = std::mem::take(&mut *self.input.borrow_mut());
        for (action, repeat, config) in inputs {
            let action = if let Some(target) = action.strip_prefix("open:") {
                let (requested, name) = target.split_once('/').unwrap_or(("", target));
                let resolved = resolve_target(config.as_deref(), requested);
                let (connection, device) = match resolved {
                    Ok(target) => target,
                    Err(error) => {
                        app.set_tv_error(error.into());
                        continue;
                    }
                };
                let connection = connection.as_str();
                self.save_view(app);
                self.device = device;
                let key = ViewKey {
                    connection: connection.into(),
                    device: self.device.clone().unwrap_or_default(),
                    name: name.into(),
                    activity: app.get_active_activity().to_string(),
                    config: crate::config_snapshot::current().map_or(0, |s| s.serial),
                };
                self.view_key = Some(key.clone());
                self.view_at = None;
                self.connection = connection.into();
                let android = crate::connections::config().is_some_and(|c| {
                    c.connection(&couch_model::Id::new(connection))
                        .is_some_and(|c| c.provider == couch_model::Provider::AndroidTv)
                });
                let apple = crate::connections::config().is_some_and(|c| {
                    c.connection(&couch_model::Id::new(connection))
                        .is_some_and(|c| c.provider == couch_model::Provider::AppleTv)
                });
                app.set_tv_sonos(connection.starts_with("sonos:"));
                app.set_tv_ir(connection.starts_with("ir:"));
                app.set_tv_android(android);
                app.set_tv_apple(apple);
                self.generation += 1;
                self.active.store(self.generation, Ordering::SeqCst);
                if android || apple {
                    self.media.open(app, self.generation, &key);
                } else {
                    self.media.clear(app);
                }
                self.choices.clear();
                self.settings_app = None;
                app.set_tv_source(
                    if app.get_tv_ir() {
                        "Loading commands…"
                    } else {
                        "Connecting…"
                    }
                    .into(),
                );
                app.set_tv_sound("Checking…".into());
                app.set_tv_picture("Checking…".into());
                app.set_tv_panel(0);
                app.set_tv_title(name.into());
                app.set_tv_status(
                    if app.get_tv_ir() {
                        "Infrared · No device feedback"
                    } else {
                        if app.get_tv_sonos() {"Connecting to Sonos…"} else {"Connecting to TV…"}
                    }
                    .into(),
                );
                app.set_tv_error("".into());
                if let Some((at, view)) = self.view_cache.get(&key) {
                    view.apply(app);
                    self.view_at = Some(at);
                }
                app.set_tv_shown(true);
                app.invoke_focus_tv();
                "retry"
            } else {
                action.as_str()
            };
            if action == "dismiss" {
                app.set_tv_panel(0);
                continue;
            }
            if ["inputs", "apps", "picture", "sound", "commands"].contains(&action) {
                if app.get_tv_sonos() { app.set_tv_error("Sonos has no TV inputs or apps".into()); continue; }
                if app.get_tv_ir() && action != "commands" {
                    app.set_tv_error("Infrared devices do not report apps or settings".into());
                    continue;
                }
                if (app.get_tv_android() || app.get_tv_apple()) && action != "apps" {
                    app.set_tv_error("This control is only available for LG webOS TVs".into());
                    continue;
                }
                if app.get_tv_android() {
                    self.choices = crate::connections::config()
                        .and_then(|c| {
                            c.app_shortcuts
                                .get(&couch_model::Id::new(&self.connection))
                                .cloned()
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(|app| {
                            (
                                format!("app:{}", app.url),
                                app.name,
                                "Configured shortcut".into(),
                            )
                        })
                        .collect();
                }
                let panel = match action {
                    "inputs" => 1,
                    "apps" | "commands" => 2,
                    "picture" => 3,
                    _ => 4,
                };
                let mut rows: Vec<TvChoice> = self
                    .choices
                    .iter()
                    .filter(|(id, _, _)| {
                        if panel == 1 {
                            id.starts_with("input:")
                        } else {
                            panel == 2
                                && (id.starts_with("app:")
                                    || (app.get_tv_ir() && id.starts_with("ir:")))
                        }
                    })
                    .map(|(id, title, detail)| TvChoice {
                        action: id.as_str().into(),
                        title: title.as_str().into(),
                        detail: detail.as_str().into(),
                    })
                    .collect();
                if panel == 3 {
                    if let Some(id) = &self.settings_app {
                        rows.push(TvChoice {
                            action: format!("app:{id}").into(),
                            title: "Open TV settings".into(),
                            detail: "Adjust picture on your TV".into(),
                        });
                    }
                }
                if panel == 4 {
                    for (id, name) in [
                        ("tv_speaker", "TV speakers"),
                        ("external_arc", "HDMI ARC / eARC"),
                    ] {
                        rows.push(TvChoice {
                            action: format!("sound:{id}").into(),
                            title: name.into(),
                            detail: "TV support required".into(),
                        });
                    }
                }
                app.set_tv_choices(ModelRc::new(VecModel::from(rows)));
                app.set_tv_panel(panel);
                continue;
            }
            if action == "close" {
                self.save_view(app);
                app.set_tv_panel(0);
                self.active.store(0, Ordering::SeqCst);
                self.media.clear(app);
                app.set_tv_shown(false);
                app.set_tv_error("".into());
                if app.get_light_shown() {
                    app.invoke_focus_light();
                } else {
                    app.invoke_focus_home();
                }
                continue;
            }
            if !app.get_tv_shown() {
                continue;
            }
            if let Some(action) = command(action) {
                app.set_tv_panel(0);
                if self
                    .tx
                    .try_send(Work {
                        connection: self.connection.clone(),
                        device: self.device.clone(),
                        generation: self.generation,
                        action,
                        at: Instant::now(),
                        repeat,
                        config,
                    })
                    .is_err()
                {
                    app.set_tv_error("TV is busy; try again.".into());
                }
            }
        }
        self.media.poll(app);
        while let Ok(event) = self.rx.try_recv() {
            if event.generation != self.active.load(Ordering::SeqCst) || !app.get_tv_shown() {
                continue;
            }
            if let Some(view) = event.details {
                self.view_at = Some(Instant::now());
                app.set_tv_source(view.source.into());
                app.set_tv_sound(
                    match view.sound.as_str() {
                        "tv_speaker" => "TV speakers",
                        "external_arc" => "HDMI ARC / eARC",
                        "" => "Unavailable",
                        v => v,
                    }
                    .into(),
                );
                app.set_tv_picture(if view.picture.is_empty() {
                    "On your TV".into()
                } else {
                    view.picture.into()
                });
                self.choices = view.choices;
                self.settings_app = view.settings_app;
                if (app.get_tv_apple() || app.get_tv_ir()) && app.get_tv_panel() == 2 {
                    app.set_tv_choices(ModelRc::new(VecModel::from(
                        self.choices
                            .iter()
                            .map(|(id, title, detail)| TvChoice {
                                action: id.as_str().into(),
                                title: title.as_str().into(),
                                detail: detail.as_str().into(),
                            })
                            .collect::<Vec<_>>(),
                    )));
                }
            }
            match event.status {
                Ok(status) => {
                    app.set_tv_error("".into());
                    if !status.is_empty() {
                        app.set_tv_status(status.into());
                    }
                }
                Err(error) => {
                    if let Some(key) = &self.view_key {
                        self.view_cache.remove(key);
                    }
                    self.view_at = None;
                    self.media.invalidate(app);
                    app.set_tv_source("Unavailable".into());
                    app.set_tv_sound("Unavailable".into());
                    app.set_tv_picture("On your TV".into());
                    app.set_tv_status("Control needs attention".into());
                    app.set_tv_error(error.into());
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_targets_preserve_network_views_and_distinguish_connection_aliases() {
        let config: couch_model::Config = serde_json::from_value(json!({"schema_version":1,
            "connections":[{"id":"shared","name":"TV","provider":{"kind":"android-tv"}}],
            "rooms":[{"id":"r","name":"Room","devices":[
                {"id":"a","name":"A","kind":"tv","integration":{"via":"connection","connection_id":"shared","resource_id":""},"ir":{"codeset":"first"}},
                {"id":"b","name":"B","kind":"tv","integration":{"via":"connection","connection_id":"shared","resource_id":""},"ir":{"codeset":"second"}},
                {"id":"ir-only","name":"IR","kind":"other","ir":{"codeset":"third"}}
            ]}]})).unwrap();
        assert_eq!(
            resolve_target(Some(&config), "device:a").unwrap(),
            ("shared".into(), Some("a".into()))
        );
        assert_eq!(
            resolve_target(Some(&config), "device:b").unwrap(),
            ("shared".into(), Some("b".into()))
        );
        assert_eq!(
            resolve_target(Some(&config), "device:ir-only").unwrap(),
            ("ir:ir-only".into(), Some("ir-only".into()))
        );
        assert!(resolve_target(Some(&config), "device:removed").is_err());
    }
    #[test]
    fn presentation_cache_is_bounded_scoped_and_does_not_renew_age() {
        let key = |id: &str, config| ViewKey {
            connection: id.into(),
            device: id.into(),
            name: "TV".into(),
            activity: String::new(),
            config,
        };
        let mut cache = ViewCache::default();
        let now = Instant::now();
        for id in ["a", "b", "c", "d"] {
            cache.put(key(id, 1), now, id.to_owned());
        }
        assert!(cache.get(&key("a", 1)).is_none());
        assert_eq!(cache.get(&key("d", 1)).unwrap().1, "d");
        let mut activity = key("d", 1);
        activity.activity = "movie".into();
        assert!(cache.get(&activity).is_none());
        cache.put(key("d", 1), now - Duration::from_secs(31), "old".into());
        assert!(cache.get(&key("d", 1)).is_none());
        assert!(cache.get(&key("b", 2)).is_none());
        assert!(cache.get(&key("b", 1)).is_none());
    }
    #[test]
    fn default_lg_power_requires_ir_codes_before_any_network_connection() {
        let root = std::env::temp_dir().join(format!(
            "couch-tv-power-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("webos-connection.json");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Settings {
            url: format!("ws://127.0.0.1:{}/", listener.local_addr().unwrap().port()),
            client_key: "fixture".into(),
            certificate: vec![],
        }
        .save(&path)
        .unwrap();
        let mut client = None;
        let result = power(&mut client, &AtomicU64::new(1), 1, &path);
        assert!(result.unwrap_err().contains("verified power IR code"));
        assert!(client.is_none());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn trays_slide_both_ways_with_toast_margins() {
        if std::env::var_os("COUCH_TEST_TRAYS").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tv::tests::trays_slide_both_ways_with_toast_margins",
                ])
                .env("COUCH_TEST_TRAYS", "1")
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
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Platform, PlatformError, WindowAdapter};
        use slint::ComponentHandle;
        struct TestPlatform {
            window: Rc<MinimalSoftwareWindow>,
            clock: Rc<std::cell::Cell<Duration>>,
        }
        impl Platform for TestPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
                Ok(self.window.clone())
            }
            fn duration_since_start(&self) -> Duration {
                self.clock.get()
            }
        }
        // Advance animation time explicitly: a compiler running in parallel
        // must not turn a requested middle frame into a completed animation.
        let clock = Rc::new(std::cell::Cell::new(Duration::ZERO));
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(slint::PhysicalSize::new(480, 800));
        slint::platform::set_platform(Box::new(TestPlatform {
            window: window.clone(),
            clock: clock.clone(),
        }))
        .unwrap();
        let app = App::new().unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_tv_action(move |action| received.borrow_mut().push(action.to_string()));
        let received = actions.clone();
        app.on_player_action(move |action, _| received.borrow_mut().push(action.to_string()));
        app.show().unwrap();
        window.dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(true));
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        let mut draw = |wait| {
            clock.set(clock.get() + Duration::from_millis(wait));
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            pixels.clone()
        };
        let surface = |p: slint::Rgb8Pixel| [p.r, p.g, p.b] == [31, 28, 23];
        let top = |frame: &[slint::Rgb8Pixel]| (0..800).find(|&y| surface(frame[y * 480 + 60]));
        for (tv, panel) in [
            (true, 1),
            (true, 2),
            (true, 3),
            (true, 4),
            (false, 1),
            (false, 2),
            (false, 3),
        ] {
            app.set_tv_shown(tv);
            app.set_player_shown(!tv);
            if tv {
                app.invoke_focus_tv();
            } else {
                app.invoke_focus_player();
            }
            let baseline = draw(220);
            if tv {
                app.set_tv_panel(panel);
            } else {
                app.set_player_panel(panel);
            }
            draw(0);
            let entering = draw(65);
            let entered = draw(220);
            assert!(
                top(&entering).unwrap() > top(&entered).unwrap(),
                "tray must move upward"
            );
            assert_eq!(top(&entered), Some(145));
            assert!(surface(entered[400 * 480 + 40]));
            assert!(!surface(entered[400 * 480 + 35]));
            assert!(!surface(entered[400 * 480 + 444]));
            assert!(!surface(entered[780 * 480 + 60]));
            use slint::platform::{PointerEventButton, WindowEvent};
            for event in [
                WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(392., 190.),
                },
                WindowEvent::PointerPressed {
                    position: slint::LogicalPosition::new(392., 190.),
                    button: PointerEventButton::Left,
                },
                WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(392., 190.),
                    button: PointerEventButton::Left,
                },
            ] {
                window.dispatch_event(event);
                draw(16);
            }
            if actions.borrow().is_empty() {
                let bytes: Vec<u8> = entered.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    "/private/tmp/tray-entered.png",
                    &bytes,
                    480,
                    800,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            }
            assert_eq!(
                actions.borrow_mut().pop().as_deref(),
                Some(if tv { "dismiss" } else { "back" }),
                "close button must remain inside the inset tray"
            );
            if tv {
                app.set_tv_panel(0);
            } else {
                app.set_player_panel(0);
            }
            draw(0);
            let exiting = draw(65);
            assert!(
                top(&exiting).unwrap() > top(&entered).unwrap(),
                "tray must stay mounted and move down"
            );
            let closed = draw(220);
            if closed != baseline {
                let bytes: Vec<u8> = closed.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    "/private/tmp/tray-closed.png",
                    &bytes,
                    480,
                    800,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            }
            assert!(
                closed == baseline,
                "dismissal must restore the closed screen: tv={tv} panel={panel}"
            );
        }
        app.hide().unwrap();
    }
    #[test]
    fn infrared_screen_routes_physical_keys_without_an_overlay() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tv::tests::android_screen_routes_physical_keys_without_an_overlay",
            ])
            .env("COUCH_TEST_ANDROID_KEYS", "1")
            .env("COUCH_TEST_IR_KEYS", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[test]
    fn android_screen_routes_physical_keys_without_an_overlay() {
        if std::env::var_os("COUCH_TEST_ANDROID_KEYS").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tv::tests::android_screen_routes_physical_keys_without_an_overlay",
                ])
                .env("COUCH_TEST_ANDROID_KEYS", "1")
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
        use slint::{
            platform::{Key, WindowEvent},
            ComponentHandle,
        };
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_tv_action(move |name| received.borrow_mut().push(name.to_string()));
        app.set_tv_android(std::env::var_os("COUCH_TEST_IR_KEYS").is_none());
        app.set_tv_ir(std::env::var_os("COUCH_TEST_IR_KEYS").is_some());
        app.set_tv_shown(true);
        app.show().unwrap();
        app.invoke_focus_tv();
        for key in [
            Key::UpArrow,
            Key::DownArrow,
            Key::LeftArrow,
            Key::RightArrow,
            Key::Return,
            Key::Escape,
            Key::Home,
            Key::F13,
            Key::F14,
            Key::F23,
            Key::F24,
            Key::F21,
            Key::F22,
        ] {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: char::from(key).to_string().into(),
            });
        }
        assert_eq!(
            &*actions.borrow(),
            &[
                "up",
                "down",
                "left",
                "right",
                "ok",
                "back",
                "home",
                "power",
                "toggle-mute",
                "volume-up",
                "volume-down",
                "channel-up",
                "channel-down"
            ]
        );
        assert_eq!(app.get_tv_panel(), 0);
        if app.get_tv_ir() {
            let controls = Controller::new(&app);
            for repeat in [false, true, false] {
                controls.physical_input(repeat, || {
                    window.dispatch_event(WindowEvent::KeyPressed {
                        text: char::from(Key::F23).to_string().into(),
                    });
                    window.dispatch_event(WindowEvent::KeyReleased {
                        text: char::from(Key::F23).to_string().into(),
                    });
                });
            }
            app.invoke_tv_action("commands".into());
            let queued = controls.input.borrow();
            assert_eq!(
                queued
                    .iter()
                    .map(|(action, repeat, _)| (action.as_str(), *repeat))
                    .collect::<Vec<_>>(),
                vec![
                    ("volume-up", false),
                    ("volume-up", true),
                    ("volume-up", false),
                    ("commands", false)
                ]
            );
        }
        if let Some(path) = std::env::var_os("COUCH_ANDROID_SCREENSHOT") {
            app.set_tv_title(
                if app.get_tv_ir() {
                    "Living room TV"
                } else {
                    "Android TV"
                }
                .into(),
            );
            app.set_tv_source(
                if app.get_tv_ir() {
                    "Infrared controls"
                } else {
                    "MiTV-AFMU0"
                }
                .into(),
            );
            app.set_tv_status(
                if app.get_tv_ir() {
                    "Infrared · No device feedback"
                } else {
                    "TV on · Volume 12"
                }
                .into(),
            );
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
                renderer.render(&mut pixels, 480);
                let bytes: Vec<u8> = pixels.into_iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    std::path::Path::new(&path),
                    &bytes,
                    480,
                    800,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            });
        }
        app.hide().unwrap();
    }
    #[test]
    fn physical_actions_are_explicit_and_back_belongs_to_tv() {
        assert!(matches!(command("back"), Some(Command::Key(Button::Back))));
        assert!(matches!(command("ok"), Some(Command::Key(Button::Enter))));
        assert!(matches!(
            command("channel-down"),
            Some(Command::Channel(false))
        ));
        assert!(command("close").is_none());
        assert!(command("unknown").is_none());
    }
}

pub(crate) fn mapped_command(
    c: &mut Client,
    function: &couch_model::commands::Function,
) -> couch_control::Result<()> {
    use couch_model::commands::Function as F;
    let action = match function {
        F::Up => Command::Key(Button::Up),
        F::Down => Command::Key(Button::Down),
        F::Left => Command::Key(Button::Left),
        F::Right => Command::Key(Button::Right),
        F::Ok => Command::Key(Button::Enter),
        F::Back => Command::Key(Button::Back),
        F::Home => Command::Key(Button::Home),
        F::Menu => Command::Key(Button::Menu),
        F::PowerOff => Command::Power,
        F::VolumeUp => Command::Volume(true),
        F::VolumeDown => Command::Volume(false),
        F::Mute => Command::ToggleMute,
        F::ChannelUp => Command::Channel(true),
        F::ChannelDown => Command::Channel(false),
        F::Red => Command::Key(Button::Red),
        F::Green => Command::Key(Button::Green),
        F::Blue => Command::Key(Button::Blue),
        F::Yellow => Command::Key(Button::Yellow),
        F::Play => Command::Play(true),
        F::Pause => Command::Play(false),
        F::Rewind => Command::Rewind(false),
        F::FastForward => Command::Rewind(true),
        F::Input(id) => Command::Input(id.clone()),
        F::App(id) => Command::App(id.clone()),
        F::Stop => return c.playback(Playback::Stop),
        _ => return Err(couch_control::Error::Protocol),
    };
    execute(c, &action)
}
