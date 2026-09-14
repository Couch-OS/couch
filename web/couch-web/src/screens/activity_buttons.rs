//! Activity mappings: visible press slots and a searchable command picker.
use crate::{api, App};
use couch_model::{
    buttons::{functions, Binding, Button, Gesture},
    Action, Activity, Config,
};
use leptos::prelude::*;
use std::sync::Arc;

// Physical keys in familiar panel order; repeat keys have only a short slot.
const KEYS: &[(Button, &str, &str, i32, i32)] = &[
    (Button::Back, "Back", "↶", 18, 40),
    (Button::Home, "Home", "⌂", 50, 40),
    (Button::Power, "Power", "⏻", 82, 40),
    (Button::Up, "D-pad up", "▲", 50, 49),
    (Button::Down, "D-pad down", "▼", 50, 66),
    (Button::Left, "D-pad left", "◀", 29, 57),
    (Button::Right, "D-pad right", "▶", 71, 57),
    (Button::Ok, "OK", "OK", 50, 57),
    (Button::VolumeUp, "Volume up", "V+", 12, 49),
    (Button::VolumeDown, "Volume down", "V−", 12, 65),
    (Button::ChannelUp, "Channel up", "C+", 88, 49),
    (Button::ChannelDown, "Channel down", "C−", 88, 65),
    (Button::Mute, "Mute", "Mute", 18, 74),
    (Button::Microphone, "Microphone", "Mic", 50, 74),
    (Button::Menu, "Menu", "☰", 82, 74),
    (Button::Lights, "Light key", "☼", 17, 83),
    (Button::Activity, "Curtain key", "▥", 39, 83),
    (Button::Music, "Media key", "♫", 61, 83),
    (Button::Tv, "Climate key", "°", 83, 83),
    (Button::Red, "Red", "R", 17, 91),
    (Button::Green, "Green", "G", 39, 91),
    (Button::Blue, "Blue", "B", 61, 91),
    (Button::Yellow, "Yellow", "Y", 83, 91),
];
pub fn editor(app: App, config: &Config, activity: &Activity) -> AnyView {
    let ir_commands=super::device_commands::Commands::new(config);
    // Reference counted: the device effect and the search box below both want
    // the whole document, and `StoredValue<Config>` hands each read a copy of
    // the house.
    let config = StoredValue::new(Arc::new(config.clone()));
    let activity = StoredValue::new(activity.clone());
    let selected = RwSignal::new(Button::Ok);
    let gesture = RwSignal::new(Gesture::Short);
    let device = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());
    let dynamic = RwSignal::new(Vec::<(String, String)>::new());
    let discovery = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let open = move |button, press| {
        selected.set(button);
        gesture.set(press);
        query.set(String::new());
        let target = activity
            .get_value()
            .buttons
            .iter()
            .find(|b| b.button == button && b.gesture == press)
            .and_then(|b| b.action.as_ref())
            .map(|a| a.device.to_string());
        device.set(target.unwrap_or_default());
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    };
    let save = move |action: Option<Option<Action>>| {
        let mut next = activity.get_value();
        next.buttons.retain(|b| {
            b.button != selected.get_untracked() || b.gesture != gesture.get_untracked()
        });
        if let Some(action) = action {
            next.buttons.push(Binding {
                button: selected.get_untracked(),
                gesture: gesture.get_untracked(),
                action,
            });
        }
        // Keep the dialog open if saving fails; the shared error remains visible here.
        app.run(async move {
            let result = api::put(format!("/api/activities/{}", next.id), next).await;
            if result.is_ok() {
                if let Some(d) = dialog.get() {
                    d.close();
                }
            }
            result
        });
    };
    Effect::new(move |_| {
        let selected = device.get();
        dynamic.set(Vec::new());
        discovery.set(String::new());
        let cfg = config.get_value();
        let connection = cfg
            .devices()
            .find(|(_, d)| d.id.as_str() == selected)
            .and_then(|(_, d)| match &d.integration {
                couch_model::Integration::Connection { connection_id, .. } => {
                    cfg.connection(connection_id)
                }
                _ => None,
            })
            .cloned();
        let Some(connection) = connection else { return };
        let prefix = match connection.provider {
            couch_model::Provider::Denon { .. } => "denon",
            couch_model::Provider::WebOs => "webos",
            couch_model::Provider::AppleTv => "appletv",
            couch_model::Provider::Tizen => "tizen",
            _ => return,
        };
        discovery.set("Loading inputs and apps…".into());
        leptos::task::spawn_local(async move {
            let mut rows = Vec::new();
            let base = format!("/api/connections/{}/{prefix}", connection.id);
            let result = if prefix == "appletv" {
                Ok(serde_json::Value::Null)
            } else {
                api::ha(
                    "GET",
                    &format!(
                        "{base}/{}",
                        if prefix == "denon" {
                            "sources"
                        } else {
                            "inputs"
                        }
                    ),
                    None,
                )
                .await
            };
            if let Ok(value) = &result {
                if prefix == "denon" {
                    if let Ok(sources) =
                        serde_json::from_value::<Vec<(String, String)>>(value.clone())
                    {
                        rows.extend(
                            sources.into_iter().map(|(id, name)| {
                                (format!("input:{id}"), format!("Input · {name}"))
                            }),
                        );
                    }
                } else if let Some(inputs) = value["devices"].as_array() {
                    for item in inputs {
                        if let Some(id) = item["id"].as_str() {
                            rows.push((
                                format!("input:{id}"),
                                format!("Input · {}", item["label"].as_str().unwrap_or(id)),
                            ));
                        }
                    }
                }
            }
            let mut discovery_failed = result.is_err();
            if matches!(prefix, "webos" | "appletv" | "tizen") {
                let app_result = api::ha("GET", &format!("{base}/apps"), None).await;
                discovery_failed |= app_result.is_err();
                if let Ok(apps) = app_result {
                    if let Some(apps) = apps["launchPoints"]
                        .as_array()
                        .or_else(|| apps["apps"].as_array())
                    {
                        for item in apps {
                            if let Some(id) = item["id"].as_str() {
                                rows.push((
                                    format!("app:{id}"),
                                    format!("App · {}", item["title"].as_str().unwrap_or(id)),
                                ));
                            }
                        }
                    }
                }
            }
            if device.try_get_untracked().as_deref() == Some(&selected) {
                dynamic.set(rows);
                discovery.set(if discovery_failed {
                    "Input or app discovery unavailable; saved mappings are retained.".into()
                } else {
                    String::new()
                });
            }
        });
    });

    // Both labels for all 23 keys from one borrow of the document. Called as a
    // closure per slot, this was two copies of the house and of the activity
    // per key.
    let labels: Vec<((String, String), (String, String))> = activity.with_value(|current| {
        let cfg = config.get_value();
        KEYS.iter()
            .map(|&(button, ..)| {
                (
                    mapping_label(&cfg, current, button, Gesture::Short),
                    mapping_label(&cfg, current, button, Gesture::Long),
                )
            })
            .collect()
    });
    let cfg = config.get_value();
    let options: Vec<_> = cfg
        .devices()
        .filter(|(_, d)| {
            cfg.resolve_integration(&d.integration)
                .is_some_and(|i| !functions(&i).is_empty())
                || d.effective_ir_codeset(&cfg).is_some()
        })
        .map(|(r, d)| (d.id.to_string(), format!("{} · {}", r.name, d.name)))
        .collect();
    view! {
        <h2 class="section">"Physical buttons"</h2>
        <p class="dim">"Select a press slot to choose its command. Mix devices freely — for example, Kodi navigation and receiver volume."</p>
        <section class="card button-editor" aria-label="Physical button mappings">
            <div class="mapping-columns"><span>"Button"</span><span>"● Short press"</span><span>"━ Long press"</span></div>
            {KEYS.iter().zip(labels).map(move |(&(button, name, glyph, _, _), (short, long))| {
                view! {
                    <div class="mapping-row">
                        <div class="mapping-key"><span class="mapping-key-glyph" aria-hidden="true">{glyph}</span><span>{name}</span></div>
                        <button class="mapping-slot" aria-label=format!("{name}, short press") disabled=move ||app.busy.get() on:click=move |_|open(button, Gesture::Short)>
                            <span class="mapping-gesture">"● Short press"</span><strong>{short.0}</strong><span class="mapping-device">{short.1}</span><span class="mapping-edit" aria-hidden="true">"↗"</span>
                        </button>
                        {if button == Button::Back { view! {
                            <div class="mapping-slot" aria-label="Back, long press: return to Couch"><span class="mapping-gesture">"━ Long press"</span><strong>"Return to Couch"</strong><span class="mapping-device">"Previous screen · Always available"</span></div>
                        }.into_any() } else if button.supports_long() { view! {
                            <button class="mapping-slot" aria-label=format!("{name}, long press") disabled=move ||app.busy.get() on:click=move |_|open(button, Gesture::Long)>
                                <span class="mapping-gesture">"━ Long press"</span><strong>{long.0}</strong><span class="mapping-device">{long.1}</span><span class="mapping-edit" aria-hidden="true">"↗"</span>
                            </button>
                        }.into_any() } else { view! { <div class="mapping-repeat"><span>"Hold to repeat"</span></div> }.into_any() }}
                    </div>
                }
            }).collect_view()}
        </section>
        <p class="dim">"Long press activates after 600 ms. D-pad, volume and channel keys repeat instead. Changes apply when you reopen the activity on the remote."</p>
        <dialog node_ref=dialog class="command-picker" aria-labelledby="command-picker-title">
            <div class="command-picker-header"><div><span class="eyebrow">"Assign command"</span><h2 id="command-picker-title">{move ||format!("{} · {}", KEYS.iter().find(|k|k.0==selected.get()).map(|k|k.1).unwrap_or("Button"), if gesture.get()==Gesture::Long {"Long press"} else {"Short press"})}</h2></div>
                <button class="command-close" aria-label="Close command picker" on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>"×"</button>
            </div>
            <p class="dim command-picker-help">"Choose a command to save. Select a device to discover its inputs and apps."</p>
            <input type="search" autofocus aria-label="Search commands" placeholder="Search commands or devices…" prop:value=move ||query.get() on:input=move |e|query.set(event_target_value(&e))/>
            <label class="field">"Device"<select aria-label="Filter commands by device" prop:value=move ||device.get() on:change=move |e|device.set(event_target_value(&e))>
                <option value="">"All devices"</option>{options.into_iter().map(|(id,name)|view!{<option value=id>{name}</option>}).collect_view()}
            </select></label>
            <div class="mapping-reset-actions">
                <button disabled=move ||app.busy.get() on:click=move |_|save(None)>"Use activity default"</button>
                <button disabled=move ||app.busy.get() on:click=move |_|save(Some(None))>"Do nothing"</button>
            </div>
            <p class="dim" role="status">{move ||if app.busy.get(){"Saving mapping…".to_string()}else{[discovery.get(),ir_commands.status()].into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join(" ")}}</p>
            {move ||app.error.get().map(|e|view!{<p class="error" role="alert">{e}</p>})}
            <div class="command-results">
                {move || {
                    let cfg=config.get_value(); let filter=device.get(); let search=query.get().to_lowercase();
                    let mut groups=Vec::new();
                    for (room,d) in cfg.devices().filter(|(_,d)|filter.is_empty()||d.id.as_str()==filter) {
                        let mut choices=ir_commands.choices(&cfg,d,if !filter.is_empty(){dynamic.get()}else{Vec::new()});
                        choices.retain(|(id,name)|format!("{} {} {id} {name}",room.name,d.name).to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ").contains(&search));
                        if choices.is_empty(){continue;}
                        let device_id=d.id.clone();
                        groups.push(view! {<section class="command-group"><h3>{d.name.clone()}<span>{room.name.clone()}</span></h3>
                            {choices.into_iter().map(move |(id,name)| {
                                let action=Action::new(device_id.clone(),id);
                                view!{<button class="command-option" disabled=move ||app.busy.get() on:click=move |_|save(Some(Some(action.clone())))><span>{name}</span><span aria-hidden="true">"＋"</span></button>}
                            }).collect_view()}
                        </section>}.into_any());
                    }
                    if groups.is_empty(){view!{<p class="dim command-empty">"No matching commands. Try another search or device."</p>}.into_any()}else{groups.collect_view().into_any()}
                }}
            </div>
        </dialog>
    }.into_any()
}

/// What one press slot shows: the function it runs, and the device it runs on.
fn mapping_label(
    cfg: &Config,
    activity: &Activity,
    button: Button,
    press: Gesture,
) -> (String, String) {
    match activity
        .buttons
        .iter()
        .find(|b| b.button == button && b.gesture == press)
    {
        None => ("Activity default".to_string(), String::new()),
        Some(Binding { action: None, .. }) => ("Do nothing".to_string(), String::new()),
        Some(Binding {
            action: Some(a), ..
        }) => {
            let target = cfg.devices().find(|(_, d)| d.id == a.device);
            let function = target
                .and_then(|(_, d)| cfg.resolve_integration(&d.integration))
                .and_then(|i| {
                    functions(&i)
                        .iter()
                        .find(|f| f.0 == a.command)
                        .map(|f| f.1.to_string())
                })
                .unwrap_or_else(|| {
                    a.command
                        .replace("input:", "Input · ")
                        .replace("app:", "App · ")
                });
            (
                function,
                target
                    .map(|(_, d)| d.name.clone())
                    .unwrap_or_else(|| "Removed device".into()),
            )
        }
    }
}
