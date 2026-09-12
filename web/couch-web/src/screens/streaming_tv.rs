//! Explicit begin → on-screen PIN → save pairing, without exposing credentials.
use crate::{api, App};
use couch_model::{Connection, Provider};
use leptos::{prelude::*, task::spawn_local};
use serde_json::json;
#[derive(Clone, serde::Deserialize)]
struct Found {
    name: String,
    address: String,
    port: u16,
}
pub fn setup(app: App, connection: &Connection) -> AnyView {
    let companion = pairing(app, connection, false);
    let metadata = if connection.provider == Provider::AppleTv {
        view! {<section class="apple-metadata"><h3>"Optional now playing"</h3><p class="dim">"Pair AirPlay separately to show titles and playback progress. Companion controls keep working without this. App metadata varies; artwork is not fetched."</p>{pairing(app, connection, true)}</section>}.into_any()
    } else {
        ().into_any()
    };
    view! {{companion}{metadata}}.into_any()
}
fn pairing(app: App, connection: &Connection, metadata: bool) -> AnyView {
    let shortcut_id = StoredValue::new(connection.id.clone());
    let apple = connection.provider == Provider::AppleTv;
    let kind = if apple { "appletv" } else { "androidtv" };
    let suffix = if metadata { "/metadata" } else { "" };
    let base = StoredValue::new(format!("/api/connections/{}/{kind}{suffix}", connection.id));
    let address = RwSignal::new(String::new());
    let port = RwSignal::new(if apple { String::new() } else { "6466".into() });
    let code = RwSignal::new(String::new());
    let token = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let paired = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let found = RwSignal::new(Vec::<Found>::new());
    spawn_local(async move {
        if let Ok(value) = api::ha("GET", &format!("{}/connection", base.get_value()), None).await {
            if paired.try_get_untracked().is_none() {
                return;
            }
            paired.set(value["paired"] == true);
            if let Some(saved) = value["address"].as_str() {
                address.set(saved.into());
            }
            if let Some(saved) = value["port"].as_u64() {
                port.set(saved.to_string());
            }
        }
    });
    let request = move |operation: &'static str| {
        if busy.get_untracked() {
            return;
        }
        let body = match operation {
            "pair-start" => {
                let Ok(port) = port.get_untracked().parse::<u16>() else {
                    message.set("Choose a discovered TV or enter its port".into());
                    return;
                };
                if port == 0 || address.get_untracked().trim().is_empty() {
                    message.set("Choose a TV or enter its address and port".into());
                    return;
                }
                Some(json!({"address":address.get_untracked().trim(),"port":port}))
            }
            "pair-finish" => {
                Some(json!({"token":token.get_untracked(),"code":code.get_untracked().trim()}))
            }
            _ => None,
        };
        busy.set(true);
        message.set(
            match operation {
                "pair-start" => "Asking the TV to display a code…",
                "pair-finish" => "Verifying the code and saving this connection…",
                "discover" => "Looking for TVs on this network…",
                _ => "Checking connection…",
            }
            .into(),
        );
        spawn_local(async move {
            let method = if matches!(operation, "pairing" | "connection") {
                "DELETE"
            } else if matches!(operation, "status" | "discover") {
                "GET"
            } else {
                "POST"
            };
            let result = api::ha(method, &format!("{}/{operation}", base.get_value()), body).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(value) => match operation {
                    "discover" => {
                        let values: Vec<Found> = serde_json::from_value(value).unwrap_or_default();
                        message.set(if values.is_empty(){"No TVs found. Make sure the TV is awake and on this network, or enter its address manually."}else{"Select your TV below."}.into());
                        found.set(values);
                    }
                    "pair-start" => {
                        token.set(value["token"].as_str().unwrap_or("").into());
                        code.set(String::new());
                        message.set("Enter the code shown on the TV within two minutes.".into());
                    }
                    "pair-finish" => {
                        paired.set(true);
                        token.set(String::new());
                        code.set(String::new());
                        message.set(if metadata { "Metadata paired. Reopen this TV on the remote to see now playing." } else if apple {
                            "Paired. Add this TV to a room and select it on the remote to take control."
                        } else {
                            "Paired. Add this TV to a room and select it on the remote to take control."
                        }.into());
                    }
                    "connection" => {
                        paired.set(false);
                        token.set(String::new());
                        code.set(String::new());
                        message.set(
                            "Metadata pairing removed. Companion controls are unchanged.".into(),
                        );
                    }
                    "status" if metadata => {
                        let m = &value["now_playing"];
                        let title = m["title"].as_str().unwrap_or("No title reported");
                        let state = m["state"].as_str().unwrap_or("unknown");
                        message.set(format!("{title} · {state}"));
                    }
                    "pairing" => {
                        token.set(String::new());
                        code.set(String::new());
                        message.set("Pairing cancelled.".into());
                    }
                    _ => message.set("Connected successfully.".into()),
                },
                Err(error) => {
                    if error.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(error.message);
                }
            }
            busy.set(false);
        });
    };
    view!{
  <p class="notice">{if metadata { "Experimental AirPlay metadata. Physical TV validation is pending." } else { "Experimental Rust client. Pairing and controls still need validation on your TV." }}</p>
  <p>{move ||if paired.get(){"Paired connection"}else{"Not paired yet"}}</p>
  <div class="actions"><button class="ghost" disabled=move ||busy.get()||!token.get().is_empty() on:click=move |_|request("discover")>"Find TVs on this network"</button></div>
  <div class="destination-grid">{move ||found.get().into_iter().map(|tv|{let name=tv.name.clone();view!{<button class="ghost" disabled=move ||busy.get()||!token.get().is_empty() on:click=move |_|{address.set(tv.address.clone());port.set(tv.port.to_string());}>{name}</button>}}).collect_view()}</div>
  {super::connections::field("TV IP address",address,"192.168.1.100")}
  {super::connections::field(if metadata{"AirPlay port"}else if apple{"Companion port"}else{"Remote control port"},port,if apple{"Choose a discovered TV"}else{"6466"})}
  <p class="dim">{if metadata{"Enable AirPlay on Apple TV. Discovery uses its AirPlay service and port; an Apple ID password is not needed."}else if apple{"Discovery fills in the Companion port. Keep the Apple TV awake while pairing; an Apple ID password is not needed."}else{"Keep Android TV Remote Service enabled. Pairing uses port 6467; ADB and developer mode are not required."}}</p>
  {move ||if token.get().is_empty(){view!{<div class="actions"><button class="primary" disabled=move ||busy.get() on:click=move |_|request("pair-start")>{move ||if paired.get(){"Pair again"}else{"Show pairing code on TV"}}</button><button class="ghost" disabled=move ||busy.get()||!paired.get() on:click=move |_|request("status")>"Test connection"</button></div>}.into_any()}else{view!{<label class="field">"Code shown on the TV"<input aria-label="TV pairing code" autocomplete="off" maxlength=if apple{4}else{6} prop:value=move ||code.get() on:input=move |e|code.set(event_target_value(&e))/></label><div class="actions"><button class="primary" disabled=move ||busy.get() on:click=move |_|request("pair-finish")>"Verify & save pairing"</button><button class="ghost" disabled=move ||busy.get() on:click=move |_|request("pairing")>"Cancel pairing"</button></div>}.into_any()}}
  <p role="status">{move ||message.get()}</p>
  {move ||if metadata && paired.get(){view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|request("connection")>"Remove metadata pairing"</button>}.into_any()}else{().into_any()}}
  {move ||if !apple && paired.get(){shortcuts(app,shortcut_id.get_value())}else{().into_any()}}
 }.into_any()
}

pub(super) fn controls(app: App, base: String) -> AnyView {
    let android = base.contains("/androidtv");
    let base = StoredValue::new(base);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    view!{<section><p class="dim">{if android {"Select this device on the remote to take control with its physical buttons."} else {"Select this device on the remote for physical controls and installed apps. Apple TV remains experimental."}}</p><div class="actions">{
  [("up","Up"),("down","Down"),("left","Left"),("right","Right"),("ok","OK"),("back","Back"),("home","Home"),("play-pause","Play / pause"),("volume-down","Volume −"),("volume-up","Volume +")].into_iter().map(move |(command,label)|view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|{if busy.get_untracked(){return}busy.set(true);spawn_local(async move{let result=api::ha("POST",&format!("{}/command",base.get_value()),Some(json!({"command":command}))).await;if busy.try_get_untracked().is_none(){return}match result{Ok(_)=>message.set("Command sent.".into()),Err(e)=>{if e.unauthorized{app.paired.set(Some(false));}message.set(e.message);}}busy.set(false);});}>{label}</button>}).collect_view()
 }</div><p role="status">{move ||message.get()}</p></section>}.into_any()
}

fn shortcuts(app: App, id: couch_model::Id) -> AnyView {
    let initial = app
        .config
        .get_untracked()
        .and_then(|c| c.app_shortcuts.get(&id).cloned())
        .unwrap_or_default();
    let rows = RwSignal::new(
        initial
            .into_iter()
            .map(|row| (RwSignal::new(row.name), RwSignal::new(row.url)))
            .collect::<Vec<_>>(),
    );
    let endpoint = StoredValue::new(format!("/api/connections/{id}/androidtv/apps"));
    view!{<section class="creation"><h3>"App shortcuts"</h3>
      <p class="dim">"Add your favorite apps using their launch links. These are configured shortcuts, not a list of installed apps. Links open only if the TV has an app that handles them. Do not include login credentials."</p>
      {move ||rows.get().into_iter().enumerate().map(move |(i,(name,url))|view!{<div class="card">
        <label class="field">"App name"<input aria-label=format!("App {} name",i+1) maxlength="64" prop:value=move ||name.get() on:input=move |e|name.set(event_target_value(&e))/></label>
        <label class="field">"App launch link"<input aria-label=format!("App {} launch link",i+1) maxlength="2048" placeholder="https://www.youtube.com/" prop:value=move ||url.get() on:input=move |e|url.set(event_target_value(&e))/></label>
        <div class="actions"><button class="ghost" disabled=i==0 aria-label=format!("Move app {} up",i+1) on:click=move |_|rows.update(|r|{if i>0 && i<r.len(){r.swap(i,i-1)}})>"Move up"</button>
        <button class="ghost" disabled={move ||i+1>=rows.get().len()} aria-label=format!("Move app {} down",i+1) on:click=move |_|rows.update(|r|{if i+1<r.len(){r.swap(i,i+1)}})>"Move down"</button>
        <button aria-label=format!("Remove app {}",i+1) on:click=move |_|rows.update(|r|{if i<r.len(){r.remove(i);}})>"Remove"</button></div>
      </div>}).collect_view()}
      <div class="actions"><button class="ghost" disabled={move ||rows.get().len()>=24} on:click=move |_|rows.update(|r|r.push((RwSignal::new(String::new()),RwSignal::new(String::new()))))>"Add app shortcut"</button>
      <button class="primary" on:click=move |_|app.run(api::put(endpoint.get_value(),rows.get_untracked().into_iter().map(|(name,url)|couch_model::AppShortcut{name:name.get_untracked(),url:url.get_untracked()}).collect::<Vec<_>>()))>"Save app shortcuts"</button></div>
    </section>}.into_any()
}
