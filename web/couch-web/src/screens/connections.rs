use crate::{api, ui, App};
use couch_model::{Config, Connection, Provider};
use leptos::prelude::*;
use serde_json::json;

pub fn screen(app: App, config: &Config) -> AnyView {
    let choice = RwSignal::new(String::new());
    let existing: Vec<_> = config.connections.iter().filter(|c|c.provider!=Provider::Ir).cloned().collect();
    let no_connections = existing.is_empty();
    let available: Vec<_> = [
        ("kodi", "Kodi"),
        ("core-elec", "CoreELEC"),
        ("sonos", "Sonos"),
        ("home-assistant", "Home Assistant"),
        ("hue", "Philips Hue"),
        ("web-os", "LG webOS TV"),
        ("android-tv", "Android / Google TV · experimental"),
        ("apple-tv", "Apple TV · experimental"),
        ("denon", "Denon AVR"),
        ("unifi-protect", "UniFi Protect"),
    ]
    .into_iter()

    .collect();
    view!{
        {ui::page_header(app,"Connections".into(),None)}
        <p class="lead">"Connections tell Couch how to reach your TVs, servers and bridges. Add devices and assign their infrared commands in Rooms & devices."</p>
        <p class="notice">"Using infrared? Open a device in Rooms & devices and choose Add IR commands. No infrared connection is needed."</p>
        <h2 class="section">"Saved connections"</h2>
        {no_connections.then(||ui::empty("No connections yet. Add your first connection below."))}
        <div class="destination-grid">{existing.into_iter().map(|c|saved(app,c)).collect_view()}</div>
        <section class="creation"><h2>"Add a connection"</h2>
        <label class="field">"Connection type"<select aria-label="Connection type" prop:value=move || choice.get() on:change=move |e|choice.set(event_target_value(&e))><option value="">"Choose a type"</option>{available.into_iter().map(|(kind,label)|view!{<option value=kind>{label}</option>}).collect_view()}</select></label>
        {move || match choice.get().as_str(){"unifi-protect"=>create_named(app,Provider::UnifiProtect),"sonos"=>super::sonos::form(app,None),"core-elec"=>super::coreelec::form(app,None),"denon"=>denon_form(app,None),"kodi"=>local_form(app,None,false),"home-assistant"=>create_named(app,Provider::HomeAssistant),"hue"=>create_named(app,Provider::Hue),"web-os"=>create_named(app,Provider::WebOs),"android-tv"=>create_named(app,Provider::AndroidTv),"apple-tv"=>create_named(app,Provider::AppleTv),_=>view!{<p class="dim">"Add multiple bridges, servers and TVs. Infrared is built into the remote and is configured on each device."</p>}.into_any()}}
        </section>
    }.into_any()
}
fn saved(app: App, c: Connection) -> AnyView {
    let label = c.provider.label();
    let title = c.name.clone();
    let id = c.id.clone();
    let usage=app.config.get_untracked().map(|cfg|cfg.devices().filter(|(_,d)|matches!(&d.integration,couch_model::Integration::Connection{connection_id,..} if connection_id==&id)).count()).unwrap_or(0);
    let edit = match c.provider {
        Provider::Sonos { .. } => view!{{super::sonos::form(app,Some(c.clone()))}{super::sonos::controls(app,c.id.to_string())}}.into_any(),
        Provider::CoreElec { .. } => view!{{super::coreelec::form(app,Some(c.clone()))}{super::kodi::setup(app,&c)}{super::coreelec::setup(app,&c)}}.into_any(),
        Provider::Kodi { .. } => view!{ {local_form(app, Some(c.clone()), false)} {super::kodi::setup(app, &c)} }.into_any(),
        Provider::Denon { .. } => view!{{denon_form(app, Some(c.clone()))}{denon_controls(app,c.id.to_string())}}.into_any(),
        Provider::Ir => local_form(app, Some(c.clone()), true),
        Provider::UnifiProtect => super::protect::setup(app, &c),
        Provider::HomeAssistant => super::home_assistant::setup(app, &c),
        Provider::Hue => super::hue::setup(app, &c),
        Provider::WebOs => super::webos::setup(app, &c),
        Provider::AndroidTv | Provider::AppleTv => super::streaming_tv::setup(app, &c),
    };
    view!{<section class="card saved-connection"><h2>{title}</h2><p>{format!("{label} · {usage} assigned devices")}</p>
        <p class="dim">{match &c.provider{Provider::Sonos{host}=>format!("{host} · Local Sonos control"),Provider::Kodi{host,port}|Provider::CoreElec{host,port}=>format!("{host}:{port} · Saved address"),Provider::Ir=>"Built-in transmitter · Codes are configured per device".into(),_=>"Credentials are kept privately on the remote".into()}}</p>
        <details open=usage==0><summary>"Connection settings"</summary>{edit}</details>
        <p class="dim">"Removing a connection requires removing its assigned devices first. Bridge credentials are retained for reconnecting."</p>
        {ui::danger_button("Remove connection",move ||app.run(api::delete(format!("/api/connections/{id}"))))}
    </section>}.into_any()
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
        let body=json!({"name":name,"provider":provider});match &existing{Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>app.run(api::post("/api/connections",body))}
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
    view!{<form on:submit=move |e|{e.prevent_default();let name=name.get_untracked().trim().to_string();if !name.is_empty(){app.run(api::post("/api/connections",json!({"name":name,"provider":provider})));}}>
    {field("Connection name",name,"Living room TV / Upstairs bridge")}
    <p class="dim">"Create a named connection, then enter its address and pair it in Connection settings above."</p>
    <button type="submit" class="primary">"Create connection"</button></form>}.into_any()
}

fn denon_form(app: App, existing: Option<Connection>) -> AnyView {
    let name = RwSignal::new(existing.as_ref().map(|c|c.name.clone()).unwrap_or("Denon AVR".into()));
    let (host, port) = match existing.as_ref().map(|c|&c.provider) {Some(Provider::Denon{host,port})=>(host.clone(),port.to_string()),_=>(String::new(),"23".into())};
    let host=RwSignal::new(host); let port=RwSignal::new(port); let error=RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let Ok(port)=port.get_untracked().parse::<u16>() else {error.set("Enter a valid TCP port".into());return};
        let body=json!({"name":name.get_untracked(),"provider":Provider::Denon{host:host.get_untracked().trim().into(),port}});
        match &existing {Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>app.run(api::post("/api/connections",body))}
    }>{field("Connection name",name,"Theater receiver")}{field("Hostname or IP address",host,"192.168.1.29")}{field("TCP port",port,"23")}
    <p class="dim">"Enable Network Control / Always On on the receiver for standby access. Add the receiver to a room, then assign activity volume, mute and power buttons to it."</p>
    <p role="alert">{move ||error.get()}</p><button class="primary">"Save connection"</button></form>}.into_any()
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
    view!{<section><p role="status">{move ||status.get()}</p><div class="actions">
        <button disabled=move ||busy.get() on:click=move |_|send(None,None)>"Test connection / refresh"</button>
        {[("power-on","On"),("power-off","Standby"),("volume-down","Volume −"),("volume-up","Volume +"),("mute","Mute"),("unmute","Unmute")].into_iter().map(move |(command,label)|view!{<button disabled=move ||busy.get() on:click=move |_|send(Some(command),None)>{label}</button>}).collect_view()}
        </div><label class="field">"Input"<select aria-label="AVR input" disabled=move ||busy.get() on:change=move |e|{let id=event_target_value(&e);if !id.is_empty(){send(Some("input"),Some(json!(id)));}}><option value="">"Choose an input (refresh to discover)"</option>{move ||sources.get().into_iter().map(|(id,name)|view!{<option value=id>{name}</option>}).collect_view()}</select></label>
    </section>}.into_any()
}
