//! One room: what it is called, and what is in it.
//!
//! The device list is the interesting part, because it is what the remote's
//! room row is rendered from. The screen shows that rendering back - "5 devices
//! · Kodi, Hue, LG C3" - so the effect of adding a device is visible where it
//! will actually appear, rather than only in a list.

use couch_model::{Config, Device, DeviceKind, Icon, Id, Integration, ALL_DEVICE_KINDS};
use leptos::prelude::*;
use serde_json::json;

use crate::route::Route;
use crate::screens::{counts, gone};
use crate::{api, ui, App};

pub fn detail(app: App, config: &Config, id: &Id) -> AnyView {
    let Some(room) = config.room(id) else {
        return gone(app, "That room has been deleted.");
    };
    let id = id.clone();

    let save = {
        let id = id.clone();
        move |name: String, icon: Option<Icon>| {
            app.run(api::put(
                format!("/api/rooms/{id}"),
                json!({ "name": name, "icon": icon }),
            ));
        }
    };
    let (for_name, for_icon) = (save.clone(), save);
    let current_icon = room.icon;
    let current_name = room.name.clone();

    // Exactly what the remote's row will say, from the model's own helpers.
    let rendered = match room.device_detail().as_str() {
        "" => room.device_summary(),
        detail => format!("{} · {detail}", room.device_summary()),
    };

    let in_areas: Vec<String> = config
        .areas
        .iter()
        .filter(|area| area.rooms.contains(&room.id))
        .map(|area| area.name.clone())
        .collect();

    let add_id = id.clone();
    let delete_id = id.clone();
    // The order the remote lists them in, after the room's pinned activities.
    let device_order: Vec<Id> = room.devices.iter().map(|d| d.id.clone()).collect();

    view! {
        {ui::page_header(app, room.name.clone(), Some(Route::Rooms))}

        <section class="card">
            {ui::text_field("Name", room.name.clone(), "Living room", move |name| {
                for_name(name, current_icon)
            })}
            {ui::icon_select(room.icon, move |icon| for_icon(current_name.clone(), icon))}
            <p class="preview">
                <span class="label">"On the remote"</span>
                <span>{rendered}</span>
            </p>
            <p class="dim">
                {if in_areas.is_empty() {
                    "Available in All rooms on the remote. Add it to a custom area when ready.".to_string()
                } else {
                    format!("Shown in: {}", in_areas.join(", "))
                }}
            </p>
        </section>

        {ui::section(
            view! { "Devices" <span class="count">{counts(&[(room.devices.len(), "device", "devices")])}</span> },
            None,
            view! {
                {room.devices.is_empty().then(|| ui::empty("No devices here yet."))}
                <ul class="rows">
                    {room.devices
                        .iter()
                        .enumerate()
                        .map(|(index, device)| device_card(app, config, &id, device, &device_order, index))
                        .collect_view()}
                </ul>
            }
            .into_any(),
        )}
        {room_scenes(app,config,&add_id)}
        {room_activities(app,config,&add_id)}
        {super::device_picker::picker(app, config, &add_id)}

        <div class="pad">
            {ui::danger_button("Delete this room", move || {
                app.go(Route::Rooms);
                app.run(api::delete(format!("/api/rooms/{delete_id}")));
            })}
            <p class="dim">
                "Deleting this room also deletes its devices and activities, removes it from all screens, and removes commands for its devices from scenes."
            </p>
        </div>
    }
    .into_any()
}

fn device_card(
    app: App,
    config: &Config,
    room: &Id,
    device: &Device,
    order: &[Id],
    index: usize,
) -> AnyView {
    let reorder = {
        let room = room.clone();
        super::reorder_buttons(order.to_vec(), index, move |next: Vec<Id>| {
            app.run(api::put(format!("/api/rooms/{room}/devices"), next));
        })
    };
    let name = RwSignal::new(device.name.clone());
    let kind = RwSignal::new(device.kind);
    let original_name = device.name.clone();
    let original_kind = device.kind;
    let original_icon = device.icon;
    let icon = RwSignal::new(device.icon);
    let base = device.clone();
    let room = room.clone();
    let delete_room = room.clone();
    let delete_id = device.id.clone();
    // The document the screen was drawn from, not a copy of it per row.
    let summary = match &device.integration {
        Integration::Connection { connection_id, .. } => config
            .connection(connection_id)
            .map(super::connections::label),
        _ => None,
    }
    .unwrap_or_else(|| super::overview::connection_summary(&device.integration));
    let summary=if device.effective_ir_codeset(config).is_some(){
        if matches!(config.resolve_integration(&device.integration),Some(Integration::None|Integration::Ir{..})){"Infrared · Built-in transmitter".into()}else{format!("{summary} · IR commands")}
    }else{summary};
    view!{<li class="card device"><div class="device-head">{reorder}<div><h3>{device.name.clone()}</h3><p class="dim">{summary}</p></div></div>
        {super::device_picker::controls(app,config,device)}
        {super::infrared::device_commands(app, config, &room, device)}
        <details><summary>"Edit device"</summary><form on:submit=move |e|{e.prevent_default();let title=name.get_untracked().trim().to_string();if title.is_empty(){return}app.run(api::put(format!("/api/rooms/{room}/devices/{}",base.id),Device{name:title,kind:kind.get_untracked(),icon:icon.get_untracked(),..base.clone()}));}>
        {super::connections::field("Device name",name,"Device name")}
        <label class="field">"Device type"<select aria-label="Device type" prop:value=move ||kind.get().name() on:change=move |e|kind.set(DeviceKind::from_name(&event_target_value(&e)).unwrap_or_default())>{ALL_DEVICE_KINDS.iter().map(|k|view!{<option value=k.name()>{k.name()}</option>}).collect_view()}</select></label>
        {ui::icon_select_signal(icon, move |_| {})}
        <p class="dim">"Manage server and bridge settings in Connections. To use another source device, remove this device and add the replacement from its connection."</p>
        <button class="primary" type="submit">"Save device"</button><button class="ghost" type="button" on:click=move |_|{name.set(original_name.clone());kind.set(original_kind);icon.set(original_icon);}>"Discard changes"</button></form></details>
        {ui::danger_button("Delete device",move ||app.run(api::delete(format!("/api/rooms/{delete_room}/devices/{delete_id}"))))}
    </li>}.into_any()
}

fn room_scenes(app: App, config: &Config, room: &Id) -> AnyView {
    let scenes: Vec<_> = config
        .scenes
        .iter()
        .filter(|s| s.rooms.contains(room))
        .collect();
    ui::section(view!{"Scenes" <span class="count">{scenes.len()}</span>},
        Some("Shown in the Scenes button at the bottom of this room on the remote."),
        view!{
        {scenes.is_empty().then(||ui::empty("No scenes here yet. Choose Hue scenes below to add one."))}
        <ul class="rows room-scenes">{scenes.into_iter().map(|scene|{let open=scene.id.clone();let name=scene.name.clone();let mut next=scene.clone();next.rooms.retain(|r|r!=room);view!{<li class="row"><button class="row-main" on:click=move |_|app.go(Route::Scene(open.clone()))><span class="row-title">{name}</span></button><button class="ghost" disabled=move ||app.busy.get() on:click=move |_|app.run(api::put(format!("/api/scenes/{}",next.id),next.clone()))>"Remove from room"</button></li>}}).collect_view()}</ul>
    }.into_any())
}

fn room_activities(app: App, config: &Config, room: &Id) -> AnyView {
    let here: Vec<_> = config
        .activities
        .iter()
        .filter(|a| &a.room == room)
        .collect();
    let elsewhere: Vec<_> = config
        .activities
        .iter()
        .filter(|a| &a.room != room)
        .cloned()
        .collect();
    let create_room = room.clone();
    let move_room = room.clone();
    view! {
        <section class="room-activities block">
            <h2 class="section">"Activities" <span class="count">{here.len()}</span></h2>
            <p class="dim">"Activities belong to a room and can also appear in areas. Open one to choose its source device and startup commands."</p>
            <ul class="rows">{here.into_iter().map(|activity| {
                let id=activity.id.clone();let name=activity.name.clone();
                view!{<li class="row"><button class="row-main" on:click=move |_|app.go(Route::Activity(id.clone()))><span class="row-title">{name}</span><span class="row-sub">"Edit activity"</span></button></li>}
            }).collect_view()}</ul>
            {ui::add_row("New room activity name", "Create activity in this room", move |name| {
                app.run(api::post("/api/activities",json!({"name":name,"room":create_room})));
            })}
            {(!elsewhere.is_empty()).then(|| {
                let options=elsewhere.iter().map(|a|(a.id.to_string(),format!("{} · {}",a.name,config.room(&a.room).map(|r|r.name.as_str()).unwrap_or("Unknown room")))).collect();
                super::pick_row(options, "Move an existing activity here", move |id| {
                    if let Some(activity)=elsewhere.iter().find(|a|a.id.as_str()==id) {
                        let mut next=activity.clone();next.room=move_room.clone();
                        app.run(api::put(format!("/api/activities/{}",next.id),next));
                    }
                })
            })}
        </section>
    }.into_any()
}
