//! A music player on the player screen: the same full-screen presentation the
//! Kodi Cinema activity uses, driven by whatever is behind a [`Backend`]. Art,
//! title, artist and album, a live progress line with seek, transport, and
//! three sheets: sources, play modes and up next. Device I/O runs on one
//! worker thread that owns the backend; the UI thread only ever sees events.
//!
//! Nothing in this file knows what kind of device it is showing. The built-in
//! Sonos client is one backend (`sonos_player.rs`); an installed package that
//! declares a media player is to be the other, and everything below the
//! [`Backend`] trait is worded so protocol 3's `media`, `artwork`, `inputs`,
//! `seek`, `set_mode` and percent volume can supply it
//! (`docs/plans/media-player-component-design.md`). Because both feed this one
//! controller, the two are the same screen.
use crate::{activity_art, App, PlayerChoice};
use couch_model::{
    commands::{Function as F, KeyPhase},
    Config,
};
use slint::{ModelRc, VecModel};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

/// How often the worker is asked for a fresh snapshot while the screen is up,
/// unless the device asks for another cadence. Position is interpolated
/// between reads, so this is about track changes made elsewhere, not the
/// clock.
const REFRESH: Duration = Duration::from_secs(3);
/// One press of a volume key on this screen.
const VOLUME_STEP: i8 = 5;
/// How many presses the key lane holds. Two may be waiting on the device at
/// once; a third repeat gives way, because the next one is milliseconds
/// behind it, and a third tap is held until there is room, so nothing a
/// person actually did vanishes.
const KEY_LANE: usize = 2;
/// How long a closed screen's presentation is kept for an instant reopen.
/// The worker re-reads the group as soon as the screen is up again, so this
/// only ever bridges the first few hundred milliseconds.
const VIEW_TTL: Duration = Duration::from_secs(30);

/// What the screen looked like when it was left, for one speaker.
struct View {
    at: Instant,
    serial: u64,
    presentation: crate::activity::Presentation,
    media: Option<Media>,
    room: slint::SharedString,
}

/// How the screen is laid out. A music player skips tracks and walks its own
/// controls with the D-pad; a video player skips time, and its keys can go
/// straight to the device that has a menu of its own on a television.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Layout {
    #[default]
    Music,
    /// Nothing built in is one yet: PR-E builds it from a package that
    /// declares `layout: video`.
    #[allow(dead_code)]
    Video,
}
/// A picture a device offers with what is playing. Cover and backdrop are the
/// same shape on this screen - the whole page, behind everything - and a logo
/// replaces the title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtRole {
    Cover,
    /// The built-in Sonos client offers a cover; a backdrop arrives with the
    /// first package that declares one.
    #[allow(dead_code)]
    Backdrop,
    Logo,
}
/// One of the (at most three) sheets under the transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sheet {
    /// What the device can play: [`Backend::sources`].
    Sources,
    /// Shuffle, repeat and crossfade, from [`Media::modes`].
    Modes,
    /// What is queued after this, from [`Media::next`].
    UpNext,
    /// A list the device names itself - chapters, audio, subtitles - read
    /// through [`Backend::list`]. `choose` is false for a list that is only
    /// ever read. Nothing built in declares one yet; PR-E builds these from a
    /// package's manifest.
    #[allow(dead_code)]
    List {
        id: String,
        label: String,
        choose: bool,
    },
}
impl Sheet {
    /// What the button under the transport says.
    fn label(&self) -> &str {
        match self {
            Sheet::Sources => "Sources",
            Sheet::Modes => "Modes",
            Sheet::UpNext => "Up next",
            Sheet::List { label, .. } => label,
        }
    }
}
/// What a device says its player screen is made of. Nothing here is a
/// judgement the screen makes: a package declares it in its manifest, and the
/// built-in Sonos client declares [`Player::music`]. Everything the screen
/// draws or sends follows from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Player {
    pub layout: Layout,
    /// The device has a menu of its own on a television: the D-pad, OK, Back,
    /// Home and Menu go straight to it instead of moving the selection on this
    /// screen.
    pub navigation: bool,
    /// The device takes the channel keys itself (a chapter step, or a channel
    /// on live TV). Without it they skip to the next and previous item.
    pub channel_keys: bool,
    /// How often the device is read while the screen is up.
    pub refresh: Duration,
    pub sheets: Vec<Sheet>,
    /// Which pictures the device offers. A role it does not name is never
    /// asked for.
    pub art_roles: Vec<ArtRole>,
}
impl Default for Player {
    fn default() -> Self {
        Self {
            layout: Layout::Music,
            navigation: false,
            channel_keys: false,
            refresh: REFRESH,
            sheets: Vec::new(),
            art_roles: Vec::new(),
        }
    }
}
impl Player {
    /// A speaker: the three sheets and the cover the built-in Sonos screen has
    /// always had, and no keys of its own.
    pub fn music() -> Self {
        Self {
            sheets: vec![Sheet::Sources, Sheet::Modes, Sheet::UpNext],
            art_roles: vec![ArtRole::Cover],
            ..Self::default()
        }
    }
}
/// What the screen was opened for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Target {
    pub device: String,
    pub name: String,
    pub room: String,
    /// What this kind of device is called in a sentence: "Sonos". For a
    /// package it is the manifest's label.
    pub label: String,
    /// What the device says its player screen is made of.
    pub player: Player,
    /// The house this device is in, for the one thing that sits above it: a
    /// per-key infrared code the owner gave this device. `None` where there is
    /// no configuration to consult.
    pub config: Option<Arc<Config>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PlayState {
    Playing,
    Buffering,
    Paused,
    #[default]
    Idle,
}
/// What makes sense to ask for right now. The screen greys the seek line from
/// `seek`; it does not hold a skip back on `next` or `previous`, because the
/// built-in Sonos screen never has: it sends the skip and words the refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Can {
    pub next: bool,
    pub previous: bool,
    pub seek: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modes {
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_one: bool,
    pub crossfade: bool,
}
/// A partial change to the play modes: `None` leaves a mode as it is. Leaving
/// "repeat this track" clears two modes in one write, which is why this is not
/// one mode and a flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModeChange {
    pub shuffle: Option<bool>,
    pub repeat: Option<bool>,
    pub repeat_one: Option<bool>,
    pub crossfade: Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NextItem {
    pub title: String,
    /// "Artist · Album", already joined.
    pub detail: String,
}
/// What is playing: the controller's only view of the device. Nothing is made
/// up: what the device did not say is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Media {
    /// Opaque identity of what is playing, when the device has one. It changes
    /// when the item changes.
    pub item: Option<String>,
    pub state: PlayState,
    /// `None` means nothing is loaded at all: the idle screen.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// The second line when there is no artist or album: a station, a
    /// playlist, the kind of input.
    pub subtitle: Option<String>,
    /// The service it is playing from. Not shown; part of what makes two items
    /// differ.
    pub source: Option<String>,
    /// `None` with a position means a stream without an end: "LIVE".
    pub duration_ms: Option<u64>,
    /// Where playback was when this was read ([`Watched::age`] ago). `None`
    /// means there is no item with a clock, and the clocks are left alone.
    pub position_ms: Option<u64>,
    /// How fast the position moves: 100 is normal speed, 0 stands still.
    pub rate_percent: i16,
    /// Opaque handle of the picture that goes behind everything, for
    /// [`Backend::artwork`]. It changes when the picture does. A cover and a
    /// backdrop are the same thing here.
    pub art: Option<String>,
    /// Opaque handle of the logo that stands in for the title, where the
    /// device offers one.
    pub logo: Option<String>,
    pub can: Can,
    pub modes: Modes,
    pub next: Option<NextItem>,
    /// This speaker plays what another one leads: that one's name. Transport,
    /// seek, sources and modes are refused here, with "open that speaker".
    pub follows: Option<String>,
    /// What the device calls itself, for the idle sentence; the device's name
    /// in Couch is used without it.
    pub device_name: Option<String>,
}
impl Media {
    /// Whether the Modes and Up next sheets would be built the same from
    /// both: the item with a clock, the one after it and the modes. An open
    /// sheet is rebuilt when they differ, and not for a position or a state.
    fn same_listing(&self, other: &Media) -> bool {
        let clocked = self.position_ms.is_some();
        clocked == other.position_ms.is_some()
            && (!clocked
                || (self.item == other.item
                    && self.title == other.title
                    && self.artist == other.artist
                    && self.album == other.album
                    && self.art == other.art
                    && self.logo == other.logo
                    && self.duration_ms == other.duration_ms
                    && self.source == other.source))
            && self.next == other.next
            && self.modes == other.modes
    }
}
/// One read of the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Watched {
    /// Rises when anything but the position changed; handed back as `after`.
    pub revision: u64,
    /// How long ago `media.position_ms` was true. Zero for a direct read; a
    /// cached copy says how old it is.
    pub age: Duration,
    pub media: Media,
}
/// One row of the Sources sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Choice {
    /// Opaque; handed back in [`Op::Source`].
    pub id: String,
    pub title: String,
    /// Where it comes from ("Apple Music", "Sonos playlist · 12 tracks").
    pub detail: String,
    /// The one playing now, if the device says: the sheet opens on it.
    pub current: bool,
}
/// Something to do to the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    PlayPause,
    Next,
    Previous,
    Seek {
        position_ms: u64,
    },
    /// Forwards or back by this much from wherever playback is, never zero:
    /// the "10s" and "30s" buttons of a video layout.
    SeekBy {
        delta_ms: i64,
    },
    /// Relative percent volume, never zero; the device answers with where it
    /// ended up.
    StepVolume(i8),
    ToggleMute,
    /// Start a row of the Sources sheet.
    Source {
        id: String,
        title: String,
    },
    Modes(ModeChange),
    /// Pick a row of a declared list: a chapter, an audio or subtitle track.
    Choose {
        list: String,
        id: String,
    },
    /// A key the device handles itself, on a screen that navigates. It is
    /// never read back, and it travels on the key lane.
    Key {
        function: F,
        phase: KeyPhase,
    },
}
impl Op {
    /// The key this is, when it is one. A per-device infrared code for that
    /// exact key is consulted before the device hears about it, and only these
    /// ops travel on the key lane.
    fn function(&self) -> Option<&F> {
        match self {
            Op::Key { function, .. } => Some(function),
            _ => None,
        }
    }
    /// Whether the press is one the panel repeated while the key was held.
    fn repeat(&self) -> bool {
        matches!(
            self,
            Op::Key {
                phase: KeyPhase::Repeat,
                ..
            }
        )
    }
}
/// What a finished [`Op`] has to tell the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Done {
    Nothing,
    /// Where a volume step ended up, 0 to 100: shown on the volume card.
    Volume(u8),
    /// Whether the device is muted now.
    Muted(bool),
}
/// Why the device did not do it. Couch words every one except `Message`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No answer from the device. After a command the worker lets go of the
    /// backend until the screen asks it to connect again.
    Unreachable,
    /// The worker has no open backend.
    NotConnected,
    /// This speaker follows `leader`; open that one.
    Follows {
        leader: String,
    },
    NothingToPlay,
    /// The screen moved on before the request went out. Never shown.
    Expired,
    /// One line in the device's own words.
    Message(String),
}
/// The device behind the screen. Every call blocks on device I/O and runs on
/// the controller's worker thread, never on the UI thread.
pub(crate) trait Backend: Send {
    /// Reach the device. Called again after `close` when the screen asks to
    /// reconnect.
    fn open(&mut self) -> Result<(), Failure>;
    /// Let go of the device: the screen closed, or a command could not reach
    /// it.
    fn close(&mut self);
    /// What is playing. A backend that reads the device directly answers at
    /// once; one in front of a cache may wait up to `wait` for a revision
    /// above `after`. The controller asks with no wait, every [`REFRESH`] and
    /// once after each command.
    fn watch(&mut self, after: u64, wait: Duration) -> Result<Watched, Failure>;
    /// Do it, unless `current` has turned false by the time it would go out.
    fn perform(&mut self, op: &Op, current: &dyn Fn() -> bool) -> Result<Done, Failure>;
    /// The rows of the Sources sheet.
    fn sources(&mut self) -> Result<Vec<Choice>, Failure>;
    /// The rows of a list the device declared ([`Sheet::List`]). A device with
    /// no lists is never asked.
    fn list(&mut self, _list: &str) -> Result<Vec<Choice>, Failure> {
        Ok(Vec::new())
    }
    /// The encoded bytes (JPEG, PNG) behind [`Media::art`] or [`Media::logo`].
    fn artwork(&mut self, art: &str) -> Result<Vec<u8>, Failure>;
    /// A second way to reach the same device, for the key lane. A backend
    /// whose requests are each their own connection hands one over here, and
    /// then a read or a picture in flight can never make a key press wait.
    /// `None` - the default - puts keys behind the reads, which is what a
    /// device held open over one connection wants.
    fn key_lane(&mut self) -> Option<Box<dyn Backend>> {
        None
    }
}
enum Request {
    /// Take this backend and open it. `known_art` is the picture the UI
    /// already shows, so it is not fetched again; `roles` is what this device
    /// offers, and nothing else is ever asked for.
    Open {
        backend: Box<dyn Backend>,
        known_art: String,
        roles: Vec<ArtRole>,
        device: String,
        config: Option<Arc<Config>>,
    },
    /// Open the same backend again, after a failure.
    Reopen,
    Refresh,
    Command(Op),
    Sources,
    List(String),
    Close,
}
/// What the key lane is asked to do. It is a lane of its own so that a read or
/// an artwork fetch on the state lane cannot make a press wait.
enum KeyRequest {
    /// Take this second handle to the device, and the house the per-key
    /// infrared override is read from.
    Open {
        backend: Box<dyn Backend>,
        device: String,
        config: Option<Arc<Config>>,
    },
    Press(Op),
    Close,
}
enum Event {
    State(Box<Result<Watched, Failure>>),
    Done(Result<String, Failure>),
    Sources(Result<Vec<Choice>, Failure>),
    /// The rows of a declared list, by its id.
    List(String, Result<Vec<Choice>, Failure>),
    /// A key the device or the blaster refused. It never clears the state
    /// lane's `busy`: the two lanes do not know about each other.
    Key(Failure),
    Art(String, Option<activity_art::Pixels>),
    /// The logo that stands in for the title.
    Logo(String, Option<activity_art::Pixels>),
}
pub struct Controller {
    tx: mpsc::SyncSender<(u64, Request)>,
    /// The key lane. Its own thread, its own handle to the device, and a queue
    /// two deep, so a refresh or a picture already in flight never delays a
    /// press.
    keys: mpsc::SyncSender<(u64, KeyRequest)>,
    /// Whether the key lane has a handle of its own. Without one a key goes
    /// out on the state lane, as everything else does.
    key_lane: bool,
    /// A tap the key lane was too full to take. It is held, not dropped, and
    /// the latest one wins; a repeat is never held, because the next one is
    /// milliseconds behind it.
    held_key: Option<Op>,
    rx: mpsc::Receiver<(u64, Event)>,
    active: Arc<AtomicU64>,
    generation: u64,
    target: Option<Target>,
    media: Option<Media>,
    /// When the media's position was read, for interpolation.
    at: Instant,
    tick: Instant,
    refresh_at: Instant,
    busy: bool,
    message_until: Option<Instant>,
    volume_until: Option<Instant>,
    art_key: String,
    logo_key: String,
    /// Whether this screen has been drawn once since it opened. Until it has,
    /// nothing on it travels: the first picture is what an opening is made
    /// from, and a bar caught mid-travel in it is a bar that jumps when the
    /// transition ends.
    drawn: bool,
    sources: Vec<Choice>,
    /// The rows of each declared list, once the device has been asked.
    lists: std::collections::HashMap<String, Vec<Choice>>,
    /// On-screen selection the D-pad moves: 1 seek, 2 previous, 3 play,
    /// 4 next, 5..=7 sheets.
    selected: i32,
    /// Recently closed screens by device id, restored on reopen.
    views: std::collections::HashMap<String, View>,
}
impl Controller {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(16);
        let (keys, key_rx) = mpsc::sync_channel(KEY_LANE);
        let (events, receive) = mpsc::sync_channel(8);
        let active = Arc::new(AtomicU64::new(0));
        let worker_active = active.clone();
        let key_events = events.clone();
        let key_active = active.clone();
        std::thread::spawn(move || worker(rx, events, worker_active));
        std::thread::spawn(move || key_worker(key_rx, key_events, key_active));
        Self {
            tx,
            keys,
            key_lane: false,
            held_key: None,
            rx: receive,
            active,
            generation: 0,
            target: None,
            media: None,
            at: Instant::now(),
            tick: Instant::now(),
            refresh_at: Instant::now(),
            busy: false,
            message_until: None,
            volume_until: None,
            art_key: String::new(),
            logo_key: String::new(),
            drawn: false,
            sources: Vec::new(),
            lists: std::collections::HashMap::new(),
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
                    media: self.media.clone(),
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
    /// Take the player screen over for the device behind `backend`. The caller
    /// has already released whatever the screen showed before.
    pub fn open(&mut self, app: &App, target: Target, mut backend: Box<dyn Backend>) {
        self.generation += 1;
        self.active.store(self.generation, Ordering::SeqCst);
        self.media = None;
        self.busy = false;
        self.held_key = None;
        self.art_key.clear();
        self.logo_key.clear();
        self.drawn = false;
        app.set_player_glide(false);
        // A speaker, or a television for a device that plays pictures, unless
        // a room row said otherwise: the screen can be reached from an
        // activity as well as from a row, and the disc in the header is never
        // empty.
        app.set_player_icon(crate::icons::image(match target.player.layout {
            Layout::Music => couch_model::Icon::Speaker,
            Layout::Video => couch_model::Icon::Tv,
        }));
        app.set_player_known(true);
        app.set_player_active(true);
        self.sources.clear();
        self.lists.clear();
        self.selected = 3;
        self.message_until = None;
        // What the keys do is what the device declared, not the layout: a
        // video player with a menu of its own takes the D-pad, and a sheet
        // over either layout takes it back for as long as it is open.
        app.set_player_music(target.player.layout == Layout::Music);
        app.set_player_navigate(target.player.navigation);
        app.set_player_sheet_keys(true);
        app.set_player_sheets(ModelRc::new(VecModel::from(sheet_labels(&target.player))));
        app.set_player_selected(self.selected);
        app.set_player_shown(true);
        app.set_player_panel(0);
        app.set_player_message("".into());
        app.set_player_logo(slint::Image::default());
        app.set_player_has_logo(false);
        app.set_player_activity(target.name.as_str().into());
        // Where it is and what drives it, the second line every screen a room
        // row opens carries - not the room's name on its own.
        app.set_player_room(Self::where_it_is(&target).into());
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
                self.media = view.media;
                self.at = view.at;
            }
            None => {
                app.set_player_ready(false);
                app.set_player_connected(false);
                app.set_player_title(format!("Connecting to {}…", target.label).into());
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
        // The key lane takes its own handle to the device where the backend
        // has one to give, and the house the per-key infrared override is read
        // from travels with it.
        let key_backend = backend.key_lane();
        self.key_lane = match key_backend {
            Some(backend) => self
                .keys
                .try_send((
                    self.generation,
                    KeyRequest::Open {
                        backend,
                        device: target.device.clone(),
                        config: target.config.clone(),
                    },
                ))
                .is_ok(),
            None => {
                let _ = self.keys.try_send((self.generation, KeyRequest::Close));
                false
            }
        };
        if self
            .tx
            .try_send((
                self.generation,
                Request::Open {
                    backend,
                    known_art,
                    roles: target.player.art_roles.clone(),
                    device: target.device.clone(),
                    config: target.config.clone(),
                },
            ))
            .is_err()
        {
            self.notice(app, "Connection busy. Reopen the speaker.");
        }
        self.refresh_at = Instant::now() + target.player.refresh;
        self.target = Some(target);
    }
    /// Leave the screen. The caller decides where focus goes next.
    pub fn close(&mut self, app: &App) {
        self.remember(app);
        self.generation += 1;
        self.active.store(self.generation, Ordering::SeqCst);
        let _ = self.tx.try_send((self.generation, Request::Close));
        let _ = self.keys.try_send((self.generation, KeyRequest::Close));
        self.target = None;
        self.media = None;
        self.busy = false;
        self.key_lane = false;
        self.held_key = None;
        self.art_key.clear();
        self.logo_key.clear();
        self.lists.clear();
        // Back to what the built-in Kodi screen is: a video layout whose keys
        // go to the player, with its own three sheets.
        app.set_player_music(false);
        app.set_player_navigate(true);
        app.set_player_sheet_keys(false);
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
        self.request(app, Request::Command(op));
    }
    /// A key on the lane that carries nothing but keys. It never waits behind
    /// a read and it is never dropped without a reason: when the lane is full
    /// a repeat gives way, and a tap is held until there is room, latest
    /// first.
    fn key(&mut self, app: &App, function: F, phase: KeyPhase) {
        let op = Op::Key { function, phase };
        if !self.key_lane {
            // One connection to the device: the key queues with everything
            // else, as it always has.
            self.send(app, op);
            return;
        }
        let repeat = op.repeat();
        if self
            .keys
            .try_send((self.generation, KeyRequest::Press(op.clone())))
            .is_err()
            && !repeat
        {
            self.held_key = Some(op);
        }
    }
    /// The held tap, once the lane has room for it.
    fn flush_key(&mut self) {
        let Some(op) = self.held_key.take() else {
            return;
        };
        if self
            .keys
            .try_send((self.generation, KeyRequest::Press(op.clone())))
            .is_err()
        {
            self.held_key = Some(op);
        }
    }
    /// Whether the device takes the D-pad, OK, Back, Home and Menu itself.
    fn navigating(&self) -> bool {
        self.target.as_ref().is_some_and(|t| t.player.navigation)
    }
    /// The sheet a button under the transport opens, 1 to 3.
    fn sheet(&self, panel: i32) -> Option<&Sheet> {
        let index = usize::try_from(panel - 1).ok()?;
        self.target.as_ref()?.player.sheets.get(index)
    }
    fn request(&mut self, app: &App, request: Request) {
        if self.busy {
            return;
        }
        if self.tx.try_send((self.generation, request)).is_ok() {
            self.busy = true;
        } else {
            self.notice(app, "Connection busy. Try again.");
        }
    }
    /// A failure in the screen's words.
    fn describe(&self, failure: Failure) -> String {
        describe(
            failure,
            self.target.as_ref().map_or("", |t| t.label.as_str()),
        )
    }
    fn coordinator(&self) -> bool {
        self.media.as_ref().is_some_and(|m| m.follows.is_none())
    }
    /// Whether a transport command may go out now: a member's playback
    /// belongs to its coordinator, and the screen says so instead of sending.
    fn transport_allowed(&mut self, app: &App) -> bool {
        let Some(media) = &self.media else {
            self.notice(app, "Still connecting to the speaker.");
            return false;
        };
        if let Some(leader) = media.follows.clone() {
            let refusal = self.describe(Failure::Follows { leader });
            self.notice(app, &refusal);
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
        let phase = if repeat {
            KeyPhase::Repeat
        } else {
            KeyPhase::Tap
        };
        match action {
            "back" | "Input.Back" => {
                if app.get_player_panel() != 0 {
                    app.set_player_panel(0);
                    app.invoke_focus_player();
                    return true;
                }
                // A device with a menu of its own gets the Back key; only a
                // held Back, which arrives as "back", leaves the screen.
                if action == "Input.Back" && self.navigating() {
                    self.key(app, F::Back, KeyPhase::Tap);
                    return true;
                }
                return false;
            }
            "Input.Home" => {
                if self.navigating() {
                    self.key(app, F::Home, KeyPhase::Tap);
                    return true;
                }
                return false;
            }
            "retry" => {
                if self.target.is_some() {
                    self.media = None;
                    self.busy = false;
                    let _ = self.tx.try_send((self.generation, Request::Reopen));
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
                // The channel keys are the device's own chapter or channel
                // step where it takes them, and the next and previous item
                // where it does not.
                if action == "chapter-step"
                    && self.target.as_ref().is_some_and(|t| t.player.channel_keys)
                {
                    let function = if forward {
                        F::ChannelUp
                    } else {
                        F::ChannelDown
                    };
                    self.key(app, function, KeyPhase::Tap);
                    return true;
                }
                if self.transport_allowed(app) {
                    self.send(app, if forward { Op::Next } else { Op::Previous });
                }
            }
            "seek" => {
                let duration = self.media.as_ref().and_then(|m| m.duration_ms);
                if let Some(duration) = duration {
                    if self.transport_allowed(app) {
                        let position_ms = (duration as f64 * value.clamp(0., 100.) / 100.) as u64;
                        self.send(app, Op::Seek { position_ms });
                    }
                }
            }
            // The "10s" and "30s" buttons of a video layout: from wherever
            // playback is, not to a place on the line.
            "skip" => {
                if self.media.as_ref().is_some_and(|m| m.can.seek) && self.transport_allowed(app) {
                    let delta_ms = (value.clamp(-3600., 3600.) * 1000.) as i64;
                    if delta_ms != 0 {
                        self.send(app, Op::SeekBy { delta_ms });
                    }
                }
            }
            "volume" => {
                let delta = (value as i32).clamp(-20, 20) as i8;
                let delta = if delta == 0 { VOLUME_STEP } else { delta };
                self.send(app, Op::StepVolume(delta));
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
            "Input.Up" => self.arrow(app, F::Up, phase, 0, -1),
            "Input.Down" => self.arrow(app, F::Down, phase, 0, 1),
            "Input.Left" => self.arrow(app, F::Left, phase, -1, 0),
            "Input.Right" => self.arrow(app, F::Right, phase, 1, 0),
            "Input.Select" => {
                // OK is a tap wherever it lands: a held OK on a device screen
                // would delay every press of it.
                if self.navigating() {
                    self.key(app, F::Ok, KeyPhase::Tap);
                } else {
                    self.activate(app);
                }
            }
            "Input.ContextMenu" => {
                if self.navigating() {
                    self.key(app, F::Menu, KeyPhase::Tap);
                } else {
                    // Menu opens what the device can play.
                    let sources = self
                        .target
                        .as_ref()
                        .and_then(|t| t.player.sheets.iter().position(|s| *s == Sheet::Sources));
                    if let Some(index) = sources {
                        self.panel(app, index as i32 + 1);
                    }
                }
            }
            _ => {}
        }
        true
    }
    /// An arrow: to the device on a screen that navigates, and to the
    /// on-screen selection on one that does not.
    fn arrow(&mut self, app: &App, function: F, phase: KeyPhase, dx: i32, dy: i32) {
        if self.navigating() {
            self.key(app, function, phase);
        } else {
            self.step(app, dx, dy);
        }
    }
    fn panel(&mut self, app: &App, panel: i32) {
        let Some(sheet) = self.sheet(panel).cloned() else {
            return;
        };
        let mut rows = Vec::new();
        let mut detail = String::new();
        // A list that marks the row playing now opens on it.
        let mut current = 0;
        let mut ask = None;
        match &sheet {
            Sheet::Sources => {
                if self.sources.is_empty() {
                    detail = "Finding sources…".into();
                    ask = Some(Request::Sources);
                } else {
                    rows = self
                        .sources
                        .iter()
                        .map(|s| PlayerChoice {
                            title: s.title.as_str().into(),
                            detail: s.detail.as_str().into(),
                        })
                        .collect();
                    current = self.sources.iter().position(|s| s.current).unwrap_or(0);
                }
            }
            Sheet::Modes => {
                let modes = self.media.as_ref().map(|m| m.modes).unwrap_or_default();
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
            Sheet::UpNext => match self.media.as_ref().and_then(|m| m.next.as_ref()) {
                Some(next) => rows.push(PlayerChoice {
                    title: next.title.as_str().into(),
                    detail: format!("{} · Press to skip to it", next.detail).into(),
                }),
                None => detail = "Nothing is queued after this.".into(),
            },
            // A list the device names itself, read once and kept until the
            // item changes under it.
            Sheet::List { id, label, .. } => match self.lists.get(id) {
                None => {
                    detail = format!("Reading {}…", label.to_lowercase());
                    ask = Some(Request::List(id.clone()));
                }
                Some(list) if list.is_empty() => {
                    detail = format!("No {} for this.", label.to_lowercase());
                }
                Some(list) => {
                    rows = list
                        .iter()
                        .map(|row| PlayerChoice {
                            title: row.title.as_str().into(),
                            detail: row.detail.as_str().into(),
                        })
                        .collect();
                    current = list.iter().position(|row| row.current).unwrap_or(0);
                }
            },
        }
        if let Some(request) = ask {
            self.request(app, request);
        }
        app.set_player_choices(ModelRc::new(VecModel::from(rows)));
        app.set_player_panel_detail(detail.into());
        app.set_player_panel(panel);
        app.invoke_set_player_choice(current as i32);
        app.invoke_focus_player();
    }
    fn choose(&mut self, app: &App, index: usize) {
        let Some(sheet) = self.sheet(app.get_player_panel()).cloned() else {
            return;
        };
        match sheet {
            Sheet::Sources => {
                if let Some(source) = self.sources.get(index).cloned() {
                    if self.transport_allowed(app) {
                        self.notice(app, &format!("Starting {}…", source.title));
                        self.send(
                            app,
                            Op::Source {
                                id: source.id,
                                title: source.title,
                            },
                        );
                        app.set_player_panel(0);
                        app.invoke_focus_player();
                    }
                }
            }
            Sheet::Modes => {
                let modes = self.media.as_ref().map(|m| m.modes).unwrap_or_default();
                let change = match index {
                    0 => ModeChange {
                        shuffle: Some(!modes.shuffle),
                        ..Default::default()
                    },
                    // Off → all → this track → off.
                    1 => {
                        if modes.repeat_one {
                            ModeChange {
                                repeat: Some(false),
                                repeat_one: Some(false),
                                ..Default::default()
                            }
                        } else if modes.repeat {
                            ModeChange {
                                repeat_one: Some(true),
                                ..Default::default()
                            }
                        } else {
                            ModeChange {
                                repeat: Some(true),
                                ..Default::default()
                            }
                        }
                    }
                    2 => ModeChange {
                        crossfade: Some(!modes.crossfade),
                        ..Default::default()
                    },
                    _ => return,
                };
                if self.transport_allowed(app) {
                    self.send(app, Op::Modes(change));
                }
            }
            Sheet::UpNext => {
                if self.transport_allowed(app) {
                    self.send(app, Op::Next);
                    app.set_player_panel(0);
                    app.invoke_focus_player();
                }
            }
            Sheet::List { id, choose, .. } => {
                if !choose {
                    return;
                }
                let row = self
                    .lists
                    .get(&id)
                    .and_then(|list| list.get(index))
                    .map(|row| row.id.clone());
                if let Some(chosen) = row {
                    if self.transport_allowed(app) {
                        self.send(
                            app,
                            Op::Choose {
                                list: id,
                                id: chosen,
                            },
                        );
                        app.set_player_panel(0);
                        app.invoke_focus_player();
                    }
                }
            }
        }
    }
    /// Where it is and what drives it: "Living room · Sonos", the second line
    /// every screen a room row opens carries.
    fn where_it_is(target: &Target) -> String {
        if target.label.is_empty() {
            target.room.clone()
        } else {
            format!("{} · {}", target.room, target.label)
        }
    }
    /// `age` is how long ago the media's position was true.
    fn present(&mut self, app: &App, media: &Media, age: f64) {
        // From the second picture on, a new position is travelled to rather
        // than jumped to; the first one is left still.
        app.set_player_glide(self.drawn);
        self.drawn = true;
        let playing = matches!(media.state, PlayState::Playing | PlayState::Buffering);
        app.set_player_connected(true);
        app.set_player_ready(media.title.is_some());
        app.set_player_paused(!playing);
        if let Some(t) = &self.target {
            app.set_player_room(
                match &media.follows {
                    Some(leader) => format!("{} · Playing from {}", t.room, leader),
                    // Where it is and what drives it, the second line every
                    // screen a room row opens carries. A speaker following
                    // another says that instead: it is the more useful fact,
                    // and the two do not fit on one line.
                    None if !t.label.is_empty() => format!("{} · {}", t.room, t.label),
                    None => t.room.clone(),
                }
                .into(),
            );
        }
        match &media.title {
            Some(title) => {
                app.set_player_title(title.as_str().into());
                let mut meta = line(
                    media.artist.as_deref().unwrap_or(""),
                    media.album.as_deref().unwrap_or(""),
                );
                if meta.is_empty() {
                    meta = media.subtitle.clone().unwrap_or_default();
                }
                app.set_player_metadata(meta.into());
                app.set_player_can_seek(media.can.seek);
            }
            None => {
                app.set_player_title(self.idle_sentence(media).into());
                app.set_player_metadata("".into());
                app.set_player_can_seek(false);
                app.set_player_elapsed("".into());
                app.set_player_remaining("".into());
                app.set_player_progress(0.);
            }
        }
        let key = media.art.as_deref().unwrap_or("");
        if key != self.art_key {
            self.art_key = key.to_owned();
            app.set_player_has_art(false);
            app.set_player_fanart(slint::Image::default());
        }
        let logo = media.logo.as_deref().unwrap_or("");
        if logo != self.logo_key {
            self.logo_key = logo.to_owned();
            app.set_player_has_logo(false);
            app.set_player_logo(slint::Image::default());
        }
        self.clock(app, media, age);
    }
    /// What the screen says when the device has nothing loaded. A device with
    /// a menu of its own is where the choosing happens, so the screen says so
    /// and stays out of the way; a speaker is asked for something to play.
    fn idle_sentence(&self, media: &Media) -> String {
        let player = self.target.as_ref().map(|t| &t.player);
        if player.is_some_and(|p| p.navigation) {
            let label = self.target.as_ref().map_or("", |t| t.label.as_str());
            return format!(
                "Connected to {label}.\nUse the remote to choose something on your TV."
            );
        }
        let name = match (&media.device_name, &self.target) {
            (Some(name), _) => name.as_str(),
            (None, Some(t)) => t.name.as_str(),
            (None, None) => "",
        };
        if player.is_some_and(|p| p.sheets.contains(&Sheet::Sources)) {
            format!("{name} is idle.\nPress Sources to play something.")
        } else {
            format!("{name} is idle.")
        }
    }
    /// The progress line, from the last read position plus the time since.
    fn clock(&self, app: &App, media: &Media, elapsed_since: f64) {
        let Some(position_ms) = media.position_ms else {
            return;
        };
        let position =
            position_ms as f64 / 1000. + elapsed_since * (f64::from(media.rate_percent) / 100.);
        match media.duration_ms {
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
                    Ok(Watched { age, media, .. }) => {
                        let changed = !self.media.as_ref().is_some_and(|m| m.same_listing(&media));
                        let now = Instant::now();
                        self.at = now.checked_sub(age).unwrap_or(now);
                        self.present(app, &media, age.as_secs_f64());
                        let panel = app.get_player_panel();
                        self.media = Some(media);
                        // A sheet built from the old state is rebuilt from the new one.
                        if changed && (panel == 2 || panel == 3) {
                            self.panel(app, panel);
                        }
                    }
                    Err(error) => {
                        self.media = None;
                        app.set_player_ready(false);
                        app.set_player_connected(false);
                        app.set_player_title(self.describe(error).into());
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
                    let error = self.describe(error);
                    self.notice(app, &error);
                }
                Event::Sources(Ok(sources)) => {
                    self.busy = false;
                    self.sources = sources;
                    if app.get_player_panel() == 1 {
                        if self.sources.is_empty() {
                            app.set_player_panel_detail(
                                format!(
                                    "No sources: add favourites or playlists in the {} app.",
                                    self.target.as_ref().map_or("", |t| t.label.as_str())
                                )
                                .into(),
                            );
                        } else {
                            self.panel(app, 1);
                        }
                    }
                }
                Event::Sources(Err(error)) => {
                    self.busy = false;
                    if app.get_player_panel() == 1 {
                        app.set_player_panel_detail(self.describe(error).into());
                    }
                }
                Event::List(id, result) => {
                    self.busy = false;
                    let open = self
                        .sheet(app.get_player_panel())
                        .is_some_and(|s| matches!(s, Sheet::List { id: open, .. } if *open == id));
                    match result {
                        Ok(rows) => {
                            self.lists.insert(id, rows);
                            if open {
                                self.panel(app, app.get_player_panel());
                            }
                        }
                        Err(error) => {
                            if open {
                                app.set_player_panel_detail(self.describe(error).into());
                            }
                        }
                    }
                }
                Event::Key(error) => {
                    let error = self.describe(error);
                    if !error.is_empty() {
                        self.notice(app, &error);
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
                Event::Logo(key, pixels) => {
                    if key == self.logo_key {
                        if let Some(pixels) = pixels {
                            app.set_player_logo(activity_art::slint_image(pixels));
                            app.set_player_has_logo(true);
                        }
                    }
                }
            }
        }
        // A press the key lane had no room for goes out as soon as it has.
        self.flush_key();
        if self.tick.elapsed() >= Duration::from_secs(1) {
            self.tick = Instant::now();
            if let Some(media) = &self.media {
                self.clock(app, media, self.at.elapsed().as_secs_f64());
            }
        }
        if Instant::now() >= self.refresh_at {
            let every = self.target.as_ref().map_or(REFRESH, |t| t.player.refresh);
            self.refresh_at = Instant::now() + every;
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
/// What the buttons under the transport say.
fn sheet_labels(player: &Player) -> Vec<slint::SharedString> {
    player.sheets.iter().map(|s| s.label().into()).collect()
}
/// "Artist · Album", or whichever of the two the player gave.
pub(crate) fn line(artist: &str, album: &str) -> String {
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
/// A failure in the screen's words. `label` is what this kind of device is
/// called.
pub(crate) fn describe(failure: Failure, label: &str) -> String {
    match failure {
        Failure::Follows { leader } => {
            format!("Playback is controlled by {leader}. Open that speaker to change it.")
        }
        Failure::Unreachable => "Cannot reach the speaker.".into(),
        Failure::NotConnected => "Not connected to the speaker.".into(),
        Failure::NothingToPlay => format!("{label} found nothing to play there."),
        Failure::Expired => String::new(),
        Failure::Message(text) => text,
    }
}
fn worker(
    rx: mpsc::Receiver<(u64, Request)>,
    events: mpsc::SyncSender<(u64, Event)>,
    active: Arc<AtomicU64>,
) {
    let mut worker = Worker::default();
    while let Ok((generation, request)) = rx.recv() {
        if active.load(Ordering::SeqCst) != generation {
            continue;
        }
        if !worker.handle(generation, request, &events, &active) {
            return;
        }
    }
}
/// The one thing that sits above the device: a per-key infrared code the owner
/// gave this exact device. `true` means the blaster did it and the device is
/// never told. Anything without a key of its own passes straight through.
fn ir_override(
    config: &Option<Arc<Config>>,
    device: &str,
    op: &Op,
    current: &dyn Fn() -> bool,
) -> Result<bool, Failure> {
    let (Some(config), Some(function)) = (config, op.function()) else {
        return Ok(false);
    };
    crate::activity_buttons::try_device_ir(config, device, function, op.repeat(), current)
        .map_err(Failure::Message)
}
/// The key lane. It carries keys and nothing else, over a handle of its own,
/// so a read or an artwork fetch already in flight on the state lane can never
/// make a press wait or vanish.
fn key_worker(
    rx: mpsc::Receiver<(u64, KeyRequest)>,
    events: mpsc::SyncSender<(u64, Event)>,
    active: Arc<AtomicU64>,
) {
    let mut lane = KeyLane::default();
    while let Ok((generation, request)) = rx.recv() {
        if active.load(Ordering::SeqCst) != generation {
            continue;
        }
        if !lane.handle(generation, request, &events, &active) {
            return;
        }
    }
}
/// What the key lane keeps between presses.
#[derive(Default)]
struct KeyLane {
    backend: Option<Box<dyn Backend>>,
    device: String,
    config: Option<Arc<Config>>,
}
impl KeyLane {
    /// One press. Returns false when the UI has gone away.
    fn handle(
        &mut self,
        generation: u64,
        request: KeyRequest,
        events: &mpsc::SyncSender<(u64, Event)>,
        active: &AtomicU64,
    ) -> bool {
        let current = || active.load(Ordering::SeqCst) == generation;
        let send = |event: Event| events.send((generation, event)).is_ok();
        match request {
            KeyRequest::Close => {
                if let Some(mut backend) = self.backend.take() {
                    backend.close();
                }
                self.device.clear();
                self.config = None;
            }
            KeyRequest::Open {
                backend,
                device,
                config,
            } => {
                if let Some(mut old) = self.backend.replace(backend) {
                    old.close();
                }
                self.device = device;
                self.config = config;
                // A lane that cannot be opened says nothing: the state lane is
                // reaching the same device and words the failure once.
                if let Some(backend) = self.backend.as_mut() {
                    if backend.open().is_err() {
                        self.backend = None;
                    }
                }
            }
            KeyRequest::Press(op) => {
                match ir_override(&self.config, &self.device, &op, &current) {
                    Ok(true) => return true,
                    Ok(false) => {}
                    Err(failure) => return send(Event::Key(failure)),
                }
                let Some(backend) = self.backend.as_mut() else {
                    return send(Event::Key(Failure::NotConnected));
                };
                match backend.perform(&op, &current) {
                    Ok(_) | Err(Failure::Expired) => {}
                    Err(failure) => return send(Event::Key(failure)),
                }
            }
        }
        true
    }
}
/// What the worker thread keeps between requests.
#[derive(Default)]
struct Worker {
    backend: Option<Box<dyn Backend>>,
    /// Whether `backend` is open. A command that could not reach the device
    /// closes it, and it stays closed until the screen asks to reconnect.
    connected: bool,
    revision: u64,
    art_sent: String,
    logo_sent: String,
    /// Which pictures this device offers; a role it did not name is never
    /// asked for.
    roles: Vec<ArtRole>,
    device: String,
    config: Option<Arc<Config>>,
}
impl Worker {
    /// One request. Returns false when the UI has gone away.
    fn handle(
        &mut self,
        generation: u64,
        request: Request,
        events: &mpsc::SyncSender<(u64, Event)>,
        active: &AtomicU64,
    ) -> bool {
        let current = || active.load(Ordering::SeqCst) == generation;
        let send = |event: Event| events.send((generation, event)).is_ok();
        match request {
            Request::Close => {
                if let Some(mut backend) = self.backend.take() {
                    backend.close();
                }
                self.connected = false;
                self.art_sent.clear();
            }
            Request::Open {
                backend,
                known_art,
                roles,
                device,
                config,
            } => {
                if let Some(mut old) = self.backend.replace(backend) {
                    old.close();
                }
                self.art_sent = known_art;
                self.logo_sent.clear();
                self.roles = roles;
                self.device = device;
                self.config = config;
                return self.open(&send, &current);
            }
            Request::Reopen => {
                self.art_sent.clear();
                self.logo_sent.clear();
                return self.open(&send, &current);
            }
            Request::Refresh => return self.refresh(&send, &current),
            Request::List(id) => {
                let result = match self.backend.as_mut().filter(|_| self.connected) {
                    Some(backend) => backend.list(&id),
                    None => Err(Failure::NotConnected),
                };
                return send(Event::List(id, result));
            }
            Request::Sources => {
                let result = match self.backend.as_mut().filter(|_| self.connected) {
                    Some(backend) => backend.sources(),
                    None => return send(Event::Done(Err(Failure::NotConnected))),
                };
                return send(Event::Sources(result));
            }
            Request::Command(op) => {
                // A key the owner gave this device an infrared code for never
                // reaches the device itself.
                match ir_override(&self.config, &self.device, &op, &current) {
                    Ok(true) => return send(Event::Done(Ok(String::new()))),
                    Ok(false) => {}
                    Err(failure) => return send(Event::Done(Err(failure))),
                }
                let Some(backend) = self.backend.as_mut().filter(|_| self.connected) else {
                    return send(Event::Done(Err(Failure::NotConnected)));
                };
                let outcome = match backend.perform(&op, &current) {
                    Ok(Done::Volume(volume)) => Ok(format!("volume:{volume}")),
                    Ok(Done::Muted(muted)) => {
                        Ok(if muted { "Muted" } else { "Unmuted" }.to_owned())
                    }
                    Ok(Done::Nothing) => Ok(match &op {
                        Op::Source { title, .. } => format!("Playing {title}"),
                        _ => String::new(),
                    }),
                    Err(Failure::Expired) => Ok(String::new()),
                    Err(other) => Err(other),
                };
                let transport_failed = matches!(outcome, Err(Failure::Unreachable));
                if !send(Event::Done(outcome)) {
                    return false;
                }
                if transport_failed {
                    backend.close();
                    self.connected = false;
                    return true;
                }
                // A source load settles over a second or two; read once now
                // and let the periodic refresh catch up.
                if matches!(op, Op::Source { .. }) {
                    std::thread::sleep(Duration::from_millis(600));
                }
                return self.refresh(&send, &current);
            }
        }
        true
    }
    fn open(&mut self, send: &dyn Fn(Event) -> bool, current: &dyn Fn() -> bool) -> bool {
        self.connected = false;
        self.revision = 0;
        if let Some(backend) = self.backend.as_mut() {
            if let Err(e) = backend.open() {
                return send(Event::State(Box::new(Err(e))));
            }
            self.connected = true;
        }
        self.refresh(send, current)
    }
    /// One read to the UI, then the artwork for an item whose art has not been
    /// sent yet. Returns false when the UI has gone away.
    fn refresh(&mut self, send: &dyn Fn(Event) -> bool, current: &dyn Fn() -> bool) -> bool {
        let Some(backend) = self.backend.as_mut().filter(|_| self.connected) else {
            return send(Event::State(Box::new(Err(Failure::NotConnected))));
        };
        if !current() {
            return true;
        }
        let watched = match backend.watch(self.revision, Duration::ZERO) {
            Ok(w) => w,
            Err(e) => return send(Event::State(Box::new(Err(e)))),
        };
        self.revision = watched.revision;
        let art = watched.media.art.clone().unwrap_or_default();
        let logo = watched.media.logo.clone().unwrap_or_default();
        if !send(Event::State(Box::new(Ok(watched)))) {
            return false;
        }
        // The picture behind everything first: it is what the page is made of.
        // A cover and a backdrop are the same shape here.
        let backdrop = self
            .roles
            .iter()
            .any(|role| matches!(role, ArtRole::Cover | ArtRole::Backdrop));
        if backdrop {
            if !art.is_empty() && art != self.art_sent && current() {
                let pixels = backend
                    .artwork(&art)
                    .ok()
                    .and_then(|bytes| activity_art::decode(&bytes, activity_art::Shape::Backdrop));
                self.art_sent = art.clone();
                if !send(Event::Art(art, pixels)) {
                    return false;
                }
            } else if art.is_empty() {
                self.art_sent.clear();
            }
        }
        if self.roles.contains(&ArtRole::Logo) {
            if !logo.is_empty() && logo != self.logo_sent && current() {
                let pixels = backend
                    .artwork(&logo)
                    .ok()
                    .and_then(|bytes| activity_art::decode(&bytes, activity_art::Shape::Logo));
                self.logo_sent = logo.clone();
                return send(Event::Logo(logo, pixels));
            }
            if logo.is_empty() {
                self.logo_sent.clear();
            }
        }
        true
    }
}
/// The speaker states the player screen's pictures are taken from, as the
/// controller sees them. A backend's own tests hold the same states in the
/// device's terms and check that they read as these, so the pictures stand for
/// that backend too.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    /// The Lounge speaker playing the second track of an album from a playlist.
    pub fn playing() -> Media {
        Media {
            item: None,
            state: PlayState::Playing,
            title: Some("Weird Fishes / Arpeggi".into()),
            artist: Some("Radiohead".into()),
            album: Some("In Rainbows".into()),
            subtitle: Some("Evening".into()),
            source: Some("Apple Music".into()),
            duration_ms: Some(318_000),
            position_ms: Some(74_210),
            rate_percent: 100,
            art: Some("http://192.0.2.9:1400/getaa?s=1&u=weird-fishes".into()),
            logo: None,
            can: Can {
                next: true,
                previous: true,
                seek: true,
            },
            modes: Modes::default(),
            next: Some(NextItem {
                title: "All I Need".into(),
                detail: "Radiohead · In Rainbows".into(),
            }),
            follows: None,
            device_name: Some("Lounge".into()),
        }
    }
    pub fn paused() -> Media {
        Media {
            state: PlayState::Paused,
            rate_percent: 0,
            ..playing()
        }
    }
    /// The same speaker as a member of the Kitchen's group.
    pub fn following(media: Media) -> Media {
        Media {
            follows: Some("Kitchen".into()),
            can: Can {
                seek: false,
                ..media.can
            },
            ..media
        }
    }
    /// A station: one item without an end and nothing after it.
    pub fn radio() -> Media {
        Media {
            title: Some("BBC Radio 6 Music".into()),
            artist: None,
            album: None,
            subtitle: Some("BBC Radio 6 Music".into()),
            duration_ms: None,
            art: Some("http://192.0.2.9:1400/getaa?s=1&u=6music".into()),
            can: Can {
                seek: false,
                ..paused().can
            },
            next: None,
            ..paused()
        }
    }
    /// The TV input: a name and a kind, no item and no clock.
    pub fn tv_input() -> Media {
        Media {
            state: PlayState::Paused,
            title: Some("TV".into()),
            subtitle: Some("linein.hometheater".into()),
            ..idle()
        }
    }
    pub fn idle() -> Media {
        Media {
            state: PlayState::Idle,
            can: Can {
                seek: false,
                ..paused().can
            },
            device_name: Some("Lounge".into()),
            ..Media::default()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::fixtures::{following, idle, paused, playing, radio, tv_input};
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
        app.set_player_sheet_keys(true);
        app.set_player_sheets(ModelRc::new(VecModel::from(sheet_labels(&Player::music()))));
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
    /// The speaker the pictures are taken of: a backend whose answers the test
    /// chooses. The rig stands where the worker thread stands and hands every
    /// request the screen makes to the real [`Worker`], on this thread, so a
    /// picture never waits on another thread.
    #[derive(Clone)]
    struct Speaker {
        /// What the speaker reports when it is read.
        now: Result<Media, Failure>,
        /// What the next command comes back with.
        answer: Result<Done, Failure>,
        sources: Result<Vec<Choice>, Failure>,
        /// The rows of each declared list.
        lists: std::collections::HashMap<String, Vec<Choice>>,
        /// Whether this device can be reached twice at once, which is what
        /// puts keys on a lane of their own.
        key_lane: bool,
        /// What the artwork handle decodes to.
        picture: Vec<u8>,
        /// Whether the speaker answers when the screen connects to it.
        connect: Result<(), Failure>,
        /// What reached the speaker, in order, in plain words.
        sent: Vec<String>,
    }
    struct Fake(Arc<std::sync::Mutex<Speaker>>);
    impl Backend for Fake {
        fn open(&mut self) -> Result<(), Failure> {
            self.0.lock().unwrap().connect.clone()
        }
        fn close(&mut self) {}
        fn watch(&mut self, after: u64, _wait: Duration) -> Result<Watched, Failure> {
            let mut speaker = self.0.lock().unwrap();
            speaker.sent.push("read".into());
            speaker.now.clone().map(|media| Watched {
                revision: after + 1,
                age: Duration::ZERO,
                media,
            })
        }
        fn perform(&mut self, op: &Op, _current: &dyn Fn() -> bool) -> Result<Done, Failure> {
            let mut speaker = self.0.lock().unwrap();
            speaker.sent.push(match op {
                Op::PlayPause => "play-pause".into(),
                Op::Next => "next".into(),
                Op::Previous => "previous".into(),
                Op::Seek { position_ms } => format!("seek to {position_ms} ms"),
                Op::StepVolume(delta) => format!("volume {delta:+}"),
                Op::ToggleMute => "mute".into(),
                Op::Source { title, .. } => format!("source {title}"),
                Op::Modes(change) => format!(
                    "modes shuffle {:?} repeat {:?} repeat-one {:?} crossfade {:?}",
                    change.shuffle, change.repeat, change.repeat_one, change.crossfade
                ),
                Op::SeekBy { delta_ms } => format!("seek by {delta_ms:+} ms"),
                Op::Choose { list, id } => format!("choose {id} from {list}"),
                Op::Key { function, phase } => {
                    format!("key {} {phase:?}", function.id()).to_lowercase()
                }
            });
            std::mem::replace(&mut speaker.answer, Ok(Done::Nothing))
        }
        fn sources(&mut self) -> Result<Vec<Choice>, Failure> {
            let mut speaker = self.0.lock().unwrap();
            speaker.sent.push("sources".into());
            speaker.sources.clone()
        }
        fn list(&mut self, list: &str) -> Result<Vec<Choice>, Failure> {
            let mut speaker = self.0.lock().unwrap();
            speaker.sent.push(format!("list {list}"));
            Ok(speaker.lists.get(list).cloned().unwrap_or_default())
        }
        fn artwork(&mut self, art: &str) -> Result<Vec<u8>, Failure> {
            let mut speaker = self.0.lock().unwrap();
            speaker.sent.push(format!("artwork {art}"));
            Ok(speaker.picture.clone())
        }
        fn key_lane(&mut self) -> Option<Box<dyn Backend>> {
            self.0
                .lock()
                .unwrap()
                .key_lane
                .then(|| Box::new(Fake(self.0.clone())) as Box<dyn Backend>)
        }
    }
    struct Rig {
        controller: Controller,
        worker: Worker,
        lane: KeyLane,
        requests: mpsc::Receiver<(u64, Request)>,
        presses: mpsc::Receiver<(u64, KeyRequest)>,
        events: mpsc::SyncSender<(u64, Event)>,
        speaker: Arc<std::sync::Mutex<Speaker>>,
        /// Whether the device hands the key lane a second way to reach it.
        key_lane: bool,
        picture: Vec<u8>,
        now: Result<Media, Failure>,
        answer: Result<Done, Failure>,
        sources: Result<Vec<Choice>, Failure>,
        lists: std::collections::HashMap<String, Vec<Choice>>,
        connect: Result<(), Failure>,
        sent: Vec<String>,
    }
    impl Rig {
        fn new() -> Self {
            let (tx, requests) = mpsc::sync_channel(16);
            let (keys, presses) = mpsc::sync_channel(KEY_LANE);
            let (events, rx) = mpsc::sync_channel(8);
            let controller = Controller {
                tx,
                keys,
                key_lane: false,
                held_key: None,
                rx,
                drawn: false,
                active: Arc::new(AtomicU64::new(0)),
                generation: 0,
                target: None,
                media: None,
                at: Instant::now(),
                tick: Instant::now(),
                refresh_at: Instant::now(),
                busy: false,
                message_until: None,
                volume_until: None,
                art_key: String::new(),
                logo_key: String::new(),
                sources: Vec::new(),
                lists: std::collections::HashMap::new(),
                selected: 3,
                views: std::collections::HashMap::new(),
            };
            let speaker = Speaker {
                now: Err(Failure::Unreachable),
                answer: Ok(Done::Nothing),
                sources: Ok(Vec::new()),
                lists: std::collections::HashMap::new(),
                key_lane: false,
                picture: cover(),
                connect: Ok(()),
                sent: Vec::new(),
            };
            Self {
                controller,
                worker: Worker::default(),
                lane: KeyLane::default(),
                requests,
                presses,
                events,
                key_lane: speaker.key_lane,
                picture: speaker.picture.clone(),
                now: speaker.now.clone(),
                answer: speaker.answer.clone(),
                sources: speaker.sources.clone(),
                lists: std::collections::HashMap::new(),
                connect: speaker.connect.clone(),
                sent: Vec::new(),
                speaker: Arc::new(std::sync::Mutex::new(speaker)),
            }
        }
        fn backend(&self) -> Box<dyn Backend> {
            {
                let mut speaker = self.speaker.lock().unwrap();
                speaker.key_lane = self.key_lane;
                speaker.picture = self.picture.clone();
            }
            Box::new(Fake(self.speaker.clone()))
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
                match &request {
                    Request::Open { known_art, .. } => {
                        self.sent.push(format!("open, showing art {known_art:?}"))
                    }
                    Request::Reopen => self.sent.push("open, showing art \"\"".into()),
                    Request::Close => self.sent.push("close".into()),
                    _ => {}
                }
                {
                    let mut speaker = self.speaker.lock().unwrap();
                    speaker.now = self.now.clone();
                    if matches!(request, Request::Command(_)) {
                        speaker.answer = std::mem::replace(&mut self.answer, Ok(Done::Nothing));
                    }
                    speaker.sources = self.sources.clone();
                    speaker.lists = self.lists.clone();
                    speaker.connect = self.connect.clone();
                }
                assert!(self.worker.handle(
                    generation,
                    request,
                    &self.events,
                    &self.controller.active
                ));
                self.sent.append(&mut self.speaker.lock().unwrap().sent);
                self.controller.poll(app);
            }
            self.keys(app);
            self.hold();
            self.controller.poll(app);
        }
        /// Everything waiting on the key lane, through the real [`KeyLane`] on
        /// this thread.
        fn keys(&mut self, app: &App) {
            while let Ok((generation, request)) = self.presses.try_recv() {
                if generation != self.controller.generation {
                    continue;
                }
                {
                    let mut speaker = self.speaker.lock().unwrap();
                    speaker.connect = self.connect.clone();
                    speaker.answer = Ok(Done::Nothing);
                }
                assert!(self.lane.handle(
                    generation,
                    request,
                    &self.events,
                    &self.controller.active
                ));
                self.sent.append(&mut self.speaker.lock().unwrap().sent);
            }
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
    /// The player screen through the controller, picture by picture, from one
    /// fixed speaker behind a fake backend. A second backend is held to the
    /// same pictures by reading the same speaker states as [`fixtures`].
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
                std::fs::create_dir_all(&dir).unwrap();
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
            label: "Sonos".into(),
            player: Player::music(),
            config: None,
        };
        let mut rig = Rig::new();
        let rig = &mut rig;

        // Opening: the waiting screen, then the speaker's first answer.
        rig.controller.open(&app, target.clone(), rig.backend());
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
        // And the play button itself selected. Its edge is the colour that
        // reads *on* the accent it is filled with - a dark ring on a light
        // disc - which no other picture in this set shows.
        rig.act(&app, "Input.Right", 0.);
        picture(rig, "05a-play-selected");
        rig.act(&app, "Input.Left", 0.);

        // Sources: asked for once, listed, and one started.
        rig.sources = Ok(vec![
            Choice {
                id: "tv".into(),
                title: "TV".into(),
                detail: "This player".into(),
                current: false,
            },
            Choice {
                id: "favorite.4".into(),
                title: "Morning Jazz".into(),
                detail: "Apple Music".into(),
                current: false,
            },
            Choice {
                id: "playlist.0".into(),
                title: "Evening".into(),
                detail: "Sonos playlist · 12 tracks".into(),
                current: false,
            },
        ]);
        rig.act(&app, "Input.Down", 0.);
        rig.controller.action(&app, "Input.Select", 0., false);
        picture(rig, "06-sources-finding");
        rig.pump(&app);
        picture(rig, "07-sources");
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
            let modes = &mut state.modes;
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
        state.next = None;
        rig.now = Ok(state.clone());
        rig.act(&app, "subtitles", 0.);
        rig.refresh(&app);
        picture(rig, "14-up-next-empty");
        rig.act(&app, "Input.Back", 0.);

        // Volume and mute: the shared card, then a sentence.
        rig.answer = Ok(Done::Volume(23));
        rig.act(&app, "volume", 5.);
        picture(rig, "15-volume-card");
        rig.expire(&app);
        rig.answer = Ok(Done::Muted(true));
        rig.act(&app, "mute", 0.);
        picture(rig, "16-muted");
        rig.expire(&app);

        // A command the speaker refuses, in the screen's words.
        rig.answer = Err(Failure::NothingToPlay);
        rig.act(&app, "previous", 0.);
        picture(rig, "17-refused");
        rig.expire(&app);

        // Leaving and coming straight back shows the same screen at once.
        rig.controller.close(&app);
        rig.pump(&app);
        app.set_player_shown(false);
        rig.controller.open(&app, target.clone(), rig.backend());
        picture(rig, "18-reopened-before-the-speaker-answers");
        rig.pump(&app);

        // A speaker that follows another one.
        rig.now = Ok(following(playing()));
        rig.refresh(&app);
        picture(rig, "19-follows-kitchen");
        rig.now = Ok(following(paused()));
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
        rig.now = Ok(radio());
        rig.refresh(&app);
        picture(rig, "22-radio");
        rig.now = Ok(tv_input());
        rig.refresh(&app);
        picture(rig, "23-tv-input");
        rig.now = Ok(idle());
        rig.refresh(&app);
        picture(rig, "24-idle");
        rig.act(&app, "play", 0.);

        // A read that fails says so and the next one recovers by itself.
        rig.now = Err(Failure::Unreachable);
        rig.refresh(&app);
        picture(rig, "25-read-cannot-reach");
        rig.now = Ok(paused());
        rig.refresh(&app);
        // A command that cannot reach the speaker drops the connection: the
        // next read says so, a command says so, and Reconnect asks again,
        // first in vain.
        rig.answer = Err(Failure::Unreachable);
        rig.act(&app, "play", 0.);
        picture(rig, "26-command-cannot-reach");
        rig.expire(&app);
        rig.refresh(&app);
        rig.act(&app, "volume", -5.);
        picture(rig, "27-not-connected");
        rig.expire(&app);
        rig.connect = Err(Failure::Unreachable);
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
}
