//! Task-oriented entry points over the existing configuration model.
use crate::{api, route::Route, ui, App};
use couch_model::Integration;
use leptos::prelude::*;
use serde_json::json;

/// Title and detail are `impl IntoView` so a row in a keyed list can pass
/// closures over its own memo and follow a rename in place.
fn destination(
    app: App,
    route: Route,
    title: impl IntoView + 'static,
    detail: impl IntoView + 'static,
) -> AnyView {
    view! { <button class="destination" on:click=move |_| app.go(route.clone())>
        <strong>{title}</strong><span>{detail}</span><span class="destination-action">"Open →"</span>
    </button> }.into_any()
}

/// Read-only, so the minimal conversion: mounted once, every count read inside
/// its own closure.
pub fn overview(app: App) -> AnyView {
    let rooms = move || app.rooms.with(Vec::len);
    let devices = move || app.devices.with(Vec::len);
    let areas = move || app.areas.with(Vec::len);
    let unconnected = move || {
        app.devices.with(|all| {
            all.iter()
                .filter(|(_, d)| matches!(d.integration, Integration::None))
                .count()
        })
    };
    let hidden_rooms = move || {
        app.areas.with(|areas| {
            app.rooms.with(|rooms| {
                rooms
                    .iter()
                    .filter(|r| !areas.iter().any(|a| a.rooms.contains(&r.id)))
                    .count()
            })
        })
    };
    view! {
        <div class="hero"><p class="eyebrow">"MAKE IT YOUR REMOTE"</p><h1>"A home that makes sense."</h1>
        <p>"Add the things you control, decide what they do together, then arrange your remote’s screens."</p></div>
        <section class="notice"><strong>"Your remote’s configuration"</strong><p>{move ||format!("{} rooms · {} devices · {} areas", rooms(), devices(), areas())}</p></section>
        <h2 class="section">"Set up your remote"</h2>
        <div class="destination-grid">
            {destination(app, Route::Connections, "01 · Connections", "Add your Kodi players, Home Assistant server, Hue bridge or infrared connection.")}
            {destination(app, Route::Rooms, "02 · Rooms & devices", move ||format!("Create a room, then choose devices from saved connections. {} rooms · {} devices", rooms(), devices()))}
            {destination(app, Route::Activities, "03 · Activities & scenes", "Activities describe what you do, such as Watch TV. Scenes collect commands, such as Movie night.")}
            {destination(app, Route::Areas, "04 · Areas", move ||format!("An area is a screen. Choose its rooms, activity strip and scene shortcuts. {} areas", areas()))}
        </div>
        {move ||(unconnected() > 0 || hidden_rooms() > 0).then(|| view! { <h2 class="section">"Finish setting up"</h2> })}
        <div class="destination-grid">
            {move ||(unconnected() > 0).then(|| destination(app, Route::Connections, format!("{} devices without a connection", unconnected()), "A connection chooses how a client reaches a device: Kodi, Home Assistant or infrared."))}
            {move ||(hidden_rooms() > 0).then(|| destination(app, Route::Rooms, format!("{} rooms not on a screen", hidden_rooms()), "Rooms can appear on several area screens. Editing a shared room updates it everywhere."))}
        </div>
        <section class="card"><h2>"How the pieces fit"</h2><p>"Connections reach servers and bridges. Rooms contain devices from those connections. Activity → room, source device and startup steps. Scene → an ordered list of device commands. Area → the rooms, activities and scenes you want on one screen."</p>
        <p class="dim">"Names and ordering save when changed. Device forms have an explicit Save button. Removing an item from a screen keeps the original; deleting it removes it from the home."</p></section>
    }.into_any()
}

pub fn rooms(app: App) -> AnyView {
    let order = super::ids(app.rooms, |r| &r.id);
    view! {
        {ui::page_header(app, "Rooms & devices", None)}
        <p class="lead">"Create rooms, then add devices from your saved connections. Set up servers and bridges in Connections."</p>
        <section class="creation"><h2>"Add a room"</h2><p class="dim">"Start with a place, such as Living room or Kitchen. Add it to an area when you’re ready."</p>
        {ui::add_row("Room name", "Create room", move |name| app.run(api::post("/api/rooms", json!({"name":name}))))}</section>
        {move || order.with(Vec::is_empty).then(|| ui::empty("No rooms yet. Create your first room above, then open it to add a device."))}
        <div class="destination-grid">
            <For each=move || order.get() key=|id| id.clone() children=move |id| {
                // Each card reads its own room, so creating a room or renaming
                // one leaves every other card alone.
                let room = app.room(id.clone());
                let title = move || room.get().map(|r| r.name).unwrap_or_default();
                let detail = move || room.get().map(|room| {
                    let areas = app.areas.with(|areas| areas.iter().filter(|a| a.rooms.contains(&room.id)).map(|a| a.name.clone()).collect::<Vec<_>>());
                    let shown = if areas.is_empty() { "Not on an area yet".into() } else { format!("Areas: {}", areas.join(", ")) };
                    format!("{} · {shown}", room.device_summary())
                }).unwrap_or_default();
                destination(app, Route::Room(id), title, detail)
            }/>
        </div>
    }.into_any()
}

pub fn connection_summary(integration: &Integration) -> String {
    match integration {
        Integration::Sonos { host } => format!("Sonos · {host}"),
        Integration::Denon { host, port } => format!("Denon AVR · {host}:{port}"),
        Integration::Connection { connection_id, .. } => format!("Connection · {connection_id}"),
        Integration::WebOs => "LG webOS TV".into(),
        Integration::AndroidTv => "Android / Google TV".into(),
        Integration::UnifiProtect { .. } => "UniFi Protect".into(),
        Integration::Matter { device } => format!("Matter · {device}"),
        Integration::Plugin { id, .. } => format!("External · {id}"),
        Integration::AppleTv => "Apple TV".into(),
        Integration::Tizen => "Samsung Tizen TV".into(),
        Integration::BluetoothTv => "Bluetooth TV".into(),
        Integration::None => "Not configured".into(),
        Integration::Kodi { host, port } => format!("Kodi · {host}:{port}"),
        Integration::Hue { light_id } => format!("Philips Hue · {light_id}"),
        Integration::HomeAssistant { entity_id } => format!("Home Assistant · {entity_id}"),
        Integration::Ir { codeset } => format!("Infrared · {codeset}"),
    }
}

pub fn connections(app: App) -> AnyView {
    super::connections::screen(app)
}
