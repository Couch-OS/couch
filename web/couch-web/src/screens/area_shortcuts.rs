//! Quick-access keys for an area: one slot per shortcut and color key, and a
//! searchable picker of everything a key can reach.
//!
//! The same slot-and-picker interaction as an activity's physical buttons,
//! with a different vocabulary: a key here does not send a device command, it
//! opens something - a device's controls, an activity, another area - or
//! switches one light. Both editors share their markup and styles so the two
//! read as one feature.
use crate::{api, App};
use couch_model::{buttons::Button, Area, Config, Id, Shortcut, ShortcutAction, SHORTCUT_BUTTONS};
use leptos::prelude::*;
use std::sync::Arc;

/// Key, label and glyph, in front-panel order.
fn key_name(button: Button) -> (&'static str, &'static str) {
    match button {
        Button::Lights => ("Light key", "☼"),
        Button::Activity => ("Curtain key", "▥"),
        Button::Music => ("Media key", "♫"),
        Button::Tv => ("Climate key", "°"),
        Button::Red => ("Red", "R"),
        Button::Green => ("Green", "G"),
        Button::Blue => ("Blue", "B"),
        Button::Yellow => ("Yellow", "Y"),
        _ => ("Key", "?"),
    }
}

/// What a slot shows for its saved action: the kind, and the target's name.
fn describe(config: &Config, action: &ShortcutAction) -> (String, String) {
    let device = |id: &Id| {
        config
            .devices()
            .find(|(_, d)| &d.id == id)
            .map(|(room, d)| format!("{} · {}", d.name, room.name))
            .unwrap_or_else(|| "Removed device".into())
    };
    match action {
        ShortcutAction::Area { area } => (
            "Show area".into(),
            config
                .area(area)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Removed area".into()),
        ),
        ShortcutAction::Activity { activity } => (
            "Open activity".into(),
            config
                .activity(activity)
                .map(|a| {
                    let room = config.room(&a.room).map(|r| r.name.as_str()).unwrap_or("");
                    format!("{} · {room}", a.name)
                })
                .unwrap_or_else(|| "Removed activity".into()),
        ),
        ShortcutAction::Device { device: id } => ("Open controls".into(), device(id)),
        ShortcutAction::Toggle { device: id } => ("Switch on / off".into(), device(id)),
    }
}

/// One choice in the picker: a group heading, the target's name and detail,
/// and the action choosing it saves.
struct Choice {
    group: &'static str,
    name: String,
    detail: String,
    action: ShortcutAction,
}

fn choices(config: &Config, area: &Id) -> Vec<Choice> {
    let mut out = Vec::new();
    for other in config.areas.iter().filter(|a| &a.id != area) {
        out.push(Choice {
            group: "Areas · show the page",
            name: other.name.clone(),
            detail: super::counts(&[(other.rooms.len(), "room", "rooms")]),
            action: ShortcutAction::Area {
                area: other.id.clone(),
            },
        });
    }
    for activity in &config.activities {
        out.push(Choice {
            group: "Activities · open",
            name: activity.name.clone(),
            detail: config
                .room(&activity.room)
                .map(|r| r.name.clone())
                .unwrap_or_default(),
            action: ShortcutAction::Activity {
                activity: activity.id.clone(),
            },
        });
    }
    for (room, device) in config.devices() {
        if config.can_toggle(device) {
            out.push(Choice {
                group: "Lights and covers · switch on / off",
                name: device.name.clone(),
                detail: room.name.clone(),
                action: ShortcutAction::Toggle {
                    device: device.id.clone(),
                },
            });
        }
    }
    for (room, device) in config.devices() {
        out.push(Choice {
            group: "Devices · open controls",
            name: device.name.clone(),
            detail: format!("{} · {}", room.name, device.kind),
            action: ShortcutAction::Device {
                device: device.id.clone(),
            },
        });
    }
    out
}

pub fn editor(app: App, config: &Config, area: &Area) -> AnyView {
    // Reference counted: one read per key slot, and one per keystroke in the
    // picker's search box, all of which wanted the whole document.
    let config = StoredValue::new(Arc::new(config.clone()));
    let area = StoredValue::new(area.clone());
    let selected = RwSignal::new(Button::Lights);
    let query = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let open = move |button: Button| {
        selected.set(button);
        query.set(String::new());
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    };
    // `None` clears the key; the whole list is sent, as the member lists are.
    let save = move |action: Option<ShortcutAction>| {
        let area = area.get_value();
        let button = selected.get_untracked();
        let mut next: Vec<Shortcut> = area
            .shortcuts
            .iter()
            .filter(|s| s.button != button)
            .cloned()
            .collect();
        if let Some(action) = action {
            next.push(Shortcut { button, action });
        }
        next.sort_by_key(|s| SHORTCUT_BUTTONS.iter().position(|b| *b == s.button));
        // Keep the dialog open if saving fails; the shared error shows inside it.
        app.run(async move {
            let result = api::put(format!("/api/areas/{}/shortcuts", area.id), next).await;
            if result.is_ok() {
                if let Some(d) = dialog.get() {
                    d.close();
                }
            }
            result
        });
    };
    let slot_label = move |button: Button| -> (String, String) {
        let area = area.get_value();
        match area.shortcuts.iter().find(|s| s.button == button) {
            None => ("Not assigned".into(), String::new()),
            Some(s) => describe(&config.get_value(), &s.action),
        }
    };
    let assigned = area.get_value().shortcuts.len();
    let body = view! {
        <section class="card button-editor shortcut-editor" aria-label="Quick-access keys">
            <div class="mapping-columns"><span>"Key"</span><span>"Press"</span></div>
            {SHORTCUT_BUTTONS.iter().map(move |&button| {
                let (name, glyph) = key_name(button);
                let (kind, target) = slot_label(button);
                view! {
                    <div class="mapping-row">
                        <div class="mapping-key"><span class="mapping-key-glyph" aria-hidden="true">{glyph}</span><span>{name}</span></div>
                        <button class="mapping-slot" aria-label=format!("{name}, quick access") disabled=move || app.busy.get() on:click=move |_| open(button)>
                            <strong>{kind}</strong><span class="mapping-device">{target}</span><span class="mapping-edit" aria-hidden="true">"↗"</span>
                        </button>
                    </div>
                }
            }).collect_view()}
        </section>
        <p class="dim">{if assigned == 0 { "No keys assigned yet. Unassigned keys do nothing on this page." } else { "Keys act on the home screen only; a device or activity screen keeps its own keys. Changes apply on the remote within a moment." }}</p>
        <dialog node_ref=dialog class="command-picker" aria-labelledby="shortcut-picker-title">
            <div class="command-picker-header"><div><span class="eyebrow">"Assign key"</span><h2 id="shortcut-picker-title">{move || key_name(selected.get()).0}</h2></div>
                <button class="command-close" aria-label="Close key picker" on:click=move |_| { if let Some(d) = dialog.get() { d.close(); } }>"×"</button>
            </div>
            <p class="dim command-picker-help">"Choose what this key reaches. Lights and covers can be switched directly; everything else opens its controls."</p>
            <input type="search" autofocus aria-label="Search targets" placeholder="Search devices, activities or areas…" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e))/>
            <div class="mapping-reset-actions">
                <button disabled=move || app.busy.get() on:click=move |_| save(None)>"Clear this key"</button>
            </div>
            <p class="dim" role="status">{move || if app.busy.get() { "Saving key…" } else { "" }}</p>
            {move || app.error.get().map(|e| view! { <p class="error" role="alert">{e}</p> })}
            <div class="command-results">
                {move || {
                    let cfg = config.get_value();
                    let search = query.get().to_lowercase();
                    let mut groups: Vec<(&'static str, Vec<Choice>)> = Vec::new();
                    for choice in choices(&cfg, &area.get_value().id) {
                        let haystack = format!("{} {} {}", choice.group, choice.name, choice.detail).to_lowercase();
                        if !search.split_whitespace().all(|word| haystack.contains(word)) { continue; }
                        match groups.iter_mut().find(|(g, _)| *g == choice.group) {
                            Some((_, list)) => list.push(choice),
                            None => groups.push((choice.group, vec![choice])),
                        }
                    }
                    if groups.is_empty() {
                        return view! { <p class="dim command-empty">"Nothing matches. Add rooms, devices or activities first, or try another search."</p> }.into_any();
                    }
                    groups.into_iter().map(|(group, list)| view! {
                        <section class="command-group"><h3>{group}</h3>
                            {list.into_iter().map(|choice| {
                                let action = choice.action;
                                let label = format!("{}: {}", group, choice.name);
                                view! { <button class="command-option" aria-label=label disabled=move || app.busy.get() on:click=move |_| save(Some(action.clone()))><span>{choice.name}<span class="dim">{format!(" · {}", choice.detail)}</span></span><span aria-hidden="true">"＋"</span></button> }
                            }).collect_view()}
                        </section>
                    }).collect_view().into_any()
                }}
            </div>
        </dialog>
    }
    .into_any();
    crate::ui::section(
        "Quick-access keys",
        Some("What the four shortcut keys and the four color keys do while this area is on screen: open a device's controls, switch a light, open an activity or jump to another area. Select a key to choose."),
        body,
    )
}
