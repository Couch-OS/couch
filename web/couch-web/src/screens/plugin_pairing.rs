//! Protocol 3 (unreleased): pairing a packaged connection with its device.
//!
//! A package never supplies any part of this dialog. It describes a step -
//! press the button, approve on the device, type the code the device is
//! showing - and Couch draws it, with Couch's own words in Couch's own
//! layout. The only thing a package may put on the screen is one line under
//! Couch's headline, and only when it knows something Couch cannot ("the
//! button is on the back").
//!
//! The owner's decisions this file exists to keep:
//!
//! * The headline per prompt is Couch's: "Press the button on the device",
//!   "Approve on the device", "Enter the code shown on the device". The
//!   package's line goes under it when there is one.
//! * A pairing never runs longer than five minutes, whatever a package asks
//!   for, and the time left is shown calmly rather than counted down at
//!   anyone.
//! * A wrong code ends the attempt. "Try again" starts a new pairing rather
//!   than letting somebody guess at the same session.
//! * Forgetting a pairing says what forgetting it really does: Couch throws
//!   its own copy of the key away and cannot tell the device anything.
//!
//! With the protocol 3 switch off no manifest this build accepts may declare
//! `pairing`, so [`Known::pairs`] is false for every connection, none of this
//! is ever drawn, and a packaged connection's page is exactly what it was.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::{json, Value};
use wasm_bindgen::JsCast;

use crate::{api, App};

/// OWNER DECISION: a pairing never runs longer than five minutes. The daemon
/// caps the window too; this is the dialog refusing to draw a countdown
/// longer than the rule even if it is told one.
const LONGEST: u64 = 300;

/// How long to wait before asking a busy remote again, and how many times. A
/// 503 on a pairing call means another call on the same session is still out;
/// nothing was done, so asking again is not asking the device twice.
const BUSY_PAUSE: Duration = Duration::from_millis(400);
const BUSY_TRIES: usize = 12;

/// What forgetting a pairing does, in the owner's own words. Couch can throw
/// its copy of the key away; it cannot tell the device to forget anything.
pub(super) const FORGET_CONFIRM: &str =
    "This removes Couch's copy of the key. The device may still list Couch as paired.";

/// What a session that is no longer on the remote leaves to say.
const INTERRUPTED: &str = "Pairing was interrupted. Start again.";

/// A step neither this page nor any page after it can read.
const UNREADABLE: &str = "Couch could not read what the remote said about this pairing.";

thread_local! {
    /// Where the focus was when the dialog opened, so closing it puts the
    /// focus back on the control that opened it rather than at the top of the
    /// page.
    static RETURNS_TO: RefCell<Option<web_sys::HtmlElement>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// What the remote says
// ---------------------------------------------------------------------------

/// What a package's manifest says about pairing, from `GET /api/integrations`.
/// Absent for every package a shipped build accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub(super) struct Pairing {
    #[serde(default)]
    pub(super) required: bool,
    #[serde(default)]
    pub(super) max_seconds: u16,
}

/// One step of a pairing, exactly as the daemon hands it over.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
enum Step {
    Waiting {
        prompt: Prompt,
        #[serde(default)]
        poll_after_ms: u32,
    },
    Done {
        #[serde(default)]
        summary: String,
        /// What was paired but could not be kept beside the key: settings the
        /// device corrected that the remote could not save. Absent from every
        /// ordinary pairing. The key is stored either way.
        #[serde(default)]
        warning: Option<String>,
    },
    Failed {
        #[serde(default)]
        reason: String,
        #[serde(default)]
        message: Option<String>,
    },
}

/// What the device is waiting for. `message` is the package's one line, and
/// is absent when it had nothing to add.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Prompt {
    PressButton {
        #[serde(default)]
        message: Option<String>,
    },
    ApproveOnDevice {
        #[serde(default)]
        message: Option<String>,
    },
    EnterCode {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        length: u8,
        #[serde(default)]
        alphabet: String,
    },
    /// A prompt from a remote that has learnt one this page has not. Couch
    /// says what it can and keeps waiting rather than throwing away an
    /// attempt it might still finish.
    #[serde(other)]
    Other,
}

/// What the dialog is showing.
#[derive(Clone, Debug, PartialEq)]
enum Shown {
    Waiting {
        prompt: Prompt,
        poll_after_ms: u32,
    },
    Done {
        summary: String,
        warning: Option<String>,
    },
    Failed {
        sentence: String,
        note: Option<String>,
    },
}

/// What the dialog is showing, as far as the focus is concerned. A poll that
/// brings back the prompt that is already up must not move anybody's focus,
/// so this is a memo and every step change is compared against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Code,
    Waiting,
    Done,
    Failed,
}

// ---------------------------------------------------------------------------
// The words
// ---------------------------------------------------------------------------

/// OWNER DECISION: Couch's own headline for each prompt. A package describes
/// a step; it never names it.
pub(super) fn headline(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::PressButton { .. } => "Press the button on the device",
        Prompt::ApproveOnDevice { .. } => "Approve on the device",
        Prompt::EnterCode { .. } => "Enter the code shown on the device",
        Prompt::Other => "Finish pairing on the device",
    }
}

/// The package's one line under the headline, when it sent one worth showing.
pub(super) fn note(prompt: &Prompt) -> Option<String> {
    let message = match prompt {
        Prompt::PressButton { message }
        | Prompt::ApproveOnDevice { message }
        | Prompt::EnterCode { message, .. } => message.as_deref(),
        Prompt::Other => None,
    };
    message
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Couch's sentence for a failure. The package's own words, when it sent any,
/// go under this rather than instead of it: a person needs to be told what
/// happened in words Couch is responsible for.
pub(super) fn failure_sentence(reason: &str) -> &'static str {
    match reason {
        "unreachable" => "Couch could not reach the device.",
        "refused" => "The device refused to pair.",
        "wrong_code" => "That was not the code the device is showing.",
        "timed_out" => "The device did not answer in time.",
        "unsupported" => "This device does not pair this way.",
        _ => "Pairing did not finish.",
    }
}

/// Couch's sentence around a warning a `done` carried. The key was stored;
/// something beside it was not, and the remote sends a clause rather than a
/// sentence. Said calmly: nothing is broken and nothing has to be done again.
pub(super) fn warning_sentence(warning: &str) -> String {
    let warning = warning.trim();
    let stop = if warning.ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    };
    format!("The pairing was saved, but {warning}{stop}")
}

/// Which characters a code may be made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alphabet {
    Digits,
    Hex,
    Alphanumeric,
}

impl Alphabet {
    /// A box for digits gets the number pad; one that can hold letters must
    /// not, or half a keyboard is missing.
    pub(super) fn input_mode(self) -> &'static str {
        match self {
            Alphabet::Digits => "numeric",
            _ => "text",
        }
    }

    /// Letters a device shows are shown in capitals, so the box uses capitals
    /// and asks a phone keyboard for them.
    pub(super) fn capitals(self) -> bool {
        self != Alphabet::Digits
    }
}

/// What the remote called the alphabet. An unknown name is the widest of the
/// three: refusing to accept a character the device is showing is worse than
/// accepting one it is not.
pub(super) fn alphabet(name: &str) -> Alphabet {
    match name {
        "digits" => Alphabet::Digits,
        "hex" => Alphabet::Hex,
        _ => Alphabet::Alphanumeric,
    }
}

/// What the box keeps of what was typed: only characters this code can be
/// made of, in capitals where the device uses them, and never more of them
/// than the device asked for.
pub(super) fn clean_code(raw: &str, length: u8, alphabet: Alphabet) -> String {
    let kept = raw.chars().filter(|c| match alphabet {
        Alphabet::Digits => c.is_ascii_digit(),
        Alphabet::Hex => c.is_ascii_hexdigit(),
        Alphabet::Alphanumeric => c.is_ascii_alphanumeric(),
    });
    let kept: String = if alphabet.capitals() {
        kept.map(|c| c.to_ascii_uppercase()).collect()
    } else {
        kept.collect()
    };
    match usize::from(length) {
        0 => kept,
        limit => kept.chars().take(limit).collect(),
    }
}

/// Whether there is as much code as the device asked for. A device that did
/// not say how long its code is gets anything that is not empty.
pub(super) fn complete(code: &str, length: u8) -> bool {
    match usize::from(length) {
        0 => !code.is_empty(),
        limit => code.chars().count() == limit,
    }
}

/// The time left, calmly. Never a bare number of seconds ticking at anybody.
pub(super) fn countdown(seconds: u64) -> String {
    format!("{}:{:02} left", seconds / 60, seconds % 60)
}

/// When the time left is worth saying out loud. A live region that announces
/// a number every second is a live region nobody can use, so the countdown is
/// read at each whole minute, at thirty seconds and at ten, and never else.
pub(super) fn announce_at(seconds: u64) -> bool {
    seconds == 30 || seconds == 10 || (seconds > 0 && seconds.is_multiple_of(60))
}

// ---------------------------------------------------------------------------
// What this page knows about each connection's pairing
// ---------------------------------------------------------------------------

/// What the remote last said about one connection's pairing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Known {
    /// The installed package describes a way of pairing. False for every
    /// package a shipped build accepts, which is why none of this is ever
    /// seen.
    pub(super) pairs: bool,
    pub(super) required: bool,
    pub(super) paired: bool,
    pub(super) summary: String,
    /// A call on this connection was refused because it is not paired.
    pub(super) unpaired: bool,
}

/// Whether this connection is asking to be paired: the settings say a pairing
/// is required and there is none, or something was refused for the want of
/// one.
pub(super) fn needs(known: &Known) -> bool {
    known.pairs && (known.unpaired || (known.required && !known.paired))
}

/// What the button that starts a pairing says. A device that is already
/// paired is being paired *again*, which is worth saying: the old pairing
/// keeps working until the new one lands.
pub(super) fn pair_label(state: State, connection: &str) -> &'static str {
    if state.of(connection).paired {
        "Pair again"
    } else {
        "Pair"
    }
}

/// The pairing state of every packaged connection this browser has touched,
/// the dialog that is open, and the count that tells a settings card to read
/// itself again.
#[derive(Clone, Copy)]
pub struct State {
    known: RwSignal<BTreeMap<String, Known>>,
    open: RwSignal<Option<Started>>,
    /// Bumped when a pairing finished or was forgotten, so the connection's
    /// settings card reads the remote again instead of showing what it
    /// happened to have.
    pub(super) refreshed: RwSignal<u32>,
}

impl State {
    pub(super) fn new() -> State {
        State {
            known: RwSignal::new(BTreeMap::new()),
            open: RwSignal::new(None),
            refreshed: RwSignal::new(0),
        }
    }

    fn of(&self, connection: &str) -> Known {
        self.known
            .with(|all| all.get(connection).cloned().unwrap_or_default())
    }

    fn edit(&self, connection: &str, change: impl FnOnce(&mut Known)) {
        self.known.update(|all| {
            change(all.entry(connection.to_owned()).or_default());
        });
    }
}

/// What a pairing that has started is: the remote's session, the first step
/// and the window it lives in.
#[derive(Clone, Debug, PartialEq)]
struct Started {
    connection: String,
    base: String,
    session: String,
    step: Value,
    expires_in: u64,
}

/// Record what the installed manifest says about this package.
pub(super) fn declares(state: State, connection: &str, pairing: Option<Pairing>) {
    state.edit(connection, |known| {
        known.pairs = pairing.is_some();
        known.required = pairing.is_some_and(|pairing| pairing.required);
    });
}

/// Record what a settings reply said. The three keys are only there for a
/// package that pairs, so their absence is not "not paired" - it is "this
/// package does not pair", which is every package today.
pub(super) fn from_settings(state: State, connection: &str, view: &Value) {
    let Some(required) = view["pairing"]["required"].as_bool() else {
        return;
    };
    state.edit(connection, |known| {
        known.pairs = true;
        known.required = required;
        known.paired = view["paired"].as_bool().unwrap_or(false);
        known.summary = view["summary"].as_str().unwrap_or_default().to_owned();
        if known.paired {
            known.unpaired = false;
        }
    });
}

/// Record that a call on this connection was refused for the want of a
/// pairing. Every plugin call can answer this, which is the point: a key the
/// device revoked shows up as a banner wherever the connection is used, not
/// as an error nobody can act on.
///
/// Whether Couch still holds a key is left exactly as it was. A refusal says
/// the device will not talk, not that the remote has thrown anything away, so
/// a connection that was paired is offered "Pair again" rather than told it
/// was never paired at all.
pub(super) fn noticed(state: State, connection: &str, error: &api::ApiError) {
    if error.code.as_deref() == Some("unpaired") {
        state.edit(connection, |known| {
            known.pairs = true;
            known.required = true;
            known.unpaired = true;
        });
    }
}

// ---------------------------------------------------------------------------
// The banner, and the pairing part of a settings card
// ---------------------------------------------------------------------------

/// "This connection needs pairing", wherever the connection shows up: its own
/// page, and the panel of any device behind it.
pub(super) fn banner(app: App, state: State, connection: &str, base: String) -> AnyView {
    let id = connection.to_owned();
    let asking = id.clone();
    let known = Memo::new(move |_| state.of(&asking));
    let message = RwSignal::new(String::new());
    let base = StoredValue::new(base);
    view! {<Show when=move ||known.with(needs)>
        <div class="notice small needs-pairing" role="status">
            <span>"This connection needs pairing"</span>
            <button class="primary" type="button" on:click={
                let id = id.clone();
                move |_| {
                    message.set(String::new());
                    begin(app, state, id.clone(), base.get_value(), move |error: api::ApiError| message.set(error.message));
                }
            }>{move ||if known.with(|known|known.paired) {"Pair again"} else {"Pair"}}</button>
        </div>
        <p role="alert">{move ||message.get()}</p>
    </Show>}
    .into_any()
}

/// The pairing part of a packaged connection's settings card: what the
/// pairing is, and the way to forget it. The button that starts one lives
/// with the settings form, because what is typed there is saved before a
/// pairing is asked for.
pub(super) fn settings_card(app: App, state: State, connection: &str, base: String) -> AnyView {
    let id = connection.to_owned();
    let known = Memo::new(move |_| state.of(&id));
    let connection = connection.to_owned();
    let base = StoredValue::new(base);
    let asking = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let held = StoredValue::new(connection);
    let forget = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let connection = held.get_value();
        spawn_local(async move {
            match api::ha("DELETE", &format!("{}/credential", base.get_value()), None).await {
                Ok(_) => {
                    state.edit(&connection, |known| {
                        known.paired = false;
                        known.unpaired = false;
                        known.summary = String::new();
                    });
                    state.refreshed.update(|count| *count += 1);
                    asking.set(false);
                    message.set("Couch has forgotten this pairing.".into());
                }
                Err(error) => {
                    if error.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(error.message);
                }
            }
            busy.set(false);
        });
    };
    view! {<Show when=move ||known.with(|known|known.pairs)>
        <div class="pairing-state">
            <p class:pairing-summary=move ||known.with(|known|known.paired)>{move ||{
                let known = known.get();
                if !known.paired {
                    "Not paired yet.".to_string()
                } else if known.summary.is_empty() {
                    "Paired.".to_string()
                } else {
                    known.summary
                }
            }}</p>
            <Show when=move ||known.with(|known|known.paired)>
                <div class="forget-pairing">
                    <Show
                        when=move ||asking.get()
                        fallback=move ||view!{<button class="danger" type="button" on:click=move |_|{message.set(String::new());asking.set(true)}>"Forget pairing"</button>}
                    >
                        <p role="alert">{FORGET_CONFIRM}</p>
                        <div class="actions">
                            <button class="danger" type="button" disabled=move ||busy.get() on:click=forget>"Forget pairing"</button>
                            <button class="ghost" type="button" on:click=move |_|asking.set(false)>"Keep it"</button>
                        </div>
                    </Show>
                </div>
            </Show>
            <p role="status">{move ||message.get()}</p>
        </div>
    </Show>}
    .into_any()
}

// ---------------------------------------------------------------------------
// Starting one
// ---------------------------------------------------------------------------

/// Ask the remote to start a pairing and open the dialog on the first step it
/// answers with.
///
/// The settings a pairing is started with are validated by the daemon and not
/// saved, so whatever was typed is saved through the settings route first and
/// this starts with what the connection already has. A refusal never opens a
/// dialog: it goes back to whoever asked, which on the settings card is the
/// form that can mark the setting a reason names.
pub(super) fn begin(
    app: App,
    state: State,
    connection: String,
    base: String,
    report: impl Fn(api::ApiError) + 'static,
) {
    remember_focus();
    spawn_local(async move {
        match api::ha("POST", &format!("{base}/pair"), None).await {
            Ok(value) => {
                let session = value["session"].as_str().unwrap_or_default().to_owned();
                if session.is_empty() {
                    report(api::ApiError {
                        message: "The remote did not say which pairing this is.".into(),
                        reason: None,
                        status: 0,
                        code: None,
                        unauthorized: false,
                        stale: false,
                        busy: false,
                    });
                    return;
                }
                state.open.set(Some(Started {
                    connection,
                    base,
                    session,
                    step: value["step"].clone(),
                    expires_in: value["expires_in"].as_u64().unwrap_or(0).min(LONGEST),
                }));
            }
            Err(error) => {
                if error.unauthorized {
                    app.paired.set(Some(false));
                }
                noticed(state, &connection, &error);
                report(error);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// The dialog
// ---------------------------------------------------------------------------

/// The one pairing dialog there is, mounted at the root so it is not inside
/// the fieldset the app disables while a write is out, and so starting a
/// pairing from a device panel and from a connection page reach the same one.
pub fn dialog(app: App) -> AnyView {
    let state = expect_context::<State>();
    view! {{move ||state.open.get().map(|started|modal(app, state, started))}}.into_any()
}

/// Everything one attempt needs, so the steps can call each other as plain
/// functions instead of as closures that have to capture one another.
#[derive(Clone, Copy)]
struct Attempt {
    app: App,
    state: State,
    base: StoredValue<String>,
    connection: StoredValue<String>,
    session: RwSignal<String>,
    shown: RwSignal<Shown>,
    code: RwSignal<String>,
    left: RwSignal<u64>,
    live: RwSignal<String>,
    busy: RwSignal<bool>,
    /// Bumped by every new pairing in this dialog. A poll or a tick from the
    /// attempt before it belongs to nothing and must touch nothing.
    round: RwSignal<u32>,
    frame: NodeRef<leptos::html::Section>,
    box_ref: NodeRef<leptos::html::Input>,
}

fn modal(app: App, state: State, started: Started) -> AnyView {
    let attempt = Attempt {
        app,
        state,
        base: StoredValue::new(started.base.clone()),
        connection: StoredValue::new(started.connection.clone()),
        session: RwSignal::new(started.session.clone()),
        shown: RwSignal::new(read_step(&started.step)),
        code: RwSignal::new(String::new()),
        left: RwSignal::new(started.expires_in.min(LONGEST)),
        live: RwSignal::new(String::new()),
        busy: RwSignal::new(false),
        round: RwSignal::new(0),
        frame: NodeRef::new(),
        box_ref: NodeRef::new(),
    };
    arrive(attempt, 0, true);

    // One tick a second for the whole life of the dialog. It only counts
    // while something is being waited for, so a summary does not sit under a
    // clock.
    let ticker = set_interval_with_handle(move || tick(attempt), Duration::from_secs(1)).ok();
    on_cleanup(move || {
        if let Some(ticker) = ticker {
            ticker.clear();
        }
        restore_focus();
    });

    let stage = Memo::new(move |_| match attempt.shown.get() {
        Shown::Waiting {
            prompt: Prompt::EnterCode { .. },
            ..
        } => Stage::Code,
        Shown::Waiting { .. } => Stage::Waiting,
        Shown::Done { .. } => Stage::Done,
        Shown::Failed { .. } => Stage::Failed,
    });
    // The dialog takes the focus when it appears, and a code box takes it
    // whenever one is on screen: typing is the only thing there is to do.
    // Keyed on the stage, so a poll that brings back the prompt already on
    // screen does not snatch the focus back from whatever somebody tabbed to.
    Effect::new(move |_| {
        let code = stage.get() == Stage::Code;
        match (code, attempt.box_ref.get(), attempt.frame.get()) {
            (true, Some(input), _) => {
                let _ = input.focus();
            }
            (_, _, Some(frame)) => {
                let _ = frame.focus();
            }
            _ => {}
        }
    });

    // Leaving the page ends the attempt. The remote reaps a dialog that has
    // gone quiet anyway, but not for five minutes, and a device left flashing
    // is a device somebody has to go and look at.
    let listener = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || cancel(attempt));
    if let Some(window) = web_sys::window() {
        let _ =
            window.add_event_listener_with_callback("pagehide", listener.as_ref().unchecked_ref());
    }
    let listener = StoredValue::new_local(listener);
    on_cleanup(move || {
        listener.with_value(|listener| {
            if let Some(window) = web_sys::window() {
                let _ = window.remove_event_listener_with_callback(
                    "pagehide",
                    listener.as_ref().unchecked_ref(),
                );
            }
        });
    });

    let prompt = Memo::new(move |_| match attempt.shown.get() {
        Shown::Waiting { prompt, .. } => Some(prompt),
        _ => None,
    });
    let title = Memo::new(move |_| match attempt.shown.get() {
        Shown::Waiting { prompt, .. } => headline(&prompt).to_string(),
        Shown::Done { .. } => "Paired".to_string(),
        Shown::Failed { .. } => "Pairing did not finish".to_string(),
    });
    view! {<div class="pairing-scrim">
        <section node_ref=attempt.frame class="card pairing-dialog" tabindex="-1"
            role="dialog" aria-modal="true" aria-labelledby="pairing-headline"
            on:keydown=move |event|keydown(attempt, &event)>
            <button class="pairing-close" type="button" aria-label="Close pairing"
                on:click=move |_|cancel(attempt)>"×"</button>
            <h2 id="pairing-headline">{move ||title.get()}</h2>
            {move ||prompt.get().and_then(|prompt|note(&prompt)).map(|line|view!{<p class="pairing-note">{line}</p>})}
            <p class="pairing-live" role="status" aria-live="polite">{move ||attempt.live.get()}</p>
            <Show when=move ||{matches!(attempt.shown.get(), Shown::Waiting{..}) && attempt.left.get() > 0}>
                <p class="pairing-countdown">{move ||countdown(attempt.left.get())}</p>
            </Show>
            {move ||match attempt.shown.get() {
                Shown::Waiting{prompt:Prompt::EnterCode{length,alphabet:name,..},..} => code_form(attempt, length, alphabet(&name)),
                Shown::Waiting{..} => view!{<div class="actions">
                    <button class="ghost" type="button" on:click=move |_|cancel(attempt)>"Cancel"</button>
                </div>}.into_any(),
                // A pairing the remote could not keep everything of stays up
                // until it is dismissed: the key is good, and the one thing
                // that did not go through is worth reading before it goes.
                Shown::Done{summary,warning} => view!{
                    <p class="pairing-summary">{if summary.is_empty(){"This device is paired.".to_string()}else{summary}}</p>
                    {warning.map(|warning|view!{<p class="notice small pairing-warning">{warning_sentence(&warning)}</p>})}
                    <div class="actions">
                        <button class="primary" type="button" on:click=move |_|finish(attempt)>"Done"</button>
                    </div>
                }.into_any(),
                Shown::Failed{sentence,note} => view!{
                    <p class="pairing-failed" role="alert">{sentence}</p>
                    {note.map(|line|view!{<p class="pairing-note">{line}</p>})}
                    <div class="actions">
                        <button class="primary" type="button" disabled=move ||attempt.busy.get() on:click=move |_|restart(attempt)>"Try again"</button>
                        <button class="ghost" type="button" on:click=move |_|close(attempt)>"Close"</button>
                    </div>
                }.into_any(),
            }}
        </section>
    </div>}
    .into_any()
}

/// The one input a code prompt has. One box, as long as the device said, in
/// the characters the device uses; nothing is sent until there is a whole
/// code.
fn code_form(attempt: Attempt, length: u8, alphabet: Alphabet) -> AnyView {
    view! {<form on:submit=move |event|{event.prevent_default();submit(attempt, length);}>
        <label class="field">"Code from the device"
            <input node_ref=attempt.box_ref class="pairing-code" type="text"
                maxlength=(length > 0).then(||length.to_string())
                inputmode=alphabet.input_mode()
                autocapitalize=if alphabet.capitals() {"characters"} else {"none"}
                autocomplete="one-time-code" spellcheck="false"
                disabled=move ||attempt.busy.get()
                prop:value=move ||attempt.code.get()
                on:input=move |event|attempt.code.set(clean_code(&event_target_value(&event), length, alphabet))/>
        </label>
        <div class="actions">
            <button class="primary" type="submit" disabled=move ||attempt.busy.get()||!complete(&attempt.code.get(), length)>"Continue"</button>
            <button class="ghost" type="button" on:click=move |_|cancel(attempt)>"Cancel"</button>
        </div>
    </form>}
    .into_any()
}

/// Escape says no, and Tab never leaves the dialog while it is open.
fn keydown(attempt: Attempt, event: &web_sys::KeyboardEvent) {
    if event.key() == "Escape" {
        cancel(attempt);
        return;
    }
    if event.key() != "Tab" {
        return;
    }
    let Some(frame) = attempt.frame.get_untracked() else {
        return;
    };
    let Ok(found) = frame.unchecked_ref::<web_sys::Element>().query_selector_all(
        "button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),a[href]",
    ) else {
        return;
    };
    let items: Vec<web_sys::HtmlElement> = (0..found.length())
        .filter_map(|index| found.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .collect();
    let (Some(first), Some(last)) = (items.first(), items.last()) else {
        return;
    };
    let active = document().active_element();
    let is = |element: &web_sys::HtmlElement| {
        active
            .as_ref()
            .is_some_and(|here| here.is_same_node(Some(element.unchecked_ref())))
    };
    if event.shift_key() {
        if is(first) {
            let _ = last.focus();
            event.prevent_default();
        }
    } else if is(last) {
        let _ = first.focus();
        event.prevent_default();
    }
}

/// Take a step the remote answered with and do whatever it asks for.
///
/// `changed` says whether this is a step the dialog was not already on. A
/// poll that brings back the prompt already on screen has to schedule the
/// next poll and nothing else: saying the headline again would talk over a
/// screen reader every two seconds.
fn arrive(attempt: Attempt, round: u32, changed: bool) {
    let shown = attempt.shown.get_untracked();
    match &shown {
        Shown::Waiting {
            prompt,
            poll_after_ms,
        } => {
            if changed {
                attempt.live.set(headline(prompt).to_string());
            }
            let wait = *poll_after_ms;
            if wait > 0 {
                let session = attempt.session.get_untracked();
                set_timeout(
                    move || poll(attempt, round, session, None, BUSY_TRIES),
                    Duration::from_millis(u64::from(wait)),
                );
            }
        }
        Shown::Done { summary, warning } => {
            if changed {
                let connection = attempt.connection.get_value();
                let summary = summary.clone();
                attempt.state.edit(&connection, |known| {
                    known.paired = true;
                    known.unpaired = false;
                    known.summary = summary.clone();
                });
                let said = if summary.is_empty() {
                    "This device is paired.".to_string()
                } else {
                    summary
                };
                attempt.live.set(match warning {
                    Some(warning) => format!("{said} {}", warning_sentence(warning)),
                    None => said,
                });
            }
        }
        Shown::Failed { sentence, .. } => {
            if changed {
                attempt.live.set(sentence.clone());
            }
        }
    }
}

/// Whether this attempt is still the one the dialog is on.
///
/// A timer that fires after the dialog has closed belongs to nothing: its
/// signals went with the view, and reading one of those is a panic rather
/// than a stale answer. Every poll, every retry and every tick asks this
/// first.
fn alive(attempt: Attempt, round: u32) -> bool {
    attempt.round.try_get_untracked() == Some(round)
}

/// Show a step, and act on it.
fn show(attempt: Attempt, round: u32, shown: Shown) {
    if !alive(attempt, round) {
        return;
    }
    if !matches!(shown, Shown::Waiting { .. }) {
        attempt.code.set(String::new());
    }
    let changed = attempt.shown.with_untracked(|current| *current != shown);
    if changed {
        attempt.shown.set(shown);
    }
    arrive(attempt, round, changed);
}

/// One call on an open session: a poll, or what somebody typed.
fn poll(attempt: Attempt, round: u32, session: String, input: Option<Value>, tries: usize) {
    if !alive(attempt, round) || attempt.session.get_untracked() != session {
        return;
    }
    let path = format!("{}/pair/{session}", attempt.base.get_value());
    let asking = input.is_some();
    if asking {
        attempt.busy.set(true);
    }
    spawn_local(async move {
        let answer = api::ha("POST", &path, input.clone()).await;
        if !alive(attempt, round) {
            return;
        }
        if asking {
            attempt.busy.set(false);
        }
        match answer {
            Ok(value) => show(attempt, round, read_step(&value["step"])),
            // Another call on this session is still out. Nothing was asked of
            // the device, so asking again is quiet and costs nobody anything.
            Err(error) if error.busy && tries > 0 => set_timeout(
                move || poll(attempt, round, session, input, tries - 1),
                BUSY_PAUSE,
            ),
            Err(error) => {
                if error.unauthorized {
                    attempt.app.paired.set(Some(false));
                    attempt.state.open.set(None);
                    return;
                }
                // A session the remote no longer has: the daemon restarted,
                // or the window closed. Its own sentence names the session
                // nobody ever saw; this one says what to do.
                let sentence = if error.status == 404 {
                    INTERRUPTED.to_string()
                } else {
                    error.message.clone()
                };
                show(
                    attempt,
                    round,
                    Shown::Failed {
                        sentence,
                        note: None,
                    },
                );
            }
        }
    });
}

/// Send what was typed. OWNER DECISION: a wrong code ends the attempt, so
/// there is nothing here that lets somebody sit and guess.
fn submit(attempt: Attempt, length: u8) {
    let code = attempt.code.get_untracked();
    if attempt.busy.get_untracked() || !complete(&code, length) {
        return;
    }
    let round = attempt.round.get_untracked();
    let session = attempt.session.get_untracked();
    poll(
        attempt,
        round,
        session,
        Some(json!({"input": {"kind": "code", "code": code}})),
        BUSY_TRIES,
    );
}

/// A second a device is still being waited for.
fn tick(attempt: Attempt) {
    let Some(round) = attempt.round.try_get_untracked() else {
        return;
    };
    if !matches!(attempt.shown.get_untracked(), Shown::Waiting { .. }) {
        return;
    }
    let left = attempt.left.get_untracked().saturating_sub(1);
    attempt.left.set(left);
    if left == 0 {
        show(
            attempt,
            round,
            Shown::Failed {
                sentence: failure_sentence("timed_out").to_string(),
                note: None,
            },
        );
    } else if announce_at(left) {
        attempt.live.set(countdown(left));
    }
}

/// "Try again": a new pairing, from the beginning, in the dialog that is
/// already open.
fn restart(attempt: Attempt) {
    if attempt.busy.get_untracked() {
        return;
    }
    attempt.busy.set(true);
    let round = attempt.round.get_untracked() + 1;
    attempt.round.set(round);
    attempt.code.set(String::new());
    attempt.live.set(String::new());
    let path = format!("{}/pair", attempt.base.get_value());
    spawn_local(async move {
        let answer = api::ha("POST", &path, None).await;
        if !alive(attempt, round) {
            return;
        }
        attempt.busy.set(false);
        match answer {
            Ok(value) => {
                attempt
                    .session
                    .set(value["session"].as_str().unwrap_or_default().to_owned());
                attempt
                    .left
                    .set(value["expires_in"].as_u64().unwrap_or(0).min(LONGEST));
                show(attempt, round, read_step(&value["step"]));
            }
            Err(error) => {
                if error.unauthorized {
                    attempt.app.paired.set(Some(false));
                    attempt.state.open.set(None);
                    return;
                }
                show(
                    attempt,
                    round,
                    Shown::Failed {
                        sentence: error.message,
                        note: None,
                    },
                );
            }
        }
    });
}

/// Close the dialog without telling the remote anything: the session is over
/// already.
fn close(attempt: Attempt) {
    attempt.round.update(|round| *round += 1);
    attempt.state.open.set(None);
}

/// A pairing that finished: read the connection's settings again, so what the
/// page shows is what the remote has rather than what it had.
fn finish(attempt: Attempt) {
    attempt.state.refreshed.update(|count| *count += 1);
    close(attempt);
}

/// Give up: tell the remote to stop, and close.
fn cancel(attempt: Attempt) {
    if matches!(attempt.shown.get_untracked(), Shown::Waiting { .. }) {
        drop_session(&attempt.base.get_value(), &attempt.session.get_untracked());
    }
    close(attempt);
}

fn read_step(value: &Value) -> Shown {
    match serde_json::from_value::<Step>(value.clone()) {
        Ok(Step::Waiting {
            prompt,
            poll_after_ms,
        }) => Shown::Waiting {
            prompt,
            poll_after_ms,
        },
        Ok(Step::Done { summary, warning }) => Shown::Done {
            summary,
            warning: warning
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty()),
        },
        Ok(Step::Failed { reason, message }) => Shown::Failed {
            sentence: failure_sentence(&reason).to_string(),
            note: message
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty()),
        },
        Err(_) => Shown::Failed {
            sentence: UNREADABLE.to_string(),
            note: None,
        },
    }
}

/// Tell the remote to drop a pairing session.
///
/// The answer is of no interest: a cancel is idempotent, and it is sent from
/// `pagehide` too, where a browser may well not finish it. That is why the
/// remote reaps a session whose dialog has gone quiet on its own - this only
/// saves a device from being left flashing for the rest of the window.
fn drop_session(base: &str, session: &str) {
    if session.is_empty() {
        return;
    }
    let path = format!("{base}/pair/{session}");
    spawn_local(async move {
        let _ = api::ha("DELETE", &path, None).await;
    });
}

fn remember_focus() {
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    RETURNS_TO.with(|slot| *slot.borrow_mut() = active);
}

fn restore_focus() {
    RETURNS_TO.with(|slot| {
        if let Some(element) = slot.borrow_mut().take() {
            let _ = element.focus();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(value: Value) -> Prompt {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn every_headline_is_couchs_own_and_the_package_only_adds_a_line() {
        let cases = [
            (
                json!({"kind": "press_button", "message": "The button is on top"}),
                "Press the button on the device",
                Some("The button is on top"),
            ),
            (
                json!({"kind": "approve_on_device"}),
                "Approve on the device",
                None,
            ),
            (
                json!({"kind": "enter_code", "message": "It is on the screen",
                       "length": 4, "alphabet": "digits"}),
                "Enter the code shown on the device",
                Some("It is on the screen"),
            ),
            // A prompt from a later remote: Couch still has words for it.
            (
                json!({"kind": "wave_at_it"}),
                "Finish pairing on the device",
                None,
            ),
        ];
        for (value, said, line) in cases {
            let prompt = prompt(value);
            assert_eq!(headline(&prompt), said);
            assert_eq!(note(&prompt).as_deref(), line);
        }
        // A package that sent nothing worth showing adds nothing.
        for empty in [json!(null), json!(""), json!("   ")] {
            assert_eq!(
                note(&prompt(json!({"kind": "press_button", "message": empty}))),
                None
            );
        }
    }

    #[test]
    fn every_failure_has_a_couch_sentence_including_ones_from_a_later_remote() {
        for (reason, sentence) in [
            ("unreachable", "Couch could not reach the device."),
            ("refused", "The device refused to pair."),
            ("wrong_code", "That was not the code the device is showing."),
            ("timed_out", "The device did not answer in time."),
            ("unsupported", "This device does not pair this way."),
            ("something_new", "Pairing did not finish."),
            ("", "Pairing did not finish."),
        ] {
            assert_eq!(failure_sentence(reason), sentence, "{reason}");
        }
    }

    #[test]
    fn a_step_is_read_or_it_is_a_failure_with_words() {
        assert_eq!(
            read_step(
                &json!({"step": "waiting", "prompt": {"kind": "press_button"},
                              "poll_after_ms": 2000})
            ),
            Shown::Waiting {
                prompt: Prompt::PressButton { message: None },
                poll_after_ms: 2000
            }
        );
        assert_eq!(
            read_step(
                &json!({"step": "done", "summary": "Paired with the hall television",
                              "settings": {"configured": true}})
            ),
            Shown::Done {
                summary: "Paired with the hall television".into(),
                warning: None,
            }
        );
        // A key that was stored beside settings that were not.
        assert_eq!(
            read_step(
                &json!({"step": "done", "summary": "Paired with the hall television",
                "settings": {"configured": true},
                "warning": "the settings this device corrected were not kept: they were refused (invalid)"})
            ),
            Shown::Done {
                summary: "Paired with the hall television".into(),
                warning: Some(
                    "the settings this device corrected were not kept: they were refused (invalid)"
                        .into()
                ),
            }
        );
        assert_eq!(
            warning_sentence(
                "the settings this device corrected were not kept: they were refused (invalid)"
            ),
            "The pairing was saved, but the settings this device corrected were not kept: \
             they were refused (invalid)."
        );
        // The remote's own full stop is not doubled.
        assert_eq!(
            warning_sentence(" one setting was dropped. "),
            "The pairing was saved, but one setting was dropped."
        );
        assert_eq!(
            read_step(&json!({"step": "failed", "reason": "wrong_code",
                              "message": "That was not the code on the screen"})),
            Shown::Failed {
                sentence: "That was not the code the device is showing.".into(),
                note: Some("That was not the code on the screen".into()),
            }
        );
        // Nothing, or something this page cannot read, still says something.
        for value in [json!(null), json!({"step": "sideways"}), json!("waiting")] {
            assert_eq!(
                read_step(&value),
                Shown::Failed {
                    sentence: UNREADABLE.into(),
                    note: None
                },
                "{value}"
            );
        }
    }

    #[test]
    fn a_code_box_keeps_only_what_the_device_can_be_showing() {
        assert_eq!(clean_code("12ab34", 4, Alphabet::Digits), "1234");
        assert_eq!(clean_code("1 2-3 4 5", 4, Alphabet::Digits), "1234");
        assert_eq!(clean_code("dead", 6, Alphabet::Hex), "DEAD");
        assert_eq!(clean_code("dz-ea9", 6, Alphabet::Hex), "DEA9");
        assert_eq!(clean_code("zx81!", 6, Alphabet::Alphanumeric), "ZX81");
        assert_eq!(clean_code("abcdefgh", 6, Alphabet::Alphanumeric), "ABCDEF");
        // A device that did not say how long its code is gets what was typed.
        assert_eq!(
            clean_code("123456789012345678", 0, Alphabet::Digits),
            "123456789012345678"
        );
        // An alphabet from a later remote is the widest of the three rather
        // than a box that refuses what the device is showing.
        assert_eq!(alphabet("digits"), Alphabet::Digits);
        assert_eq!(alphabet("hex"), Alphabet::Hex);
        assert_eq!(alphabet("alphanumeric"), Alphabet::Alphanumeric);
        assert_eq!(alphabet("words"), Alphabet::Alphanumeric);
        assert_eq!(Alphabet::Digits.input_mode(), "numeric");
        assert_eq!(Alphabet::Hex.input_mode(), "text");
        assert_eq!(Alphabet::Alphanumeric.input_mode(), "text");
        assert!(!Alphabet::Digits.capitals());
        assert!(Alphabet::Hex.capitals() && Alphabet::Alphanumeric.capitals());
    }

    #[test]
    fn nothing_is_sent_until_there_is_a_whole_code() {
        assert!(!complete("", 4));
        assert!(!complete("123", 4));
        assert!(complete("1234", 4));
        assert!(!complete("", 0));
        assert!(complete("1", 0));
    }

    #[test]
    fn the_countdown_reads_calmly_and_is_only_said_out_loud_now_and_then() {
        assert_eq!(countdown(272), "4:32 left");
        assert_eq!(countdown(300), "5:00 left");
        assert_eq!(countdown(61), "1:01 left");
        assert_eq!(countdown(9), "0:09 left");
        for said in [300, 240, 180, 120, 60, 30, 10] {
            assert!(announce_at(said), "{said}");
        }
        for quiet in [299, 61, 59, 31, 29, 11, 9, 1, 0] {
            assert!(!announce_at(quiet), "{quiet}");
        }
    }

    #[test]
    fn a_connection_asks_to_be_paired_only_when_its_package_pairs() {
        let pairs = |paired, required, unpaired| Known {
            pairs: true,
            required,
            paired,
            summary: String::new(),
            unpaired,
        };
        // Every package a shipped build runs: nothing is ever asked for.
        assert!(!needs(&Known::default()));
        assert!(!needs(&Known {
            pairs: false,
            required: true,
            unpaired: true,
            ..Known::default()
        }));
        assert!(needs(&pairs(false, true, false)));
        assert!(!needs(&pairs(true, true, false)));
        // A key the device threw away: refused, so asked for again.
        assert!(needs(&pairs(true, true, true)));
        // A package that can pair but does not have to.
        assert!(!needs(&pairs(false, false, false)));
        assert!(needs(&pairs(false, false, true)));
    }
}
