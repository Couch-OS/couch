//! Samsung Tizen setup: find or enter the TV, allow Couch on its screen, then
//! test controls. Credentials never reach the browser.
use crate::{api, App};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
#[derive(Clone, serde::Deserialize)]
struct Found {
    name: String,
    address: String,
    model: String,
}
fn fail(app: App, message: RwSignal<String>, error: api::ApiError) {
    if error.unauthorized {
        app.paired.set(Some(false));
    }
    message.set(error.message);
}
pub fn setup(app: App, connection: &couch_model::Connection) -> AnyView {
    let base = StoredValue::new(format!("/api/connections/{}/tizen", connection.id));
    let address = RwSignal::new(String::new());
    let legacy = RwSignal::new(false);
    let paired = RwSignal::new(false);
    let detail = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let found = RwSignal::new(Vec::<Found>::new());
    let describe = move |v: &Value| {
        let model = v["model"].as_str().unwrap_or("");
        let name = v["name"].as_str().unwrap_or("");
        let mut text = format!("{name} {model}").trim().to_string();
        if v["wake_supported"] == false {
            text.push_str(
                " · No MAC reported: Wake-on-LAN unavailable until paired again with the TV on",
            );
        }
        if v["frame_tv"] == true {
            text.push_str(" · The Frame: power holds the key for three seconds");
        }
        if v["secure"] == false {
            text.push_str(" · Unencrypted legacy connection");
        }
        text
    };
    spawn_local(async move {
        let result = api::ha("GET", &format!("{}/connection", base.get_value()), None).await;
        if paired.try_get_untracked().is_none() {
            return;
        }
        match result {
            Ok(v) => {
                paired.set(v["paired"] == true);
                if let Some(saved) = v["address"].as_str() {
                    address.set(saved.into());
                }
                legacy.set(v["secure"] == false && v["paired"] == true);
                if v["paired"] == true {
                    detail.set(describe(&v));
                    message.set(
                        "Pairing saved. Test connection to check that the TV is reachable.".into(),
                    );
                }
            }
            Err(e) => fail(app, message, e),
        }
    });
    let request = move |operation: &'static str| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(
            match operation {
                "discover" => "Looking for Samsung TVs on this network…",
                "pair" => "Look at your TV and choose Allow. Waiting up to 60 seconds…",
                _ => "Checking the saved connection…",
            }
            .into(),
        );
        spawn_local(async move {
            let result = match operation {
                "discover" => api::ha("GET", &format!("{}/discover", base.get_value()), None).await,
                "pair" => {
                    api::ha(
                        "PUT",
                        &format!("{}/connection", base.get_value()),
                        Some(json!({"address":address.get_untracked().trim(),"legacy":legacy.get_untracked()})),
                    )
                    .await
                }
                _ => api::ha("GET", &format!("{}/status", base.get_value()), None).await,
            };
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(v) => {
                    match operation {
                        "discover" => {
                            let tvs: Vec<Found> = serde_json::from_value(v).unwrap_or_default();
                            message.set(if tvs.is_empty() { "No Samsung TVs answered. Make sure the TV is on and on this network, or enter its address manually." } else { "Select your TV below." }.into());
                            found.set(tvs);
                        }
                        "pair" => {
                            paired.set(true);
                            detail.set(describe(&v));
                            message.set("TV allowed Couch and the pairing is saved. Add it in Rooms & devices.".into());
                        }
                        _ => {
                            let power = v["power_state"].as_str().unwrap_or("not reported");
                            message.set(format!("TV is reachable · power state {power}. Connection saved; add it in Rooms & devices."));
                        }
                    }
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="card"><h2>"Samsung Tizen TV"</h2>
        <p class="notice">"Experimental Rust client written from protocol documentation. Pairing and controls still need validation on a real Samsung TV; please report what works."</p>
        <p>"Turn on the TV and connect it to your network. Find it below or enter its IP address, then choose Allow on the TV when it asks about “couch.”."</p>
        <div class="actions"><button class="ghost" disabled=move ||busy.get() on:click=move |_|request("discover")>"Find Samsung TVs"</button></div>
        <div class="destination-grid">{move ||found.get().into_iter().map(|tv|{let label=format!("{} · {}",tv.name,tv.model);let target=tv.address.clone();view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|address.set(target.clone())>{label}</button>}}).collect_view()}</div>
        <label class="field">"TV IP address"<input type="text" placeholder="192.168.1.50" prop:value=move ||address.get() disabled=move ||busy.get() on:input=move |e|address.set(event_target_value(&e))/></label>
        <label class="field"><span><input type="checkbox" prop:checked=move ||legacy.get() disabled=move ||busy.get() on:change=move |e|legacy.set(event_target_checked(&e))/>{" Use the unencrypted 2016-model connection (port 8001, no token)"}</span></label>
        <p class="dim">"Encrypted, token-based pairing on port 8002 is the default for 2017 and newer TVs; the TV’s certificate is pinned while you approve the prompt. If the TV later denies Couch, pair again. Each named connection has its own pairing."</p>
        <div class="actions"><button class="primary" disabled=move ||busy.get() on:click=move |_|request("pair")>{move ||if paired.get(){"Pair again / change TV"}else{"Pair TV"}}</button>
        <Show when=move ||paired.get()><button class="ghost" disabled=move ||busy.get() on:click=move |_|request("status")>"Test connection"</button></Show></div>
        <p class="dim">{move ||detail.get()}</p>
        <p role="status">{move ||message.get()}</p>
        <Show when=move ||paired.get()>{move ||controls(app,base.get_value())}</Show>
        </section>
    }.into_any()
}
pub fn controls(app: App, path: String) -> AnyView {
    let base = StoredValue::new(path);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let apps = RwSignal::new(Vec::<Value>::new());
    let send = move |command: String| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set("Contacting TV…".into());
        spawn_local(async move {
            let result = api::ha(
                "POST",
                &format!("{}/command", base.get_value()),
                Some(json!({"command":command})),
            )
            .await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(v) => message.set(
                    v["note"]
                        .as_str()
                        .unwrap_or("Command sent. Keys have no acknowledgement; watch the TV.")
                        .into(),
                ),
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    let load_apps = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set("Asking the TV for its installed apps…".into());
        spawn_local(async move {
            let result = api::ha("GET", &format!("{}/apps", base.get_value()), None).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(v) => {
                    let items = v["apps"].as_array().cloned().unwrap_or_default();
                    message.set(
                        if items.is_empty() {
                            "The TV reported no apps."
                        } else {
                            "Choose an app to open it on the TV."
                        }
                        .into(),
                    );
                    apps.set(items);
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <h3>"Test TV controls"</h3>
        <p class="dim">"Select this device on the remote to take control with its physical buttons. Power is the TV’s toggle key; Wake sends Wake-on-LAN to the MAC the TV reported."</p>
        <div class="actions">{
            [("up","Up"),("down","Down"),("left","Left"),("right","Right"),("ok","OK"),("back","Back"),("home","Home"),("menu","Menu"),("volume-down","Volume −"),("volume-up","Volume +"),("mute","Mute"),("play","Play"),("pause","Pause"),("power-on","Wake"),("power-off","Power key")]
            .into_iter().map(move |(command,label)|view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|send(command.into())>{label}</button>}).collect_view()
        }</div>
        <div class="actions">{
            couch_model::commands::TIZEN_INPUTS.iter().map(move |id|{let command=format!("input:{id}");let label=format!("Input · {}",id.to_ascii_uppercase());view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|send(command.clone())>{label}</button>}}).collect_view()
        }</div>
        <div class="actions"><button class="ghost" disabled=move ||busy.get() on:click=load_apps>"Load apps"</button></div>
        <div class="actions">{move ||apps.get().into_iter().map(|v|{let id=v["id"].as_str().unwrap_or("").to_string();let label=v["title"].as_str().unwrap_or(&id).to_string();let command=format!("app:{id}");view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|send(command.clone())>{label}</button>}}).collect_view()}</div>
        <p role="status">{move ||message.get()}</p>
    }.into_any()
}
