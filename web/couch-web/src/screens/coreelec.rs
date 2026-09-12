use crate::{api, App};
use couch_model::{Connection, Provider};
use leptos::{prelude::*, task::spawn_local};
use serde_json::json;
pub fn form(app: App, existing: Option<Connection>) -> AnyView {
    let name = RwSignal::new(
        existing
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default(),
    );
    let (h, p) = match existing.as_ref().map(|c| &c.provider) {
        Some(Provider::CoreElec { host, port }) => (host.clone(), port.to_string()),
        _ => (String::new(), "9090".into()),
    };
    let host = RwSignal::new(h);
    let port = RwSignal::new(p);
    let error = RwSignal::new(String::new());
    view!{<form on:submit=move|e|{e.prevent_default();let Ok(port)=port.get_untracked().parse::<u16>()else{error.set("Enter a TCP port from 1 to 65535".into());return};let host=host.get_untracked().trim().to_string();let name=name.get_untracked().trim().to_string();if host.is_empty()||name.is_empty()||port==0{error.set("Enter a name, address and TCP port".into());return}let body=json!({"name":name,"provider":Provider::CoreElec{host,port}});match &existing{Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>app.run(api::post("/api/connections",body))}}>
    {super::connections::field("Connection name",name,"Living room CoreELEC")}
    {super::connections::field("Hostname or IP address",host,"192.168.1.20")}
    {super::connections::field("Kodi TCP port",port,"9090")}
    <p class="dim">"Enable Kodi remote control. Add this connection as a media player in Rooms & devices to use playback and navigation on the remote. OS controls are optional and require an IP address."</p>
    <p role="alert">{move||error.get()}</p><button class="primary" type="submit">"Save CoreELEC connection"</button></form>}.into_any()
}
pub fn setup(app: App, c: &Connection) -> AnyView {
    let base = StoredValue::new(format!("/api/connections/{}/coreelec", c.id));
    let configured = RwSignal::new(false);
    let user = RwSignal::new("root".to_string());
    let port = RwSignal::new("22".to_string());
    let key = RwSignal::new(String::new());
    let known = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let status = RwSignal::new(String::new());
    let confirmed = RwSignal::new(false);
    let action = RwSignal::new("restart-kodi".to_string());
    spawn_local(async move {
        match api::ha("GET", &format!("{}/connection", base.get_value()), None).await {
            Ok(v) => {
                configured.set(v["configured"].as_bool().unwrap_or(false));
                user.set(v["user"].as_str().unwrap_or("root").into());
                port.set(v["port"].to_string());
            }
            Err(e) => {
                if e.unauthorized {
                    app.paired.set(Some(false));
                }
                message.set(e.message);
            }
        }
    });
    let refresh = move || {
        busy.set(true);
        spawn_local(async move {
            match api::ha("GET", &format!("{}/status", base.get_value()), None).await {
                Ok(v) => {
                    let i = &v["identity"];
                    let s = &v["service"];
                    status.set(format!(
                        "CoreELEC {} · {} · Kodi {} ({})",
                        i["version"].as_str().unwrap_or("unknown"),
                        i["architecture"].as_str().unwrap_or("unknown hardware"),
                        s["active"].as_str().unwrap_or("unknown"),
                        s["sub"].as_str().unwrap_or("unknown")
                    ));
                    message.set(String::new());
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
    view!{<section class="coreelec-settings"><h3>"CoreELEC OS access"</h3>
    <p>"Optional. Kodi playback works without SSH. OS access adds verified version information, Kodi restart, reboot and power off."</p>
    <p class="notice">"Enable SSH in CoreELEC Settings → Services. Install your public key on that device and independently verify its SSH host fingerprint before enrolling. Use a dedicated key without a passphrase."</p>
    <p>{move||if configured.get(){"SSH access is enrolled"}else{"SSH access is not enrolled"}}</p>
    <details><summary>{move||if configured.get(){"Replace SSH enrollment"}else{"Enroll SSH access"}}</summary>
    {super::connections::field("SSH username",user,"root")}{super::connections::field("SSH port",port,"22")}
    <label class="field">"Private SSH key"<textarea aria-label="Private SSH key" autocomplete="off" spellcheck="false" prop:value=move||key.get() on:input=move|e|key.set(event_target_value(&e))></textarea></label>
    <label class="field">"Verified known_hosts entry"<textarea aria-label="Verified known_hosts entry" autocomplete="off" spellcheck="false" prop:value=move||known.get() on:input=move|e|known.set(event_target_value(&e))></textarea></label>
    <p class="dim">"These credentials stay in private storage on the remote. They are never returned to the browser or included in exported configuration. The host entry must match this connection’s IP and SSH port."</p>
    <button class="primary" disabled=move||busy.get() on:click=move|_|{let Ok(port)=port.get_untracked().parse::<u16>()else{message.set("Enter a valid SSH port".into());return};let body=json!({"user":user.get_untracked(),"port":port,"private_key":key.get_untracked(),"known_hosts":known.get_untracked()});busy.set(true);message.set("Verifying SSH host key and CoreELEC identity…".into());spawn_local(async move{match api::ha("PUT",&format!("{}/connection",base.get_value()),Some(body)).await{Ok(_)=>{configured.set(true);key.set(String::new());known.set(String::new());message.set("SSH verified and saved".into());},Err(e)=>{if e.unauthorized{app.paired.set(Some(false));}message.set(e.message);}}busy.set(false);});}>"Verify & save SSH access"</button>
    </details>
    <Show when=move||configured.get()><p>{move||status.get()}</p><button class="ghost" disabled=move||busy.get() on:click=move|_|refresh()>"Refresh OS status"</button>
    <label class="field">"OS action"<select aria-label="CoreELEC OS action" prop:value=move||action.get() on:change=move|e|{action.set(event_target_value(&e));confirmed.set(false);}>
    <option value="restart-kodi">"Restart Kodi"</option><option value="reboot">"Reboot CoreELEC"</option><option value="poweroff">"Power off CoreELEC"</option></select></label>
    <label><input type="checkbox" prop:checked=move||confirmed.get() on:change=move|e|confirmed.set(event_target_checked(&e))/>"I understand this interrupts playback; powering off may require a physical power button to turn it on again."</label>
    <button class="primary" disabled=move||busy.get()||!confirmed.get() on:click=move|_|{busy.set(true);let body=json!({"action":action.get_untracked(),"confirm":confirmed.get_untracked()});confirmed.set(false);spawn_local(async move{match api::ha("POST",&format!("{}/action",base.get_value()),Some(body)).await{Ok(_)=>message.set("Request accepted. Refresh status after the device restarts.".into()),Err(e)=>{if e.unauthorized{app.paired.set(Some(false));}message.set(e.message);}}busy.set(false);});}>"Run OS action"</button>
    <button class="ghost" disabled=move||busy.get() on:click=move|_|{busy.set(true);spawn_local(async move{match api::ha("DELETE",&format!("{}/connection",base.get_value()),None).await{Ok(_)=>{configured.set(false);status.set(String::new());message.set("SSH access removed. Kodi control remains available.".into());},Err(e)=>message.set(e.message)}busy.set(false);});}>"Remove SSH access"</button>
    </Show><p role="status">{move||message.get()}</p></section>}.into_any()
}
