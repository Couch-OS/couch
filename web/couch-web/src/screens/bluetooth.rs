//! A device's own Bluetooth pairing: the remote is the HID peripheral, the
//! device's TV bonds to it, and the bond is stored on this device so a second
//! TV can have its own. The window is the daemon's (two minutes); this section
//! opens it for the device, follows `GET /api/remote/device` while it is open,
//! and reloads the configuration once couch-confd has stored the bond.
use crate::{api, App};
use couch_model::{Config, Device, Id};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Peer {
    #[serde(default)]
    address: String,
    #[serde(default)]
    name: String,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Pairing {
    #[serde(default)]
    phase: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    bonded: Option<Peer>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Bluetooth {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    pairing: Pairing,
    #[serde(default)]
    link: Option<Peer>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct RemoteDevice {
    #[serde(default)]
    bluetooth: Bluetooth,
}

impl Pairing {
    fn in_window(&self) -> bool {
        matches!(self.phase.as_str(), "pairing" | "connected" | "paired")
    }
    /// The window is this device's: opened from here, or from the remote's
    /// screen for the same device.
    fn is_for(&self, id: &str) -> bool {
        self.device.as_deref() == Some(id)
    }
    fn text(&self) -> String {
        match self.phase.as_str() {
            "pairing" => {
                "Pairing mode: on the TV, open Bluetooth settings and choose Couch Remote. A TV already connected over Bluetooth is disconnected until the window ends.".into()
            }
            "connected" => format!("Connected to {}…", self.detail),
            "paired" => format!(
                "Paired with {}. If the TV asks you to press a key, use Send key.",
                self.detail
            ),
            "done" => format!("Done: {} is paired.", self.detail),
            "failed" if self.detail == "timeout" => "No TV paired (timed out). Try again.".into(),
            "failed" => "Pairing cancelled.".into(),
            _ => String::new(),
        }
    }
}

/// What the device is paired with, for the card's line.
pub fn summary(device: &Device) -> Option<String> {
    let bond = device.bluetooth.as_ref()?;
    Some(if bond.addressed() {
        format!("Bluetooth · {}", bond.label())
    } else {
        "Bluetooth · address not recorded, pair again".into()
    })
}

pub fn device_bluetooth(app: App, _config: &Config, room: &Id, device: &Device) -> AnyView {
    let id = StoredValue::new(device.id.to_string());
    let path = StoredValue::new(format!("/api/rooms/{room}/devices/{}", device.id));
    let saved = StoredValue::new(device.clone());
    let bond = device.bluetooth.clone();
    let remote = RwSignal::new(RemoteDevice::default());
    let loaded = RwSignal::new(false);
    let note = RwSignal::new(String::new());
    // Set once this section opened a window; the reload after `done` happens
    // only for a window that is this device's.
    let watching = RwSignal::new(false);
    let refresh = move || {
        spawn_local(async move {
            if let Ok(v) = api::ha("GET", "/api/remote/device", None).await {
                if let Ok(v) = serde_json::from_value::<RemoteDevice>(v) {
                    if remote.try_get_untracked().is_none() {
                        return;
                    }
                    remote.set(v);
                    loaded.set(true);
                }
            }
        })
    };
    refresh();
    // Every second while a window is open for this device; the bond lands
    // in the configuration within a second of `done`, which the reload picks
    // up, and the section is then redrawn from the device.
    let settled = RwSignal::new(0u8);
    let timer = set_interval_with_handle(
        move || {
            let b = remote.get_untracked().bluetooth;
            let mine = b.pairing.is_for(&id.get_value());
            if b.pairing.in_window() && mine {
                watching.set(true);
                refresh();
            } else if watching.get_untracked() && b.pairing.phase == "done" {
                let n = settled.get_untracked() + 1;
                settled.set(n);
                if n == 2 {
                    watching.set(false);
                    app.run(api::load());
                } else {
                    refresh();
                }
            } else if watching.get_untracked() && b.pairing.phase == "failed" {
                watching.set(false);
            }
        },
        std::time::Duration::from_secs(1),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    let control = move |action: &'static str, with_device: bool| {
        note.set(String::new());
        spawn_local(async move {
            let mut body = serde_json::json!({"action": action});
            if with_device {
                body["device"] = serde_json::Value::String(id.get_value());
            }
            match api::ha("POST", "/api/remote/bluetooth", Some(body)).await {
                Ok(_) => {
                    if action == "pair" {
                        watching.set(true);
                        settled.set(0);
                    }
                    refresh();
                }
                Err(e) => {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    note.set(e.message);
                }
            }
        });
    };
    let status = move || {
        let b = remote.get().bluetooth;
        if !loaded.get() {
            return String::new();
        }
        if !b.available {
            return "The remote's kernel has no Bluetooth; install the current boot image.".into();
        }
        if !b.running {
            return "Turn Bluetooth on under Remote first.".into();
        }
        if b.pairing.is_for(&id.get_value()) && (b.pairing.in_window() || watching.get()) {
            return b.pairing.text();
        }
        if b.pairing.in_window() {
            return "The remote is pairing another device right now.".into();
        }
        String::new()
    };
    let bonded_line = match &bond {
        Some(b) if b.addressed() => format!("Paired with {} ({})", b.label(), b.address),
        Some(b) => format!(
            "Paired with {} · address not recorded: pair again to pin this device to its TV",
            b.label()
        ),
        None => "Not paired. The remote appears on the TV as \"Couch Remote\".".into(),
    };
    let bonded = bond.is_some();
    view! {<section class="device-ir device-bluetooth">
        <div class="device-ir-heading"><div><h4>"Bluetooth"</h4><p class="dim">{bonded_line}</p></div>
        <div class="power-actions">
            <button type="button" class="ghost" disabled=move || { let b = remote.get().bluetooth; app.busy.get() || !b.running || b.pairing.in_window() } on:click=move |_| control("pair", true)>{if bonded {"Pair again"} else {"Pair over Bluetooth"}}</button>
            {move || { let b = remote.get().bluetooth; (b.pairing.in_window() && b.pairing.is_for(&id.get_value())).then(|| view! {
                <button type="button" class="ghost" on:click=move |_| control("enter", false)>"Send key"</button>
                <button type="button" class="ghost" on:click=move |_| control("stop", false)>"Cancel"</button>
            }) }}
            {bonded.then(|| view! {<button type="button" class="link" disabled=move || app.busy.get() on:click=move |_| {
                let next = Device { bluetooth: None, ..saved.get_value() };
                app.run(api::put(path.get_value(), next));
            }>"Unpair"</button>})}
        </div></div>
        <p role="status">{move || if note.get().is_empty() { status() } else { note.get() }}</p>
    </section>}.into_any()
}
