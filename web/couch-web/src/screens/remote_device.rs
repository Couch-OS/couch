//! The remote's own settings on the web: what its Settings menu shows, mirrored.
//!
//! Display and keys, SSH, Bluetooth, network and power. The daemon reads and writes the
//! same file the remote does, so a change here shows up on the remote within
//! a second, and the remote's menu changes show up here on reload.
use crate::{api, ui, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Ssh {
    available: bool,
    enabled: bool,
    running: bool,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Bluetooth {
    available: bool,
    enabled: bool,
    running: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    detail: String,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Device {
    brightness: i32,
    keys: bool,
    dim_index: i32,
    off_index: i32,
    #[serde(default)]
    dim_choices: Vec<String>,
    #[serde(default)]
    off_choices: Vec<String>,
    ssh: Ssh,
    #[serde(default)]
    bluetooth: Bluetooth,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Network {
    address: String,
    gateway: String,
    dns: String,
    mac: String,
    web: String,
    host: String,
}

fn blank(value: &str) -> String {
    if value.is_empty() {
        "none".into()
    } else {
        value.to_owned()
    }
}

pub fn sections(app: App) -> AnyView {
    let device = RwSignal::new(Device::default());
    let loaded = RwSignal::new(false);
    let network = RwSignal::new(Network::default());
    let error = RwSignal::new(String::new());
    let power_note = RwSignal::new(String::new());
    spawn_local(async move {
        match api::ha("GET", "/api/remote/device", None).await {
            Ok(v) => {
                if let Ok(v) = serde_json::from_value::<Device>(v) {
                    device.set(v);
                    loaded.set(true);
                }
            }
            Err(e) => {
                if e.unauthorized {
                    app.paired.set(Some(false));
                }
                error.set(e.message);
            }
        }
        if let Ok(v) = api::ha("GET", "/api/remote/network", None).await {
            if let Ok(v) = serde_json::from_value::<Network>(v) {
                network.set(v);
            }
        }
    });
    let save = move |next: Device| {
        error.set(String::new());
        spawn_local(async move {
            let body = serde_json::json!({
                "brightness": next.brightness, "keys": next.keys,
                "dim_index": next.dim_index, "off_index": next.off_index, "ssh": next.ssh.enabled,
                "bluetooth": next.bluetooth.enabled,
            });
            match api::ha("PUT", "/api/remote/device", Some(body)).await {
                Ok(v) => {
                    if let Ok(v) = serde_json::from_value::<Device>(v) {
                        device.set(v);
                    }
                }
                Err(e) => {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    error.set(e.message);
                }
            }
        });
    };
    let power = move |action: &'static str| {
        power_note.set(String::new());
        spawn_local(async move {
            match api::ha(
                "POST",
                "/api/remote/power",
                Some(serde_json::json!({"action": action, "confirm": true})),
            )
            .await
            {
                Ok(_) => power_note.set(match action {
                    "off" => "Powering off. The remote will not answer until it is switched on again.".into(),
                    "restart" => "Restarting. The web UI is back in about two minutes.".into(),
                    _ => "Restarting into recovery. There is no web UI in recovery; see docs/device-recovery.md to return.".into(),
                }),
                Err(e) => {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    power_note.set(e.message);
                }
            }
        });
    };
    let choices = move |list: Vec<String>, current: i32, commit: std::rc::Rc<dyn Fn(i32)>| {
        view! {
            <select prop:value=move || current.to_string() on:change=move |e| { if let Ok(i) = event_target_value(&e).parse::<i32>() { commit(i); } }>
                {list.into_iter().enumerate().map(|(i, label)| view! { <option value=i.to_string() selected=(i as i32)==current>{label}</option> }).collect_view()}
            </select>
        }
    };
    view! {
        {ui::section("Display & keys", Some("The same rows as the remote's Settings → Display. Changes apply on the remote within a second."), view! {
            <div class:dim=move || !loaded.get()>
            <label class="field">"Brightness"
                <select aria-label="Brightness" prop:value=move || device.get().brightness.to_string() on:change=move |e| { if let Ok(v) = event_target_value(&e).parse::<i32>() { let mut d = device.get_untracked(); d.brightness = v; save(d); } }>
                    {(1..=10).map(|n| { let pct = n * 10; view! { <option value=pct.to_string() selected=move || device.get().brightness == pct>{format!("{pct}%")}</option> } }).collect_view()}
                </select>
            </label>
            <label><input type="checkbox" prop:checked=move || device.get().keys on:change=move |e| { let mut d = device.get_untracked(); d.keys = event_target_checked(&e); save(d); }/>"Light the keypad while the screen is awake"</label>
            <label class="field">"Dim after"
                {move || choices(device.get().dim_choices, device.get().dim_index, std::rc::Rc::new(move |i| { let mut d = device.get_untracked(); d.dim_index = i; save(d); }))}
            </label>
            <label class="field">"Screen off after"
                {move || choices(device.get().off_choices, device.get().off_index, std::rc::Rc::new(move |i| { let mut d = device.get_untracked(); d.off_index = i; save(d); }))}
            </label>
            <p role="alert">{move || error.get()}</p>
            </div>
        }.into_any())}
        {ui::section("SSH", Some("Enrol a key or password from the remote's setup page first; without one there is nobody to let in."), view! {
            <label><input type="checkbox" disabled=move || !device.get().ssh.available prop:checked=move || device.get().ssh.enabled on:change=move |e| { let mut d = device.get_untracked(); d.ssh.enabled = event_target_checked(&e); save(d); }/>"SSH access"</label>
            <p class="dim">{move || { let s = device.get().ssh; if !s.available { "Nothing enrolled".to_string() } else if s.running { "sshd is running".into() } else { "sshd is stopped".into() } }}</p>
        }.into_any())}
        {ui::section("Bluetooth", Some("The remote advertises as \"Couch Remote\" while this is on; pair it from the TV's Bluetooth menu. Needs the current boot image; older kernels have no Bluetooth."), view! {
            <label><input type="checkbox" disabled=move || { let b = device.get().bluetooth; !b.available || b.state == "starting" } prop:checked=move || device.get().bluetooth.enabled on:change=move |e| { let mut d = device.get_untracked(); d.bluetooth.enabled = event_target_checked(&e); save(d); }/>"Bluetooth"</label>
            <p class="dim">{move || { let b = device.get().bluetooth; if !b.available { "No kernel support".to_string() } else if b.running { "On: advertising as Couch Remote".into() } else if b.state == "starting" { "Starting the Bluetooth stack…".into() } else if b.state == "error" { format!("Failed: {}", b.detail) } else { "Off".into() } }}</p>
        }.into_any())}
        {ui::section("Network", None, view! {
            <dl class="facts">
                <dt>"Address"</dt><dd>{move || blank(&network.get().address)}</dd>
                <dt>"Gateway"</dt><dd>{move || blank(&network.get().gateway)}</dd>
                <dt>"DNS"</dt><dd>{move || blank(&network.get().dns)}</dd>
                <dt>"Wi-Fi MAC"</dt><dd>{move || blank(&network.get().mac)}</dd>
                <dt>"Web UI"</dt><dd>{move || network.get().web}</dd>
            </dl>
        }.into_any())}
        {ui::section("Power", Some("Each button asks once more before acting. Recovery has no web UI: the remote stays there until its flag is cleared over USB or SSH."), view! {
            <div class="power-actions">
                {ui::confirm_button("Restart", "Confirm restart", move || power("restart"))}
                {ui::confirm_button("Power off", "Confirm power off", move || power("off"))}
                {ui::confirm_button("Restart into recovery", "Confirm recovery", move || power("recovery"))}
            </div>
            <p role="status">{move || power_note.get()}</p>
        }.into_any())}
    }
    .into_any()
}
