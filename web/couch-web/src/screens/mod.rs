//! The screens, and the bits more than one of them needs.
//!
//! A screen is built once, when its route is opened, and updates itself after
//! that: it reads the slice memos on [`App`] inside its own closures, its lists
//! are keyed `<For>`s over ids, and each row reads its item through
//! [`App::room`] and friends. Nothing here may read a slice while it is being
//! constructed - that would make the router's closure depend on the document
//! and rebuild the whole screen on every write, which is what this replaced.
//!
//! [`keyed`] is the old behaviour, for screens not converted yet: they still
//! take a `&Config` and are redrawn whole when the revision changes.
//!
//! Transient editor state - the open tab, the open IR editor, a filter box - is
//! what a user is in the middle of rather than anything the house knows about.
//! Each screen declares its own and [`provide_editor_state`] creates it at the
//! root, so it also survives leaving a screen and coming back to it.

pub mod updates;
pub mod activities;
mod activity_buttons;
mod area_shortcuts;
mod activity_sequences;
pub mod areas;
mod home_assistant;
pub mod overview;
pub mod rooms;
pub mod scenes;

use couch_model::{Config, Id};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::route::Route;
use crate::App;

/// Create the transient editor state every screen reads through context.
///
/// Called once, from the root component, so it outlives both a screen being
/// left and a screen still on the keyed path being redrawn.
pub fn provide_editor_state() {
    provide_context(activities::State::new());
    provide_context(activity_sequences::State::new());
    provide_context(device_picker::State::new());
    provide_context(infrared::State::new());
}

pub fn render(app: App, route: Route) -> AnyView {
    match route {
        Route::Overview => overview::overview(app),
        Route::Rooms => overview::rooms(app),
        Route::Connections => overview::connections(app),
        Route::Connection(id) => connections::detail(app, id),
        Route::Settings => remote::screen(app),
        Route::Updates => updates::screen(app),
        Route::Areas => areas::list(app),
        Route::Area(id) => areas::detail(app, id),
        Route::Room(id) => rooms::detail(app, id),
        Route::Scene(id) => scenes::detail(app, id),
        Route::Activities => activities::list(app),
        Route::Activity(id) => activities::detail(app, id),
        Route::NotFound => gone(app, "That page does not exist."),
    }
}

/// A screen, or one block of a converted screen, that still reads the whole
/// document when it is built.
///
/// It is thrown away and drawn again whenever the revision changes, which is
/// what every screen did before the slices existed. Converting a screen means
/// reading the slices it draws inside its own closures and dropping this.
pub fn keyed(app: App, draw: impl Fn(App, &Config) -> AnyView + Send + Sync + 'static) -> AnyView {
    view! {
        {move || {
            app.revision.track();
            app.config.with_untracked(|c| c.as_ref().map(|config| draw(app, config)))
        }}
    }
    .into_any()
}

/// Shown when a detail screen's subject is not in the config.
///
/// The common way to get here is deleting a thing and then pressing Back onto
/// its own page, which is a normal gesture and should not look like a crash.
pub fn gone(app: App, message: &'static str) -> AnyView {
    view! {
        <div class="pad">
            <p class="dim">{message}</p>
            <button class="primary" on:click=move |_| app.go(Route::Areas)>"Back to areas"</button>
        </div>
    }
    .into_any()
}

/// A `<select>` over every device in the house, grouped by room.
///
/// Scene steps and activity sources both name a device without naming its
/// room, so the room only appears as an optgroup label.
///
/// `sticks` says whether the config is going to come back holding what was
/// picked. It does for a source device, and it does not for an "add a step"
/// picker, which must go back to its placeholder - see [`reset_to`].
pub fn device_select(
    config: &Config,
    selected: Option<&Id>,
    allow_none: bool,
    sticks: bool,
    commit: impl Fn(Option<Id>) + 'static,
) -> AnyView {
    let selected = selected.map(|id| id.to_string()).unwrap_or_default();
    let restore = selected.clone();

    let groups: Vec<(String, Vec<(String, String)>)> = config
        .rooms
        .iter()
        .filter(|room| !room.devices.is_empty())
        .map(|room| {
            (
                room.name.clone(),
                room.devices
                    .iter()
                    .map(|d| (d.id.to_string(), d.name.clone()))
                    .collect(),
            )
        })
        .collect();

    let current = selected.clone();
    view! {
        <select on:change=move |ev| {
            let value = event_target_value(&ev);
            if !sticks {
                reset_to(&ev, &restore);
            }
            commit(if value.is_empty() { None } else { Some(Id::new(value)) })
        }>

            {allow_none.then(|| view! {
                <option value="" selected=current.is_empty()>"(none)"</option>
            })}
            {groups
                .into_iter()
                .map(|(room, devices)| {
                    let selected = selected.clone();
                    view! {
                        <optgroup label=room>
                            {devices
                                .into_iter()
                                .map(|(id, name)| {
                                    let is = selected == id;
                                    view! { <option value=id selected=is>{name}</option> }
                                })
                                .collect_view()}
                        </optgroup>
                    }
                })
                .collect_view()}
        </select>
    }
    .into_any()
}

/// Put a `<select>` back to the value the config says it has.
///
/// A control whose pick the config does not adopt - "add a step" chooses a
/// device and then leaves - would otherwise sit there showing the last device
/// chosen. That is not only untidy: the view is rebuilt by diffing, and an
/// option's `selected` attribute that did not change between two renders is
/// never written back, so nothing else will ever clear it. It also makes the
/// same device unpickable twice running, because selecting the value a select
/// already holds fires no `change` at all.
fn reset_to(ev: &leptos::ev::Event, value: &str) {
    if let Some(select) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok())
    {
        select.set_value(value);
    }
}

/// The name of a device, wherever it lives.

pub fn device_name(config: &Config, id: &Id) -> String {
    config
        .devices()
        .find(|(_, d)| &d.id == id)
        .map(|(room, d)| format!("{} · {}", d.name, room.name))
        // A step pointing at a device that is gone cannot normally exist -
        // validation rejects it - but a hand-edited file can still produce one,
        // and it should be visible rather than blank.
        .unwrap_or_else(|| format!("{id} (missing)"))
}

/// Move an item within an ordered list, for the up/down buttons.
pub fn moved(list: &[Id], from: usize, delta: isize) -> Option<Vec<Id>> {
    let to = from as isize + delta;
    if to < 0 || to as usize >= list.len() {
        return None;
    }
    let mut next = list.to_vec();
    next.swap(from, to as usize);
    Some(next)
}

/// The reorder pair, rendered only where a move is possible.
pub fn reorder_buttons(
    list: Vec<Id>,
    index: usize,
    commit: impl Fn(Vec<Id>) + Clone + 'static,
) -> AnyView {
    let up = moved(&list, index, -1);
    let down = moved(&list, index, 1);
    let up_commit = commit.clone();
    view! {
        <span class="reorder">
            <button
                class="icon"
                aria-label="Move up"
                disabled=up.is_none()
                on:click=move |_| if let Some(next) = up.clone() { up_commit(next) }
            >"↑"</button>
            <button
                class="icon"
                aria-label="Move down"
                disabled=down.is_none()
                on:click=move |_| if let Some(next) = down.clone() { commit(next) }
            >"↓"</button>
        </span>
    }
    .into_any()
}

/// The ids of a collection, as their own memo: what a keyed `<For>` iterates.
///
/// It only changes when something is added, removed or reordered, so editing
/// one row never makes the list diff itself, let alone rebuild its siblings.
pub fn ids<T: Send + Sync + 'static>(slice: Memo<Vec<T>>, id: fn(&T) -> &Id) -> Memo<Vec<Id>> {
    Memo::new(move |_| slice.with(|items| items.iter().map(|item| id(item).clone()).collect()))
}

/// A room's name, read from the slice.
///
/// Call this from inside a closure rather than `App::room`, which allocates a
/// memo: one per lookup per recomputation is a leak the screen only gives back
/// when it unmounts.
pub fn room_name(app: App, id: &Id) -> String {
    app.rooms.with(|rooms| {
        rooms
            .iter()
            .find(|r| &r.id == id)
            .map(|r| r.name.clone())
            .unwrap_or_default()
    })
}

/// [`device_name`] read from the slices instead of from a document.
pub fn device_label(app: App, id: &Id) -> String {
    device_place(app, id.clone())()
        .map(|(device, room)| format!("{device} · {room}"))
        .unwrap_or_else(|| format!("{id} (missing)"))
}

/// A device's own name and the name of the room it is in, read reactively.
///
/// The pair every picker labels a device with. Both halves come from their own
/// slice, so a room rename updates the label without the row being rebuilt.
pub fn device_place(app: App, id: Id) -> impl Fn() -> Option<(String, String)> + Copy {
    let id = StoredValue::new(id);
    move || {
        let (room, name) = app.devices.with(|all| {
            id.with_value(|id| {
                all.iter()
                    .find(|(_, d)| &d.id == id)
                    .map(|(room, d)| (room.clone(), d.name.clone()))
            })
        })?;
        let room = app
            .rooms
            .with(|rooms| rooms.iter().find(|r| r.id == room).map(|r| r.name.clone()))?;
        Some((name, room))
    }
}

/// [`reorder_buttons`] for a row inside a keyed `<For>`.
///
/// The row does not know its index - that is the point of keying by id - so
/// the buttons find their own place in the order and enable themselves. Moving
/// a neighbour then updates two buttons instead of rebuilding the list.
pub fn reorder_in(
    order: Memo<Vec<Id>>,
    id: Id,
    commit: impl Fn(Vec<Id>) + Clone + Send + Sync + 'static,
) -> AnyView {
    let id = StoredValue::new(id);
    let step = move |delta: isize| {
        order.with(|list| {
            let at = id.with_value(|id| list.iter().position(|other| other == id))?;
            moved(list, at, delta)
        })
    };
    let up_commit = commit.clone();
    view! {
        <span class="reorder">
            <button
                class="icon"
                aria-label="Move up"
                disabled=move || step(-1).is_none()
                on:click=move |_| if let Some(next) = step(-1) { up_commit(next) }
            >"↑"</button>
            <button
                class="icon"
                aria-label="Move down"
                disabled=move || step(1).is_none()
                on:click=move |_| if let Some(next) = step(1) { commit(next) }
            >"↓"</button>
        </span>
    }
    .into_any()
}

/// A select that acts on what is chosen and then goes back to its label.
///
/// Used for "attach something that already exists". It renders as nothing when
/// there is nothing left to attach, which is the common state of a small house
/// and reads better than a disabled control.
pub fn pick_row(
    options: Vec<(String, String)>,
    label: &'static str,
    pick: impl Fn(String) + 'static,
) -> AnyView {
    if options.is_empty() {
        return ().into_any();
    }
    view! {
        <div class="add-row">
            <select
                prop:value=""
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    if !value.is_empty() {
                        // The config signal is replaced by the response, which
                        // rebuilds this control back to its placeholder.
                        pick(value);
                    }
                }
            >
                <option value="" selected=true>{label}</option>
                {options
                    .into_iter()
                    .map(|(id, name)| view! { <option value=id>{name}</option> })
                    .collect_view()}
            </select>
        </div>
    }
    .into_any()
}

/// A "…and N more" style count line.
pub fn counts(parts: &[(usize, &str, &str)]) -> String {
    parts
        .iter()
        .map(|(n, one, many)| format!("{n} {}", if *n == 1 { one } else { many }))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reordering_preserves_members_and_rejects_edges() {
        let ids = vec![Id::new("one"), Id::new("two"), Id::new("three")];
        assert_eq!(moved(&ids, 0, -1), None);
        assert_eq!(moved(&ids, 2, 1), None);
        assert_eq!(
            moved(&ids, 1, -1).unwrap(),
            vec![ids[1].clone(), ids[0].clone(), ids[2].clone()]
        );
    }
}

mod hue;

mod connections;
mod device_picker;

mod appearance;

mod webos;
mod tizen;

mod kodi;

mod remote;
mod remote_device;

mod activity_pages;

mod streaming_tv;

mod infrared;
mod bluetooth;

mod device_commands;

mod sonos;
mod coreelec;
mod protect;
mod matter;
