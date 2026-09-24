use crate::{api, App};
use couch_model::Connection;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

pub(super) fn setup(app: App, connection: &Connection) -> AnyView {
    let base = StoredValue::new(format!("/api/connections/{}/protect", connection.id));
    let address = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(saved) = api::ha("GET", &format!("{}/connection", base.get_value()), None).await {
            address.set(saved["address"].as_str().unwrap_or("").into());
            if saved["key_set"] == true {
                message.set(
                    "Enrollment saved. Enter the API key to test or replace these settings.".into(),
                );
            }
        }
    });
    let save = move |_| {
        if busy.get_untracked() {
            return;
        }
        let data = json!({"address":address.get_untracked().trim(),"api_key":key.get_untracked()});
        key.set(String::new());
        busy.set(true);
        message.set("Checking the NVR and camera stream…".into());
        spawn_local(async move {
            match api::ha(
                "PUT",
                &format!("{}/connection", base.get_value()),
                Some(data),
            )
            .await
            {
                Ok(_) => message.set("Connected. Add cameras from Rooms & devices.".into()),
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
    view! {
        <section class="protect-setup">
        <h3>"UniFi Protect"</h3>
        <p>"Enter the local NVR IP and its Integration API key. Couch records the NVR certificates during enrollment and refuses changed certificates later. Camera settings and shared streams are never changed."</p>
        <label class="field">"NVR IP address"<input type="text" inputmode="decimal" placeholder="192.168.1.20" prop:value=move ||address.get() disabled=move ||busy.get() on:input=move |e|address.set(event_target_value(&e))/></label>
        <label class="field">"Integration API key"<input type="password" autocomplete="new-password" prop:value=move ||key.get() disabled=move ||busy.get() on:input=move |e|key.set(event_target_value(&e))/></label>
        <button class="primary" disabled=move ||busy.get() on:click=save>"Test & save"</button>
        <p role="status">{move ||message.get()}</p>
        </section>
    }.into_any()
}
