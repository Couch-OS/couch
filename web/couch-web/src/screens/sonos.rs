use crate::{api, App};
use couch_model::{Connection, Provider};
use leptos::{prelude::*, task::spawn_local};
use serde_json::json;

pub fn form(app: App, existing: Option<Connection>) -> AnyView {
    let name = RwSignal::new(
        existing
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or("Sonos".into()),
    );
    let host = RwSignal::new(match existing.as_ref().map(|c| &c.provider) {
        Some(Provider::Sonos { host }) => host.clone(),
        _ => String::new(),
    });
    let error = RwSignal::new(String::new());
    view! {<form on:submit=move |e| {
        e.prevent_default();
        let name = name.get_untracked().trim().to_owned();
        let host = host.get_untracked().trim().to_owned();
        if name.is_empty() || host.parse::<std::net::Ipv4Addr>().is_err() { error.set("Enter a connection name and the speaker’s IPv4 address".into()); return; }
        let body = json!({"name":name,"provider":Provider::Sonos{host}});
        match &existing { Some(c) => app.run(api::put(format!("/api/connections/{}",c.id),body)), None => app.run(api::post("/api/connections",body)) }
    }>
        {super::connections::field("Connection name",name,"Living room Sonos")}
        {super::connections::field("Speaker IPv4 address",host,"192.168.1.50")}
        <p class="dim">"Find the speaker’s address in the Sonos app or router. Keep Couch and Sonos on the same network. No cloud login is needed."</p>
        <p role="alert">{move || error.get()}</p><button type="submit" class="primary">"Save Sonos connection"</button>
    </form>}.into_any()
}

pub fn controls(app: App, id: String) -> AnyView {
    let base = StoredValue::new(format!("/api/connections/{id}/sonos"));
    let message = RwSignal::new("Refresh to check this speaker and its group.".to_owned());
    let busy = RwSignal::new(false);
    let volume = RwSignal::new("20".to_owned());
    let coordinator = RwSignal::new(false);
    let send = move |command: Option<&'static str>, value: Option<serde_json::Value>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            if let Some(command) = command {
                if let Err(e) = api::ha(
                    "POST",
                    &format!("{}/command", base.get_value()),
                    Some(json!({"command":command,"value":value})),
                )
                .await
                {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(e.message);
                    busy.set(false);
                    return;
                }
            }
            match api::ha("GET", &format!("{}/status", base.get_value()), None).await {
                Ok(s) => {
                    let own = s["player"]["uuid"]
                        .as_str()
                        .zip(s["coordinator"].as_str())
                        .is_some_and(|(a, b)| a == b);
                    coordinator.set(own);
                    if let Some(v) = s["volume"].as_u64() {
                        volume.set(v.to_string());
                    }
                    message.set(format!("{} · {} · Volume {}{} · {}",s["player"]["name"].as_str().unwrap_or("Sonos"),s["transport"].as_str().unwrap_or("Unknown state"),s["volume"],if s["muted"] == true {" · Muted"} else {""},if own {"Playback controls this speaker’s group".into()} else {format!("Playback coordinator: {}. Select its connection to control playback.",s["coordinator"].as_str().unwrap_or("unknown"))}));
                }
                Err(e) => {
                    coordinator.set(false);
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(if command.is_some() {
                        format!("Command acknowledged; status refresh failed: {}", e.message)
                    } else {
                        e.message
                    });
                }
            }
            busy.set(false);
        });
    };
    view! {<section><h3>"Sonos controls"</h3><p role="status">{move || message.get()}</p>
        <p class="dim">"Playback affects the selected coordinator’s group. Volume and mute affect this speaker. Couch never changes groups or forwards commands to another speaker."</p>
        <div class="actions"><button class="ghost" disabled=move ||busy.get() on:click=move |_|send(None,None)>"Test connection / refresh"</button>
        {[("play","Play"),("pause","Pause"),("stop","Stop"),("previous","Previous"),("next","Next")].into_iter().map(move |(command,label)|view!{<button class="ghost" disabled=move ||busy.get() || !coordinator.get() on:click=move |_|send(Some(command),None)>{label}</button>}).collect_view()}
        {[("volume-down","Volume −"),("volume-up","Volume +"),("mute-on","Mute"),("mute-off","Unmute")].into_iter().map(move |(command,label)|view!{<button class="ghost" disabled=move ||busy.get() on:click=move |_|send(Some(command),None)>{label}</button>}).collect_view()}
        </div>{super::connections::field("Volume (0–100)",volume,"20")}
        <button class="primary" disabled=move ||busy.get() on:click=move |_| {
            match volume.get_untracked().parse::<u8>() { Ok(v) if v<=100 => send(Some("volume"),Some(json!(v))), _ => message.set("Enter volume from 0 to 100".into()) }
        }>"Set volume"</button>
    </section>}.into_any()
}
