//! Sonos from the room list. While a speaker row is highlighted, the volume keys
//! set its volume, Mute toggles mute, the channel keys skip tracks and Menu
//! opens a source picker, all without leaving the room. Every network call runs
//! on one worker thread; the UI thread only ever sees the feedback card, the
//! same large card brightness and scene changes use.
use crate::{App, ChoiceItem};
use couch_sonos::{Client, Source, SourceId};
use slint::{ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    net::Ipv4Addr,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

/// The chooser title the source picker is shown under; `on_chosen` routes by it.
pub const CHOOSER_TITLE: &str = "SONOS SOURCES";
/// One press of a volume key. Holding repeats, and repeats are summed into one
/// write, so a hold is a smooth ramp rather than a queue of tiny requests.
const VOLUME_STEP: i8 = 2;
/// The most a coalesced hold may move the volume in one request.
const VOLUME_BURST: i8 = 20;

/// How long each card stays up once nothing newer replaces it.
const VOLUME_CARD: Duration = Duration::from_millis(1500);
const CARD: Duration = Duration::from_secs(2);
const ERROR_CARD: Duration = Duration::from_secs(4);
const SEARCH_CARD: Duration = Duration::from_secs(8);

/// One feedback card: the bold line, the caption above it, and a volume
/// level when there is a meter to draw.
struct Card {
    name: String,
    caption: String,
    volume: Option<u8>,
}
enum Input {
    /// A physical key on a highlighted row: (row, command, repeat).
    Key(usize, String, bool),
    Menu,
    Choose(usize),
}
#[derive(Clone, Debug, PartialEq)]
struct Target {
    id: String,
    name: String,
    host: Ipv4Addr,
}
enum Op {
    Volume(i8),
    ToggleMute,
    Skip(bool),
    Sources,
    Select(SourceId, String),
}
struct Request {
    generation: u64,
    target: Target,
    op: Op,
}
enum Answer {
    Card(Card),
    Sources(Target, Vec<Source>),
}
pub struct Controller {
    input: Rc<RefCell<VecDeque<Input>>>,
    physical_repeat: Rc<Cell<bool>>,
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<(u64, Result<Answer, Card>)>,
    active: Arc<AtomicU64>,
    generation: u64,
    room: String,
    /// When the card on screen goes away by itself.
    card_until: Option<Instant>,
    /// The sources the open chooser lists, in row order.
    offered: Option<(Target, Vec<Source>)>,
    /// The chooser was asked for and has not been seen open yet.
    chooser_pending: bool,
}
impl Controller {
    pub fn install(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let physical_repeat = Rc::new(Cell::new(false));
        let (queue, repeat) = (input.clone(), physical_repeat.clone());
        app.on_room_media(move |index, command| {
            if index >= 0 {
                queue.borrow_mut().push_back(Input::Key(
                    index as usize,
                    command.to_string(),
                    repeat.get(),
                ));
            }
        });
        let (tx, requests) = mpsc::channel::<Request>();
        let (events, rx) = mpsc::channel();
        let active = Arc::new(AtomicU64::new(0));
        let worker_active = active.clone();
        std::thread::spawn(move || worker(requests, events, worker_active));
        Self {
            input,
            physical_repeat,
            tx,
            rx,
            active,
            generation: 0,
            room: String::new(),
            card_until: None,
            offered: None,
            chooser_pending: false,
        }
    }
    fn show(&mut self, app: &App, card: Card, for_: Duration) {
        app.set_feedback_enabled(true);
        app.set_sonos_feedback_name(card.name.as_str().into());
        app.set_sonos_feedback_caption(card.caption.as_str().into());
        app.set_sonos_feedback_meter(card.volume.is_some());
        app.set_sonos_feedback_volume(i32::from(card.volume.unwrap_or(0)));
        app.set_sonos_feedback_shown(true);
        self.card_until = Some(Instant::now() + for_);
    }
    fn hide(&mut self, app: &App) {
        self.card_until = None;
        app.set_sonos_feedback_shown(false);
    }
    /// Slint synthesizes a release after each press; keep the physical repeat
    /// flag only for the duration of this dispatch, like the other controllers.
    pub fn physical_input(&self, repeat: bool, dispatch: impl FnOnce()) {
        let previous = self.physical_repeat.replace(repeat);
        dispatch();
        self.physical_repeat.set(previous);
    }
    /// The menu key, pressed while the room list is up.
    pub fn menu(&self) {
        self.input.borrow_mut().push_back(Input::Menu);
    }
    /// For `on_chosen`: the row picked in the source chooser.
    pub fn chooser(&self) -> impl Fn(usize) + 'static {
        let input = self.input.clone();
        move |index| input.borrow_mut().push_back(Input::Choose(index))
    }
    fn invalidate(&mut self) {
        self.generation += 1;
        self.active.store(self.generation, Ordering::SeqCst);
        self.input.borrow_mut().clear();
        self.offered = None;
        self.chooser_pending = false;
    }
    fn send(&self, target: Target, op: Op) {
        let _ = self.tx.send(Request {
            generation: self.generation,
            target,
            op,
        });
    }
    /// Returns true when the source chooser should be slid in: its rows and
    /// title are set by then.
    pub fn poll(&mut self, app: &App) -> bool {
        let mut open_chooser = false;
        let room = app.get_light_room_id().to_string();
        if !app.get_light_shown() || room != self.room {
            self.room = room;
            if self.generation != 0 || !self.input.borrow().is_empty() || self.offered.is_some() {
                self.invalidate();
            }
            if self.card_until.is_some() {
                self.hide(app);
            }
            if !app.get_light_shown() {
                return false;
            }
        }
        if self.card_until.is_some_and(|until| Instant::now() >= until) {
            self.hide(app);
        }
        if self.chooser_pending && app.get_chooser_shown() {
            self.chooser_pending = false;
            // The list is on screen; the "finding" card has done its job.
            self.hide(app);
        } else if self.offered.is_some()
            && !self.chooser_pending
            && (!app.get_chooser_shown() || app.get_chooser_title() != CHOOSER_TITLE)
        {
            // Closed without a pick, or another list took the chooser over.
            self.offered = None;
        }
        loop {
            let Some(input) = self.input.borrow_mut().pop_front() else {
                break;
            };
            match input {
                Input::Key(index, command, repeat) => {
                    let Some(target) = target(app, index) else {
                        continue;
                    };
                    let Some(op) = operation(&command, repeat) else {
                        continue;
                    };
                    self.send(target, op);
                }
                Input::Menu => {
                    let index = app.get_light_index();
                    if index < 0 || app.get_chooser_shown() {
                        continue;
                    }
                    let Some(target) = target(app, index as usize) else {
                        continue;
                    };
                    let name = target.name.clone();
                    self.send(target, Op::Sources);
                    self.show(
                        app,
                        Card {
                            name,
                            caption: "Finding Sonos sources…".into(),
                            volume: None,
                        },
                        SEARCH_CARD,
                    );
                }
                Input::Choose(index) => {
                    let Some((target, sources)) = self.offered.take() else {
                        continue;
                    };
                    let Some(source) = sources.get(index) else {
                        continue;
                    };
                    self.show(
                        app,
                        Card {
                            name: source.name.clone(),
                            caption: format!("Starting on {}…", target.name),
                            volume: None,
                        },
                        SEARCH_CARD,
                    );
                    self.send(target, Op::Select(source.id.clone(), source.name.clone()));
                }
            }
        }
        while let Ok((generation, answer)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            match answer {
                Ok(Answer::Card(card)) => {
                    let for_ = if card.volume.is_some() {
                        VOLUME_CARD
                    } else {
                        CARD
                    };
                    self.show(app, card, for_);
                }
                Ok(Answer::Sources(target, sources)) => {
                    if sources.is_empty() {
                        self.show(
                            app,
                            Card {
                                name: "Add favourites in the Sonos app".into(),
                                caption: format!("No sources for {}", target.name),
                                volume: None,
                            },
                            ERROR_CARD,
                        );
                        continue;
                    }
                    // The room must still be up and own the screen; a list
                    // arriving over another page is dropped, not shown later.
                    if !app.get_light_shown()
                        || app.get_chooser_shown()
                        || app.get_tv_shown()
                        || app.get_player_shown()
                        || app.get_thermostat_shown()
                        || app.get_camera_shown()
                        || app.get_settings_shown()
                        || app.get_keyboard_shown()
                        || app.get_pair_shown()
                    {
                        continue;
                    }
                    app.set_chooser_items(ModelRc::new(VecModel::from(
                        sources
                            .iter()
                            .map(|s| ChoiceItem {
                                title: s.name.as_str().into(),
                                detail: s.detail.as_str().into(),
                                active: false,
                                light: false,
                                media: false,
                                power_known: false,
                                icon: slint::Image::default(),
                            })
                            .collect::<Vec<_>>(),
                    )));
                    app.set_chooser_title(CHOOSER_TITLE.into());
                    app.set_chooser_index(0);
                    self.offered = Some((target, sources));
                    self.chooser_pending = true;
                    open_chooser = true;
                }
                Err(card) => self.show(app, card, ERROR_CARD),
            }
        }
        open_chooser
    }
}
/// The Sonos speaker on this row of the open room, if that is what it is. Rows
/// follow the room's device order, as the light controller already relies on.
fn target(app: &App, index: usize) -> Option<Target> {
    let config = crate::connections::config()?;
    let room = config.room(&couch_model::Id::new(app.get_light_room_id().as_str()))?;
    let device = room.devices.get(index)?;
    match config.resolve_integration(&device.integration)? {
        couch_model::Integration::Sonos { host } => Some(Target {
            id: device.id.to_string(),
            name: device.name.clone(),
            host: host.parse().ok()?,
        }),
        _ => None,
    }
}
/// Map a key command to work. Skips never repeat: a held channel key must not
/// run through an album.
fn operation(command: &str, repeat: bool) -> Option<Op> {
    Some(match command {
        "volume-up" => Op::Volume(VOLUME_STEP),
        "volume-down" => Op::Volume(-VOLUME_STEP),
        "mute" if !repeat => Op::ToggleMute,
        "next" if !repeat => Op::Skip(true),
        "previous" if !repeat => Op::Skip(false),
        _ => return None,
    })
}
fn worker(
    rx: mpsc::Receiver<Request>,
    reply: mpsc::Sender<(u64, Result<Answer, Card>)>,
    active: Arc<AtomicU64>,
) {
    let mut cached: Option<(Ipv4Addr, Client)> = None;
    let mut carry: Option<Request> = None;
    loop {
        let mut request = match carry.take() {
            Some(request) => request,
            None => match rx.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };
        if let Op::Volume(mut delta) = request.op {
            // Sum the presses that piled up behind this one for the same
            // speaker; anything else waits its turn.
            while let Ok(next) = rx.try_recv() {
                match next.op {
                    Op::Volume(step)
                        if next.target == request.target
                            && next.generation == request.generation =>
                    {
                        delta = (delta + step).clamp(-VOLUME_BURST, VOLUME_BURST);
                    }
                    _ => {
                        carry = Some(next);
                        break;
                    }
                }
            }
            request.op = Op::Volume(delta);
        }
        if active.load(Ordering::SeqCst) != request.generation {
            continue;
        }
        let current = || active.load(Ordering::SeqCst) == request.generation;
        let result = match perform(&mut cached, &request, &current) {
            Ok(None) => continue,
            Ok(Some(answer)) => Ok(answer),
            Err(card) => Err(card),
        };
        if reply.send((request.generation, result)).is_err() {
            return;
        }
    }
}
fn perform(
    cached: &mut Option<(Ipv4Addr, Client)>,
    request: &Request,
    current: &dyn Fn() -> bool,
) -> Result<Option<Answer>, Card> {
    let host = request.target.host;
    let name = &request.target.name;
    if !matches!(cached, Some((cached_host, _)) if *cached_host == host) {
        *cached = None;
        let client = Client::connect(host).map_err(|e| describe(name, e))?;
        *cached = Some((host, client));
    }
    let Some((_, client)) = cached.as_ref() else {
        unreachable!("client cached above")
    };
    let result = run(client, request, current);
    if matches!(result, Err(couch_sonos::Error::Transport)) {
        // The kept connection is what failed; the next press reconnects.
        *cached = None;
    }
    match result {
        Ok(answer) => Ok(Some(answer)),
        // Expired before dispatch: nothing happened, so nothing to show.
        Err(couch_sonos::Error::Cancelled) => Ok(None),
        Err(error) => Err(describe(name, error)),
    }
}
fn run(
    client: &Client,
    request: &Request,
    current: &dyn Fn() -> bool,
) -> couch_sonos::Result<Answer> {
    let name = request.target.name.clone();
    let card = |caption: String, volume: Option<u8>| {
        Answer::Card(Card {
            name: name.clone(),
            caption,
            volume,
        })
    };
    Ok(match &request.op {
        Op::Volume(delta) => {
            if !current() {
                return Err(couch_sonos::Error::Cancelled);
            }
            client.nudge_volume(*delta)?;
            card("Volume".into(), Some(client.volume()?))
        }
        Op::ToggleMute => {
            let muted = client.muted()?;
            if !current() {
                return Err(couch_sonos::Error::Cancelled);
            }
            client.set_muted(!muted)?;
            card(if muted { "Unmuted" } else { "Muted" }.into(), None)
        }
        Op::Skip(next) => {
            client.command_if_current(if *next { "next" } else { "previous" }, current)?;
            card(
                if *next {
                    "Next track"
                } else {
                    "Previous track"
                }
                .into(),
                None,
            )
        }
        Op::Sources => {
            if !current() {
                return Err(couch_sonos::Error::Cancelled);
            }
            Answer::Sources(request.target.clone(), client.sources()?)
        }
        Op::Select(source, label) => {
            client.select_source_if_current(source, current)?;
            Answer::Card(Card {
                name: label.clone(),
                caption: format!("Playing on {name}"),
                volume: None,
            })
        }
    })
}
/// Card-sized wording: the problem in bold, the speaker as its caption. A
/// group member's playback lives with its coordinator; everything else is the
/// client's own sentence.
fn describe(name: &str, error: couch_sonos::Error) -> Card {
    let (line, caption) = match error {
        couch_sonos::Error::NotCoordinator { coordinator } => (
            format!("Control playback on {coordinator}"),
            format!("{name} is grouped"),
        ),
        couch_sonos::Error::Transport => ("Cannot reach the speaker".to_owned(), name.to_owned()),
        couch_sonos::Error::Api(code) if code == "ERROR_PLAYBACK_NO_CONTENT" => (
            "Sonos found nothing to play there".to_owned(),
            name.to_owned(),
        ),
        other => (other.to_string(), name.to_owned()),
    };
    Card {
        name: line,
        caption,
        volume: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn volume_keys_repeat_but_skips_and_mute_do_not() {
        assert!(matches!(
            operation("volume-up", true),
            Some(Op::Volume(VOLUME_STEP))
        ));
        assert!(
            matches!(operation("volume-down", false), Some(Op::Volume(d)) if d == -VOLUME_STEP)
        );
        assert!(matches!(operation("next", false), Some(Op::Skip(true))));
        assert!(matches!(
            operation("previous", false),
            Some(Op::Skip(false))
        ));
        assert!(matches!(operation("mute", false), Some(Op::ToggleMute)));
        assert!(operation("next", true).is_none());
        assert!(operation("previous", true).is_none());
        assert!(operation("mute", true).is_none());
        assert!(operation("power", false).is_none());
    }
    #[test]
    fn a_held_volume_key_becomes_one_bounded_write_and_other_work_keeps_its_turn() {
        let (tx, rx) = mpsc::channel::<Request>();
        let active = Arc::new(AtomicU64::new(7));
        let target = Target {
            id: "a".into(),
            name: "Kitchen".into(),
            host: Ipv4Addr::new(192, 0, 2, 1),
        };
        // Fifteen repeats of one held key, then a mute press behind them.
        for _ in 0..15 {
            tx.send(Request {
                generation: 7,
                target: target.clone(),
                op: Op::Volume(VOLUME_STEP),
            })
            .unwrap();
        }
        tx.send(Request {
            generation: 7,
            target: target.clone(),
            op: Op::ToggleMute,
        })
        .unwrap();
        drop(tx);
        // Drive the coalescing loop by hand: the same code the worker runs,
        // minus the network, so this test needs no speaker.
        let mut carry: Option<Request> = None;
        let mut seen = Vec::new();
        loop {
            let mut request = match carry.take() {
                Some(r) => r,
                None => match rx.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                },
            };
            if let Op::Volume(mut delta) = request.op {
                while let Ok(next) = rx.try_recv() {
                    match next.op {
                        Op::Volume(step)
                            if next.target == request.target
                                && next.generation == request.generation =>
                        {
                            delta = (delta + step).clamp(-VOLUME_BURST, VOLUME_BURST);
                        }
                        _ => {
                            carry = Some(next);
                            break;
                        }
                    }
                }
                request.op = Op::Volume(delta);
            }
            assert_eq!(active.load(Ordering::SeqCst), request.generation);
            seen.push(match request.op {
                Op::Volume(d) => format!("volume {d}"),
                Op::ToggleMute => "mute".into(),
                _ => "other".into(),
            });
        }
        assert_eq!(seen, ["volume 20", "mute"]);
    }
    #[test]
    fn grouped_members_and_unreachable_speakers_get_card_sized_wording() {
        let grouped = describe(
            "Kitchen",
            couch_sonos::Error::NotCoordinator {
                coordinator: "Living room".into(),
            },
        );
        assert_eq!(
            (
                grouped.name.as_str(),
                grouped.caption.as_str(),
                grouped.volume
            ),
            (
                "Control playback on Living room",
                "Kitchen is grouped",
                None
            )
        );
        let gone = describe("Kitchen", couch_sonos::Error::Transport);
        assert_eq!(
            (gone.name.as_str(), gone.caption.as_str()),
            ("Cannot reach the speaker", "Kitchen")
        );
        let http = describe("Kitchen", couch_sonos::Error::Http(500));
        assert_eq!(
            (http.name.as_str(), http.caption.as_str()),
            ("Sonos HTTP error 500", "Kitchen")
        );
        let empty = describe(
            "Kitchen",
            couch_sonos::Error::Api("ERROR_PLAYBACK_NO_CONTENT".into()),
        );
        assert_eq!(empty.name, "Sonos found nothing to play there");
    }
    #[test]
    fn only_sonos_rows_of_the_open_room_are_targets() {
        let config: couch_model::Config = serde_json::from_value(serde_json::json!({
            "schema_version":1,"connections":[
                {"id":"s","name":"S","provider":{"kind":"sonos","host":"192.0.2.9"}}],
            "rooms":[{"id":"den","name":"Den","devices":[
                {"id":"lamp","name":"Lamp","kind":"light","integration":{"via":"hue","light_id":"1"}},
                {"id":"speaker","name":"Den Sonos","kind":"speaker","integration":{"via":"connection","connection_id":"s"}}]}]
        })).unwrap();
        let room = config.room(&couch_model::Id::new("den")).unwrap();
        let sonos = |index: usize| {
            room.devices.get(index).and_then(|d| {
                match config.resolve_integration(&d.integration)? {
                    couch_model::Integration::Sonos { host } => Some((d.name.clone(), host)),
                    _ => None,
                }
            })
        };
        assert_eq!(sonos(0), None);
        assert_eq!(sonos(1), Some(("Den Sonos".into(), "192.0.2.9".into())));
        assert_eq!(sonos(2), None);
    }
}
