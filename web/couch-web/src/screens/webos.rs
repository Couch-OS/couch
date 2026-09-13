use crate::{api, App};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn fail(app: App, message: RwSignal<String>, error: api::ApiError) {
    if error.unauthorized {
        app.paired.set(Some(false));
    }
    message.set(error.message);
}
pub fn setup(app: App, connection: &couch_model::Connection) -> AnyView {
    let base=StoredValue::new(format!("/api/connections/{}/webos",connection.id));
    let address = RwSignal::new(String::new());
    let legacy = RwSignal::new(false);
    let paired = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    spawn_local(async move {
        match api::ha("GET", &format!("{}/connection",base.get_value()), None).await {
            Ok(v) => {
                paired.set(v["paired"] == true);
                let url = v["url"].as_str().unwrap_or("");
                legacy.set(url.starts_with("ws://"));
                let host = url.split_once("://").map(|(_, s)| s).unwrap_or("");
                address.set(
                    host.rsplit_once(':')
                        .map(|(h, _)| h.trim_matches(['[', ']']))
                        .unwrap_or("")
                        .into(),
                );
                if v["paired"] == true {
                    message.set(
                        "Pairing saved. Test connection to check that the TV is reachable.".into(),
                    );
                }
            }
            Err(e) => fail(app, message, e),
        }
    });
    let pair = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(
            "Look at your TV and accept the connection request. Waiting up to 60 seconds…".into(),
        );
        let body = json!({"address":address.get_untracked(),"legacy":legacy.get_untracked()});
        spawn_local(async move {
            match api::ha("PUT", &format!("{}/connection",base.get_value()), Some(body)).await {
                Ok(_) => {
                    paired.set(true);

                    message
                        .set("TV connected and pairing saved. Add it in Rooms & devices.".into());
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="card"><h2>"LG webOS TV"</h2>
        <p>"Turn on the TV, connect it to your network, then enter its IP address. Accept Couch’s connection request on the TV when prompted."</p>
        <label class="field">"TV IP address"<input type="text" placeholder="192.168.1.176" prop:value=move ||address.get() disabled=move ||busy.get() on:input=move |e|address.set(event_target_value(&e))/></label>
        <label class="field"><span><input type="checkbox" prop:checked=move ||legacy.get() disabled=move ||busy.get() on:change=move |e|legacy.set(event_target_checked(&e))/>{" Use unencrypted connection for older TVs (port 3000)"}</span></label>
        <p class="dim">"Encrypted pairing is the default. Enable LG Connect Apps or mobile-device control in the TV’s settings if connections are disabled. Each named connection has its own TV pairing. To add another TV, create another connection."</p>
        <div class="actions"><button class="primary" disabled=move ||busy.get() on:click=pair>{move ||if paired.get(){"Pair again / change TV"}else{"Pair TV"}}</button>
        <Show when=move ||paired.get()><button class="ghost" disabled=move ||busy.get() on:click=move |_|{busy.set(true);message.set("Testing saved connection…".into());spawn_local(async move{match api::ha("GET",&format!("{}/status",base.get_value()),None).await{Ok(_)=>{message.set("TV is reachable. Connection saved; add it in Rooms & devices.".into());},Err(e)=>fail(app,message,e)}busy.set(false);});}>"Test connection & save"</button></Show></div>
        <p role="status">{move ||message.get()}</p>
        <Show when=move ||paired.get()>{move ||power_settings(app,base.get_value())}{move ||controls(app,base.get_value())}</Show>
        </section>
    }.into_any()
}
pub fn controls(app: App, path: String) -> AnyView {
    let base=StoredValue::new(path);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Value>);
    let inputs = RwSignal::new(Vec::<Value>::new());
    let apps = RwSignal::new(Vec::<Value>::new());
    let perform = move |action: Option<Value>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set("Contacting TV…".into());
        spawn_local(async move {
            if let Some(action) = action {
                if let Err(e) = api::ha("POST", &format!("{}/command",base.get_value()), Some(action)).await {
                    fail(app, message, e);
                    busy.set(false);
                    return;
                }
            }
            match api::ha("GET", &format!("{}/status",base.get_value()), None).await {
                Ok(v) => {
                    status.set(Some(v));
                    message.set("TV connected.".into());
                }
                Err(e) => {
                    status.set(None);
                    fail(app, message, e);
                }
            }
            busy.set(false);
        });
    };
    let fetch = move |kind: &'static str| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::ha("GET", &format!("{}/{kind}",base.get_value()), None).await {
                Ok(v) => {
                    let items = v[if kind == "inputs" {
                        "devices"
                    } else {
                        "launchPoints"
                    }]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                    if kind == "inputs" {
                        inputs.set(items);
                    } else {
                        apps.set(items);
                    }
                    message.set("Choose an input or app to switch the TV.".into());
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <h3>"Test TV controls"</h3>
        <p>{move ||status.get().map(|s|{let v=if s["volume"]["volumeStatus"].is_object(){&s["volume"]["volumeStatus"]}else{&s["volume"]};format!("Power: {} · Volume: {} · App: {}",s["power"]["state"].as_str().unwrap_or("unknown"),v["volume"],s["app"]["appId"].as_str().unwrap_or("unknown"))}).unwrap_or_else(||"Load status to check the TV.".into())}</p>
        <div class="actions"><button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(None)>"Read TV status"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"volume-down"})))>"Volume −"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"volume-up"})))>"Volume +"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"mute","on":true})))>"Mute"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"mute","on":false})))>"Unmute"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|fetch("inputs")>"Load inputs"</button>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|fetch("apps")>"Load apps"</button></div>
        <div class="actions">{move ||inputs.get().into_iter().map(|v|{let id=v["id"].as_str().unwrap_or("").to_string();let label=v["label"].as_str().unwrap_or(&id).to_string();view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"input","id":id})))>{label}</button>}}).collect_view()}</div>
        <div class="actions">{move ||apps.get().into_iter().map(|v|{let id=v["id"].as_str().unwrap_or("").to_string();let label=v["title"].as_str().unwrap_or(&id).to_string();view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(Some(json!({"action":"launch","id":id})))>{label}</button>}}).collect_view()}</div>
        <p role="status">{move ||message.get()}</p>
    }.into_any()
}


fn power_settings(app: App, base: String) -> AnyView {
    let endpoint=StoredValue::new(format!("{base}/power"));
    let method=RwSignal::new("ir".to_string());
    let codeset=RwSignal::new(String::new());
    let busy=RwSignal::new(true);
    let loaded=RwSignal::new(false);
    let available=RwSignal::new(false);
    let message=RwSignal::new(String::new());
    spawn_local(async move {
        let response=api::ha("GET",&endpoint.get_value(),None).await;
        if busy.try_get_untracked().is_none(){return;}
        match response {
            Ok(v)=>{method.set(v["method"].as_str().unwrap_or("ir").into());codeset.set(v["codeset"].as_str().unwrap_or("").into());available.set(v["blaster_available"]==true);loaded.set(true);},
            Err(e)=>fail(app,message,e),
        }
        busy.set(false);
    });
    view!{<section class="card"><h3>"Power control"</h3>
        <p>"IR is the default and uses the remote’s own blaster. Network power is an explicit alternative: Wake-on-LAN to turn on, webOS to turn off."</p>
        <label class="field">"Power method"<select prop:value=move ||method.get() disabled=move ||busy.get()||!loaded.get() on:change=move |e|method.set(event_target_value(&e))>
            <option value="ir">"IR (default)"</option><option value="network">"Network (Wake-on-LAN)"</option>
        </select></label>
        <Show when=move ||method.get()=="ir">
            <p class="dim">{move ||if available.get(){"IR device node is present. Confirm your TV’s codes and line of sight before use."}else{"IR blaster is unavailable on this system. Power will report an error until its driver is enabled; select network power explicitly if needed."}}</p>
            <details><summary>"Choose power codes from the library"</summary>{super::infrared::library(app,codeset,true)}</details>
            <label class="field">"IR power code assignments"<textarea rows="4" maxlength="4096" prop:value=move ||codeset.get() disabled=move ||busy.get() on:input=move |e|codeset.set(event_target_value(&e)) /></label>
            <p class="dim">"Use couch-ir codeset lines: button protocol address command. Use power for the physical toggle, and power-on / power-off for activity commands. Enter codes verified for your TV; none are supplied automatically. A missing discrete code never falls back to toggle."</p>
        </Show>
        <button class="primary" disabled=move ||busy.get()||!loaded.get() on:click=move |_|{
            busy.set(true);let body=json!({"method":method.get_untracked(),"codeset":codeset.get_untracked()});
            spawn_local(async move{let response=api::ha("PUT",&endpoint.get_value(),Some(body)).await;if busy.try_get_untracked().is_none(){return;}match response{Ok(_)=>message.set("Power settings saved. No command was sent.".into()),Err(e)=>fail(app,message,e)}busy.set(false);});
        }>"Save power settings"</button><p role="status">{move ||message.get()}</p>
    </section>}.into_any()
}
