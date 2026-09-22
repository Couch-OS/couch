//! Protocol 3 (unreleased): the many devices behind one packaged connection.
//!
//! A bridge is one connection and many lamps. The daemon lists them at
//! `…/plugin/children`; this is the page that shows that list, the one place a
//! child becomes a room device or a room scene, and the small light and blind
//! panels that drive one afterwards.
//!
//! Three rules shape all of it:
//!
//! * **Nothing is ever added by itself.** The room the bridge puts a lamp in is
//!   only ever a hint: it preselects a filter and nothing else. "Add all shown"
//!   adds what the boxes are showing, and only after a confirmation that names
//!   every one of them.
//! * **The browser never describes a child.** A save carries the connection and
//!   the resource and nothing else; the daemon stamps what the child is from
//!   the package's own listing.
//! * **A child that stops being listed is kept.** It is badged, never hidden,
//!   and only ever removed by hand.
//!
//! With the protocol 3 switch off no package this build accepts may declare a
//! kind of child, so `Provider::Plugin.children` is always empty, none of this
//! is reachable, and the picker behaves exactly as it did.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use couch_model::{ActionKind, ChildComponent, Config, Connection, Device, Id, Integration};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{api, ui, App};

/// How long to wait before asking again when the remote says it is busy with
/// this connection, and how many times. Reading a cold listing is the only
/// thing that holds a connection for long, and it has ten seconds to finish.
const BUSY_PAUSE: Duration = Duration::from_millis(400);
const BUSY_TRIES: usize = 8;

// ---------------------------------------------------------------------------
// What the daemon answers
// ---------------------------------------------------------------------------

/// Deliberately its own shapes rather than the model's: every field is
/// optional and nothing is refused for being unknown, so a remote that has
/// learnt to say more about a child does not stop this page listing one.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct Listing {
    #[serde(default)]
    pub kinds: Vec<Kind>,
    #[serde(default)]
    pub children: Vec<Child>,
    /// Devices and scenes made from a child this connection no longer lists.
    #[serde(default)]
    pub missing: Vec<Child>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct Kind {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    /// `light`, `cover`, `climate` or `scene`.
    #[serde(default)]
    pub component: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct Child {
    #[serde(default)]
    pub id: String,
    /// `null` for a saved device a rollback stripped that has not healed yet.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: String,
    /// The room the bridge itself puts it in. A hint for the person choosing.
    #[serde(default)]
    pub room_hint: Option<String>,
    #[serde(default)]
    pub light: Option<LightTraits>,
    #[serde(default)]
    pub cover: Option<CoverTraits>,
    /// Where it already is, if it is anywhere.
    #[serde(default)]
    pub assigned: Option<Assigned>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub(super) struct LightTraits {
    #[serde(default)]
    pub dimmable: bool,
    /// The colour temperature range in mirek, coolest first.
    #[serde(default)]
    pub mirek: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub(super) struct CoverTraits {
    #[serde(default)]
    pub position: bool,
    #[serde(default)]
    pub stop: bool,
}

/// `{"room":…,"device":…}` for a room device, `{"scene":…}` for a package
/// scene. Ids; the names come from the configuration.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct Assigned {
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub scene: Option<String>,
}

// ---------------------------------------------------------------------------
// The parts with a rule in them, as plain functions
// ---------------------------------------------------------------------------

/// The component a kind is drawn with, `""` when the kind is not declared.
pub(super) fn component(kinds: &[Kind], kind: Option<&str>) -> String {
    let Some(kind) = kind else {
        return String::new();
    };
    kinds
        .iter()
        .find(|k| k.kind == kind)
        .map(|k| k.component.clone())
        .unwrap_or_default()
}

/// The words a kind is shown under, falling back to its identifier so a kind
/// this page does not know is still readable rather than blank.
pub(super) fn kind_label(kinds: &[Kind], kind: Option<&str>) -> String {
    let Some(kind) = kind else {
        return "Device".into();
    };
    kinds
        .iter()
        .find(|k| k.kind == kind)
        .map(|k| k.label.clone())
        .unwrap_or_else(|| kind.to_string())
}

/// The distinct rooms the connection itself reports, in a stable order. This
/// is the generalisation of the Hue picker's "Hue room or zone" box.
pub(super) fn hints(children: &[Child]) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for child in children {
        let Some(hint) = child.room_hint.as_deref() else {
            continue;
        };
        if !hint.trim().is_empty() && !all.iter().any(|seen| seen == hint) {
            all.push(hint.to_string());
        }
    }
    all.sort();
    all
}

/// OWNER DECISION: when the connection has a room of its own with this Couch
/// room's name, that is the filter the picker opens on. Case and surrounding
/// space do not count; anything else opens on every room.
pub(super) fn preselect(hints: &[String], room: &str) -> String {
    let wanted = room.trim().to_lowercase();
    if wanted.is_empty() {
        return String::new();
    }
    hints
        .iter()
        .find(|hint| hint.trim().to_lowercase() == wanted)
        .cloned()
        .unwrap_or_default()
}

/// One child against the three boxes. The search covers the name and the
/// connection's own room, word by word, so "study lamp" finds a lamp the
/// bridge put in the study.
pub(super) fn shows(child: &Child, search: &str, kind: &str, hint: &str) -> bool {
    let text = format!(
        "{} {}",
        child.name,
        child.room_hint.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    (kind.is_empty() || child.kind.as_deref() == Some(kind))
        && (hint.is_empty() || child.room_hint.as_deref() == Some(hint))
        && search
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
}

/// Everything the boxes are showing, in the order the package listed it.
pub(super) fn shown<'a>(
    children: &'a [Child],
    search: &str,
    kind: &str,
    hint: &str,
) -> Vec<&'a Child> {
    children
        .iter()
        .filter(|child| shows(child, search, kind, hint))
        .collect()
}

/// Exactly what "Add all shown" would add: what is on screen, minus anything
/// already in the house, minus the scenes - a scene is not a device and goes
/// to the room's Scenes button one at a time.
pub(super) fn add_all<'a>(
    listing: &'a Listing,
    search: &str,
    kind: &str,
    hint: &str,
) -> Vec<&'a Child> {
    shown(&listing.children, search, kind, hint)
        .into_iter()
        .filter(|child| {
            child.assigned.is_none() && component(&listing.kinds, child.kind.as_deref()) != "scene"
        })
        .collect()
}

/// The question the confirmation asks. Nothing is added without it.
pub(super) fn confirm_question(count: usize, room: &str) -> String {
    if count == 1 {
        format!("Add this device to {room}?")
    } else {
        format!("Add these {count} devices to {room}?")
    }
}

/// What a refusal says. The sentence is what every screen has always shown; a
/// protocol 3 package may also send words of its own, and those are added when
/// they say something the sentence does not.
pub(super) fn refusal(error: &api::ApiError) -> String {
    let detail = match &error.reason {
        Some(api::Reason::InvalidSetting { text, .. } | api::Reason::Message { text }) => {
            text.as_str()
        }
        None => "",
    };
    if detail.is_empty() || detail == error.message {
        error.message.clone()
    } else {
        format!("{} ({detail})", error.message)
    }
}

/// What one lamp, blind or thermostat reports, in a line. `None` is "nothing
/// has been read yet", which is not the same as a device that says nothing.
pub(super) fn state_line(status: Option<&Value>, component: ChildComponent) -> String {
    let Some(status) = status else {
        return "State not read yet".into();
    };
    match component {
        ChildComponent::Light => {
            let light = &status["light"];
            match light["on"].as_bool() {
                Some(true) => match light["brightness"].as_u64() {
                    Some(level) => format!("On · {level}%"),
                    None => "On".into(),
                },
                Some(false) => "Off".into(),
                None => "Unavailable".into(),
            }
        }
        ChildComponent::Cover => {
            let cover = &status["cover"];
            match (cover["open"].as_bool(), cover["position"].as_u64()) {
                (_, Some(position)) => format!(
                    "{} · {position}% open",
                    if position == 0 { "Closed" } else { "Open" }
                ),
                (Some(true), None) => "Open".into(),
                (Some(false), None) => "Closed".into(),
                (None, None) => "Unavailable".into(),
            }
        }
        ChildComponent::Climate => {
            let climate = &status["climate"];
            if climate["available"] == false {
                return "Unavailable".into();
            }
            let degrees = |key: &str| {
                climate[key]
                    .as_i64()
                    .map(|tenths| format!("{:.1}°", tenths as f64 / 10.0))
            };
            let mut parts: Vec<String> = Vec::new();
            if let Some(mode) = climate["mode"].as_str() {
                parts.push(mode.replace('_', " "));
            }
            if let Some(now) = degrees("current_tenths") {
                parts.push(format!("{now} now"));
            }
            if let Some(set) = degrees("target_tenths") {
                parts.push(format!("{set} set"));
            }
            if parts.is_empty() {
                "Unavailable".into()
            } else {
                parts.join(" · ")
            }
        }
        ChildComponent::Scene => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// What the children picker is showing, and what a batch add is in the middle
/// of. Provided at the root by [`super::provide_editor_state`], because every
/// save replaces the configuration and rebuilds the room screen under it: a
/// batch keeping its progress inside the view would lose it on its own first
/// success.
#[derive(Clone, Copy)]
pub(super) struct State {
    pub kind: RwSignal<String>,
    pub hint: RwSignal<String>,
    /// Whether the person has chosen a room themselves. Until they do, every
    /// fresh listing preselects the one that matches this Couch room.
    pub hint_chosen: RwSignal<bool>,
    /// One listing per connection, shared by the picker and by the panel of
    /// every device made from that connection, so a room of twenty lamps asks
    /// once.
    pub listings: RwSignal<BTreeMap<String, Loaded>>,
    pub batch: RwSignal<Option<Batch>>,
}

impl State {
    pub(super) fn new() -> State {
        State {
            kind: RwSignal::new(String::new()),
            hint: RwSignal::new(String::new()),
            hint_chosen: RwSignal::new(false),
            listings: RwSignal::new(BTreeMap::new()),
            batch: RwSignal::new(None),
        }
    }

    /// Forget the filters and any finished batch: another connection lists
    /// other kinds and other rooms.
    pub(super) fn reset(self) {
        self.kind.set(String::new());
        self.hint.set(String::new());
        self.hint_chosen.set(false);
        self.batch.set(None);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Loaded {
    pub asked: bool,
    pub busy: bool,
    pub listing: Option<Listing>,
    pub message: String,
}

/// One run of "Add all shown", from the confirmation to the last save.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Batch {
    pub room: Id,
    pub connection: String,
    /// Resource id and name, in the order they will be sent.
    pub items: Vec<(String, String)>,
    pub done: usize,
    pub confirming: bool,
    pub running: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Talking to the daemon
// ---------------------------------------------------------------------------

/// One request to a package, retrying a Busy quietly.
///
/// Busy means the remote is reading this connection's children for somebody
/// else and did nothing with this request, so asking again is not repeating a
/// command. Every other refusal is shown.
fn quietly(
    method: &'static str,
    path: String,
    body: Option<Value>,
    tries: usize,
    done: impl Fn(Result<Value, api::ApiError>) + Clone + 'static,
) {
    spawn_local(async move {
        match api::ha(method, &path, body.clone()).await {
            Err(error) if error.busy && tries > 0 => set_timeout(
                move || quietly(method, path, body, tries - 1, done),
                BUSY_PAUSE,
            ),
            answer => done(answer),
        }
    });
}

/// Read a connection's children. `refresh` asks the package again instead of
/// answering from the remote's five-minute cache. `once` is for a screen that
/// needs the listing but did not ask for it: it does nothing if the connection
/// has been listed already.
pub(super) fn load(
    app: App,
    state: State,
    pairing: super::plugin_pairing::State,
    connection: String,
    refresh: bool,
    once: bool,
) {
    let listings = state.listings;
    // Deferred, because this is called while the screen is being built and a
    // listing is not something to start during a render.
    spawn_local(async move {
        let skip = listings.with_untracked(|all| {
            all.get(&connection)
                .is_some_and(|entry| entry.busy || (once && entry.asked))
        });
        if skip {
            return;
        }
        listings.update(|all| {
            let entry = all.entry(connection.clone()).or_default();
            entry.asked = true;
            entry.busy = true;
            entry.message = "Finding devices…".into();
        });
        let path = format!("/api/connections/{connection}/plugin/children");
        let (method, path) = if refresh {
            ("POST", format!("{path}/refresh"))
        } else {
            ("GET", path)
        };
        quietly(method, path, None, BUSY_TRIES, move |answer| {
            listings.update(|all| {
                let entry = all.entry(connection.clone()).or_default();
                entry.busy = false;
                match &answer {
                    Ok(value) => match serde_json::from_value::<Listing>(value.clone()) {
                        Ok(listing) => {
                            entry.message = found(listing.children.len());
                            entry.listing = Some(listing);
                        }
                        Err(_) => {
                            entry.message =
                                "The remote sent a device list this page cannot read.".into()
                        }
                    },
                    Err(error) => {
                        if error.unauthorized {
                            app.paired.set(Some(false));
                        }
                        super::plugin_pairing::noticed(pairing, &connection, error);
                        entry.message = refusal(error);
                    }
                }
            });
        });
    });
}

fn found(count: usize) -> String {
    format!(
        "{count} device{} found. Choose the ones that belong in this room.",
        if count == 1 { "" } else { "s" }
    )
}

/// The listing of one connection, as the screen has it.
fn listing_of(state: State, connection: &str) -> Option<Listing> {
    state
        .listings
        .with(|all| all.get(connection).and_then(|entry| entry.listing.clone()))
}

// ---------------------------------------------------------------------------
// The picker
// ---------------------------------------------------------------------------

/// The "Add to this room" panel for a packaged connection that lists devices.
pub(super) fn picker(app: App, house: Arc<Config>, connection: Connection, room: Id) -> AnyView {
    let pairing = expect_context::<super::plugin_pairing::State>();
    let state = expect_context::<State>();
    let picking = expect_context::<super::device_picker::State>();
    let id = connection.id.to_string();
    let room_name = house
        .rooms
        .iter()
        .find(|r| r.id == room)
        .map(|r| r.name.clone())
        .unwrap_or_default();
    load(app, state, pairing, id.clone(), false, false);

    // OWNER DECISION, applied once the listing is here and only until the
    // person chooses a room for themselves.
    let preselect_room = room_name.clone();
    let preselect_of = id.clone();
    Effect::new(move |_| {
        let Some(listing) = listing_of(state, &preselect_of) else {
            return;
        };
        if !state.hint_chosen.get_untracked() {
            state
                .hint
                .set(preselect(&hints(&listing.children), &preselect_room));
        }
    });

    let refresh_id = id.clone();
    let listing = {
        let id = id.clone();
        move || listing_of(state, &id).unwrap_or_default()
    };
    let busy = {
        let id = id.clone();
        move || {
            state
                .listings
                .with(|all| all.get(&id).is_some_and(|e| e.busy))
        }
    };
    let message = {
        let id = id.clone();
        move || {
            state
                .listings
                .with(|all| all.get(&id).map(|e| e.message.clone()).unwrap_or_default())
        }
    };
    let (kinds_for_select, hints_for_select, cards, missing) = (
        listing.clone(),
        listing.clone(),
        listing.clone(),
        listing.clone(),
    );
    let plan = listing.clone();
    let (house_cards, house_missing) = (house.clone(), house.clone());
    let (connection_cards, connection_missing) = (connection.clone(), connection.clone());
    let (room_cards, room_plan) = (room.clone(), room.clone());
    let (id_plan, id_dialog) = (id.clone(), id.clone());
    let (room_dialog, name_dialog) = (room.clone(), room_name.clone());
    let busy_for_refresh = busy.clone();

    view! {<p class="dim">"Add the lights, blinds and scenes this integration lists. Nothing is added until you choose it."</p>
        <label class="field">"Device type"<select aria-label="Device type from the integration" prop:value=move ||state.kind.get()
            on:change=move |e|state.kind.set(event_target_value(&e))>
            <option value="">"All types"</option>
            {move ||kinds_for_select().kinds.into_iter().map(|k|view!{<option value=k.kind.clone()>{k.label.clone()}</option>}).collect_view()}
        </select></label>
        <button class="ghost" disabled=move ||busy_for_refresh() on:click=move |_|load(app,state,pairing,refresh_id.clone(),true,false)>"Refresh devices"</button>
        {super::connections::field("Search devices",picking.filter,"Filter by name or room")}
        <label class="field">"Room from the integration"<select aria-label="Room from the integration" prop:value=move ||state.hint.get()
            on:change=move |e|{state.hint_chosen.set(true);state.hint.set(event_target_value(&e));}>
            <option value="">"All rooms"</option>
            {move ||hints(&hints_for_select().children).into_iter().map(|hint|view!{<option value=hint.clone()>{hint.clone()}</option>}).collect_view()}
        </select></label>
        <p role="status">{move ||message()}</p>

        // OWNER DECISION: one button that adds everything on screen, and a
        // confirmation naming all of it. The button alone never adds anything.
        {move ||{
            let listing=plan();
            let wanted=add_all(&listing,&picking.filter.get(),&state.kind.get(),&state.hint.get());
            let count=wanted.len();
            let items:Vec<(String,String)>=wanted.into_iter().map(|c|(c.id.clone(),c.name.clone())).collect();
            let (room,connection)=(room_plan.clone(),id_plan.clone());
            (count>0).then(move ||view!{<button class="ghost add-all" disabled=move ||app.busy.get()
                on:click=move |_|state.batch.set(Some(Batch{room:room.clone(),connection:connection.clone(),items:items.clone(),
                    done:0,confirming:true,running:false,message:String::new()}))>
                {format!("Add all shown ({count})")}</button>})
        }}
        {move ||batch_panel(app,state,&room_dialog,&id_dialog,&name_dialog)}

        <div class="discovered-devices">{move ||{
            let listing=cards();
            let (search,kind,hint)=(picking.filter.get(),state.kind.get(),state.hint.get());
            shown(&listing.children,&search,&kind,&hint).into_iter()
                .map(|child|card(app,&house_cards,&listing,&connection_cards,&room_cards,child))
                .collect_view()
        }}</div>

        // A child the connection has stopped listing keeps its device, its
        // keys and its scenes. It is named here, and removed only by hand.
        {move ||{
            let listing=missing();
            (!listing.missing.is_empty()).then(||view!{<div class="discovered-devices gone">
                {listing.missing.iter().map(|child|vanished(app,&house_missing,&connection_missing,child)).collect_view()}
            </div>})
        }}
    }.into_any()
}

/// One listed child.
fn card(
    app: App,
    house: &Config,
    listing: &Listing,
    connection: &Connection,
    room: &Id,
    child: &Child,
) -> AnyView {
    let scene = component(&listing.kinds, child.kind.as_deref()) == "scene";
    let saved_scene = child
        .assigned
        .as_ref()
        .and_then(|at| at.scene.as_deref())
        .and_then(|id| house.scenes.iter().find(|s| s.id.as_str() == id))
        .cloned();
    let here = saved_scene
        .as_ref()
        .is_some_and(|scene| scene.rooms.contains(room));
    // A scene already saved for another room can still be shown in this one;
    // anything else that is anywhere is not offered twice.
    let used = if scene {
        here
    } else {
        child.assigned.is_some()
    };
    let detail = match (&child.assigned, scene) {
        (Some(at), _) => assigned_line(house, at),
        (None, true) => "Scene · Appears in this room’s Scenes button".into(),
        (None, false) => {
            let mut parts = vec![kind_label(&listing.kinds, child.kind.as_deref())];
            if let Some(hint) = child.room_hint.as_deref().filter(|h| !h.is_empty()) {
                parts.push(hint.to_string());
            }
            if let Some(traits) = child.light {
                let mut what = Vec::new();
                if traits.dimmable {
                    what.push("brightness");
                }
                if traits.mirek.is_some() {
                    what.push("colour temperature");
                }
                if !what.is_empty() {
                    parts.push(format!("On/off and {}", what.join(", ")));
                }
            }
            if let Some(traits) = child.cover {
                parts.push(
                    if traits.position {
                        "Open, close and position"
                    } else {
                        "Open and close"
                    }
                    .into(),
                );
            }
            parts.join(" · ")
        }
    };
    let (name, resource) = (child.name.clone(), child.id.clone());
    let (room, connection_id) = (room.clone(), connection.id.clone());
    view! {<div class="card discovered-device"><strong>{child.name.clone()}</strong><p class="dim">{detail}</p>
        <button class="primary" disabled=move ||app.busy.get()||used on:click=move |_|{
            if scene {
                if let Some(mut saved)=saved_scene.clone() {
                    if !saved.rooms.contains(&room) {saved.rooms.push(room.clone());}
                    app.run(api::put(format!("/api/scenes/{}",saved.id),saved));
                } else {
                    app.run(api::post("/api/scenes",json!({"name":name,"rooms":[room],
                        "resource":{"connection_id":connection_id,"resource_id":resource}})));
                }
            } else {
                // Only the connection and the resource. What the child is, and
                // therefore what kind of device this becomes, is the package's
                // to say and the daemon's to stamp.
                app.run(api::post(format!("/api/rooms/{room}/devices"),
                    json!({"name":name,"integration":{"via":"connection","connection_id":connection_id,"resource_id":resource}})));
            }
        }>{if used {"Added"} else {"Add to this room"}}</button>
    </div>}.into_any()
}

/// A device or scene made from a child the connection no longer lists.
fn vanished(app: App, house: &Config, connection: &Connection, child: &Child) -> AnyView {
    let label = super::connections::label(connection);
    let at = child.assigned.clone().unwrap_or_default();
    let scene = at.scene.clone();
    let room = at
        .room
        .as_deref()
        .and_then(|id| house.rooms.iter().find(|r| r.id.as_str() == id))
        .map(|r| r.id.clone());
    let device = at.device.clone();
    let where_it_is = assigned_line(house, &at);
    view! {<div class="card discovered-device"><strong>{child.name.clone()}</strong>
        <p class="notice small child-gone">{format!("No longer reported by {label}.")}</p>
        <p class="dim">{format!("{where_it_is} · Kept until you remove it")}</p>
        {scene.map(|id|ui::danger_button("Remove this scene",move ||app.run(api::delete(format!("/api/scenes/{id}")))))}
        {room.zip(device).map(|(room,device)|ui::danger_button("Remove this device",
            move ||app.run(api::delete(format!("/api/rooms/{room}/devices/{device}")))))}
    </div>}.into_any()
}

/// Where a child already is, in this house's own words.
fn assigned_line(house: &Config, at: &Assigned) -> String {
    if let Some(scene) = at.scene.as_deref() {
        return match house.scenes.iter().find(|s| s.id.as_str() == scene) {
            Some(scene) => format!("Already a scene · {}", scene.name),
            None => "Already a scene".into(),
        };
    }
    let room = at
        .room
        .as_deref()
        .and_then(|id| house.rooms.iter().find(|r| r.id.as_str() == id))
        .map(|r| r.name.clone());
    let device = at
        .device
        .as_deref()
        .and_then(|id| house.devices().find(|(_, d)| d.id.as_str() == id))
        .map(|(_, d)| d.name.clone());
    match (room, device) {
        (Some(room), Some(device)) => format!("Already in {room} · {device}"),
        (Some(room), None) => format!("Already in {room}"),
        _ => "Already in this home".into(),
    }
}

/// The confirmation that names everything "Add all shown" would add, and the
/// progress line of the run it starts.
fn batch_panel(app: App, state: State, room: &Id, connection: &str, room_name: &str) -> AnyView {
    let Some(batch) = state.batch.get() else {
        return ().into_any();
    };
    if &batch.room != room || batch.connection != connection {
        return ().into_any();
    }
    if !batch.confirming {
        return view! {<p class="notice small" role="status">{batch.message.clone()}</p>}
            .into_any();
    }
    let question = confirm_question(batch.items.len(), room_name);
    let names: Vec<String> = batch.items.iter().map(|(_, name)| name.clone()).collect();
    // The question takes the focus when it appears, so somebody on a keyboard
    // is asked rather than left to find it, and Escape - which any control
    // inside bubbles up to - says no.
    let asking: NodeRef<leptos::html::Section> = NodeRef::new();
    Effect::new(move |_| {
        if let Some(section) = asking.get() {
            let _ = section.focus();
        }
    });
    view! {<section node_ref=asking tabindex="-1" class="card confirm-add" role="dialog" aria-label=question.clone()
            on:keydown=move |e|if e.key()=="Escape" {state.batch.set(None)}>
        <h3>{question.clone()}</h3>
        <ul class="rows">{names.into_iter().map(|name|view!{<li class="row"><span class="row-title">{name}</span></li>}).collect_view()}</ul>
        <div class="actions">
            <button class="primary" on:click=move |_|start(app,state)>"Add them"</button>
            <button class="ghost" on:click=move |_|state.batch.set(None)>"Cancel"</button>
        </div>
    </section>}.into_any()
}

fn start(app: App, state: State) {
    if app.busy.get_untracked() {
        return;
    }
    state.batch.update(|batch| {
        if let Some(batch) = batch {
            batch.confirming = false;
            batch.running = true;
            batch.done = 0;
            batch.message = String::new();
        }
    });
    app.busy.set(true);
    step(app, state);
}

/// One save at a time, so a failure says exactly how far it got and nothing
/// after it is sent.
fn step(app: App, state: State) {
    let Some(batch) = state.batch.get_untracked() else {
        app.busy.set(false);
        return;
    };
    let Some((resource, name)) = batch.items.get(batch.done).cloned() else {
        finish(
            app,
            state,
            format!(
                "Added {} device{} to this room.",
                batch.done,
                if batch.done == 1 { "" } else { "s" }
            ),
        );
        return;
    };
    let (room, connection, at, total) = (
        batch.room.clone(),
        batch.connection.clone(),
        batch.done,
        batch.items.len(),
    );
    state.batch.update(|batch| {
        if let Some(batch) = batch {
            batch.message = format!("Adding {} of {total}: {name}…", at + 1);
        }
    });
    spawn_local(async move {
        let body = json!({"name":name,"integration":{"via":"connection","connection_id":connection,"resource_id":resource}});
        match api::post(format!("/api/rooms/{room}/devices"), body).await {
            Ok(config) => {
                app.config.set(Some(config));
                state.batch.update(|batch| {
                    if let Some(batch) = batch {
                        batch.done += 1;
                    }
                });
                step(app, state);
            }
            Err(error) if error.unauthorized => {
                app.paired.set(Some(false));
                finish(app, state, String::new());
            }
            Err(error) => finish(
                app,
                state,
                format!(
                    "Added {at} of {total}. {name} was refused: {}. Nothing after it was added.",
                    refusal(&error)
                ),
            ),
        }
    });
}

fn finish(app: App, state: State, message: String) {
    state.batch.update(|batch| {
        if let Some(batch) = batch {
            batch.running = false;
            batch.confirming = false;
            if !message.is_empty() {
                batch.message = message;
            }
        }
    });
    app.busy.set(false);
}

// ---------------------------------------------------------------------------
// Driving one child
// ---------------------------------------------------------------------------

/// The panel under a room device that is one child of a packaged connection.
/// `().into_any()` for every other device, which is every device a shipped
/// build has.
pub(super) fn controls(app: App, config: &Config, device: &Device) -> AnyView {
    let Integration::Connection {
        connection_id,
        resource_id,
        child: Some(snapshot),
    } = &device.integration
    else {
        return ().into_any();
    };
    let state = expect_context::<State>();
    let pairing = expect_context::<super::plugin_pairing::State>();
    let held = StoredValue::new(connection_id.to_string());
    let connection = config.connection(connection_id);
    let label = connection
        .map(super::connections::label)
        .unwrap_or_else(|| connection_id.to_string());
    let kind = config.device_child_kind(&device.integration);
    let id = connection_id.to_string();
    load(app, state, pairing, id.clone(), false, true);

    // OWNER DECISION: a saved device the connection has stopped listing is
    // badged where it lives, and only ever removed by hand.
    let resource = resource_id.clone();
    let gone = {
        let id = id.clone();
        move || {
            listing_of(state, &id)
                .is_some_and(|listing| listing.missing.iter().any(|child| child.id == resource))
        }
    };
    let badge = view! {<Show when=move ||gone()>
        <p class="notice small child-gone">{format!("No longer reported by {label}.")}
        " Its keys and scenes are kept; remove it below when you no longer want it."</p>
    </Show>};

    let Some(kind) = kind else {
        return view! {{badge}<p class="dim small">"This integration no longer describes this device."</p>}.into_any();
    };
    let base = StoredValue::new(format!(
        "/api/connections/{connection_id}/plugin/children/{resource_id}"
    ));
    let component = kind.component;
    let capabilities: Vec<(String, String)> = kind
        .capabilities
        .iter()
        .map(|c| (c.id.clone(), c.label.clone()))
        .collect();
    let typed = |wanted: ActionKind| kind.actions.iter().any(|a| a.kind() == wanted);
    let status = RwSignal::new(None::<Value>);
    let message = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    // A write may answer with the state it left, and that is the state shown:
    // no read follows one, and nothing is ever assumed from what was asked
    // for. A write that only says "accepted" is followed by one read.
    let perform = move |verb: &'static str, body: Option<Value>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(String::new());
        let method = if body.is_some() { "POST" } else { "GET" };
        let path = format!("{}/{verb}", base.get_value());
        quietly(method, path, body, BUSY_TRIES, move |answer| match answer {
            Ok(value) if value.get("accepted").and_then(Value::as_bool) == Some(true) => {
                quietly(
                    "GET",
                    format!("{}/status", base.get_value()),
                    None,
                    BUSY_TRIES,
                    move |answer| {
                        adopt(app, pairing, held, status, message, answer);
                        busy.set(false);
                    },
                );
            }
            answer => {
                adopt(app, pairing, held, status, message, answer);
                busy.set(false);
            }
        });
    };
    let command = move |id: String| perform("action", Some(json!({ "command": id })));
    let act = move |action: Value| perform("typed-action", Some(action));

    let keys = capabilities.clone();
    let panel = match component {
        ChildComponent::Light => {
            let traits = snapshot.light.unwrap_or_default();
            let can_set = typed(ActionKind::SetLight);
            let level = RwSignal::new(50u8);
            let mirek = traits.mirek.filter(|_| can_set);
            let warm = RwSignal::new(mirek.map(|(low, high)| low + (high - low) / 2).unwrap_or(0));
            view! {
                {(can_set && traits.dimmable).then(move ||view!{
                    <label class="field">{move ||format!("Brightness · {}%",level.get())}
                        <input type="range" aria-label="Brightness" min="0" max="100" step="1"
                            prop:value=move ||level.get().to_string() disabled=move ||busy.get()
                            on:input=move |e|{if let Ok(v)=event_target_value(&e).parse::<u8>(){level.set(v)}}
                            on:change=move |e|{if let Ok(v)=event_target_value(&e).parse::<u8>(){level.set(v);act(json!({"action":"set_light","brightness":v}))}}/>
                    </label>})}
                {mirek.map(move |(low,high)|view!{
                    <label class="field">{move ||format!("Colour temperature · {} mirek",warm.get())}
                        <input type="range" aria-label="Colour temperature" min=low.to_string() max=high.to_string() step="1"
                            prop:value=move ||warm.get().to_string() disabled=move ||busy.get()
                            on:input=move |e|{if let Ok(v)=event_target_value(&e).parse::<u16>(){warm.set(v)}}
                            on:change=move |e|{if let Ok(v)=event_target_value(&e).parse::<u16>(){warm.set(v);act(json!({"action":"set_light","mirek":v}))}}/>
                    </label>})}
            }
            .into_any()
        }
        ChildComponent::Cover => {
            let traits = snapshot.cover.unwrap_or_default();
            let can_set = typed(ActionKind::SetCover);
            let where_it_is = RwSignal::new(50u8);
            view! {{(can_set && traits.position).then(move ||view!{
                <label class="field">{move ||format!("Position · {}% open",where_it_is.get())}
                    <input type="range" aria-label="Position" min="0" max="100" step="1"
                        prop:value=move ||where_it_is.get().to_string() disabled=move ||busy.get()
                        on:input=move |e|{if let Ok(v)=event_target_value(&e).parse::<u8>(){where_it_is.set(v)}}
                        on:change=move |e|{if let Ok(v)=event_target_value(&e).parse::<u8>(){where_it_is.set(v);act(json!({"action":"set_cover","position":v}))}}/>
                </label>})}}
            .into_any()
        }
        // Wave 3 packages a thermostat; until then this says so rather than
        // offering half a control.
        ChildComponent::Climate => view! {
            <p class="dim small">"Controls for thermostats arrive with a later update."</p>
        }
        .into_any(),
        ChildComponent::Scene => ().into_any(),
    };

    view! {<div class="child-controls">{badge}
        // Protocol 3 (unreleased): a connection whose key the device no
        // longer honours says so wherever it is used, not only on its page.
        {super::plugin_pairing::banner(app,pairing,&held.get_value(),format!("/api/connections/{}/plugin",held.get_value()))}
        <p>{move ||state_line(status.get().as_ref(),component)}</p>
        <div class="actions">
            {keys.into_iter().filter(|(id,_)|matches!(id.as_str(),"on"|"off"|"toggle"|"open"|"close"|"stop"))
                .map(move |(id,label)|view!{<button class="ghost" disabled=move ||busy.get()
                    on:click=move |_|command(id.clone())>{label}</button>}).collect_view()}
            <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform("status",None)>"Refresh state"</button>
        </div>
        {panel}
        <p role="status">{move ||message.get()}</p>
    </div>}.into_any()
}

/// Take a status reply as the state, or show why there is none.
fn adopt(
    app: App,
    pairing: super::plugin_pairing::State,
    connection: StoredValue<String>,
    status: RwSignal<Option<Value>>,
    message: RwSignal<String>,
    answer: Result<Value, api::ApiError>,
) {
    match answer {
        Ok(value) => status.set(Some(value)),
        Err(error) => {
            if error.unauthorized {
                app.paired.set(Some(false));
            }
            super::plugin_pairing::noticed(pairing, &connection.get_value(), &error);
            message.set(refusal(&error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Listing {
        serde_json::from_value(json!({
            "kinds": [
                {"kind":"light","label":"Light","device_kind":"light","component":"light",
                 "capabilities":[{"id":"toggle","label":"Toggle"}],"actions":[{"action":"set_light"}]},
                {"kind":"blind","label":"Blind","device_kind":"blind","component":"cover"},
                {"kind":"scene","label":"Scene","device_kind":"other","component":"scene"}
            ],
            "children": [
                {"id":"lamp/1","kind":"light","name":"Desk","room_hint":"Study",
                 "light":{"dimmable":true,"mirek":[153,500]},"assigned":null},
                {"id":"lamp/2","kind":"light","name":"Reading","room_hint":"kitchen ",
                 "light":{"dimmable":false},"assigned":null},
                {"id":"lamp/3","kind":"light","name":"Counter","room_hint":"kitchen ",
                 "assigned":{"room":"kitchen","device":"kitchen-counter"}},
                {"id":"blind/1","kind":"blind","name":"Kitchen blind","room_hint":"kitchen ",
                 "cover":{"position":true,"stop":true},"assigned":null},
                {"id":"scene/1","kind":"scene","name":"Kitchen relax","room_hint":"kitchen ",
                 "assigned":null},
                {"id":"lamp/4","kind":"light","name":"Hall","assigned":null}
            ],
            "missing": [{"id":"lamp/9","kind":"light","name":"Corner lamp",
                         "assigned":{"room":"kitchen","device":"kitchen-corner"}}],
            "fetched_ms": 0
        }))
        .unwrap()
    }

    /// The daemon says more about a kind and a child than this page reads, and
    /// a later one will say more still. Neither may stop a list being shown.
    #[test]
    fn a_listing_is_read_without_insisting_on_the_fields_it_does_not_use() {
        let listing = listing();
        assert_eq!(listing.children.len(), 6);
        assert_eq!(listing.missing.len(), 1);
        assert_eq!(listing.children[0].light.unwrap().mirek, Some((153, 500)));
        assert!(listing.children[3].cover.unwrap().stop);
        assert_eq!(
            listing.children[2]
                .assigned
                .as_ref()
                .unwrap()
                .device
                .as_deref(),
            Some("kitchen-counter")
        );
        // A child a rollback stripped arrives without a kind at all.
        let stripped: Child =
            serde_json::from_value(json!({"id":"lamp/9","kind":null,"name":"Corner"})).unwrap();
        assert_eq!(stripped.kind, None);
        assert_eq!(component(&listing.kinds, stripped.kind.as_deref()), "");
        assert_eq!(
            kind_label(&listing.kinds, stripped.kind.as_deref()),
            "Device"
        );
        // A kind this page has never heard of is still listed, under its id.
        assert_eq!(kind_label(&listing.kinds, Some("siren")), "siren");
    }

    #[test]
    fn the_rooms_offered_are_the_distinct_ones_the_connection_reports() {
        assert_eq!(hints(&listing().children), vec!["Study", "kitchen "]);
        assert!(hints(&[Child::default()]).is_empty());
    }

    /// OWNER DECISION: a room of the same name, whatever its case or spacing,
    /// is the one the picker opens on. Nothing else preselects anything.
    #[test]
    fn the_room_with_this_rooms_name_is_the_one_preselected() {
        let hints = hints(&listing().children);
        assert_eq!(preselect(&hints, "Kitchen"), "kitchen ");
        assert_eq!(preselect(&hints, "  KITCHEN "), "kitchen ");
        assert_eq!(preselect(&hints, "study"), "Study");
        assert_eq!(preselect(&hints, "Living room"), "");
        assert_eq!(preselect(&hints, ""), "");
        assert_eq!(preselect(&[], "Kitchen"), "");
    }

    #[test]
    fn the_search_covers_the_name_and_the_room_the_connection_reports() {
        let listing = listing();
        let names = |search: &str, kind: &str, hint: &str| {
            shown(&listing.children, search, kind, hint)
                .into_iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("", "", "").len(), 6);
        assert_eq!(names("desk", "", ""), vec!["Desk"]);
        assert_eq!(names("DESK", "", ""), vec!["Desk"]);
        // The hint is searched too, and every word has to match.
        assert_eq!(
            names("kitchen", "", ""),
            vec!["Reading", "Counter", "Kitchen blind", "Kitchen relax"]
        );
        assert_eq!(names("kitchen blind", "", ""), vec!["Kitchen blind"]);
        assert_eq!(names("", "blind", ""), vec!["Kitchen blind"]);
        assert_eq!(
            names("", "", "kitchen "),
            vec!["Reading", "Counter", "Kitchen blind", "Kitchen relax"]
        );
        assert_eq!(names("", "light", "kitchen "), vec!["Reading", "Counter"]);
        assert!(names("nothing here", "", "").is_empty());
    }

    /// OWNER DECISION: "Add all shown" is exactly what is on screen, minus
    /// what is already in the house and minus the scenes.
    #[test]
    fn add_all_is_the_shown_unassigned_devices_and_never_a_scene() {
        let listing = listing();
        let plan = |search: &str, kind: &str, hint: &str| {
            add_all(&listing, search, kind, hint)
                .into_iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            plan("", "", ""),
            vec!["Desk", "Reading", "Kitchen blind", "Hall"]
        );
        // The counter lamp is already a device and the relax scene is a scene.
        assert_eq!(plan("", "", "kitchen "), vec!["Reading", "Kitchen blind"]);
        assert_eq!(plan("", "scene", ""), Vec::<&str>::new());
        assert_eq!(plan("counter", "", ""), Vec::<&str>::new());
    }

    #[test]
    fn the_confirmation_counts_and_names_the_room() {
        assert_eq!(
            confirm_question(7, "Kitchen"),
            "Add these 7 devices to Kitchen?"
        );
        assert_eq!(
            confirm_question(1, "Kitchen"),
            "Add this device to Kitchen?"
        );
    }

    #[test]
    fn a_refusal_adds_the_packages_own_words_only_when_they_say_more() {
        let refused = |message: &str, reason| api::ApiError {
            message: message.into(),
            reason,
            status: 502,
            code: Some("rejected".into()),
            unauthorized: false,
            stale: false,
            busy: false,
        };
        assert_eq!(
            refusal(&refused("The bridge refused", None)),
            "The bridge refused"
        );
        assert_eq!(
            refusal(&refused(
                "The device refused the request",
                Some(api::Reason::Message {
                    text: "That lamp is not on the bridge any more".into()
                })
            )),
            "The device refused the request (That lamp is not on the bridge any more)"
        );
        // The daemon puts the same words in both places for a screen that only
        // shows a sentence; they are not worth saying twice.
        assert_eq!(
            refusal(&refused(
                "Pair this bridge again",
                Some(api::Reason::Message {
                    text: "Pair this bridge again".into()
                })
            )),
            "Pair this bridge again"
        );
    }

    #[test]
    fn a_state_line_never_invents_what_a_child_did_not_report() {
        let line = |status: Value, component| state_line(Some(&status), component);
        assert_eq!(
            state_line(None, ChildComponent::Light),
            "State not read yet"
        );
        assert_eq!(line(json!({}), ChildComponent::Light), "Unavailable");
        assert_eq!(
            line(
                json!({"light":{"on":true,"brightness":60}}),
                ChildComponent::Light
            ),
            "On · 60%"
        );
        assert_eq!(
            line(json!({"light":{"on":true}}), ChildComponent::Light),
            "On"
        );
        assert_eq!(
            line(
                json!({"light":{"on":false,"brightness":60}}),
                ChildComponent::Light
            ),
            "Off"
        );
        assert_eq!(
            line(
                json!({"cover":{"open":true,"position":40}}),
                ChildComponent::Cover
            ),
            "Open · 40% open"
        );
        assert_eq!(
            line(json!({"cover":{"open":false}}), ChildComponent::Cover),
            "Closed"
        );
        assert_eq!(
            line(
                json!({"climate":{"available":true,"mode":"heat_cool","current_tenths":195,"target_tenths":210}}),
                ChildComponent::Climate
            ),
            "heat cool · 19.5° now · 21.0° set"
        );
        assert_eq!(
            line(
                json!({"climate":{"available":false}}),
                ChildComponent::Climate
            ),
            "Unavailable"
        );
    }
}
