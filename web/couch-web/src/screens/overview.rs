//! Task-oriented entry points over the existing configuration model.
use crate::{api, route::Route, ui, App};
use couch_model::{Config, Integration};
use leptos::prelude::*;
use serde_json::json;

fn destination(app: App, route: Route, title: String, detail: String) -> AnyView {
    view! { <button class="destination" on:click=move |_| app.go(route.clone())>
        <strong>{title}</strong><span>{detail}</span><span class="destination-action">"Open →"</span>
    </button> }.into_any()
}

pub fn overview(app: App, config: &Config) -> AnyView {
    let unconnected = config
        .devices()
        .filter(|(_, d)| matches!(d.integration, Integration::None))
        .count();
    let hidden_rooms = config
        .rooms
        .iter()
        .filter(|r| !config.areas.iter().any(|a| a.rooms.contains(&r.id)))
        .count();
    view! {
        <div class="hero"><p class="eyebrow">"MAKE IT YOUR REMOTE"</p><h1>"A home that makes sense."</h1>
        <p>"Add the things you control, decide what they do together, then arrange your remote’s screens."</p></div>
        <section class="notice"><strong>"Your remote’s configuration"</strong><p>"Rooms and their screen order update on the remote. Home Assistant and Philips Hue lights support on/off and brightness. Hue rooms support grouped on/off, and imported Hue scenes can be activated from the home and room scene lists. Kodi activities open full-screen playback controls with artwork, seeking, chapters and audio tracks. Activity startup commands and custom device-step scenes are not executed yet."</p></section>
        <h2 class="section">"Set up your remote"</h2>
        <div class="destination-grid">
            {destination(app, Route::Connections, "01 · Connections".into(), "Add your Kodi players, Home Assistant server, Hue bridge or infrared connection.".into())}
            {destination(app, Route::Rooms, "02 · Rooms & devices".into(), format!("Create a room, then choose devices from saved connections. {} rooms · {} devices", config.rooms.len(), config.devices().count()))}
            {destination(app, Route::Activities, "03 · Activities & scenes".into(), "Activities describe what you do, such as Watch TV. Scenes collect commands, such as Movie night.".into())}
            {destination(app, Route::Areas, "04 · Areas".into(), format!("An area is a screen. Choose its rooms, activity strip and scene shortcuts. {} areas", config.areas.len()))}
        </div>
        {(unconnected > 0 || hidden_rooms > 0).then(|| view! { <h2 class="section">"Finish setting up"</h2> })}
        <div class="destination-grid">
            {(unconnected > 0).then(|| destination(app, Route::Connections, format!("{unconnected} devices without a connection"), "A connection chooses how a client reaches a device: Kodi, Home Assistant or infrared.".into()))}
            {(hidden_rooms > 0).then(|| destination(app, Route::Rooms, format!("{hidden_rooms} rooms not on a screen"), "Rooms can appear on several area screens. Editing a shared room updates it everywhere.".into()))}
        </div>
        <section class="card"><h2>"How the pieces fit"</h2><p>"Connections reach servers and bridges. Rooms contain devices from those connections. Activity → room, source device and startup steps. Scene → an ordered list of device commands. Area → the rooms, activities and scenes you want on one screen."</p>
        <p class="dim">"Names and ordering save when changed. Device forms have an explicit Save button. Removing an item from a screen keeps the original; deleting it removes it from the home."</p></section>
    }.into_any()
}

pub fn rooms(app: App, config: &Config) -> AnyView {
    view! {
        {ui::page_header(app, "Rooms & devices".into(), None)}
        <p class="lead">"Create rooms, then add devices from your saved connections. Set up servers and bridges in Connections."</p>
        <section class="creation"><h2>"Add a room"</h2><p class="dim">"Start with a place, such as Living room or Kitchen. Add it to an area when you’re ready."</p>
        {ui::add_row("Room name", "Create room", move |name| app.run(api::post("/api/rooms", json!({"name":name}))))}</section>
        {config.rooms.is_empty().then(|| ui::empty("No rooms yet. Create your first room above, then open it to add a device."))}
        <div class="destination-grid">{config.rooms.iter().map(|room| {
            let areas: Vec<_> = config.areas.iter().filter(|a| a.rooms.contains(&room.id)).map(|a| a.name.as_str()).collect();
            let shown = if areas.is_empty() { "Not on an area yet".into() } else { format!("Areas: {}", areas.join(", ")) };
            destination(app, Route::Room(room.id.clone()), room.name.clone(), format!("{} · {shown}", room.device_summary()))
        }).collect_view()}</div>
    }.into_any()
}

pub fn connection_summary(integration: &Integration) -> String {
    match integration {
        Integration::Sonos { host } => format!("Sonos · {host}"),
        Integration::Denon{host,port}=>format!("Denon AVR · {host}:{port}"),
        Integration::Connection { connection_id, .. } => format!("Connection · {connection_id}"),
        Integration::WebOs => "LG webOS TV".into(),
        Integration::AndroidTv => "Android / Google TV".into(),
        Integration::UnifiProtect { .. } => "UniFi Protect".into(),
        Integration::AppleTv => "Apple TV".into(),
        Integration::None => "Not configured".into(),
        Integration::Kodi { host, port } => format!("Kodi · {host}:{port}"),
        Integration::Hue { light_id } => format!("Philips Hue · {light_id}"),
        Integration::HomeAssistant { entity_id } => format!("Home Assistant · {entity_id}"),
        Integration::Ir { codeset } => format!("Infrared · {codeset}"),
    }
}

pub fn connections(app: App, config: &Config) -> AnyView {
    super::connections::screen(app, config)
}
