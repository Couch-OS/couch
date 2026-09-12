//! The screens, and the bits more than one of them needs.
//!
//! Every screen is a function of `(&Config, Route)`. They hold no state of
//! their own beyond what a half-typed field needs, because the config signal is
//! replaced wholesale after each edit and anything else would have to be
//! reconciled with it.

pub mod updates;
pub mod activities;
mod activity_buttons;
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

pub fn render(app: App, config: &Config, route: Route) -> AnyView {
    match route {
        Route::Overview => overview::overview(app, config),
        Route::Rooms => overview::rooms(app, config),
        Route::Connections => overview::connections(app, config),
        Route::Settings => remote::screen(app, config),
        Route::Updates => updates::screen(app),
        Route::Areas => areas::list(app, config),
        Route::Area(id) => areas::detail(app, config, &id),
        Route::Room(id) => rooms::detail(app, config, &id),
        Route::Scene(id) => scenes::detail(app, config, &id),
        Route::Activities => activities::list(app, config),
        Route::Activity(id) => activities::detail(app, config, &id),
        Route::NotFound => gone(app, "That page does not exist."),
    }
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

mod kodi;

mod remote;

mod activity_pages;

mod streaming_tv;

mod infrared;

mod device_commands;

mod sonos;
mod coreelec;
mod protect;
