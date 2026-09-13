//! Areas: the list, and one area's activities, rooms and scenes.
//!
//! An area does not own any of the three, it references them, so "add a room
//! here" is genuinely two gestures - attach one that exists, or make a new one
//! - and the screen offers both rather than hiding the distinction and
//! creating a duplicate Bedroom the first time somebody sets up an upstairs.
//!
//! The sections are in the order the remote draws them: the activity strip on
//! top, then the rooms, then the scenes. An editor that mirrors the page it
//! edits saves the reader working out which is which.

use couch_model::{Area, Config, Icon, Id};
use leptos::prelude::*;
use serde_json::json;

use crate::route::Route;
use crate::screens::{counts, gone, pick_row, reorder_buttons};
use crate::{api, ui, App};

pub fn list(app: App, config: &Config) -> AnyView {
    let rows = config
        .areas
        .iter()
        .enumerate()
        .map(|(index, area)| {
            let id = area.id.clone();
            let open = id.clone();
            let subtitle = counts(&[
                (area.rooms.len(), "room", "rooms"),
                (area.scenes.len(), "scene", "scenes"),
                (area.activities.len(), "activity", "activities"),
            ]);

            let name = area.name.clone();
            let order: Vec<Id> = config.areas.iter().map(|a| a.id.clone()).collect();
            let base = config.clone();
            let reorder = move |order: Vec<Id>| {
                let mut next = base.clone();
                next.areas
                    .sort_by_key(|a| order.iter().position(|id| id == &a.id));
                app.run(api::put("/api/config", next));
            };
            view! {
                <li class="row">
                    {reorder_buttons(order, index, reorder)}
                    <button class="row-main" on:click=move |_| app.go(Route::Area(open.clone()))>
                        <span class="row-title">{name}</span>
                        <span class="row-sub">{subtitle}</span>
                    </button>
                    {ui::danger_button("Delete", move || {
                        app.run(api::delete(format!("/api/areas/{id}")))
                    })}
                </li>
            }
        })
        .collect_view();

    let empty = config.areas.is_empty();
    view! {
        {ui::page_header(app, "Areas".to_string(), None)}

        <p class="dim pad-x">
            "Each area is one remote screen, such as Whole home or Upstairs. Choose its rooms, activity strip and scene shortcuts. Use the arrows to set the left-to-right screen order."
        </p>
        <section class="block">
            <ul class="rows">{rows}</ul>
            {empty.then(|| ui::empty("No custom areas yet. Your rooms are available in All rooms."))}
            {ui::add_row("Area name", "Create area", move |name| {
                app.run(api::post("/api/areas", json!({ "name": name })))
            })}
        </section>
    }
    .into_any()
}

pub fn detail(app: App, config: &Config, id: &Id) -> AnyView {
    let Some(area) = config.area(id) else {
        return gone(app, "That area has been deleted.");
    };
    let id = id.clone();

    view! {
        {ui::page_header(app, area.name.clone(), Some(Route::Areas))}
        {screen_preview(config, area)}
        <p class="lead">"Edit this area’s screen below. Changes to names and order save automatically. Unlink removes only the shortcut from this screen."</p>
        <section class="card">
            {name_and_icon(app, area)}
        </section>

        {ui::section(
            "Activities",
            Some("The strip across the top of this page, when one of them is running."),
            view! {
                <ul class="rows">{activity_rows(app, config, area)}</ul>
                {area.activities.is_empty().then(|| {
                    ui::empty("Nothing on the strip. This page will start at its rooms.")
                })}
                {attach_existing_activity(app, config, area)}
                {new_activity(app, config, area)}
            }
            .into_any(),
        )}

        {ui::section("Rooms", None, view! {
            <ul class="rows">{room_rows(app, config, area)}</ul>
            {area.rooms.is_empty().then(|| ui::empty("No rooms on this screen. Add an existing room below, or create a new one."))}
            {attach_existing_room(app, config, area)}
            {
                let for_new = id.clone();
                ui::add_row("New room name", "Create & add room", move |name| {
                    app.run(api::post(
                        format!("/api/areas/{for_new}/rooms"),
                        json!({ "name": name }),
                    ))
                })
            }
        }
        .into_any())}

        {ui::section("Scenes", None, view! {
            <ul class="rows">{scene_rows(app, config, area)}</ul>
            {attach_existing_scene(app, config, area)}
            {
                let for_new = id.clone();
                ui::add_row("New scene name", "Create & add scene", move |name| {
                    app.run(api::post(
                        format!("/api/areas/{for_new}/scenes"),
                        json!({ "name": name }),
                    ))
                })
            }
        }
        .into_any())}

        {super::area_shortcuts::editor(app, config, area)}

        <div class="pad">
            {ui::danger_button("Delete this area", move || {
                app.go(Route::Areas);
                app.run(api::delete(format!("/api/areas/{id}")));
            })}
            <p class="dim">
                "Deleting an area leaves its rooms, scenes and activities alone - \
                 they belong to the house, not to the area."
            </p>

        </div>
    }
    .into_any()
}

/// The name and icon of an area, saved as one `PUT` so neither can clear the
/// other: the endpoint takes the whole pair.
fn name_and_icon(app: App, area: &Area) -> AnyView {
    let save = {
        let id = area.id.clone();
        move |name: String, icon: Option<Icon>| {
            app.run(api::put(
                format!("/api/areas/{id}"),
                json!({ "name": name, "icon": icon }),
            ));
        }
    };
    let (for_name, for_icon) = (save.clone(), save);
    let current_icon = area.icon;
    let current_name = area.name.clone();

    view! {
        {ui::text_field("Name", area.name.clone(), "Upstairs", move |name| {
            for_name(name, current_icon)
        })}
        {ui::icon_select(area.icon, move |icon| {
            for_icon(current_name.clone(), icon)
        })}
    }
    .into_any()
}

fn room_rows(app: App, config: &Config, area: &Area) -> AnyView {
    let order = area.rooms.clone();
    area.rooms
        .iter()
        .enumerate()
        .map(|(index, room_id)| {
            let Some(room) = config.room(room_id) else {
                return view! { <li class="row dim">{format!("{room_id} (missing)")}</li> }
                    .into_any();
            };
            let area_id = area.id.clone();
            let open = room.id.clone();
            let detach = room.id.clone();
            let detach_area = area_id.clone();
            let summary = match room.device_detail().as_str() {
                "" => room.device_summary(),
                detail => format!("{} · {detail}", room.device_summary()),
            };
            let name = room.name.clone();
            let commit_order = move |next: Vec<Id>| {
                app.run(api::put(format!("/api/areas/{area_id}/rooms"), next));
            };
            view! {
                <li class="row">
                    {reorder_buttons(order.clone(), index, commit_order)}
                    <button class="row-main" on:click=move |_| app.go(Route::Room(open.clone()))>
                        <span class="row-title">{name}</span>
                        <span class="row-sub">{summary}</span>
                    </button>
                    <button
                        class="ghost"
                        title="Remove from this area"
                        on:click=move |_| app.run(api::delete(
                            format!("/api/areas/{detach_area}/rooms/{detach}")
                        ))
                    >"Unlink"</button>
                </li>
            }
            .into_any()
        })
        .collect_view()
        .into_any()
}

fn scene_rows(app: App, config: &Config, area: &Area) -> AnyView {
    let order = area.scenes.clone();
    area.scenes
        .iter()
        .enumerate()
        .map(|(index, scene_id)| {
            let Some(scene) = config.scene(scene_id) else {
                return view! { <li class="row dim">{format!("{scene_id} (missing)")}</li> }
                    .into_any();
            };
            let area_id = area.id.clone();
            let open = scene.id.clone();
            let detach = scene.id.clone();
            let detach_area = area_id.clone();
            let name = scene.name.clone();
            let subtitle = counts(&[(scene.steps.len(), "step", "steps")]);
            let commit_order = move |next: Vec<Id>| {
                app.run(api::put(format!("/api/areas/{area_id}/scenes"), next));
            };
            view! {
                <li class="row">
                    {reorder_buttons(order.clone(), index, commit_order)}
                    <button class="row-main" on:click=move |_| app.go(Route::Scene(open.clone()))>
                        <span class="row-title">{name}</span>
                        <span class="row-sub">{subtitle}</span>
                    </button>
                    <button
                        class="ghost"
                        on:click=move |_| app.run(api::delete(
                            format!("/api/areas/{detach_area}/scenes/{detach}")
                        ))
                    >"Unlink"</button>
                </li>
            }
            .into_any()
        })
        .collect_view()
        .into_any()
}

fn activity_rows(app: App, config: &Config, area: &Area) -> AnyView {
    let order = area.activities.clone();
    area.activities
        .iter()
        .enumerate()
        .map(|(index, activity_id)| {
            let Some(activity) = config.activity(activity_id) else {
                return view! { <li class="row dim">{format!("{activity_id} (missing)")}</li> }
                    .into_any();
            };
            let area_id = area.id.clone();
            let open = activity.id.clone();
            let detach = activity.id.clone();
            let detach_area = area_id.clone();
            let name = activity.name.clone();
            // Whose room it is names it better than a step count would: the
            // same "Watch TV" can exist in two rooms.
            let room = config
                .room(&activity.room)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| format!("{} (missing)", activity.room));
            // An area can list an activity from a room it does not contain -
            // the model allows it - but it is nearly always a mistake, so say
            // so rather than leaving someone to wonder why the strip is odd.
            let subtitle = if area.rooms.contains(&activity.room) {
                room
            } else {
                format!("{room} - not a room in this area")
            };
            let commit_order = move |next: Vec<Id>| {
                app.run(api::put(format!("/api/areas/{area_id}/activities"), next));
            };
            view! {
                <li class="row">
                    {reorder_buttons(order.clone(), index, commit_order)}
                    <button
                        class="row-main"
                        on:click=move |_| app.go(Route::Activity(open.clone()))
                    >
                        <span class="row-title">{name}</span>
                        <span class="row-sub">{subtitle}</span>
                    </button>
                    <button
                        class="ghost"
                        title="Take off this area's strip"
                        on:click=move |_| app.run(api::delete(
                            format!("/api/areas/{detach_area}/activities/{detach}")
                        ))
                    >"Unlink"</button>
                </li>
            }
            .into_any()
        })
        .collect_view()
        .into_any()
}

/// Offered from the area's own rooms only.
///
/// Every activity in the house would technically be attachable, but a strip
/// entry for a room this page does not show is the kind of thing somebody
/// configures once by accident and spends an evening explaining.
fn attach_existing_activity(app: App, config: &Config, area: &Area) -> AnyView {
    let options: Vec<(String, String)> = config
        .activities_hosted_by(area)
        .filter(|activity| !area.activities.contains(&activity.id))
        .map(|activity| (activity.id.to_string(), activity.name.clone()))
        .collect();
    let area_id = area.id.clone();
    pick_row(options, "Add an activity from these rooms", move |id| {
        app.run(api::post(
            format!("/api/areas/{area_id}/activities"),
            json!({ "activity": id }),
        ))
    })
}

/// Creating one from here needs a room, and the area's first is the only
/// defensible guess. It is named in the caption rather than silently applied,
/// and the activity's own screen can move it.
fn new_activity(app: App, config: &Config, area: &Area) -> AnyView {
    if area.rooms.is_empty() {
        return ui::empty("Add a room to this screen before creating an activity here.");
    }
    let room = RwSignal::new(area.rooms[0].to_string());
    let area_id = area.id.clone();
    view! { <section class="creation compact"><h3>"Create an activity for this screen"</h3>
        <label class="field"><span class="label">"Activity room"</span><select prop:value=move || room.get() on:change=move |e| room.set(event_target_value(&e))>
        {config.rooms_in_area(area).map(|r| view! { <option value=r.id.to_string()>{r.name.clone()}</option> }).collect_view()}</select></label>
        {ui::add_row("New activity name", "Create & add activity", move |name| app.run(api::post(format!("/api/areas/{area_id}/activities"), json!({"name":name,"room":room.get()}))))}
    </section> }.into_any()
}

fn screen_preview(config: &Config, area: &Area) -> AnyView {
    view! { <section class="screen-preview" aria-label="Configured screen preview">
        <div><p class="eyebrow">"SCREEN STRUCTURE PREVIEW"</p><h2>{area.name.clone()}</h2><p class="dim">"This shows saved content and order, not live device state. The visual layout is fixed; individual buttons cannot be placed freely."</p></div>
        <div class="remote-outline"><span class="label">"Activity strip · shown when running"</span>
        <div class="preview-chips">{config.activities_in_area(area).map(|a| view! { <span>{a.name.clone()}</span> }).collect_view()}</div>
        <span class="label">"Rooms · top to bottom"</span>
        {config.rooms_in_area(area).map(|r| view! { <div class="preview-room"><strong>{r.name.clone()}</strong><small>{r.device_summary()}</small></div> }).collect_view()}
        <span class="label">"Scene shortcuts"</span><div class="preview-chips">{config.scenes_in_area(area).map(|s| view! { <span>{s.name.clone()}</span> }).collect_view()}</div></div>
    </section> }.into_any()
}

fn attach_existing_room(app: App, config: &Config, area: &Area) -> AnyView {
    let options: Vec<(String, String)> = config
        .rooms
        .iter()
        .filter(|room| !area.rooms.contains(&room.id))
        .map(|room| (room.id.to_string(), room.name.clone()))
        .collect();
    let area_id = area.id.clone();
    pick_row(options, "Add an existing room", move |id| {
        app.run(api::post(
            format!("/api/areas/{area_id}/rooms"),
            json!({ "room": id }),
        ))
    })
}

fn attach_existing_scene(app: App, config: &Config, area: &Area) -> AnyView {
    let options: Vec<(String, String)> = config
        .scenes
        .iter()
        .filter(|scene| !area.scenes.contains(&scene.id))
        .map(|scene| (scene.id.to_string(), scene.name.clone()))
        .collect();
    let area_id = area.id.clone();
    pick_row(options, "Add an existing scene", move |id| {
        app.run(api::post(
            format!("/api/areas/{area_id}/scenes"),
            json!({ "scene": id }),
        ))
    })
}
