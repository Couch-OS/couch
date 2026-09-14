//! Connections: the list, and one page per connection.
//!
//! The list only says what each connection is and how much depends on it;
//! everything that can be changed about a connection lives on its own page,
//! reached by opening its card. Creating a connection lands on that page too,
//! because for most providers creating the record is the first of two steps
//! and the second (address, pairing, credentials) is only offered there.
use crate::{api, route::Route, ui, App};
use couch_model::{Config, Connection, Id, Provider};
use leptos::prelude::*;
use serde_json::{json, Value};

pub fn screen(app: App, config: &Config) -> AnyView {
    let choice = RwSignal::new(String::new());
    let existing: Vec<_> = config.connections.iter().filter(|c|c.provider!=Provider::Ir).cloned().collect();
    let no_connections = existing.is_empty();
    let usage: Vec<usize> = existing.iter().map(|c| assigned(config, &c.id).len()).collect();
    let available: Vec<_> = [
        ("kodi", "Kodi"),
        ("core-elec", "CoreELEC"),
        ("sonos", "Sonos"),
        ("home-assistant", "Home Assistant"),
        ("hue", "Philips Hue"),
        ("web-os", "LG webOS TV"),
        ("android-tv", "Android / Google TV · experimental"),
        ("apple-tv", "Apple TV · experimental"),
        ("tizen", "Samsung Tizen TV · experimental"),
        ("bluetooth-tv", "Bluetooth TV · pairs to the remote"),
        ("denon", "Denon AVR"),
        ("unifi-protect", "UniFi Protect"),
        ("matter", "Matter · experimental"),
    ]
    .into_iter()
    .collect();
    view!{
        {ui::page_header(app,"Connections".into(),None)}
        <p class="lead">"Connections tell Couch how to reach your TVs, speakers, servers and bridges. Open one to change its address, pair it or test it. Add devices and assign their infrared commands in Rooms & devices."</p>
        <p class="notice">"Using infrared? Open a device in Rooms & devices and choose Add IR commands. No infrared connection is needed."</p>
        <h2 class="section">"Saved connections" <span class="count">{ui_count(existing.len())}</span></h2>
        {no_connections.then(||ui::empty("No connections yet. Add your first connection below."))}
        <div class="destination-grid">{existing.into_iter().zip(usage).map(|(c,used)|card(app,c,used)).collect_view()}</div>
        <section class="creation"><h2>"Add a connection"</h2>
        <label class="field">"Connection type"<select aria-label="Connection type" prop:value=move || choice.get() on:change=move |e|choice.set(event_target_value(&e))><option value="">"Choose a type"</option>{available.into_iter().map(|(kind,label)|view!{<option value=kind>{label}</option>}).collect_view()}</select></label>
        {move || match choice.get().as_str(){"unifi-protect"=>create_named(app,Provider::UnifiProtect),"matter"=>create_named(app,Provider::Matter),"sonos"=>super::sonos::form(app,None),"core-elec"=>super::coreelec::form(app,None),"denon"=>denon_form(app,None),"kodi"=>local_form(app,None,false),"home-assistant"=>create_named(app,Provider::HomeAssistant),"hue"=>create_named(app,Provider::Hue),"web-os"=>create_named(app,Provider::WebOs),"android-tv"=>create_named(app,Provider::AndroidTv),"apple-tv"=>create_named(app,Provider::AppleTv),"tizen"=>create_named(app,Provider::Tizen),"bluetooth-tv"=>create_named(app,Provider::BluetoothTv),_=>view!{<p class="dim">"Add multiple bridges, servers and TVs. Infrared is built into the remote and is configured on each device."</p>}.into_any()}}
        </section>
    }.into_any()
}

/// A Bluetooth TV has nothing to configure here: the remote is the peripheral
/// and the TV does the pairing.
fn bluetooth_notes() -> AnyView {
    view!{
        <p>"The remote itself is the Bluetooth device. Turn Bluetooth on under Remote, then open the TV's Bluetooth or remote-control settings and pair \"Couch Remote\". Add the TV to a room from this connection; its mapped buttons then go straight over Bluetooth."</p>
        <p class="dim">"Keys are standard consumer-control usages (volume, navigation, playback, power toggle). Which ones a TV honours depends on its make."</p>
    }.into_any()
}

fn ui_count(n: usize) -> String {
    super::counts(&[(n, "connection", "connections")])
}

/// The devices that reach the house through this connection, with their rooms.
fn assigned(config: &Config, id: &Id) -> Vec<(Id, String, String)> {
    config
        .devices()
        .filter(|(_, d)| matches!(&d.integration, couch_model::Integration::Connection{connection_id,..} if connection_id==id))
        .map(|(room, d)| (room.id.clone(), room.name.clone(), d.name.clone()))
        .collect()
}

/// One line saying where a connection points, without any secret.
fn address(c: &Connection) -> String {
    match &c.provider {
        Provider::Sonos { host } => format!("{host} · Local Sonos control"),
        Provider::Kodi { host, port } | Provider::CoreElec { host, port } => format!("{host}:{port} · Saved address"),
        Provider::Denon { host, port } => format!("{host}:{port} · Telnet control"),
        Provider::Ir => "Built-in transmitter · Codes are configured per device".into(),
        _ => "Credentials are kept privately on the remote".into(),
    }
}

/// A saved connection in the list: what it is, what depends on it, and a way in.
fn card(app: App, c: Connection, used: usize) -> AnyView {
    let route = Route::Connection(c.id.clone());
    let devices = super::counts(&[(used, "assigned device", "assigned devices")]);
    view! { <button class="destination" on:click=move |_| app.go(route.clone())>
        <strong>{c.name.clone()}</strong>
        <span>{format!("{} · {devices}", c.provider.label())}</span>
        <span>{address(&c)}</span>
        <span class="destination-action">"Open →"</span>
    </button> }.into_any()
}

/// One connection's own page: what it is, what uses it, its settings, and
/// the way to remove it.
pub fn detail(app: App, config: &Config, id: &Id) -> AnyView {
    let Some(c) = config.connection(id).cloned() else {
        return super::gone(app, "That connection has been removed.");
    };
    let used = assigned(config, id);
    let usage = used.len();
    let label = c.provider.label();
    let delete_id = c.id.clone();
    let settings = match c.provider {
        Provider::Sonos { .. } => view!{{titled("Connection",super::sonos::form(app,Some(c.clone())))}{super::sonos::controls(app,c.id.to_string())}}.into_any(),
        Provider::CoreElec { .. } => view!{{titled("Connection",super::coreelec::form(app,Some(c.clone())))}{super::kodi::setup(app,&c)}{super::coreelec::setup(app,&c)}}.into_any(),
        Provider::Kodi { .. } => view!{{titled("Connection",local_form(app, Some(c.clone()), false))}{super::kodi::setup(app, &c)}}.into_any(),
        Provider::Denon { .. } => view!{{titled("Connection",denon_form(app, Some(c.clone())))}{denon_controls(app,c.id.to_string())}}.into_any(),
        Provider::Ir => titled("Connection", local_form(app, Some(c.clone()), true)),
        Provider::UnifiProtect => super::protect::setup(app, &c),
        Provider::Matter => super::matter::setup(app, &c),
        Provider::HomeAssistant => super::home_assistant::setup(app, &c),
        Provider::Hue => super::hue::setup(app, &c),
        Provider::WebOs => super::webos::setup(app, &c),
        Provider::AndroidTv | Provider::AppleTv => titled(label, super::streaming_tv::setup(app, &c)),
        Provider::Tizen => super::tizen::setup(app, &c),
        Provider::BluetoothTv => titled(label, bluetooth_notes()),
    };
    view!{
        {ui::page_header(app, c.name.clone(), Some(Route::Connections))}
        <p class="lead">{format!("{label} · {}", address(&c))}</p>

        <div class="connection-settings">{settings}</div>

        <section class="card">
            <h2>"Assigned devices" <span class="count">{super::counts(&[(usage, "device", "devices")])}</span></h2>
            {(usage == 0).then(|| view!{<p class="dim">"No device uses this connection yet. Add one in Rooms & devices and choose From connection."</p>})}
            <ul class="rows">{used.into_iter().map(|(room_id, room, device)| {
                let route = Route::Room(room_id);
                view!{<li class="row"><button class="row-main" on:click=move |_| app.go(route.clone())>
                    <span class="row-title">{device}</span><span class="row-sub">{room}</span>
                </button></li>}
            }).collect_view()}</ul>
        </section>

        <section class="card danger-zone">
            <h2>"Remove connection"</h2>
            <p class="dim">"Removing a connection requires removing its assigned devices first. Bridge credentials are retained for reconnecting."</p>
            {ui::danger_button("Remove connection",move ||app.run_then(api::delete(format!("/api/connections/{delete_id}")), move |_| app.go(Route::Connections)))}
        </section>
    }.into_any()
}

/// A settings form on the connection page, in its own card under a heading.
fn titled(heading: &'static str, body: AnyView) -> AnyView {
    view!{<section class="card"><h2>{heading}</h2>{body}</section>}.into_any()
}

/// Create a connection, then open its page.
///
/// The response is the whole configuration; the new record is the one whose id
/// was not there when the form was drawn.
pub(super) fn create(app: App, body: Value) {
    let known: Vec<Id> = app
        .config
        .get_untracked()
        .map(|c| c.connections.iter().map(|c| c.id.clone()).collect())
        .unwrap_or_default();
    app.run_then(api::post("/api/connections", body), move |config| {
        if let Some(fresh) = config.connections.iter().find(|c| !known.contains(&c.id)) {
            app.go(Route::Connection(fresh.id.clone()));
        }
    });
}

fn local_form(app: App, existing: Option<Connection>, infrared: bool) -> AnyView {
    let name = RwSignal::new(
        existing
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or(if infrared {
                "Infrared".into()
            } else {
                String::new()
            }),
    );
    let (initial_host, initial_port) = match existing.as_ref().map(|c| &c.provider) {
        Some(Provider::Kodi { host, port }) => (host.clone(), port.to_string()),
        _ => (String::new(), "9090".into()),
    };
    let host = RwSignal::new(initial_host);
    let port = RwSignal::new(initial_port);
    let error = RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let name=name.get_untracked().trim().to_string();if name.is_empty(){error.set("Enter a connection name".into());return}
        let provider=if infrared{Provider::Ir}else{let Ok(port)=port.get_untracked().parse::<u16>()else{error.set("Enter a TCP port from 1 to 65535".into());return};if port==0 || host.get_untracked().trim().is_empty(){error.set("Enter a hostname and a TCP port from 1 to 65535".into());return}Provider::Kodi{host:host.get_untracked().trim().into(),port}};
        let body=json!({"name":name,"provider":provider});match &existing{Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>create(app,body)}
    }>
        {field("Connection name",name,"Living room Kodi")}
        {(!infrared).then(||view!{<p class="dim">"Enable remote control in Kodi. Saving an address does not test connectivity."</p>{field("Hostname or IP address",host,"kodi.local")}{field("TCP port",port,"9090")}})}
        {infrared.then(||view!{<p class="notice">"Use the remote’s built-in infrared transmitter. In Rooms & devices, choose a brand and model from the library or import your own codes, then assign commands. Sending requires a working IR driver; learning is not available."</p>})}
        <p role="alert">{move ||error.get()}</p><button class="primary" type="submit">"Save connection"</button>
    </form>}.into_any()
}
pub(super) fn field(
    label: &'static str,
    value: RwSignal<String>,
    placeholder: &'static str,
) -> AnyView {
    view!{<label class="field">{label}<input type="text" prop:value=move ||value.get() placeholder=placeholder on:input=move |e|value.set(event_target_value(&e))/></label>}.into_any()
}

pub(super) fn label(c: &Connection) -> String {
    if c.name == c.provider.label() {
        c.name.clone()
    } else {
        format!("{} · {}", c.name, c.provider.label())
    }
}

fn create_named(app:App, provider:Provider)->AnyView {
    let name=RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let name=name.get_untracked().trim().to_string();if !name.is_empty(){create(app,json!({"name":name,"provider":provider}));}}>
    {field("Connection name",name,"Living room TV / Upstairs bridge")}
    <p class="dim">"Create a named connection. Its page opens next, where you enter its address and pair it."</p>
    <button type="submit" class="primary">"Create connection"</button></form>}.into_any()
}

fn denon_form(app: App, existing: Option<Connection>) -> AnyView {
    let name = RwSignal::new(existing.as_ref().map(|c|c.name.clone()).unwrap_or("Denon AVR".into()));
    let (host, port) = match existing.as_ref().map(|c|&c.provider) {Some(Provider::Denon{host,port})=>(host.clone(),port.to_string()),_=>(String::new(),"23".into())};
    let host=RwSignal::new(host); let port=RwSignal::new(port); let error=RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let Ok(port)=port.get_untracked().parse::<u16>() else {error.set("Enter a valid TCP port".into());return};
        let body=json!({"name":name.get_untracked(),"provider":Provider::Denon{host:host.get_untracked().trim().into(),port}});
        match &existing {Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>create(app,body)}
    }>{field("Connection name",name,"Theater receiver")}{field("Hostname or IP address",host,"192.168.1.29")}{field("TCP port",port,"23")}
    <p class="dim">"Enable Network Control / Always On on the receiver for standby access. Add the receiver to a room, then assign activity volume, mute and power buttons to it."</p>
    <p role="alert">{move ||error.get()}</p><button class="primary" type="submit">"Save connection"</button></form>}.into_any()
}

pub(super) fn denon_controls(app: App, id: String) -> AnyView {
    use leptos::task::spawn_local;
    let base=StoredValue::new(format!("/api/connections/{id}/denon"));
    let status=RwSignal::new(String::new());let busy=RwSignal::new(false);let sources=RwSignal::new(Vec::<(String,String)>::new());
    let send=move |command:Option<&'static str>, value:Option<serde_json::Value>| {
        if busy.get_untracked(){return}busy.set(true);
        spawn_local(async move {
            let (method,path,body)=match command {Some(command)=>("POST","command",Some(json!({"command":command,"value":value}))),None=>("GET","status",None)};
            match api::ha(method,&format!("{}/{path}",base.get_value()),body).await {
                Ok(s)=>{status.set(format!("{} · {} · {}{}",if s["on"]==true{"Main zone on"}else{"Standby"},s["input"].as_str().unwrap_or("Unknown input"),s["volume_db"].as_f64().map(|db|format!("{db:.1} dB")).unwrap_or("Minimum volume".into()),if s["muted"]==true{" · Muted"}else{""}));
                    if command.is_none(){if let Ok(v)=api::ha("GET",&format!("{}/sources",base.get_value()),None).await {sources.set(serde_json::from_value(v).unwrap_or_default());}}
                },Err(e)=>{if e.unauthorized {app.paired.set(Some(false));}status.set(e.message);}
            }busy.set(false);
        });
    };
    view!{<section><h3>"Receiver controls"</h3><p class="dim">"Test the saved address from here. Power, volume and mute act on the main zone."</p><p role="status">{move ||status.get()}</p><div class="actions">
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|send(None,None)>"Test connection / refresh"</button>
        {[("power-on","On"),("power-off","Standby"),("volume-down","Volume −"),("volume-up","Volume +"),("mute","Mute"),("unmute","Unmute")].into_iter().map(move |(command,label)|view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|send(Some(command),None)>{label}</button>}).collect_view()}
        </div><label class="field">"Input"<select aria-label="AVR input" disabled=move ||busy.get() on:change=move |e|{let id=event_target_value(&e);if !id.is_empty(){send(Some("input"),Some(json!(id)));}}><option value="">"Choose an input (refresh to discover)"</option>{move ||sources.get().into_iter().map(|(id,name)|view!{<option value=id>{name}</option>}).collect_view()}</select></label>
    </section>}.into_any()
}
