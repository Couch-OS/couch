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
//!
//! Every row is keyed by the id it references and reads the referenced thing
//! through its own memo, so unlinking a scene does not redraw the rooms above
//! it and renaming a room updates one line.

use couch_model::{Area, Id};
use leptos::prelude::*;
use serde_json::json;

use crate::route::Route;
use crate::screens::{counts, gone, ids, keyed, pick_row, reorder_in, room_name};
use crate::{api, ui, App};

pub fn list(app: App) -> AnyView {
    let order = ids(app.areas, |a| &a.id);
    view! {
        {ui::page_header(app, "Areas", None)}

        <p class="dim pad-x">
            "Each area is one remote screen, such as Whole home or Upstairs. Choose its rooms, activity strip and scene shortcuts. Use the arrows to set the left-to-right screen order."
        </p>
        <section class="block">
            <ul class="rows"><For each=move ||order.get() key=|id|id.clone() children=move |id|{
                let area = app.area(id.clone());
                let open = id.clone();
                let delete = id.clone();
                // Ordering an area means reordering the document's own list, so
                // this one sends the whole house back - read on the click, not
                // captured when the row was drawn.
                let reorder = reorder_in(order, id, move |order: Vec<Id>| {
                    let mut next = app.house();
                    next.areas.sort_by_key(|a| order.iter().position(|id| id == &a.id));
                    app.run(api::put("/api/config", next));
                });
                view! {
                    <li class="row">
                        {reorder}
                        <button class="row-main" on:click=move |_| app.go(Route::Area(open.clone()))>
                            <span class="row-title">{move ||area.get().map(|a|a.name)}</span>
                            <span class="row-sub">{move ||area.get().map(|a|counts(&[
                                (a.rooms.len(), "room", "rooms"),
                                (a.scenes.len(), "scene", "scenes"),
                                (a.activities.len(), "activity", "activities"),
                            ]))}</span>
                        </button>
                        {ui::danger_button("Delete", move || {
                            app.run(api::delete(format!("/api/areas/{delete}")))
                        })}
                    </li>
                }
            }/></ul>
            {move ||order.with(Vec::is_empty).then(|| ui::empty("No custom areas yet. Your rooms are available in All rooms."))}
            {ui::add_row("Area name", "Create area", move |name| {
                app.run(api::post("/api/areas", json!({ "name": name })))
            })}
        </section>
    }
    .into_any()
}

pub fn detail(app: App, id: Id) -> AnyView {
    let area = app.area(id.clone());
    view! {
        <Show
            when=move || area.with(Option::is_some)
            fallback=move || gone(app, "That area has been deleted.")
        >
            {page(app, id.clone())}
        </Show>
    }
    .into_any()
}

/// Nothing here may read a slice while it is being built - see [`super::rooms`].
fn page(app: App, id: Id) -> AnyView {
    let area = app.area(id.clone());
    let key = StoredValue::new(id);
    let members = |pick: fn(&Area) -> &Vec<Id>, area: Memo<Option<Area>>| {
        Memo::new(move |_| area.with(|a| a.as_ref().map(|a| pick(a).clone()).unwrap_or_default()))
    };
    let rooms = members(|a| &a.rooms, area);
    let scenes = members(|a| &a.scenes, area);
    let activities = members(|a| &a.activities, area);

    view! {
        {ui::page_header(app, move ||area.get().map(|a|a.name), Some(Route::Areas))}
        {screen_preview(app, area, rooms, scenes, activities)}
        <p class="lead">"Edit this area’s screen below. Changes to names and order save automatically. Unlink removes only the shortcut from this screen."</p>
        <section class="card">
            {name_and_icon(app, area, key)}
        </section>

        {ui::section(
            "Activities",
            Some("The strip across the top of this page, when one of them is running."),
            view! {
                <ul class="rows"><For each=move ||activities.get() key=|id|id.clone()
                    children=move |id| activity_row(app, key, activities, rooms, id)/></ul>
                {move ||activities.with(Vec::is_empty).then(|| {
                    ui::empty("Nothing on the strip. This page will start at its rooms.")
                })}
                {attach_existing_activity(app, key, rooms, activities)}
                {new_activity(app, key, rooms)}
            }
            .into_any(),
        )}

        {ui::section("Rooms", None, view! {
            <ul class="rows"><For each=move ||rooms.get() key=|id|id.clone()
                children=move |id| room_row(app, key, rooms, id)/></ul>
            {move ||rooms.with(Vec::is_empty).then(|| ui::empty("No rooms on this screen. Add an existing room below, or create a new one."))}
            {attach_existing_room(app, key, rooms)}
            {ui::add_row("New room name", "Create & add room", move |name| {
                app.run(api::post(
                    format!("/api/areas/{}/rooms", key.get_value()),
                    json!({ "name": name }),
                ))
            })}
        }
        .into_any())}

        {ui::section("Scenes", None, view! {
            <ul class="rows"><For each=move ||scenes.get() key=|id|id.clone()
                children=move |id| scene_row(app, key, scenes, id)/></ul>
            {attach_existing_scene(app, key, scenes)}
            {ui::add_row("New scene name", "Create & add scene", move |name| {
                app.run(api::post(
                    format!("/api/areas/{}/scenes", key.get_value()),
                    json!({ "name": name }),
                ))
            })}
        }
        .into_any())}

        // Not converted: the shortcut editor walks areas, rooms, devices and
        // activities together to describe what a key does.
        {keyed(app, move |app, config| match config.area(&key.get_value()) {
            Some(area) => super::area_shortcuts::editor(app, config, area),
            None => ().into_any(),
        })}

        <div class="pad">
            {ui::danger_button("Delete this area", move || {
                app.go(Route::Areas);
                app.run(api::delete(format!("/api/areas/{}", key.get_value())));
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
fn name_and_icon(app: App, area: Memo<Option<Area>>, key: StoredValue<Id>) -> AnyView {
    let put = move |body: serde_json::Value| {
        app.run(api::put(format!("/api/areas/{}", key.get_value()), body));
    };
    let name = Memo::new(move |_| area.with(|a| a.as_ref().map(|a| a.name.clone())));
    let icon = RwSignal::new(area.with_untracked(|a| a.as_ref().and_then(|a| a.icon)));
    view! {
        {move || ui::text_field("Name", name.get().unwrap_or_default(), "Upstairs", move |name| {
            put(json!({ "name": name, "icon": icon.get_untracked() }))
        })}
        {ui::icon_select_signal(icon, move |icon| {
            put(json!({ "name": name.get_untracked().unwrap_or_default(), "icon": icon }))
        })}
    }
    .into_any()
}

fn room_row(app: App, key: StoredValue<Id>, order: Memo<Vec<Id>>, id: Id) -> AnyView {
    let room = app.room(id.clone());
    let open = id.clone();
    let detach = id.clone();
    let reorder = reorder_in(order, id, move |next: Vec<Id>| {
        app.run(api::put(
            format!("/api/areas/{}/rooms", key.get_value()),
            next,
        ));
    });
    view! {
        <li class="row">
            {reorder}
            <button class="row-main" on:click=move |_| app.go(Route::Room(open.clone()))>
                <span class="row-title">{move ||room.get().map(|r|r.name)}</span>
                <span class="row-sub">{move ||room.get().map(|room| match room.device_detail().as_str() {
                    "" => room.device_summary(),
                    detail => format!("{} · {detail}", room.device_summary()),
                })}</span>
            </button>
            <button
                class="ghost"
                title="Remove from this area"
                on:click=move |_| app.run(api::delete(
                    format!("/api/areas/{}/rooms/{detach}", key.get_value())
                ))
            >"Unlink"</button>
        </li>
    }
    .into_any()
}

fn scene_row(app: App, key: StoredValue<Id>, order: Memo<Vec<Id>>, id: Id) -> AnyView {
    let scene = app.scene(id.clone());
    let open = id.clone();
    let detach = id.clone();
    let reorder = reorder_in(order, id, move |next: Vec<Id>| {
        app.run(api::put(
            format!("/api/areas/{}/scenes", key.get_value()),
            next,
        ));
    });
    view! {
        <li class="row">
            {reorder}
            <button class="row-main" on:click=move |_| app.go(Route::Scene(open.clone()))>
                <span class="row-title">{move ||scene.get().map(|s|s.name)}</span>
                <span class="row-sub">{move ||scene.get().map(|s|counts(&[(s.steps.len(), "step", "steps")]))}</span>
            </button>
            <button
                class="ghost"
                on:click=move |_| app.run(api::delete(
                    format!("/api/areas/{}/scenes/{detach}", key.get_value())
                ))
            >"Unlink"</button>
        </li>
    }
    .into_any()
}

fn activity_row(
    app: App,
    key: StoredValue<Id>,
    order: Memo<Vec<Id>>,
    rooms: Memo<Vec<Id>>,
    id: Id,
) -> AnyView {
    let activity = app.activity(id.clone());
    let open = id.clone();
    let detach = id.clone();
    let reorder = reorder_in(order, id, move |next: Vec<Id>| {
        app.run(api::put(
            format!("/api/areas/{}/activities", key.get_value()),
            next,
        ));
    });
    // Whose room it is names it better than a step count would: the same
    // "Watch TV" can exist in two rooms. An area can list an activity from a
    // room it does not contain - the model allows it - but it is nearly always
    // a mistake, so say so rather than leaving someone to wonder.
    let subtitle = move || {
        let activity = activity.get()?;
        let room = match room_name(app, &activity.room) {
            name if name.is_empty() => format!("{} (missing)", activity.room),
            name => name,
        };
        Some(if rooms.with(|here| here.contains(&activity.room)) {
            room
        } else {
            format!("{room} - not a room in this area")
        })
    };
    view! {
        <li class="row">
            {reorder}
            <button
                class="row-main"
                on:click=move |_| app.go(Route::Activity(open.clone()))
            >
                <span class="row-title">{move ||activity.get().map(|a|a.name)}</span>
                <span class="row-sub">{subtitle}</span>
            </button>
            <button
                class="ghost"
                title="Take off this area's strip"
                on:click=move |_| app.run(api::delete(
                    format!("/api/areas/{}/activities/{detach}", key.get_value())
                ))
            >"Unlink"</button>
        </li>
    }
    .into_any()
}

/// Offered from the area's own rooms only.
///
/// Every activity in the house would technically be attachable, but a strip
/// entry for a room this page does not show is the kind of thing somebody
/// configures once by accident and spends an evening explaining.
fn attach_existing_activity(
    app: App,
    key: StoredValue<Id>,
    rooms: Memo<Vec<Id>>,
    listed: Memo<Vec<Id>>,
) -> AnyView {
    view! {{move || {
        let options: Vec<(String, String)> = app.activities.with(|all| all.iter()
            .filter(|a| rooms.with(|here| here.contains(&a.room)))
            .filter(|a| listed.with(|listed| !listed.contains(&a.id)))
            .map(|a| (a.id.to_string(), a.name.clone()))
            .collect());
        pick_row(options, "Add an activity from these rooms", move |id| {
            app.run(api::post(
                format!("/api/areas/{}/activities", key.get_value()),
                json!({ "activity": id }),
            ))
        })
    }}}
    .into_any()
}

/// Creating one from here needs a room, and the area's first is the only
/// defensible guess. It is named in the caption rather than silently applied,
/// and the activity's own screen can move it.
fn new_activity(app: App, key: StoredValue<Id>, rooms: Memo<Vec<Id>>) -> AnyView {
    let room = RwSignal::new(String::new());
    view! {
        <Show
            when=move || !rooms.with(Vec::is_empty)
            fallback=|| ui::empty("Add a room to this screen before creating an activity here.")
        >
            <section class="creation compact"><h3>"Create an activity for this screen"</h3>
            <label class="field"><span class="label">"Activity room"</span><select prop:value=move ||room.get() on:change=move |e|room.set(event_target_value(&e))>
                {move ||rooms.get().into_iter().map(|id|{let name=room_name(app,&id);view!{<option value=id.to_string()>{name}</option>}}).collect_view()}
            </select></label>
            {ui::add_row("New activity name", "Create & add activity", move |name| {
                // Default to the screen's first room, resolved on the click so
                // a room added since the form was drawn is offered.
                let chosen = match room.get_untracked() {
                    picked if !picked.is_empty() => picked,
                    _ => rooms.with_untracked(|r| r.first().map(Id::to_string).unwrap_or_default()),
                };
                app.run(api::post(format!("/api/areas/{}/activities", key.get_value()), json!({"name":name,"room":chosen})))
            })}
            </section>
        </Show>
    }.into_any()
}

fn screen_preview(
    app: App,
    area: Memo<Option<Area>>,
    rooms: Memo<Vec<Id>>,
    scenes: Memo<Vec<Id>>,
    activities: Memo<Vec<Id>>,
) -> AnyView {
    view! { <section class="screen-preview" aria-label="Configured screen preview">
        <div><p class="eyebrow">"SCREEN STRUCTURE PREVIEW"</p><h2>{move ||area.get().map(|a|a.name)}</h2><p class="dim">"This shows saved content and order, not live device state. The visual layout is fixed; individual buttons cannot be placed freely."</p></div>
        <div class="remote-outline"><span class="label">"Activity strip · shown when running"</span>
        <div class="preview-chips">{move ||app.activities.with(|all|activities.get().iter().filter_map(|id|all.iter().find(|a|&a.id==id)).map(|a| view! { <span>{a.name.clone()}</span> }).collect_view())}</div>
        <span class="label">"Rooms · top to bottom"</span>
        {move ||app.rooms.with(|all|rooms.get().iter().filter_map(|id|all.iter().find(|r|&r.id==id)).map(|r| view! { <div class="preview-room"><strong>{r.name.clone()}</strong><small>{r.device_summary()}</small></div> }).collect_view())}
        <span class="label">"Scene shortcuts"</span><div class="preview-chips">{move ||app.scenes.with(|all|scenes.get().iter().filter_map(|id|all.iter().find(|s|&s.id==id)).map(|s| view! { <span>{s.name.clone()}</span> }).collect_view())}</div></div>
    </section> }.into_any()
}

fn attach_existing_room(app: App, key: StoredValue<Id>, listed: Memo<Vec<Id>>) -> AnyView {
    view! {{move || {
        let options: Vec<(String, String)> = app.rooms.with(|all| all.iter()
            .filter(|room| listed.with(|listed| !listed.contains(&room.id)))
            .map(|room| (room.id.to_string(), room.name.clone()))
            .collect());
        pick_row(options, "Add an existing room", move |id| {
            app.run(api::post(
                format!("/api/areas/{}/rooms", key.get_value()),
                json!({ "room": id }),
            ))
        })
    }}}
    .into_any()
}

fn attach_existing_scene(app: App, key: StoredValue<Id>, listed: Memo<Vec<Id>>) -> AnyView {
    view! {{move || {
        let options: Vec<(String, String)> = app.scenes.with(|all| all.iter()
            .filter(|scene| listed.with(|listed| !listed.contains(&scene.id)))
            .map(|scene| (scene.id.to_string(), scene.name.clone()))
            .collect());
        pick_row(options, "Add an existing scene", move |id| {
            app.run(api::post(
                format!("/api/areas/{}/scenes", key.get_value()),
                json!({ "scene": id }),
            ))
        })
    }}}
    .into_any()
}
