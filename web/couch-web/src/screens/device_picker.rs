//! Room-scoped discovery and assignment. Connection credentials never appear here.
use crate::{api, route::Route, App};
use couch_model::{Config, Connection, Device, Id, Integration, Provider};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

pub fn picker(app: App, config: &Config, room: &Id) -> AnyView {
    let connections: Vec<_> = config.connections.iter().filter(|c|c.provider!=Provider::Ir).cloned().collect();
    let room = StoredValue::new(room.clone());
    if app.device_source.get_untracked() != "manual-ir" && !connections.iter().any(|c|c.id.as_str()==app.device_source.get_untracked()) {
        app.device_source.set(if connections.len()==1 {connections[0].id.to_string()} else {String::new()});
    }
    let options=connections.clone();
    view!{<section class="creation device-picker"><h2>"Add to this room"</h2><p class="dim">"Choose a connected device, or add a device controlled by infrared. You can also add IR commands to any device already in this room."</p>
        <label class="field">"Device source"<select aria-label="From connection" prop:value=move ||app.device_source.get() on:change=move |e|{app.device_filter.set(String::new());app.device_source.set(event_target_value(&e));}>
            <option value="">"Choose a device source"</option>{options.into_iter().map(|c|view!{<option value=c.id.to_string()>{super::connections::label(&c)}</option>}).collect_view()}
            <option value="manual-ir">"Manual / infrared"</option>
        </select></label>
        {move ||if app.device_source.get()=="manual-ir" {super::infrared::device_setup(app,room.get_value(),None)}else{
            connections.iter().find(|c|c.id.as_str()==app.device_source.get()).map(|c|match c.provider{Provider::UnifiProtect|Provider::Hue|Provider::HomeAssistant=>discover(app,c.clone(),room.get_value()),_=>manual(app,c.clone(),room.get_value())}).unwrap_or_else(||view!{<p class="dim">"Need a server or bridge first?" <button class="ghost" on:click=move |_|app.go(Route::Connections)>"Manage connections"</button></p>}.into_any())
        }}
    </section>}.into_any()
}

fn assigned(app: App, connection: &Connection, resource: &str) -> Option<String> {
    app.config.get_untracked().and_then(|cfg|cfg.devices().find_map(|(r,d)|{
        let same=matches!(&d.integration,Integration::Connection{connection_id,resource_id} if connection_id==&connection.id && resource_id==resource)
            || match cfg.resolve_integration(&d.integration){Some(Integration::Hue{light_id})=>connection.provider==Provider::Hue && light_id==format!("{}/{resource}",connection.id),Some(Integration::HomeAssistant{entity_id})=>connection.provider==Provider::HomeAssistant && entity_id==format!("{}/{resource}",connection.id),Some(Integration::Kodi{host,port})=>connection.provider==Provider::Kodi{host,port},_=>false};
        same.then(||r.name.clone())
    }))
}
fn discover(app: App, connection: Connection, room: Id) -> AnyView {
    let list = RwSignal::new(Vec::<Value>::new());
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let prefix = if connection.provider == Provider::UnifiProtect { "protect" } else if connection.provider == Provider::Hue {
        "hue"
    } else {
        "ha"
    };
    let base=StoredValue::new(format!("/api/connections/{}/{prefix}",connection.id));
    let category = if prefix == "hue" {
        app.hue_category
    } else {
        RwSignal::new(if prefix == "protect" { "cameras" } else { "lights" }.to_string())
    };
    let fetch = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set("Finding devices…".into());
        let category = category.get_untracked();
        spawn_local(async move {
            match api::ha("GET", &format!("{}/{category}",base.get_value()), None).await {
                Ok(v) => {
                    let values = v.as_array().cloned().unwrap_or_default();
                    message.set(format!(
                        "{} controls found. Choose the ones that belong in this room.",
                        values.len()
                    ));
                    list.set(values);
                }
                Err(e) => {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(e.message);
                }
            }
            busy.set(false);
        });
    };
    fetch();
    view!{<p class="dim">{if prefix=="protect" {"Add cameras to this room to view them on your remote."} else if prefix=="hue" {"Add lights, grouped room controls or scenes. Scenes go straight into this room’s Scenes button on the remote."} else {"Add lights, blinds or thermostats. Their controls adapt to the features Home Assistant exposes."}}</p>
        {(prefix=="hue").then(||view!{<label class="field">"Hue controls"<select aria-label="Hue controls" prop:value=move ||category.get() disabled=move ||busy.get() on:change=move |e|{category.set(event_target_value(&e));app.device_filter.set(String::new());app.hue_room_filter.set(String::new());list.set(Vec::new());fetch();}><option value="lights">"Lights"</option><option value="rooms">"Hue rooms"</option><option value="scenes">"Hue scenes"</option></select></label>})}
        {(prefix=="ha").then(||view!{<label class="field">"Device type"<select aria-label="Home Assistant device type" prop:value=move ||category.get() disabled=move ||busy.get() on:change=move |e|{category.set(event_target_value(&e));app.device_filter.set(String::new());list.set(Vec::new());fetch();}><option value="lights">"Lights"</option><option value="covers">"Blinds"</option><option value="climates">"Thermostats"</option></select></label>})}
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|fetch()>"Refresh devices"</button>
        {super::connections::field("Search devices",app.device_filter,"Filter by name")}
        {(prefix=="hue").then(||view!{<label class="field">"Hue room or zone"<select aria-label="Hue room or zone" prop:value=move ||app.hue_room_filter.get() on:change=move |e|app.hue_room_filter.set(event_target_value(&e))><option value="">"All bridge rooms and zones"</option>{move ||list.get().iter().filter_map(|v|v["room_name"].as_str()).filter(|s|!s.is_empty()).map(str::to_string).collect::<std::collections::BTreeSet<_>>().into_iter().map(|name|view!{<option value=name.clone()>{name.clone()}</option>}).collect_view()}</select></label>})}
        <p role="status">{move ||message.get()}</p>
        <div class="discovered-devices">{move ||list.get().into_iter().filter(|d|{
            let text=format!("{} {}",d["name"].as_str().unwrap_or(""),d["room_name"].as_str().unwrap_or("")).to_lowercase();
            app.device_filter.get().to_lowercase().split_whitespace().all(|word|text.contains(word))
                && (prefix!="hue" || app.hue_room_filter.get().is_empty() || d["room_name"].as_str()==Some(app.hue_room_filter.get().as_str()))
        }).map(|d| discovery_card(app,&connection,&room,d)).collect_view()}</div>
    }.into_any()
}
fn discovery_card(app: App, connection: &Connection, room: &Id, value: Value) -> AnyView {
    let id = value[if connection.provider==Provider::UnifiProtect {"id"}else{"entity_id"}].as_str().unwrap_or("").to_string();
    let name = value["name"].as_str().unwrap_or(&id).to_string();
    let scene = value["resource_kind"] == "scene";
    let kind = if connection.provider == Provider::UnifiProtect { "camera" } else if connection.provider == Provider::HomeAssistant {
        if id.starts_with("cover.") { "blind" } else if id.starts_with("climate.") { "thermostat" } else { "light" }
    } else { "light" };
    let saved = if scene {
        app.config.get().and_then(|c| {
            c.scenes
                .iter()
                .find(|s| {
                    s.hue.as_ref().is_some_and(|h| {
                        h.connection_id == connection.id
                            && Some(h.scene_id.as_str()) == id.strip_prefix("scene:")
                    })
                })
                .cloned()
        })
    } else {
        None
    };
    let existing = if scene {
        None
    } else {
        assigned(app, connection, &id)
    };
    let used = if scene {
        saved.as_ref().is_some_and(|s| s.rooms.contains(room))
    } else {
        existing.is_some()
    };
    let detail = if scene {
        format!(
            "Hue scene · {} · Appears in this room’s Scenes button",
            value["room_name"].as_str().unwrap_or("")
        )
    } else {
        existing
            .map(|r| format!("Already in {r}"))
            .unwrap_or_else(|| {
                if kind == "camera" {
                    format!("Camera · {}", value["state"].as_str().unwrap_or("Unknown"))
                } else if value["resource_kind"] == "room" {
                    "Hue room · Control all its lights together".into()
                } else if kind == "blind" {
                    if value["state"].is_null() { "Blind · Unavailable".into() } else { "Blind · Open, close and supported position controls".into() }
                } else if kind == "thermostat" {
                    if value["available"] == false { "Thermostat · Unavailable".into() } else { "Thermostat · Temperature and HVAC mode".into() }
                } else if value["on"].is_null() {
                    "Unavailable".into()
                } else {
                    "Light · On/off and brightness".into()
                }
            })
    };
    let title = name.clone();
    let room = room.clone();
    let connection_id = connection.id.clone();
    view!{<div class="card discovered-device"><strong>{title}</strong><p class="dim">{detail}</p>
        <button class="primary" disabled=move ||app.busy.get()||used on:click=move |_|{
            if scene {
                if let Some(mut saved)=saved.clone() {
                    if !saved.rooms.contains(&room) {saved.rooms.push(room.clone());}
                    app.run(api::put(format!("/api/scenes/{}",saved.id),saved));
                } else {
                    app.run(api::post("/api/scenes",json!({"name":name,"rooms":[room],"hue":{"connection_id":connection_id,"scene_id":id.strip_prefix("scene:").unwrap_or("")}})));
                }
            } else { app.run(api::post(format!("/api/rooms/{room}/devices"),json!({"name":name,"kind":kind,"integration":{"via":"connection","connection_id":connection_id,"resource_id":id}}))); }
        }>{if used {"Added"} else {"Add to this room"}}</button>
    </div>}.into_any()
}
fn manual(app: App, connection: Connection, room: Id) -> AnyView {
    if connection.provider == Provider::Ir { return super::infrared::device_setup(app, room, None); }
    let television = matches!(connection.provider, Provider::WebOs | Provider::AndroidTv | Provider::AppleTv);
    let receiver = matches!(connection.provider, Provider::Denon { .. } | Provider::Sonos { .. });
    let existing = assigned(app, &connection, "");
    let name = RwSignal::new(connection.name.clone());
    view!{<form on:submit=move |e|{e.prevent_default();let title=name.get_untracked().trim().to_string();if title.is_empty(){return}app.run(api::post(format!("/api/rooms/{room}/devices"),json!({"name":title,"kind":if television{"tv"}else if receiver{"speaker"}else{"media-player"},"integration":{"via":"connection","connection_id":connection.id,"resource_id":""}})));}>
        {super::connections::field("Device name",name,"Living room TV")}
        {existing.as_ref().map(|r|view!{<p>{format!("This device is already in {r}")}</p>})}
        <button type="submit" class="primary" disabled=move ||app.busy.get()||existing.is_some()>"Add to this room"</button>
    </form>}.into_any()
}

pub fn controls(app: App, device: &Device) -> AnyView {
    let config = app.config.get_untracked().unwrap_or_default();
    let (prefix, id) = match config.resolve_integration(&device.integration) {
        Some(Integration::Sonos { .. }) => {
            return match &device.integration {
                Integration::Connection {connection_id,..} => super::sonos::controls(app,connection_id.to_string()),
                _ => ().into_any(),
            };
        }
        Some(Integration::Denon { .. }) => {
            return match &device.integration {
                Integration::Connection { connection_id, .. } => {
                    super::connections::denon_controls(app, connection_id.to_string())
                }
                _ => ().into_any(),
            }
        }
        Some(Integration::AndroidTv | Integration::AppleTv) => {
            let apple = matches!(
                config.resolve_integration(&device.integration),
                Some(Integration::AppleTv)
            );
            return match &device.integration {
                Integration::Connection { connection_id, .. } => super::streaming_tv::controls(
                    app,
                    format!(
                        "/api/connections/{connection_id}/{}",
                        if apple { "appletv" } else { "androidtv" }
                    ),
                ),
                _ => ().into_any(),
            };
        }
        Some(Integration::WebOs) => {
            let id=match &device.integration {Integration::Connection{connection_id,..}=>Some(connection_id.clone()),_=>config.connections.iter().find(|c|c.provider==Provider::WebOs).map(|c|c.id.clone())};
            return id.map(|id|super::webos::controls(app,format!("/api/connections/{id}/webos"))).unwrap_or_else(||().into_any());
        },
        Some(Integration::Hue { light_id }) => ("hue", light_id),
        Some(Integration::HomeAssistant { entity_id })
            if matches!(device.kind, couch_model::DeviceKind::Light | couch_model::DeviceKind::Blind | couch_model::DeviceKind::Thermostat) =>
        {
            ("ha", entity_id)
        }
        _ => return ().into_any(),
    };
    let (base,id)=if let Some((connection,resource))=id.split_once('/') {(format!("/api/connections/{connection}/{prefix}"),resource.to_string())}else{(format!("/api/{prefix}"),id)};
    let category = if prefix == "ha" && id.starts_with("cover.") { "covers" } else if prefix == "ha" && id.starts_with("climate.") { "climates" } else { "lights" };
    let base=StoredValue::new(base);
    let value = RwSignal::new(None::<Value>);
    let message = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|{let id=id.clone();busy.set(true);spawn_local(async move{match api::ha("GET",&format!("{}/{category}/{id}",base.get_value()),None).await{Ok(v)=>value.set(Some(v)),Err(e)=>{if e.unauthorized{app.paired.set(Some(false));}message.set(e.message);}}busy.set(false);});}>"Show device controls"</button><p role="status">{move ||message.get()}</p>{move ||value.get().map(|v|if prefix=="hue"{super::hue::controls(app,v,base.get_value())}else{super::home_assistant::controls(app,v,base.get_value())})}}.into_any()
}
