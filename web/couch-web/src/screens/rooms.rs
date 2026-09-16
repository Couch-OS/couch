//! One room: what it is called, and what is in it.
//!
//! The device list is the interesting part, because it is what the remote's
//! room row is rendered from. The screen shows that rendering back - "5 devices
//! · Kodi, Hue, LG C3" - so the effect of adding a device is visible where it
//! will actually appear, rather than only in a list.
//!
//! Everything reads the room through [`App::room`] inside a closure and the
//! device list is a keyed `<For>`, so saving one device leaves the other rows -
//! their open editors, their half-typed names, the scroll position - alone.

use couch_model::{Device, DeviceKind, Id, Integration, Transport, ALL_DEVICE_KINDS};
use leptos::prelude::*;
use serde_json::json;

use crate::route::Route;
use crate::screens::{counts, gone, ids, keyed, reorder_in};
use crate::{api, ui, App};

pub fn detail(app: App, id: Id) -> AnyView {
    let room = app.room(id.clone());
    view! {
        <Show
            when=move || room.with(Option::is_some)
            fallback=move || gone(app, "That room has been deleted.")
        >
            {page(app, id.clone())}
        </Show>
    }
    .into_any()
}

/// Nothing here may read a slice while it is being built: the `<Show>` above
/// would then depend on the document and take the page down on every write.
fn page(app: App, id: Id) -> AnyView {
    let room = app.room(id.clone());
    let key = StoredValue::new(id);
    let put = move |body: serde_json::Value| {
        app.run(api::put(format!("/api/rooms/{}", key.get_value()), body));
    };
    let name = Memo::new(move |_| room.with(|r| r.as_ref().map(|r| r.name.clone())));
    // The picker owns the icon it shows once it is open, so it is built once
    // and keeps its <details> open across the save it makes itself.
    let icon = RwSignal::new(room.with_untracked(|r| r.as_ref().and_then(|r| r.icon)));
    let saved_name = move || name.get_untracked().unwrap_or_default();

    let devices = Memo::new(move |_| {
        room.with(|r| {
            r.as_ref()
                .map(|r| r.devices.iter().map(|d| d.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
    });

    view! {
        {ui::page_header(app, move || name.get(), Some(Route::Rooms))}

        <section class="card">
            {move || ui::text_field("Name", name.get().unwrap_or_default(), "Living room", move |name| {
                put(json!({ "name": name, "icon": icon.get_untracked() }))
            })}
            {ui::icon_select_signal(icon, move |icon| put(json!({ "name": saved_name(), "icon": icon })))}
            // Exactly what the remote's row will say, from the model's own helpers.
            <p class="preview">
                <span class="label">"On the remote"</span>
                <span>{move || room.get().map(|room| match room.device_detail().as_str() {
                    "" => room.device_summary(),
                    detail => format!("{} · {detail}", room.device_summary()),
                })}</span>
            </p>
            <p class="dim">
                {move || {
                    let here: Vec<String> = app.areas.with(|areas| areas.iter()
                        .filter(|area| key.with_value(|id| area.rooms.contains(id)))
                        .map(|area| area.name.clone())
                        .collect());
                    if here.is_empty() {
                        "Available in All rooms on the remote. Add it to a custom area when ready.".to_string()
                    } else {
                        format!("Shown in: {}", here.join(", "))
                    }
                }}
            </p>
        </section>

        {ui::section(
            view! { "Devices" <span class="count">{move || counts(&[(devices.with(Vec::len), "device", "devices")])}</span> },
            None,
            view! {
                {move || devices.with(Vec::is_empty).then(|| ui::empty("No devices here yet."))}
                <ul class="rows">
                    <For each=move || devices.get() key=|id| id.clone()
                        children=move |id| device_card(app, key.get_value(), devices, id)/>
                </ul>
            }
            .into_any(),
        )}
        {room_scenes(app, key)}
        {room_activities(app, key)}
        // Not converted: the picker reads connections, live discovery results
        // and the room's devices together. Redrawn on a write, as before.
        {keyed(app, move |app, config| super::device_picker::picker(app, config, &key.get_value()))}

        <div class="pad">
            {ui::danger_button("Delete this room", move || {
                app.go(Route::Rooms);
                app.run(api::delete(format!("/api/rooms/{}", key.get_value())));
            })}
            <p class="dim">
                "Deleting this room also deletes its devices and activities, removes it from all screens, and removes commands for its devices from scenes."
            </p>
        </div>
    }
    .into_any()
}

/// One device row, keyed by its id.
///
/// The draft signals behind "Edit device" are created once with the row, so a
/// save on a neighbouring device - or anywhere else in the house - no longer
/// throws away what is half typed here.
fn device_card(app: App, room: Id, order: Memo<Vec<Id>>, id: Id) -> AnyView {
    let device = app.device(id.clone());
    let room = StoredValue::new(room);
    let key = StoredValue::new(id.clone());
    let path = move || {
        format!(
            "/api/rooms/{}/devices/{}",
            room.get_value(),
            key.get_value()
        )
    };

    let reorder = reorder_in(order, id, move |next: Vec<Id>| {
        app.run(api::put(
            format!("/api/rooms/{}/devices", room.get_value()),
            next,
        ));
    });

    let start = device.get_untracked();
    let name = RwSignal::new(start.as_ref().map(|d| d.name.clone()).unwrap_or_default());
    let kind = RwSignal::new(start.as_ref().map(|d| d.kind).unwrap_or_default());
    let preferred = RwSignal::new(start.as_ref().and_then(|d| d.preferred_transport));
    let icon = RwSignal::new(start.and_then(|d| d.icon));

    // The connection's own name is part of the line; the document itself is
    // read untracked, only to resolve a codeset the device inherits.
    let summary = move || {
        app.connections.track();
        let Some(device) = device.get() else {
            return String::new();
        };
        let house = app.house();
        let line = match &device.integration {
            Integration::Connection { connection_id, .. } => house
                .connection(connection_id)
                .map(super::connections::label),
            _ => None,
        }
        .unwrap_or_else(|| super::overview::connection_summary(&device.integration));
        // One line, one transport each: the connection, then IR, then the
        // Bluetooth bond, with the preferred one first when it is not.
        let network = device.network_integration(&house).is_some();
        let mut parts: Vec<String> = Vec::new();
        if network {
            parts.push(line.clone());
        }
        if device.effective_ir_codeset(&house).is_some() {
            parts.push(if network { "IR commands".into() } else { "Infrared · Built-in transmitter".into() });
        }
        if let Some(bluetooth) = super::bluetooth::summary(&device) {
            parts.push(bluetooth);
        }
        if parts.is_empty() {
            return line;
        }
        if let Some(t) = device.preferred_transport.filter(|_| parts.len() > 1) {
            let first = match t { Transport::Ip => 0, Transport::Ir => usize::from(network), Transport::Bluetooth => parts.len() - 1 };
            if first < parts.len() && device.has_transport(&house, t) {
                let lead = parts.remove(first);
                parts.insert(0, format!("{lead} (preferred)"));
            }
        }
        parts.join(" · ")
    };
    // The control methods the device has, for the preference below: only
    // offered when there is a choice to make.
    let transports = move || {
        device.get().map(|d| d.transports(&app.house())).unwrap_or_default()
    };

    view!{<li class="card device"><div class="device-head">{reorder}<div><h3>{move ||device.get().map(|d|d.name)}</h3><p class="dim">{summary}</p></div></div>
        // Both of these still take the whole document. They are redrawn when
        // this device changes rather than when anything in the house does.
        {move ||device.get().map(|d|super::device_picker::controls(app,&app.house(),&d))}
        {move ||device.get().map(|d|super::infrared::device_commands(app,&app.house(),&room.get_value(),&d))}
        {move ||device.get().map(|d|super::bluetooth::device_bluetooth(app,&app.house(),&room.get_value(),&d))}
        <details><summary>"Edit device"</summary><form on:submit=move |e|{e.prevent_default();let Some(base)=device.get_untracked() else{return};let title=name.get_untracked().trim().to_string();if title.is_empty(){return}app.run(api::put(path(),Device{name:title,kind:kind.get_untracked(),icon:icon.get_untracked(),preferred_transport:preferred.get_untracked(),..base}));}>
        {super::connections::field("Device name",name,"Device name")}
        <label class="field">"Device type"<select aria-label="Device type" prop:value=move ||kind.get().name() on:change=move |e|kind.set(DeviceKind::from_name(&event_target_value(&e)).unwrap_or_default())>{ALL_DEVICE_KINDS.iter().map(|k|view!{<option value=k.name()>{k.name()}</option>}).collect_view()}</select></label>
        {ui::icon_select_signal(icon, move |_| {})}
        {move ||{let have=transports();(have.len()>1).then(||view!{<label class="field">"Preferred control"<select aria-label="Preferred control" prop:value=move ||preferred.get().map(|t|t.name()).unwrap_or("") on:change=move |e|preferred.set(Transport::from_name(&event_target_value(&e)))>
            <option value="">"Automatic"</option>
            {have.iter().map(|t|view!{<option value=t.name()>{t.label()}</option>}).collect_view()}
        </select></label>
        <p class="dim">"Buttons try this first and fall back to the device's other methods when it is unavailable: a TV that is asleep on the network, or not on the Bluetooth link. Automatic tries infrared, then the connection, then Bluetooth."</p>})}}
        <p class="dim">"Manage server and bridge settings in Connections. To use another source device, remove this device and add the replacement from its connection."</p>
        <button class="primary" type="submit">"Save device"</button><button class="ghost" type="button" on:click=move |_|{if let Some(d)=device.get_untracked(){name.set(d.name);kind.set(d.kind);icon.set(d.icon);preferred.set(d.preferred_transport);}}>"Discard changes"</button></form></details>
        {ui::danger_button("Delete device",move ||app.run(api::delete(path())))}
    </li>}.into_any()
}

fn room_scenes(app: App, room: StoredValue<Id>) -> AnyView {
    let here = Memo::new(move |_| {
        app.scenes.with(|scenes| {
            scenes
                .iter()
                .filter(|s| room.with_value(|room| s.rooms.contains(room)))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let order = ids(here, |s| &s.id);
    ui::section(view!{"Scenes" <span class="count">{move ||order.with(Vec::len)}</span>},
        Some("Shown in the Scenes button at the bottom of this room on the remote."),
        view!{
        {move ||order.with(Vec::is_empty).then(||ui::empty("No scenes here yet. Choose Hue scenes below to add one."))}
        <ul class="rows room-scenes"><For each=move ||order.get() key=|id|id.clone() children=move |id|{
            let scene=app.scene(id.clone());let open=id.clone();
            view!{<li class="row"><button class="row-main" on:click=move |_|app.go(Route::Scene(open.clone()))><span class="row-title">{move ||scene.get().map(|s|s.name)}</span></button><button class="ghost" disabled=move ||app.busy.get() on:click=move |_|{let Some(mut next)=scene.get_untracked() else{return};next.rooms.retain(|r|room.with_value(|room|r!=room));app.run(api::put(format!("/api/scenes/{}",next.id),next));}>"Remove from room"</button></li>}
        }/></ul>
    }.into_any())
}

fn room_activities(app: App, room: StoredValue<Id>) -> AnyView {
    let here = Memo::new(move |_| {
        app.activities.with(|all| {
            all.iter()
                .filter(|a| room.with_value(|room| &a.room == room))
                .map(|a| a.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let elsewhere = Memo::new(move |_| {
        app.activities.with(|all| {
            all.iter()
                .filter(|a| room.with_value(|room| &a.room != room))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    view! {
        <section class="room-activities block">
            <h2 class="section">"Activities" <span class="count">{move ||here.with(Vec::len)}</span></h2>
            <p class="dim">"Activities belong to a room and can also appear in areas. Open one to choose its source device and startup commands."</p>
            <ul class="rows"><For each=move ||here.get() key=|id|id.clone() children=move |id|{
                let activity=app.activity(id.clone());
                view!{<li class="row"><button class="row-main" on:click=move |_|app.go(Route::Activity(id.clone()))><span class="row-title">{move ||activity.get().map(|a|a.name)}</span><span class="row-sub">"Edit activity"</span></button></li>}
            }/></ul>
            {ui::add_row("New room activity name", "Create activity in this room", move |name| {
                app.run(api::post("/api/activities",json!({"name":name,"room":room.get_value()})));
            })}
            {move ||(!elsewhere.with(Vec::is_empty)).then(|| {
                let options=elsewhere.with(|all|all.iter().map(|a|(a.id.to_string(),format!("{} · {}",a.name,match super::room_name(app,&a.room){name if name.is_empty()=>"Unknown room".into(),name=>name}))).collect());
                super::pick_row(options, "Move an existing activity here", move |id| {
                    if let Some(mut next)=elsewhere.with_untracked(|all|all.iter().find(|a|a.id.as_str()==id).cloned()) {
                        next.room=room.get_value();
                        app.run(api::put(format!("/api/activities/{}",next.id),next));
                    }
                })
            })}
        </section>
    }.into_any()
}
