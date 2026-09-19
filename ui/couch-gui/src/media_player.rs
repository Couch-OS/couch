//! Sonos on the player screen: the same full-screen presentation the Kodi
//! Cinema activity uses, driven by a Sonos group. Art, title, artist and
//! album, a live progress line with seek, transport, and three sheets:
//! sources, play modes and up next. Network I/O runs on one worker thread
//! that keeps its connection to the player; the UI thread only ever sees
//! events.
use crate::{activity_art, App, PlayerChoice};
use couch_sonos::{Client, PlayModeChange, Snapshot, Source, SourceId};
use slint::{ModelRc, VecModel};
use std::{
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

/// How often the worker is asked for a fresh snapshot while the screen is up.
/// Position is interpolated between reads, so this is about track changes made
/// elsewhere, not the clock.
const REFRESH: Duration = Duration::from_secs(3);
/// One press of a volume key on this screen.
const VOLUME_STEP: i8 = 5;
const SHEETS: [&str; 3] = ["Sources", "Modes", "Up next"];
/// How long a closed screen's presentation is kept for an instant reopen.
/// The worker re-reads the group as soon as the screen is up again, so this
/// only ever bridges the first few hundred milliseconds.
const VIEW_TTL: Duration = Duration::from_secs(30);

/// What the screen looked like when it was left, for one speaker.
struct View {
    at: Instant,
    serial: u64,
    presentation: crate::activity::Presentation,
    snapshot: Option<Snapshot>,
    room: slint::SharedString,
}

/// What the screen was opened for.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub device: String,
    pub name: String,
    pub room: String,
    pub host: Ipv4Addr,
}
enum Op {
    PlayPause,
    Skip(bool),
    Seek(u64),
    Volume(i8),
    ToggleMute,
    Sources,
    Select(SourceId, String),
    Modes(PlayModeChange),
}
enum Request {
    /// Connect to the player; the string is the artwork URL the UI already
    /// shows, so it is not fetched again.
    Open(Ipv4Addr, String),
    Refresh,
    Command(Op),
    Close,
}
enum Event {
    State(Box<Result<Snapshot, String>>),
    Done(Result<String, String>),
    Sources(Result<Vec<Source>, String>),
    Art(String, Option<activity_art::Pixels>),
}
pub struct Controller {
    tx: mpsc::SyncSender<(u64, Request)>,
    rx: mpsc::Receiver<(u64, Event)>,
    active: Arc<AtomicU64>,
    generation: u64,
    target: Option<Target>,
    snapshot: Option<Snapshot>,
    /// When the snapshot's position was read, for interpolation.
    at: Instant,
    tick: Instant,
    refresh_at: Instant,
    busy: bool,
    message_until: Option<Instant>,
    volume_until: Option<Instant>,
    art_key: String,
    sources: Vec<Source>,
    /// On-screen selection the D-pad moves: 1 seek, 2 previous, 3 play,
    /// 4 next, 5..=7 sheets.
    selected: i32,
    /// Recently closed screens by device id, restored on reopen.
    views: std::collections::HashMap<String, View>,
}
impl Controller {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(16);
        let (events, receive) = mpsc::sync_channel(8);
        let active = Arc::new(AtomicU64::new(0));
        let worker_active = active.clone();
        std::thread::spawn(move || worker(rx, events, worker_active));
        Self {
            tx,
            rx: receive,
            active,
            generation: 0,
            target: None,
            snapshot: None,
            at: Instant::now(),
            tick: Instant::now(),
            refresh_at: Instant::now(),
            busy: false,
            message_until: None,
            volume_until: None,
            art_key: String::new(),
            sources: Vec::new(),
            selected: 3,
            views: std::collections::HashMap::new(),
        }
    }
    fn serial() -> u64 {
        crate::config_snapshot::current().map_or(0, |c| c.serial)
    }
    /// Keep what the screen shows for this speaker, for an instant reopen.
    fn remember(&mut self, app: &App) {
        let Some(t) = &self.target else { return };
        if app.get_player_shown() && app.get_player_connected() {
            self.views.insert(
                t.device.clone(),
                View {
                    at: Instant::now(),
                    serial: Self::serial(),
                    presentation: crate::activity::Presentation::capture(app, &self.art_key),
                    snapshot: self.snapshot.clone(),
                    room: app.get_player_room(),
                },
            );
        }
        let now = Instant::now();
        self.views
            .retain(|_, v| now.duration_since(v.at) < VIEW_TTL);
    }
    pub fn is_open(&self) -> bool {
        self.target.is_some()
    }
    /// Take the player screen over for a Sonos device. The caller has already
    /// released whatever the screen showed before.
    pub fn open(&mut self, app: &App, target: Target) {
        self.generation += 1;
        self.active.store(self.generation, Ordering::SeqCst);
        self.snapshot = None;
        self.busy = false;
        self.art_key.clear();
        self.sources.clear();
        self.selected = 3;
        self.message_until = None;
        app.set_player_music(true);
        app.set_player_sheets(ModelRc::new(VecModel::from(
            SHEETS
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        app.set_player_selected(self.selected);
        app.set_player_shown(true);
        app.set_player_panel(0);
        app.set_player_message("".into());
        app.set_player_logo(slint::Image::default());
        app.set_player_has_logo(false);
        app.set_player_activity(target.name.as_str().into());
        app.set_player_room(target.room.as_str().into());
        // A screen left within the last half minute comes back as it was,
        // artwork included, while the worker reads the group again behind it.
        let serial = Self::serial();
        let restored = self
            .views
            .remove(&target.device)
            .filter(|v| v.at.elapsed() < VIEW_TTL && v.serial == serial);
        let mut known_art = String::new();
        match restored {
            Some(view) => {
                view.presentation.restore(app);
                app.set_player_room(view.room);
                self.art_key = view.presentation.art_key.clone();
                known_art = self.art_key.clone();
                self.snapshot = view.snapshot;
                self.at = view.at;
            }
            None => {
                app.set_player_ready(false);
                app.set_player_connected(false);
                app.set_player_title("Connecting to Sonos…".into());
                app.set_player_metadata("".into());
                app.set_player_elapsed("".into());
                app.set_player_remaining("".into());
                app.set_player_progress(0.);
                app.set_player_can_seek(false);
                app.set_player_paused(true);
                app.set_player_fanart(slint::Image::default());
                app.set_player_has_art(false);
            }
        }
        app.invoke_focus_player();
        if self
            .tx
            .try_send((self.generation, Request::Open(target.host, known_art)))
            .is_err()
        {
            self.notice(app, "Connection busy. Reopen the speaker.");
        }
        self.refresh_at = Instant::now() + REFRESH;
        self.target = Some(target);
    }
    /// Leave the screen. The caller decides where focus goes next.
    pub fn close(&mut self, app: &App) {
        self.remember(app);
        self.generation += 1;
        self.active.store(self.generation, Ordering::SeqCst);
        let _ = self.tx.try_send((self.generation, Request::Close));
        self.target = None;
        self.snapshot = None;
        self.busy = false;
        self.art_key.clear();
        app.set_player_music(false);
        app.set_player_sheets(ModelRc::new(VecModel::from(
            ["Chapters", "Audio", "Subtitles"]
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        app.set_player_selected(-1);
        app.set_player_message("".into());
        app.set_player_fanart(slint::Image::default());
        app.set_player_has_art(false);
        app.set_volume_shown(false);
    }
    fn notice(&mut self, app: &App, text: &str) {
        app.set_player_message(text.into());
        self.message_until = Some(Instant::now() + Duration::from_secs(4));
    }
    fn send(&mut self, app: &App, op: Op) {
        if self.busy {
            return;
        }
        if self
            .tx
            .try_send((self.generation, Request::Command(op)))
            .is_ok()
        {
            self.busy = true;
        } else {
            self.notice(app, "Connection busy. Try again.");
        }
    }
    fn coordinator(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| s.status.coordinator == s.status.player.uuid)
    }
    /// Whether a transport command may go out now: a member's playback
    /// belongs to its coordinator, and the screen says so instead of sending.
    fn transport_allowed(&mut self, app: &App) -> bool {
        if self.snapshot.is_none() {
            self.notice(app, "Still connecting to the speaker.");
            return false;
        }
        if !self.coordinator() {
            let coordinator = self
                .snapshot
                .as_ref()
                .map(|s| s.status.coordinator_name.clone())
                .unwrap_or_default();
            self.notice(
                app,
                &format!(
                    "Playback is controlled by {coordinator}. Open that speaker to change it."
                ),
            );
            return false;
        }
        true
    }
    fn select(&mut self, app: &App, selected: i32) {
        self.selected = selected;
        app.set_player_selected(selected);
    }
    /// D-pad movement over the on-screen controls. Rows: the seek line, the
    /// transport, and the three sheets.
    fn step(&mut self, app: &App, dx: i32, dy: i32) {
        if !app.get_player_ready() {
            return;
        }
        let rows: [&[i32]; 3] = [&[1], &[2, 3, 4], &[5, 6, 7]];
        let (mut row, mut column) = rows
            .iter()
            .enumerate()
            .find_map(|(r, cells)| {
                cells
                    .iter()
                    .position(|c| *c == self.selected)
                    .map(|c| (r as i32, c as i32))
            })
            .unwrap_or((1, 1));
        if dy != 0 {
            row = (row + dy).clamp(0, 2);
            // Keep the column where it makes sense; centre the seek row.
            column = column.min(rows[row as usize].len() as i32 - 1);
            if rows[row as usize].len() == 1 {
                column = 0;
            }
        }
        if dx != 0 {
            column = (column + dx).clamp(0, rows[row as usize].len() as i32 - 1);
        }
        let next = rows[row as usize][column as usize];
        self.select(app, next);
    }
    fn activate(&mut self, app: &App) {
        let action = match self.selected {
            1 | 3 => "play",
            2 => "previous",
            4 => "next",
            5 => "chapters",
            6 => "audio",
            7 => "subtitles",
            _ => return,
        };
        self.action(app, action, 0., false);
    }
    /// An action from the screen or a physical key, in the Cinema vocabulary.
    /// Returns false for "back" at the top level, which the owner handles by
    /// closing the screen.
    pub fn action(&mut self, app: &App, action: &str, value: f64, repeat: bool) -> bool {
        match action {
            "back" | "Input.Back" => {
                if app.get_player_panel() != 0 {
                    app.set_player_panel(0);
                    app.invoke_focus_player();
                    return true;
                }
                return false;
            }
            "Input.Home" => return false,
            "retry" => {
                if let Some(t) = &self.target {
                    self.snapshot = None;
                    self.busy = false;
                    let _ = self
                        .tx
                        .try_send((self.generation, Request::Open(t.host, String::new())));
                }
            }
            "play" => {
                if self.transport_allowed(app) {
                    self.send(app, Op::PlayPause);
                }
            }
            "next" | "previous" | "chapter-step" => {
                if repeat {
                    return true;
                }
                let forward = action == "next" || (action == "chapter-step" && value > 0.);
                if self.transport_allowed(app) {
                    self.send(app, Op::Skip(forward));
                }
            }
            "seek" => {
                let duration = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.now_playing.current.as_ref())
                    .and_then(|t| t.duration_ms);
                if let Some(duration) = duration {
                    if self.transport_allowed(app) {
                        let position = (duration as f64 * value.clamp(0., 100.) / 100.) as u64;
                        self.send(app, Op::Seek(position));
                    }
                }
            }
            "skip" => {}
            "volume" => {
                let delta = (value as i32).clamp(-20, 20) as i8;
                let delta = if delta == 0 { VOLUME_STEP } else { delta };
                self.send(app, Op::Volume(delta));
            }
            "mute" => {
                if !repeat {
                    self.send(app, Op::ToggleMute);
                }
            }
            "chapters" => self.panel(app, 1),
            "audio" => self.panel(app, 2),
            "subtitles" => self.panel(app, 3),
            "choose" => self.choose(app, value as usize),
            "Input.Up" => self.step(app, 0, -1),
            "Input.Down" => self.step(app, 0, 1),
            "Input.Left" => self.step(app, -1, 0),
            "Input.Right" => self.step(app, 1, 0),
            "Input.Select" => self.activate(app),
            "Input.ContextMenu" => self.panel(app, 1),
            _ => {}
        }
        true
    }
    fn panel(&mut self, app: &App, panel: i32) {
        let mut rows = Vec::new();
        let mut detail = String::new();
        match panel {
            1 => {
                if self.sources.is_empty() {
                    detail = "Finding sources…".into();
                    self.send(app, Op::Sources);
                } else {
                    rows = self
                        .sources
                        .iter()
                        .map(|s| PlayerChoice {
                            title: s.name.as_str().into(),
                            detail: s.detail.as_str().into(),
                        })
                        .collect();
                }
            }
            2 => {
                let modes = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.playback.modes)
                    .unwrap_or_default();
                let on = |b: bool| if b { "On" } else { "Off" };
                rows = vec![
                    PlayerChoice {
                        title: "Shuffle".into(),
                        detail: on(modes.shuffle).into(),
                    },
                    PlayerChoice {
                        title: "Repeat".into(),
                        detail: if modes.repeat_one {
                            "This track"
                        } else if modes.repeat {
                            "All"
                        } else {
                            "Off"
                        }
                        .into(),
                    },
                    PlayerChoice {
                        title: "Crossfade".into(),
                        detail: on(modes.crossfade).into(),
                    },
                ];
                if !self.coordinator() {
                    detail = "Play modes belong to the group's coordinator.".into();
                }
            }
            3 => {
                match self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.now_playing.next.as_ref())
                {
                    Some(next) => rows.push(PlayerChoice {
                        title: next.name.as_str().into(),
                        detail: format!(
                            "{} · Press to skip to it",
                            line(&next.artist, &next.album)
                        )
                        .into(),
                    }),
                    None => detail = "Nothing is queued after this.".into(),
                }
            }
            _ => {}
        }
        app.set_player_choices(ModelRc::new(VecModel::from(rows)));
        app.set_player_panel_detail(detail.into());
        app.set_player_panel(panel);
        app.invoke_set_player_choice(0);
        app.invoke_focus_player();
    }
    fn choose(&mut self, app: &App, index: usize) {
        match app.get_player_panel() {
            1 => {
                if let Some(source) = self.sources.get(index).cloned() {
                    if self.transport_allowed(app) {
                        self.notice(app, &format!("Starting {}…", source.name));
                        self.send(app, Op::Select(source.id, source.name));
                        app.set_player_panel(0);
                        app.invoke_focus_player();
                    }
                }
            }
            2 => {
                let modes = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.playback.modes)
                    .unwrap_or_default();
                let change = match index {
                    0 => PlayModeChange {
                        shuffle: Some(!modes.shuffle),
                        ..Default::default()
                    },
                    // Off → all → this track → off.
                    1 => {
                        if modes.repeat_one {
                            PlayModeChange {
                                repeat: Some(false),
                                repeat_one: Some(false),
                                ..Default::default()
                            }
                        } else if modes.repeat {
                            PlayModeChange {
                                repeat_one: Some(true),
                                ..Default::default()
                            }
                        } else {
                            PlayModeChange {
                                repeat: Some(true),
                                ..Default::default()
                            }
                        }
                    }
                    2 => PlayModeChange {
                        crossfade: Some(!modes.crossfade),
                        ..Default::default()
                    },
                    _ => return,
                };
                if self.transport_allowed(app) {
                    self.send(app, Op::Modes(change));
                }
            }
            3 if self.transport_allowed(app) => {
                self.send(app, Op::Skip(true));
                app.set_player_panel(0);
                app.invoke_focus_player();
            }
            _ => {}
        }
    }
    fn present(&mut self, app: &App, snapshot: &Snapshot) {
        let playing = matches!(snapshot.playback.state.as_str(), "PLAYING" | "BUFFERING");
        let member = snapshot.status.coordinator != snapshot.status.player.uuid;
        let track = snapshot.now_playing.current.as_ref();
        let has_content = track.is_some() || !snapshot.now_playing.container.is_empty();
        app.set_player_connected(true);
        app.set_player_ready(has_content);
        app.set_player_paused(!playing);
        if let Some(t) = &self.target {
            app.set_player_room(
                if member {
                    format!(
                        "{} · Playing from {}",
                        t.room, snapshot.status.coordinator_name
                    )
                } else {
                    t.room.clone()
                }
                .into(),
            );
        }
        match track {
            Some(track) => {
                app.set_player_title(track.name.as_str().into());
                let mut meta = line(&track.artist, &track.album);
                if meta.is_empty() {
                    meta = snapshot.now_playing.container.clone();
                }
                app.set_player_metadata(meta.into());
                app.set_player_can_seek(
                    snapshot.playback.can_seek && track.duration_ms.is_some() && !member,
                );
            }
            None if has_content => {
                app.set_player_title(snapshot.now_playing.container.as_str().into());
                app.set_player_metadata(
                    match snapshot.now_playing.container_type.as_str() {
                        "" => String::new(),
                        kind => kind.replace('_', " ").to_lowercase(),
                    }
                    .into(),
                );
                app.set_player_can_seek(false);
            }
            None => {
                app.set_player_title(
                    format!(
                        "{} is idle.\nPress Sources to play something.",
                        snapshot.status.player.name
                    )
                    .into(),
                );
                app.set_player_metadata("".into());
                app.set_player_can_seek(false);
                app.set_player_elapsed("".into());
                app.set_player_remaining("".into());
                app.set_player_progress(0.);
            }
        }
        let key = track.map(|t| t.image_url.clone()).unwrap_or_default();
        if key != self.art_key {
            self.art_key = key;
            app.set_player_has_art(false);
            app.set_player_fanart(slint::Image::default());
        }
        self.clock(app, snapshot, 0.);
    }
    /// The progress line, from the last read position plus the time since.
    fn clock(&self, app: &App, snapshot: &Snapshot, elapsed_since: f64) {
        let Some(track) = snapshot.now_playing.current.as_ref() else {
            return;
        };
        let playing = matches!(snapshot.playback.state.as_str(), "PLAYING");
        let position =
            snapshot.playback.position_ms as f64 / 1000. + if playing { elapsed_since } else { 0. };
        match track.duration_ms {
            Some(duration) => {
                let total = duration as f64 / 1000.;
                let position = position.clamp(0., total);
                app.set_player_elapsed(clock(position).into());
                app.set_player_remaining(format!("−{}", clock(total - position)).into());
                app.set_player_progress((position / total * 100.) as f32);
            }
            None => {
                app.set_player_elapsed(clock(position).into());
                app.set_player_remaining("LIVE".into());
                app.set_player_progress(0.);
            }
        }
    }
    fn show_volume(&mut self, app: &App, volume: u8) {
        if let Some(t) = &self.target {
            // The card is shared: a power result may have left its caption.
            app.set_volume_caption("Volume".into());
            app.set_volume_target(t.name.as_str().into());
        }
        app.set_volume(i32::from(volume));
        app.set_volume_text("".into());
        app.set_volume_meter(true);
        app.set_feedback_enabled(true);
        app.set_volume_shown(true);
        self.volume_until = Some(Instant::now() + Duration::from_millis(1500));
    }
    pub fn poll(&mut self, app: &App) {
        if self.target.is_none() {
            return;
        }
        if !app.get_player_shown() {
            self.close(app);
            return;
        }
        while let Ok((generation, event)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            match event {
                Event::State(state) => match *state {
                    Ok(snapshot) => {
                        let changed = self.snapshot.as_ref().map(|s| {
                            (
                                &s.now_playing.current,
                                &s.now_playing.next,
                                s.playback.modes,
                            )
                        }) != Some((
                            &snapshot.now_playing.current,
                            &snapshot.now_playing.next,
                            snapshot.playback.modes,
                        ));
                        self.at = Instant::now();
                        self.present(app, &snapshot);
                        let panel = app.get_player_panel();
                        self.snapshot = Some(snapshot);
                        // A sheet built from the old state is rebuilt from the new one.
                        if changed && (panel == 2 || panel == 3) {
                            self.panel(app, panel);
                        }
                    }
                    Err(error) => {
                        self.snapshot = None;
                        app.set_player_ready(false);
                        app.set_player_connected(false);
                        app.set_player_title(error.into());
                        app.set_player_has_art(false);
                        self.art_key.clear();
                    }
                },
                Event::Done(Ok(message)) => {
                    self.busy = false;
                    if let Some(volume) = message.strip_prefix("volume:") {
                        if let Ok(volume) = volume.parse::<u8>() {
                            self.show_volume(app, volume);
                        }
                    } else if !message.is_empty() {
                        self.notice(app, &message);
                    }
                }
                Event::Done(Err(error)) => {
                    self.busy = false;
                    self.notice(app, &error);
                }
                Event::Sources(Ok(sources)) => {
                    self.busy = false;
                    self.sources = sources;
                    if app.get_player_panel() == 1 {
                        if self.sources.is_empty() {
                            app.set_player_panel_detail(
                                "No sources: add favourites or playlists in the Sonos app.".into(),
                            );
                        } else {
                            self.panel(app, 1);
                        }
                    }
                }
                Event::Sources(Err(error)) => {
                    self.busy = false;
                    if app.get_player_panel() == 1 {
                        app.set_player_panel_detail(error.into());
                    }
                }
                Event::Art(key, pixels) => {
                    if key == self.art_key {
                        if let Some(pixels) = pixels {
                            app.set_player_fanart(activity_art::slint_image(pixels));
                            app.set_player_has_art(true);
                        }
                    }
                }
            }
        }
        if self.tick.elapsed() >= Duration::from_secs(1) {
            self.tick = Instant::now();
            if let Some(snapshot) = &self.snapshot {
                self.clock(app, snapshot, self.at.elapsed().as_secs_f64());
            }
        }
        if Instant::now() >= self.refresh_at {
            self.refresh_at = Instant::now() + REFRESH;
            if !self.busy {
                let _ = self.tx.try_send((self.generation, Request::Refresh));
            }
        }
        if self.message_until.is_some_and(|t| Instant::now() >= t) {
            app.set_player_message("".into());
            self.message_until = None;
        }
        if self.volume_until.is_some_and(|t| Instant::now() >= t) {
            app.set_volume_shown(false);
            self.volume_until = None;
        }
    }
}
/// The group's metadata once a skip has taken effect. A skip is acknowledged
/// before the player switches tracks, so a read straight after it still shows
/// the track being left; poll briefly until the current track differs from
/// `before` (or until a second has passed and the read is what it is).
pub(crate) fn now_playing_after_skip(
    client: &Client,
    before: Option<&couch_sonos::Track>,
) -> Option<couch_sonos::NowPlaying> {
    let same = |now: &couch_sonos::NowPlaying| match (before, now.current.as_ref()) {
        (Some(b), Some(c)) => {
            b.name == c.name && b.artist == c.artist && b.image_url == c.image_url
        }
        (None, None) => true,
        _ => false,
    };
    let mut latest = None;
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        match client.now_playing() {
            Ok(now) => {
                let unchanged = same(&now);
                latest = Some(now);
                if !unchanged {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    latest
}
/// "Artist · Album", or whichever of the two the player gave.
fn line(artist: &str, album: &str) -> String {
    match (artist.is_empty(), album.is_empty()) {
        (false, false) if artist != album => format!("{artist} · {album}"),
        (false, _) => artist.to_owned(),
        (true, false) => album.to_owned(),
        (true, true) => String::new(),
    }
}
fn clock(t: f64) -> String {
    let t = t.max(0.) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}
fn describe(error: couch_sonos::Error) -> String {
    match error {
        couch_sonos::Error::NotCoordinator { coordinator } => {
            format!("Playback is controlled by {coordinator}. Open that speaker to change it.")
        }
        couch_sonos::Error::Transport => "Cannot reach the speaker.".into(),
        couch_sonos::Error::Api(code) if code == "ERROR_PLAYBACK_NO_CONTENT" => {
            "Sonos found nothing to play there.".into()
        }
        other => other.to_string(),
    }
}
fn worker(
    rx: mpsc::Receiver<(u64, Request)>,
    events: mpsc::SyncSender<(u64, Event)>,
    active: Arc<AtomicU64>,
) {
    let mut client: Option<Client> = None;
    let mut art_sent = String::new();
    while let Ok((generation, request)) = rx.recv() {
        if active.load(Ordering::SeqCst) != generation {
            continue;
        }
        let current = || active.load(Ordering::SeqCst) == generation;
        let send = |event: Event| events.send((generation, event)).is_ok();
        match request {
            Request::Close => {
                client = None;
                art_sent.clear();
            }
            Request::Open(host, known_art) => {
                art_sent = known_art;
                match Client::connect(host) {
                    Ok(c) => {
                        client = Some(c);
                    }
                    Err(e) => {
                        client = None;
                        if !send(Event::State(Box::new(Err(describe(e))))) {
                            return;
                        }
                        continue;
                    }
                }
                if !refresh(client.as_ref(), &send, &mut art_sent, &current) {
                    return;
                }
            }
            Request::Refresh => {
                if !refresh(client.as_ref(), &send, &mut art_sent, &current) {
                    return;
                }
            }
            Request::Command(op) => {
                let Some(c) = client.as_ref() else {
                    if !send(Event::Done(Err("Not connected to the speaker.".into()))) {
                        return;
                    }
                    continue;
                };
                let outcome = match &op {
                    Op::Sources => {
                        let result = c.sources().map_err(describe);
                        if !send(Event::Sources(result)) {
                            return;
                        }
                        continue;
                    }
                    Op::PlayPause => c
                        .command_if_current("play-pause", &current)
                        .map(|_| String::new()),
                    Op::Skip(forward) => {
                        let before = c.now_playing().ok().and_then(|n| n.current);
                        c.command_if_current(if *forward { "next" } else { "previous" }, &current)
                            .map(|_| {
                                // Let the player switch before the refresh reads it.
                                now_playing_after_skip(c, before.as_ref());
                                String::new()
                            })
                    }
                    Op::Seek(position) => c
                        .seek_if_current(*position, &current)
                        .map(|_| String::new()),
                    Op::Volume(delta) => c
                        .nudge_volume(*delta)
                        .and_then(|_| c.volume())
                        .map(|v| format!("volume:{v}")),
                    Op::ToggleMute => c
                        .muted()
                        .and_then(|muted| c.set_muted(!muted).map(|_| muted))
                        .map(|was| if was { "Unmuted" } else { "Muted" }.to_owned()),
                    Op::Select(source, label) => c
                        .select_source_if_current(source, &current)
                        .map(|_| format!("Playing {label}")),
                    Op::Modes(change) => c
                        .set_play_modes_if_current(*change, &current)
                        .map(|_| String::new()),
                };
                let outcome = match outcome {
                    Err(couch_sonos::Error::Cancelled) => Ok(String::new()),
                    other => other.map_err(describe),
                };
                let transport_failed =
                    matches!(outcome, Err(ref e) if e == "Cannot reach the speaker.");
                if !send(Event::Done(outcome)) {
                    return;
                }
                if transport_failed {
                    client = None;
                    continue;
                }
                // A source load settles over a second or two; read once now
                // and let the periodic refresh catch up.
                if matches!(op, Op::Select(..)) {
                    std::thread::sleep(Duration::from_millis(600));
                }
                if !refresh(client.as_ref(), &send, &mut art_sent, &current) {
                    return;
                }
            }
        }
    }
}
/// One snapshot to the UI, then the artwork for a track whose art has not been
/// sent yet. Returns false when the UI has gone away.
fn refresh(
    client: Option<&Client>,
    send: &dyn Fn(Event) -> bool,
    art_sent: &mut String,
    current: &dyn Fn() -> bool,
) -> bool {
    let Some(client) = client else {
        return send(Event::State(Box::new(Err(
            "Not connected to the speaker.".into()
        ))));
    };
    if !current() {
        return true;
    }
    let snapshot = match client.snapshot() {
        Ok(s) => s,
        Err(e) => return send(Event::State(Box::new(Err(describe(e))))),
    };
    let art = snapshot
        .now_playing
        .current
        .as_ref()
        .map(|t| t.image_url.clone())
        .unwrap_or_default();
    if !send(Event::State(Box::new(Ok(snapshot)))) {
        return false;
    }
    if !art.is_empty() && art != *art_sent && current() {
        let pixels = client
            .artwork(&art)
            .ok()
            .and_then(|bytes| activity_art::decode(&bytes, activity_art::Shape::Backdrop));
        *art_sent = art.clone();
        return send(Event::Art(art, pixels));
    }
    if art.is_empty() {
        art_sent.clear();
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn music_player_screen_renders_and_its_controls_dispatch() {
        // Slint is single-threaded on this target: run the window fixture in
        // its own process, like the other screen tests.
        if std::env::var_os("COUCH_TEST_SONOS_PLAYER").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "media_player::tests::music_player_screen_renders_and_its_controls_dispatch",
                ])
                .env("COUCH_TEST_SONOS_PLAYER", "1")
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
            platform::{Key, PointerEventButton, WindowEvent},
            ComponentHandle,
        };
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_player_action(move |name, value| {
            received.borrow_mut().push(format!("{name}:{value}"))
        });
        app.set_player_shown(true);
        app.set_player_music(true);
        app.set_player_sheets(ModelRc::new(VecModel::from(
            SHEETS
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        app.set_player_connected(true);
        app.set_player_ready(true);
        app.set_player_paused(false);
        app.set_player_can_seek(true);
        app.set_player_activity("Living room Sonos".into());
        app.set_player_room("Living room".into());
        app.set_player_title("Sun Ain't Even Gone Down Yet".into());
        app.set_player_metadata("Brothers Osborne · Brothers Osborne".into());
        app.set_player_elapsed("1:01".into());
        app.set_player_remaining("−2:01".into());
        app.set_player_progress(33.);
        app.set_player_selected(3);
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        app.invoke_focus_player();
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        if let Some(path) = std::env::var_os("COUCH_SONOS_PLAYER_SCREENSHOT") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
        }
        // Transport row: previous, play/pause, next; then the three sheets.
        for (x, y) in [
            (32. + 38. + 35., 800. - 330. + 93. + 35.),
            (240., 800. - 330. + 84. + 44.),
            (480. - 32. - 108. + 35., 800. - 330. + 93. + 35.),
            (32. + 60., 800. - 330. + 198. + 38.),
            (240., 800. - 330. + 198. + 38.),
            (480. - 32. - 60., 800. - 330. + 198. + 38.),
        ] {
            let position = slint::LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        }
        // Physical keys: the D-pad goes to the screen's own selection, the
        // channel keys skip, mute and volume reach the speaker.
        for key in [Key::RightArrow, Key::Return, Key::F21, Key::F14, Key::F23] {
            let text = char::from(key).to_string().into();
            window.dispatch_event(WindowEvent::KeyPressed { text });
        }
        assert_eq!(
            &*actions.borrow(),
            &[
                "previous:0",
                "play:0",
                "next:0",
                "chapters:0",
                "audio:0",
                "subtitles:0",
                "Input.Right:0",
                "Input.Select:0",
                "chapter-step:1",
                "mute:0",
                "volume:5",
            ]
        );
        app.hide().unwrap();
    }
    /// The speaker the pictures are taken of: one fixed Sonos player whose
    /// answers the test chooses. It stands where the worker thread stands,
    /// without a network: every request the screen makes is answered the way
    /// `worker` answers it, from `now`, `answer` and `sources`.
    struct Rig {
        controller: Controller,
        requests: mpsc::Receiver<(u64, Request)>,
        events: mpsc::SyncSender<(u64, Event)>,
        /// What the speaker reports when it is read.
        now: Result<Snapshot, String>,
        /// What the next command comes back with.
        answer: Result<String, String>,
        sources: Result<Vec<Source>, String>,
        /// Whether the speaker answers when the screen connects to it.
        connect: Result<(), String>,
        connected: bool,
        art_sent: String,
        /// What reached the speaker, in order, in plain words.
        sent: Vec<String>,
    }
    impl Rig {
        fn new() -> Self {
            let (tx, requests) = mpsc::sync_channel(16);
            let (events, rx) = mpsc::sync_channel(8);
            let controller = Controller {
                tx,
                rx,
                active: Arc::new(AtomicU64::new(0)),
                generation: 0,
                target: None,
                snapshot: None,
                at: Instant::now(),
                tick: Instant::now(),
                refresh_at: Instant::now(),
                busy: false,
                message_until: None,
                volume_until: None,
                art_key: String::new(),
                sources: Vec::new(),
                selected: 3,
                views: std::collections::HashMap::new(),
            };
            Self {
                controller,
                requests,
                events,
                now: Err("Cannot reach the speaker.".into()),
                answer: Ok(String::new()),
                sources: Ok(Vec::new()),
                connect: Ok(()),
                connected: false,
                art_sent: String::new(),
                sent: Vec::new(),
            }
        }
        fn read(&mut self) {
            let generation = self.controller.generation;
            if !self.connected {
                let lost = Err("Not connected to the speaker.".to_owned());
                self.events
                    .send((generation, Event::State(Box::new(lost))))
                    .unwrap();
                return;
            }
            let art = self
                .now
                .as_ref()
                .ok()
                .and_then(|s| s.now_playing.current.as_ref())
                .map(|t| t.image_url.clone())
                .unwrap_or_default();
            self.sent.push("read".into());
            self.events
                .send((generation, Event::State(Box::new(self.now.clone()))))
                .unwrap();
            if self.now.is_ok() && !art.is_empty() && art != self.art_sent {
                self.sent.push(format!("artwork {art}"));
                self.art_sent = art.clone();
                let pixels = activity_art::decode(&cover(), activity_art::Shape::Backdrop);
                self.events
                    .send((generation, Event::Art(art, pixels)))
                    .unwrap();
            } else if art.is_empty() {
                self.art_sent.clear();
            }
        }
        /// Hold the one-second clock and the three-second re-read still, so a
        /// picture never depends on how long the test took to get here.
        fn hold(&mut self) {
            self.controller.tick = Instant::now();
            self.controller.refresh_at = Instant::now() + REFRESH;
        }
        /// Answer everything the screen has asked for, then let it take the
        /// answers in.
        fn pump(&mut self, app: &App) {
            loop {
                self.hold();
                let Ok((generation, request)) = self.requests.try_recv() else {
                    break;
                };
                if generation != self.controller.generation {
                    continue;
                }
                match request {
                    Request::Close => {
                        self.sent.push("close".into());
                        self.art_sent.clear();
                    }
                    Request::Open(_, known_art) => {
                        self.sent.push(format!("open, showing art {known_art:?}"));
                        self.art_sent = known_art;
                        self.connected = self.connect.is_ok();
                        match self.connect.clone() {
                            Ok(()) => self.read(),
                            Err(e) => self
                                .events
                                .send((generation, Event::State(Box::new(Err(e)))))
                                .unwrap(),
                        }
                    }
                    Request::Refresh => self.read(),
                    Request::Command(Op::Sources) => {
                        self.sent.push("sources".into());
                        self.events
                            .send((generation, Event::Sources(self.sources.clone())))
                            .unwrap();
                    }
                    Request::Command(_) if !self.connected => {
                        let lost = Err("Not connected to the speaker.".to_owned());
                        self.events.send((generation, Event::Done(lost))).unwrap();
                    }
                    Request::Command(op) => {
                        self.sent.push(match &op {
                            Op::PlayPause => "play-pause".into(),
                            Op::Skip(true) => "next".into(),
                            Op::Skip(false) => "previous".into(),
                            Op::Seek(position) => format!("seek to {position} ms"),
                            Op::Volume(delta) => format!("volume {delta:+}"),
                            Op::ToggleMute => "mute".into(),
                            Op::Select(_, name) => format!("source {name}"),
                            Op::Modes(change) => format!(
                                "modes shuffle {:?} repeat {:?} repeat-one {:?} crossfade {:?}",
                                change.shuffle, change.repeat, change.repeat_one, change.crossfade
                            ),
                            Op::Sources => unreachable!(),
                        });
                        let answer = std::mem::replace(&mut self.answer, Ok(String::new()));
                        let lost = answer
                            .as_ref()
                            .is_err_and(|e| e == "Cannot reach the speaker.");
                        self.events.send((generation, Event::Done(answer))).unwrap();
                        if lost {
                            self.connected = false;
                        } else {
                            self.read();
                        }
                    }
                }
                self.controller.poll(app);
            }
            self.hold();
            self.controller.poll(app);
        }
        fn act(&mut self, app: &App, action: &str, value: f64) {
            self.controller.action(app, action, value, false);
            self.pump(app);
        }
        /// The periodic re-read, now rather than in three seconds.
        fn refresh(&mut self, app: &App) {
            self.controller.tick = Instant::now();
            self.controller.refresh_at = Instant::now();
            self.controller.poll(app);
            self.pump(app);
        }
        /// Let a notice and the volume card run out now rather than in seconds.
        fn expire(&mut self, app: &App) {
            let now = Instant::now();
            self.controller.message_until = self.controller.message_until.map(|_| now);
            self.controller.volume_until = self.controller.volume_until.map(|_| now);
            self.pump(app);
        }
    }
    /// Cover art: a fixed picture made here, so nothing binary is in the tree.
    fn cover() -> Vec<u8> {
        let picture = image::RgbImage::from_fn(600, 600, |x, y| {
            let tile = (x / 75 + y / 75) % 2 == 0;
            image::Rgb([
                (x * 255 / 600) as u8,
                if tile { 200 } else { 90 },
                (y * 255 / 600) as u8,
            ])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        picture
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }
    fn track(
        name: &str,
        artist: &str,
        album: &str,
        art: &str,
        ms: Option<u64>,
    ) -> couch_sonos::Track {
        couch_sonos::Track {
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            image_url: art.into(),
            duration_ms: ms,
            service: "Apple Music".into(),
        }
    }
    /// The Lounge speaker playing the second track of an album from a playlist.
    fn playing() -> Snapshot {
        Snapshot {
            status: couch_sonos::Status {
                player: couch_sonos::Player {
                    uuid: "RINCON_LOUNGE".into(),
                    name: "Lounge".into(),
                    model: "Era 100".into(),
                },
                coordinator: "RINCON_LOUNGE".into(),
                coordinator_name: "Lounge".into(),
                transport: "PLAYING".into(),
                volume: 18,
                muted: false,
            },
            playback: couch_sonos::PlaybackStatus {
                state: "PLAYING".into(),
                position_ms: 74_210,
                modes: couch_sonos::PlayModes::default(),
                can_seek: true,
                can_skip: true,
                can_skip_back: true,
            },
            now_playing: couch_sonos::NowPlaying {
                container: "Evening".into(),
                container_type: "playlist".into(),
                current: Some(track(
                    "Weird Fishes / Arpeggi",
                    "Radiohead",
                    "In Rainbows",
                    "http://192.0.2.9:1400/getaa?s=1&u=weird-fishes",
                    Some(318_000),
                )),
                next: Some(track(
                    "All I Need",
                    "Radiohead",
                    "In Rainbows",
                    "http://192.0.2.9:1400/getaa?s=1&u=all-i-need",
                    Some(229_000),
                )),
            },
        }
    }
    fn paused() -> Snapshot {
        let mut s = playing();
        s.playback.state = "PAUSED".into();
        s.status.transport = "PAUSED".into();
        s
    }
    /// Everything on the screen that is not a pixel, as text, so a change
    /// shows up as a readable difference on any machine.
    fn words(app: &App) -> String {
        use slint::Model;
        let mut flags = Vec::new();
        for (on, name) in [
            (app.get_player_shown(), "shown"),
            (app.get_player_music(), "music"),
            (app.get_player_connected(), "connected"),
            (app.get_player_ready(), "ready"),
            (app.get_player_paused(), "paused"),
            (app.get_player_can_seek(), "can-seek"),
            (app.get_player_has_art(), "art"),
            (app.get_player_has_logo(), "logo"),
        ] {
            if on {
                flags.push(name);
            }
        }
        let mut out = format!(
            "  heading: {} / {}\n  title: {:?}\n  line: {:?}\n  clock: {:?} {:?} {:.2}%\n  flags: {}\n  selected: {}\n  sheets: {}\n",
            app.get_player_activity(),
            app.get_player_room(),
            app.get_player_title(),
            app.get_player_metadata(),
            app.get_player_elapsed(),
            app.get_player_remaining(),
            app.get_player_progress(),
            flags.join(" "),
            app.get_player_selected(),
            app.get_player_sheets()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        if app.get_player_panel() != 0 {
            out += &format!(
                "  sheet {}: {:?}\n",
                app.get_player_panel(),
                app.get_player_panel_detail()
            );
            for row in app.get_player_choices().iter() {
                out += &format!("    {:?} / {:?}\n", row.title, row.detail);
            }
        }
        if !app.get_player_message().is_empty() {
            out += &format!("  notice: {:?}\n", app.get_player_message());
        }
        if app.get_volume_shown() {
            out += &format!(
                "  volume card: {} {} for {:?}\n",
                app.get_volume_caption(),
                app.get_volume(),
                app.get_volume_target()
            );
        }
        out
    }
    /// The Sonos player screen, picture by picture, from one fixed speaker.
    ///
    /// Three records come out of one run. The words on the screen and what was
    /// sent to the speaker are compared with `tests/golden/player-screen.txt`
    /// on every run. With `COUCH_PLAYER_SCREENSHOTS=<dir>` each 480x800 picture
    /// is written there as a PNG, and with `COUCH_PLAYER_GOLDENS=<dir>` each
    /// picture must equal the PNG of the same name in that directory, pixel for
    /// pixel: that is how a change to the code behind the screen is shown to
    /// leave the screen alone. Nothing binary is kept in the tree.
    #[test]
    fn the_player_screen_is_the_same_picture_for_the_same_speaker() {
        if std::env::var_os("COUCH_TEST_PLAYER_PICTURES").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "media_player::tests::the_player_screen_is_the_same_picture_for_the_same_speaker",
                ])
                .env("COUCH_TEST_PLAYER_PICTURES", "1")
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
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        let record = std::cell::RefCell::new(String::new());
        let differing = std::cell::RefCell::new(Vec::new());
        // One buffer for the whole run, as on the panel: the renderer redraws
        // only what changed since the last frame.
        let frame = std::cell::RefCell::new(vec![slint::Rgb8Pixel::default(); 480 * 800]);
        // One picture: let the 200 ms sheet and card movements finish, draw,
        // and note the words beside it.
        let picture = |rig: &mut Rig, name: &str| {
            for _ in 0..15 {
                slint::platform::update_timers_and_animations();
                std::thread::sleep(Duration::from_millis(16));
            }
            slint::platform::update_timers_and_animations();
            let mut pixels = frame.borrow_mut();
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            let file = format!("player-{name}.png");
            if let Some(dir) = std::env::var_os("COUCH_PLAYER_SCREENSHOTS") {
                let path = std::path::Path::new(&dir).join(&file);
                image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
            }
            if let Some(dir) = std::env::var_os("COUCH_PLAYER_GOLDENS") {
                let golden = image::open(std::path::Path::new(&dir).join(&file))
                    .unwrap()
                    .to_rgb8();
                let wrong = golden
                    .as_raw()
                    .chunks(3)
                    .zip(bytes.chunks(3))
                    .filter(|(a, b)| a != b)
                    .count();
                if golden.dimensions() != (480, 800) || wrong != 0 {
                    differing
                        .borrow_mut()
                        .push(format!("{file}: {wrong} pixels"));
                }
            }
            let mut record = record.borrow_mut();
            *record += &format!("== {name}\n{}", words(&app));
            if !rig.sent.is_empty() {
                *record += &format!("  sent: {}\n", rig.sent.join("; "));
                rig.sent.clear();
            }
        };
        let target = Target {
            device: "lounge-sonos".into(),
            name: "Lounge Sonos".into(),
            room: "Living room".into(),
            host: Ipv4Addr::new(192, 0, 2, 9),
        };
        let mut rig = Rig::new();
        let rig = &mut rig;

        // Opening: the waiting screen, then the speaker's first answer.
        rig.controller.open(&app, target.clone());
        picture(rig, "01-connecting");
        rig.now = Ok(paused());
        rig.pump(&app);
        picture(rig, "02-paused");
        rig.now = Ok(playing());
        rig.act(&app, "play", 0.);
        // A playing clock moves: draw at once.
        picture(rig, "03-playing");
        rig.now = Ok(paused());
        rig.act(&app, "play", 0.);

        // The D-pad walks the controls; OK on the seek line plays or pauses.
        rig.act(&app, "Input.Up", 0.);
        picture(rig, "04-seek-line-selected");
        rig.act(&app, "seek", 50.);
        rig.act(&app, "Input.Down", 0.);
        rig.act(&app, "Input.Left", 0.);
        rig.act(&app, "Input.Select", 0.);
        rig.act(&app, "next", 0.);
        picture(rig, "05-previous-selected");

        // Sources: asked for once, listed, and one started.
        rig.sources = Ok(vec![
            Source {
                id: SourceId::HomeTheater,
                name: "TV".into(),
                detail: "This player".into(),
            },
            Source {
                id: SourceId::Favorite("4".into()),
                name: "Morning Jazz".into(),
                detail: "Apple Music".into(),
            },
            Source {
                id: SourceId::Playlist("0".into()),
                name: "Evening".into(),
                detail: "Sonos playlist · 12 tracks".into(),
            },
        ]);
        rig.act(&app, "Input.Down", 0.);
        rig.controller.action(&app, "Input.Select", 0., false);
        picture(rig, "06-sources-finding");
        rig.pump(&app);
        picture(rig, "07-sources");
        rig.answer = Ok("Playing Morning Jazz".into());
        rig.controller.action(&app, "choose", 1., false);
        picture(rig, "08-source-starting");
        rig.pump(&app);
        picture(rig, "09-source-started");
        rig.expire(&app);

        // Modes: shuffle on, then repeat all, this track, off.
        rig.act(&app, "audio", 0.);
        picture(rig, "10-modes");
        let mut state = paused();
        for (row, change) in [
            (0., "shuffle"),
            (1., "all"),
            (1., "one"),
            (1., "off"),
            (2., "crossfade"),
        ] {
            let modes = &mut state.playback.modes;
            match change {
                "shuffle" => modes.shuffle = true,
                "all" => modes.repeat = true,
                "one" => (modes.repeat, modes.repeat_one) = (false, true),
                "off" => modes.repeat_one = false,
                _ => modes.crossfade = true,
            }
            rig.now = Ok(state.clone());
            rig.act(&app, "choose", row);
            if change == "one" {
                picture(rig, "11-modes-shuffled-repeating-this-track");
            }
        }
        picture(rig, "12-modes-shuffle-and-crossfade");
        rig.act(&app, "back", 0.);

        // Up next, with and without something queued.
        rig.act(&app, "subtitles", 0.);
        picture(rig, "13-up-next");
        rig.act(&app, "choose", 0.);
        state.now_playing.next = None;
        rig.now = Ok(state.clone());
        rig.act(&app, "subtitles", 0.);
        rig.refresh(&app);
        picture(rig, "14-up-next-empty");
        rig.act(&app, "Input.Back", 0.);

        // Volume and mute: the shared card, then a sentence.
        rig.answer = Ok("volume:23".into());
        rig.act(&app, "volume", 5.);
        picture(rig, "15-volume-card");
        rig.expire(&app);
        rig.answer = Ok("Muted".into());
        rig.act(&app, "mute", 0.);
        picture(rig, "16-muted");
        rig.expire(&app);

        // A command the speaker refuses, in the screen's words.
        rig.answer = Err("Sonos found nothing to play there.".into());
        rig.act(&app, "previous", 0.);
        picture(rig, "17-refused");
        rig.expire(&app);

        // Leaving and coming straight back shows the same screen at once.
        rig.controller.close(&app);
        rig.pump(&app);
        app.set_player_shown(false);
        rig.controller.open(&app, target.clone());
        picture(rig, "18-reopened-before-the-speaker-answers");
        rig.pump(&app);

        // A speaker that follows another one.
        let mut member = playing();
        member.status.coordinator = "RINCON_KITCHEN".into();
        member.status.coordinator_name = "Kitchen".into();
        rig.now = Ok(member);
        rig.refresh(&app);
        picture(rig, "19-follows-kitchen");
        let mut member = paused();
        member.status.coordinator = "RINCON_KITCHEN".into();
        member.status.coordinator_name = "Kitchen".into();
        rig.now = Ok(member);
        rig.refresh(&app);
        rig.act(&app, "play", 0.);
        picture(rig, "20-follows-kitchen-refuses-play");
        rig.expire(&app);
        rig.act(&app, "audio", 0.);
        picture(rig, "21-follows-kitchen-modes");
        rig.act(&app, "choose", 0.);
        rig.act(&app, "back", 0.);
        rig.expire(&app);

        // A radio stream, the TV input, and nothing at all.
        let mut radio = paused();
        radio.now_playing.container = "BBC Radio 6 Music".into();
        radio.now_playing.container_type = "station".into();
        radio.now_playing.current = Some(track(
            "BBC Radio 6 Music",
            "",
            "",
            "http://192.0.2.9:1400/getaa?s=1&u=6music",
            None,
        ));
        radio.now_playing.next = None;
        rig.now = Ok(radio);
        rig.refresh(&app);
        picture(rig, "22-radio");
        let mut tv = paused();
        tv.now_playing = couch_sonos::NowPlaying {
            container: "TV".into(),
            container_type: "linein.homeTheater".into(),
            current: None,
            next: None,
        };
        rig.now = Ok(tv.clone());
        rig.refresh(&app);
        picture(rig, "23-tv-input");
        tv.now_playing.container.clear();
        tv.now_playing.container_type.clear();
        tv.playback.state = "IDLE".into();
        rig.now = Ok(tv);
        rig.refresh(&app);
        picture(rig, "24-idle");
        rig.act(&app, "play", 0.);

        // A read that fails says so and the next one recovers by itself.
        rig.now = Err("Cannot reach the speaker.".into());
        rig.refresh(&app);
        picture(rig, "25-read-cannot-reach");
        rig.now = Ok(paused());
        rig.refresh(&app);
        // A command that cannot reach the speaker drops the connection: the
        // next read says so, a command says so, and Reconnect asks again,
        // first in vain.
        rig.answer = Err("Cannot reach the speaker.".into());
        rig.act(&app, "play", 0.);
        picture(rig, "26-command-cannot-reach");
        rig.expire(&app);
        rig.refresh(&app);
        rig.act(&app, "volume", -5.);
        picture(rig, "27-not-connected");
        rig.expire(&app);
        rig.connect = Err("Cannot reach the speaker.".into());
        rig.act(&app, "retry", 0.);
        picture(rig, "28-reconnect-failed");
        rig.connect = Ok(());
        rig.act(&app, "retry", 0.);
        picture(rig, "29-reconnected");

        rig.controller.close(&app);
        rig.pump(&app);
        let record = record.into_inner();
        if let Some(dir) = std::env::var_os("COUCH_PLAYER_SCREENSHOTS") {
            std::fs::write(
                std::path::Path::new(&dir).join("player-screen.txt"),
                &record,
            )
            .unwrap();
        }
        assert!(
            differing.borrow().is_empty(),
            "pictures differ from the goldens: {:?}",
            differing.borrow()
        );
        let golden = include_str!("../tests/golden/player-screen.txt");
        assert!(
            record == golden,
            "the player screen's words changed; this run said:\n{record}"
        );
        app.hide().unwrap();
    }
    #[test]
    fn dpad_walks_the_controls_and_the_sheets() {
        // Pure state: no window needed. Start on play (3), move around the
        // grid and confirm the clamps.
        let rows: [&[i32]; 3] = [&[1], &[2, 3, 4], &[5, 6, 7]];
        let step = |selected: i32, dx: i32, dy: i32| -> i32 {
            let (mut row, mut column) = rows
                .iter()
                .enumerate()
                .find_map(|(r, cells)| {
                    cells
                        .iter()
                        .position(|c| *c == selected)
                        .map(|c| (r as i32, c as i32))
                })
                .unwrap_or((1, 1));
            if dy != 0 {
                row = (row + dy).clamp(0, 2);
                column = column.min(rows[row as usize].len() as i32 - 1);
                if rows[row as usize].len() == 1 {
                    column = 0;
                }
            }
            if dx != 0 {
                column = (column + dx).clamp(0, rows[row as usize].len() as i32 - 1);
            }
            rows[row as usize][column as usize]
        };
        assert_eq!(step(3, 1, 0), 4);
        assert_eq!(step(4, 1, 0), 4);
        assert_eq!(step(3, -1, 0), 2);
        assert_eq!(step(3, 0, -1), 1);
        assert_eq!(step(1, 0, 1), 2);
        assert_eq!(step(3, 0, 1), 6);
        assert_eq!(step(7, 0, 1), 7);
        assert_eq!(step(7, 0, -1), 4);
    }
    #[test]
    fn metadata_line_prefers_both_names_and_never_repeats_one() {
        assert_eq!(line("Sublime", "Sublime"), "Sublime");
        assert_eq!(line("Lorde", "Pure Heroine"), "Lorde · Pure Heroine");
        assert_eq!(line("", "Pure Heroine"), "Pure Heroine");
        assert_eq!(line("", ""), "");
    }
    #[test]
    fn clocks_read_like_a_player() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(182.), "3:02");
        assert_eq!(clock(3725.), "1:02:05");
    }
    #[test]
    fn errors_are_sentences_for_the_screen() {
        assert_eq!(
            describe(couch_sonos::Error::NotCoordinator {
                coordinator: "Kitchen".into()
            }),
            "Playback is controlled by Kitchen. Open that speaker to change it."
        );
        assert_eq!(
            describe(couch_sonos::Error::Transport),
            "Cannot reach the speaker."
        );
    }
}
