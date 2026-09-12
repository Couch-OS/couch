use crate::{api, App};
use couch_model::Connection;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

pub(super) fn setup(app: App, connection: &Connection) -> AnyView {
    let base = StoredValue::new(format!("/api/connections/{}/protect", connection.id));
    let origin = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    let pin = RwSignal::new(String::new());
    let media_origin = RwSignal::new(String::new());
    let media_pin = RwSignal::new(String::new());
    let stream_host = RwSignal::new(String::new());
    let approved = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(saved) = api::ha("GET", &format!("{}/connection", base.get_value()), None).await {
            origin.set(saved["origin"].as_str().unwrap_or("").into());
            media_origin.set(saved["media_origin"].as_str().unwrap_or("").into());
            media_pin.set(
                saved["media_certificate_sha256"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
            );
            stream_host.set(saved["stream_host"].as_str().unwrap_or("").into());
            pin.set(saved["certificate_sha256"].as_str().unwrap_or("").into());
            if saved["key_set"] == true {
                message.set(
                    "Enrollment saved. Enter an API key only when replacing these settings.".into(),
                );
            }
        }
    });
    let save = move |_| {
        if busy.get_untracked() {
            return;
        }
        if (!pin.get_untracked().is_empty() || !media_pin.get_untracked().is_empty())
            && !approved.get_untracked()
        {
            message
                .set("Confirm the certificate fingerprint through a trusted channel first.".into());
            return;
        }
        let fingerprint = pin.get_untracked().trim().to_owned();
        let media_fingerprint = media_pin.get_untracked().trim().to_owned();
        let host = stream_host.get_untracked().trim().to_owned();
        let data = json!({"origin":origin.get_untracked().trim(),"api_key":key.get_untracked(),"media_origin":if media_origin.get_untracked().trim().is_empty(){None}else{Some(media_origin.get_untracked().trim().to_string())},"media_certificate_sha256":if media_fingerprint.is_empty(){None}else{Some(media_fingerprint)},"stream_host":if host.is_empty(){None}else{Some(host)},"certificate_sha256":if fingerprint.is_empty(){None}else{Some(fingerprint)}});
        key.set(String::new());
        busy.set(true);
        message.set("Checking camera access…".into());
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
        <p>"Connect to your local Protect console using its Integration API key. Camera settings and shared streams are never changed."</p>
        <label class="field">"Console HTTPS address"<input type="url" placeholder="https://nvr.unifi" prop:value=move ||origin.get() disabled=move ||busy.get() on:input=move |e|{origin.set(event_target_value(&e));approved.set(false);}/></label>
        <label class="field">"Integration API key"<input type="password" autocomplete="new-password" prop:value=move ||key.get() disabled=move ||busy.get() on:input=move |e|key.set(event_target_value(&e))/></label>
        <details><summary>"Private console certificate"</summary>
        <p>"Leave empty for normal certificate authority and hostname verification. For a self-signed console, explicitly verify the SHA-256 certificate fingerprint with your administrator. A changed certificate will stop the connection."</p>
        <label class="field">"Certificate SHA-256 (64 hexadecimal characters)"<input type="text" prop:value=move ||pin.get() disabled=move ||busy.get() on:input=move |e|{pin.set(event_target_value(&e));approved.set(false);}/></label>
        <label class="field">"Pinned media origin (for example rtsps://nvr.unifi:7441)"<input type="url" prop:value=move ||media_origin.get() disabled=move ||busy.get() on:input=move |e|{media_origin.set(event_target_value(&e));approved.set(false);}/></label>
        <label class="field">"Media certificate SHA-256"<input type="text" prop:value=move ||media_pin.get() disabled=move ||busy.get() on:input=move |e|{media_pin.set(event_target_value(&e));approved.set(false);}/></label>
        <p>"The stream port may use a different certificate. Verify its fingerprint separately. Media uses normal CA and hostname checks when this field is empty."</p>
        <label class="field">"Additional trusted stream hostname or IP (optional)"<input type="text" prop:value=move ||stream_host.get() disabled=move ||busy.get() on:input=move |e|{stream_host.set(event_target_value(&e));approved.set(false);}/></label>
        <label><input type="checkbox" prop:checked=move ||approved.get() on:change=move |e|approved.set(event_target_checked(&e))/>"I verified these fingerprints and trust these certificates for the configured console and media endpoints."</label>
        </details>
        <button class="primary" disabled=move ||busy.get() on:click=save>"Test & save"</button>
        <p role="status">{move ||message.get()}</p>
        </section>
    }.into_any()
}
