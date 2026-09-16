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
    let base = StoredValue::new(format!("/api/connections/{}/ha", connection.id));
    let url = RwSignal::new(String::new());
    let token = RwSignal::new(String::new());
    let token_set = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());

    spawn_local(async move {
        match api::ha("GET", &format!("{}/connection", base.get_value()), None).await {
            Ok(s) => {
                url.set(s["url"].as_str().unwrap_or("").into());
                token_set.set(s["token_set"].as_bool().unwrap_or(false));
            }
            Err(e) => fail(app, message, e),
        }
    });
    let save = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set("Testing the connection…".into());
        let data = json!({"url":url.get_untracked().trim(),"token":token.get_untracked().trim()});
        spawn_local(async move {
            match api::ha(
                "PUT",
                &format!("{}/connection", base.get_value()),
                Some(data),
            )
            .await
            {
                Ok(_result) => {
                    token.set(String::new());
                    token_set.set(true);
                    message.set("Connected and saved. Add devices from Rooms & devices.".into());
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="card ha-connection"><h2>"Home Assistant"</h2>
        <p>"Save the server and credentials used to reach your Home Assistant devices. Add its lights, blinds and thermostats from inside a room."</p>
        <label class="field">"Server URL"<input type="url" placeholder="http://homeassistant.local:8123" prop:value=move || url.get() disabled=move || busy.get() on:input=move |e| url.set(event_target_value(&e))/></label>
        <label class="field">"Long-lived access token"<input type="password" autocomplete="new-password" placeholder=move || if token_set.get() {"Saved — leave blank to keep"} else {"Paste a token from your Home Assistant profile"} prop:value=move || token.get() disabled=move || busy.get() on:input=move |e| token.set(event_target_value(&e))/></label>
        <p class="dim">"Stored privately on the remote, separately from your house configuration."</p>
        <div class="actions"><button class="primary" disabled=move || busy.get() on:click=save>"Test & save connection"</button>

        </div>
        <p role="status">{move || message.get()}</p>

        </section>
    }.into_any()
}
pub(super) fn controls(app: App, initial: Value, path: String) -> AnyView {
    if initial["entity_id"]
        .as_str()
        .is_some_and(|s| s.starts_with("cover.") || s.starts_with("climate."))
    {
        return environment_controls(app, initial, path);
    }
    let base = StoredValue::new(path);
    let light = RwSignal::new(initial);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let level = RwSignal::new("50".to_string());
    let perform = move |action: Option<Value>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let id = light.get_untracked()["entity_id"]
            .as_str()
            .unwrap_or("")
            .to_string();
        spawn_local(async move {
            if let Some(action) = action {
                if let Err(e) = api::ha(
                    "POST",
                    &format!("{}/lights/{id}/command", base.get_value()),
                    Some(action),
                )
                .await
                {
                    fail(app, message, e);
                    busy.set(false);
                    return;
                }
                message.set("Request accepted. Refresh to check the latest state.".into());
            } else {
                message.set(String::new());
            }
            match api::ha("GET", &format!("{}/lights/{id}", base.get_value()), None).await {
                Ok(value) => light.set(value),
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <p>{move || { let s = light.get(); match s["on"].as_bool() { Some(true) => match s["brightness_percent"].as_u64() {Some(n) => format!("On · {n}%"),None => "On".into()},Some(false) => "Off".into(),None => "Unavailable".into()} }}</p>
        <div class="actions"><button class="ghost" disabled=move || busy.get() || light.get()["on"].is_null() on:click=move |_| perform(Some(json!({"action":"on"})))>"Turn on"</button>
        <button class="ghost" disabled=move || busy.get() || light.get()["on"].is_null() on:click=move |_| perform(Some(json!({"action":"off"})))>"Turn off"</button>
        <button class="ghost" disabled=move || busy.get() on:click=move |_| perform(None)>"Refresh state"</button></div>
        <Show when=move || light.get()["dimmable"] == true>
            <label class="field">"Brightness (%)"<input type="number" min="0" max="100" prop:value=move || level.get() disabled=move || busy.get() on:input=move |e| level.set(event_target_value(&e))/></label>
            <button class="ghost" disabled=move || busy.get() || light.get()["on"].is_null() on:click=move |_| {
                match level.get_untracked().parse::<u8>() { Ok(p) if p <= 100 => perform(Some(json!({"action":"brightness","brightness":p}))), _ => message.set("Enter brightness from 0 to 100".into()) }
            }>"Apply brightness"</button>
        </Show>
        <p role="status">{move || message.get()}</p>
    }.into_any()
}

fn environment_controls(app: App, initial: Value, path: String) -> AnyView {
    let cover = initial["entity_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("cover."));
    let endpoint = StoredValue::new(format!(
        "{}/{}/{}",
        path,
        if cover { "covers" } else { "climates" },
        initial["entity_id"].as_str().unwrap_or("")
    ));
    let temperature = RwSignal::new(
        initial["target_temperature"]
            .as_f64()
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let low = RwSignal::new(
        initial["target_temperature_low"]
            .as_f64()
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let high = RwSignal::new(
        initial["target_temperature_high"]
            .as_f64()
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let position = RwSignal::new(
        initial["position_percent"]
            .as_u64()
            .unwrap_or(50)
            .to_string(),
    );
    let state = RwSignal::new(initial);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let unavailable = move || {
        if cover {
            state.get()["state"].is_null()
        } else {
            state.get()["available"] != true
        }
    };
    let perform = move |action: Option<Value>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            if let Some(action) = action {
                if let Err(e) = api::ha(
                    "POST",
                    &format!("{}/command", endpoint.get_value()),
                    Some(action),
                )
                .await
                {
                    fail(app, message, e);
                    busy.set(false);
                    return;
                }
                message.set("Request accepted. Refresh to check the latest state.".into());
            } else {
                message.set(String::new());
            }
            match api::ha("GET", &endpoint.get_value(), None).await {
                Ok(value) => {
                    temperature.set(
                        value["target_temperature"]
                            .as_f64()
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                    );
                    low.set(
                        value["target_temperature_low"]
                            .as_f64()
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                    );
                    high.set(
                        value["target_temperature_high"]
                            .as_f64()
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                    );
                    position.set(value["position_percent"].as_u64().unwrap_or(50).to_string());
                    state.set(value);
                }
                Err(e) => fail(app, message, e),
            }
            busy.set(false);
        });
    };
    view! {
        <p>{move || if unavailable() { "Unavailable".to_string() } else if cover {
            let v = state.get();
            let label = v["state"].as_str().unwrap_or("Unknown");
            v["position_percent"].as_u64().map(|p|format!("{label} · {p}% open")).unwrap_or_else(|| label.into())
        } else {
            let v = state.get();
            let unit = v["temperature_unit"].as_str().unwrap_or("");
            let current = v["current_temperature"].as_f64().map(|t|format!(" · Current {t}{unit}")).unwrap_or_default();
            format!("{}{}", v["hvac_mode"].as_str().unwrap_or("Unknown mode"), current)
        }}</p>
        <Show when=move ||cover>
            <div class="actions">
                <button class="ghost" disabled=move ||busy.get()||unavailable()||state.get()["can_open"]!=true on:click=move |_|perform(Some(json!({"action":"open"})))>"Open"</button>
                <button class="ghost" disabled=move ||busy.get()||unavailable()||state.get()["can_close"]!=true on:click=move |_|perform(Some(json!({"action":"close"})))>"Close"</button>
                <Show when=move ||state.get()["can_stop"]==true><button class="ghost" disabled=move ||busy.get()||unavailable() on:click=move |_|perform(Some(json!({"action":"stop"})))>"Stop"</button></Show>
            </div>
            <Show when=move ||state.get()["can_set_position"]==true>
                <label class="field">"Position (% open)"<input type="number" min="0" max="100" prop:value=move ||position.get() disabled=move ||busy.get()||unavailable() on:input=move |e|position.set(event_target_value(&e))/></label>
                <button class="ghost" disabled=move ||busy.get()||unavailable() on:click=move |_| { match position.get_untracked().parse::<u8>() {Ok(p) if p<=100=>perform(Some(json!({"action":"position","position":p}))),_=>message.set("Enter a position from 0 to 100".into())} }>"Set position"</button>
            </Show>
        </Show>
        <Show when=move ||!cover>
            <Show when=move ||state.get()["supports_target_temperature"]==true>
                <label class="field">{move ||format!("Target temperature ({})",state.get()["temperature_unit"].as_str().unwrap_or(""))}<input type="number" min=move ||state.get()["min_temperature"].to_string() max=move ||state.get()["max_temperature"].to_string() step=move ||state.get()["temperature_step"].to_string() prop:value=move ||temperature.get() disabled=move ||busy.get()||unavailable() on:input=move |e|temperature.set(event_target_value(&e))/></label>
                <button class="ghost" disabled=move ||busy.get()||unavailable() on:click=move |_| { match temperature.get_untracked().parse::<f64>() {Ok(t) if t.is_finite()=>perform(Some(json!({"action":"temperature","temperature":t}))),_=>message.set("Enter a target temperature".into())} }>"Set temperature"</button>
            </Show>
            <Show when=move ||state.get()["supports_target_range"]==true>
                <label class="field">"Lower target"<input type="number" step=move ||state.get()["temperature_step"].to_string() prop:value=move ||low.get() disabled=move ||busy.get()||unavailable() on:input=move |e|low.set(event_target_value(&e))/></label>
                <label class="field">"Upper target"<input type="number" step=move ||state.get()["temperature_step"].to_string() prop:value=move ||high.get() disabled=move ||busy.get()||unavailable() on:input=move |e|high.set(event_target_value(&e))/></label>
                <button class="ghost" disabled=move ||busy.get()||unavailable() on:click=move |_| { match (low.get_untracked().parse::<f64>(),high.get_untracked().parse::<f64>()) {(Ok(low),Ok(high)) if low.is_finite()&&high.is_finite()&&low<=high=>perform(Some(json!({"action":"range","low":low,"high":high}))),_=>message.set("Enter a lower target at or below the upper target".into())} }>"Set temperature range"</button>
            </Show>
            <label class="field">"HVAC mode"<select prop:value=move ||state.get()["hvac_mode"].as_str().unwrap_or("").to_string() disabled=move ||busy.get()||unavailable() on:change=move |e|perform(Some(json!({"action":"mode","mode":event_target_value(&e)})))>{move ||state.get()["hvac_modes"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v|v.as_str().map(str::to_string)).map(|mode|view!{<option value=mode.clone()>{mode.clone()}</option>}).collect_view()}</select></label>
        </Show>
        <button class="ghost" disabled=move ||busy.get() on:click=move |_|perform(None)>"Refresh state"</button>
        <p role="status">{move ||message.get()}</p>
    }.into_any()
}
