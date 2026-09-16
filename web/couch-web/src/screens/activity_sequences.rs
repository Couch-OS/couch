use crate::{api, App};
use couch_model::{Action, Activity, Config, Provider, SequenceStep};
use leptos::prelude::*;
use std::sync::Arc;

/// Which sequence is being edited, and the two boxes that filter its command
/// library.
///
/// Provided at the root by [`super::provide_editor_state`]: an accepted write
/// replaces the config signal and rebuilds this screen, so a signal created
/// here would be thrown away after every step added.
#[derive(Clone, Copy)]
pub(super) struct State {
    /// True while the on sequence is the one being edited.
    pub on: RwSignal<bool>,
    pub device: RwSignal<String>,
    pub search: RwSignal<String>,
}
impl State {
    pub(super) fn new() -> State {
        State {
            on: RwSignal::new(true),
            device: RwSignal::new(String::new()),
            search: RwSignal::new(String::new()),
        }
    }
}

pub fn editor(app: App, config: &Config, activity: &Activity) -> AnyView {
    let ir_commands = super::device_commands::Commands::new(config);
    let editing = expect_context::<State>();
    let base = StoredValue::new(activity.clone());
    // Reference counted: the step list and the command library both read the
    // whole document, once per step and once per keystroke.
    let cfg = StoredValue::new(Arc::new(config.clone()));
    let save =
        move |next: Activity| app.run(api::put(format!("/api/activities/{}", next.id), next));
    let dynamic = RwSignal::new(Vec::<(String, String)>::new());
    let status = RwSignal::new(String::new());
    let delay = RwSignal::new(1000u32);
    let level = RwSignal::new(50i32);
    let dragged = RwSignal::new(None::<usize>);
    let selected = move || {
        let c = cfg.get_value();
        let a = base.get_value();
        a.setup
            .devices
            .iter()
            .find(|id| id.as_str() == editing.device.get())
            .or(a.setup.devices.first())
            .and_then(|id| {
                c.devices()
                    .find(|(_, d)| &d.id == id)
                    .map(|(_, d)| d.clone())
            })
    };
    Effect::new(move |_| {
        let device = selected();
        dynamic.set(Vec::new());
        status.set(String::new());
        let Some(device) = device else { return };
        let c = cfg.get_value();
        let connection = match &device.integration {
            couch_model::Integration::Connection { connection_id, .. } => {
                c.connection(connection_id).cloned()
            }
            _ => None,
        };
        let Some(connection) = connection else { return };
        let prefix = match &connection.provider {
            Provider::Denon { .. } => "denon",
            Provider::WebOs => "webos",
            Provider::AppleTv => "appletv",
            Provider::Tizen => "tizen",
            Provider::Plugin {
                supports_inputs: true,
                ..
            } => "plugin",
            _ => return,
        };
        status.set("Loading inputs and apps…".into());
        leptos::task::spawn_local(async move {
            let mut rows = Vec::new();
            let path = format!("/api/connections/{}/{prefix}", connection.id);
            let inputs = if prefix == "appletv" {
                Ok(serde_json::Value::Null)
            } else {
                api::ha(
                    "GET",
                    &format!(
                        "{path}/{}",
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
            if let Ok(value) = &inputs {
                if prefix == "denon" {
                    if let Ok(v) = serde_json::from_value::<Vec<(String, String)>>(value.clone()) {
                        rows.extend(v.into_iter().map(|(id, label)| {
                            (format!("input:{id}"), format!("Input · {label}"))
                        }));
                    }
                } else if prefix == "plugin" {
                    for item in value
                        .as_array()
                        .or_else(|| value["inputs"].as_array())
                        .into_iter()
                        .flatten()
                    {
                        if let Some(id) = item["id"].as_str() {
                            rows.push((
                                format!("input:{id}"),
                                format!("Input · {}", item["name"].as_str().unwrap_or(id)),
                            ));
                        }
                    }
                } else if let Some(items) = value["devices"].as_array() {
                    for item in items {
                        if let Some(id) = item["id"].as_str() {
                            rows.push((
                                format!("input:{id}"),
                                format!("Input · {}", item["label"].as_str().unwrap_or(id)),
                            ));
                        }
                    }
                }
            }
            let mut discovery_failed = inputs.is_err();
            if matches!(prefix, "webos" | "appletv" | "tizen") {
                let app_result = api::ha("GET", &format!("{path}/apps"), None).await;
                discovery_failed |= app_result.is_err();
                if let Ok(value) = app_result {
                    if let Some(items) = value["launchPoints"]
                        .as_array()
                        .or_else(|| value["apps"].as_array())
                    {
                        for item in items {
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
            if dynamic.try_get_untracked().is_some()
                && selected().is_some_and(|d| d.id == device.id)
            {
                dynamic.set(rows);
                status.set(if discovery_failed {
                    "Device unavailable; standard commands are still available.".into()
                } else {
                    String::new()
                });
            }
        });
    });
    let add = move |step: SequenceStep| {
        let mut next = base.get_value();
        if editing.on.get_untracked() {
            next.setup.on.push(step);
        } else {
            next.setup.off.push(step);
        }
        save(next);
    };
    let sequence = move || {
        let a = base.get_value();
        let on = editing.on.get();
        let steps = if on { a.setup.on } else { a.setup.off };
        let empty = steps.is_empty();
        view!{
            <ol class="activity-sequence-list">
                {steps.into_iter().enumerate().map(move |(index,step)| {
                    let label=match step {SequenceStep::Delay{ms}=>format!("Wait {:.1} seconds",ms as f64/1000.),SequenceStep::Command{action}=>{let c=cfg.get_value();let label=c.devices().find(|(_,d)|d.id==action.device).and_then(|(_,d)|c.resolve_integration(&d.integration)).and_then(|i|couch_model::buttons::function_choices(&i).into_iter().find(|f|f.0==action.command).map(|f|f.1)).or_else(||super::device_commands::value_label(&action.command)).unwrap_or(action.command);format!("{} · {label}",super::device_name(&c,&action.device))}};
                    let modify=move |operation:i32|{let mut a=base.get_value();let steps=if on{&mut a.setup.on}else{&mut a.setup.off};match operation{-1 if index>0=>steps.swap(index,index-1),1 if index+1<steps.len()=>steps.swap(index,index+1),0=>{steps.remove(index);},_=>return}save(a);};
                    view!{<li class="activity-sequence-step" draggable="true" on:dragstart=move |_|dragged.set(Some(index)) on:dragover=move |ev|ev.prevent_default() on:drop=move |ev|{
                        ev.prevent_default();if let Some(from)=dragged.get_untracked(){let mut a=base.get_value();let steps=if on{&mut a.setup.on}else{&mut a.setup.off};if from<steps.len()&&index<steps.len(){let step=steps.remove(from);steps.insert(index,step);save(a);}}dragged.set(None);
                    }><span class="sequence-number">{index+1}</span><span class="sequence-label">{label}</span>
                    <div class="sequence-actions"><button class="ghost" aria-label=format!("Move step {} up",index+1) disabled=move ||app.busy.get()||index==0 on:click=move |_|modify(-1)>"↑"</button><button class="ghost" aria-label=format!("Move step {} down",index+1) disabled=move ||app.busy.get() on:click=move |_|modify(1)>"↓"</button><button class="ghost" aria-label=format!("Remove step {}",index+1) disabled=move ||app.busy.get() on:click=move |_|modify(0)>"×"</button></div>
                    </li>}
                }).collect_view()}
            </ol>
            {empty.then(||view!{<div class="activity-sequence-empty"><h3>"Build your sequence"</h3><p>"Choose a device command or add a delay. Steps run from top to bottom."</p></div>})}
        }.into_any()
    };
    let commands = move || {
        let Some(device) = selected() else {
            return view! {<p class="dim">"Include a device to see its commands."</p>}.into_any();
        };
        let c = cfg.get_value();
        let functions = ir_commands.choices(&c, &device, dynamic.get());
        let filter = editing.search.get().to_lowercase();
        // A level needs the number below, so it is a control here rather than a catalog row.
        let levels: Vec<_> = super::device_commands::levels(&c, &device)
            .into_iter()
            .filter(|(_, label)| label.to_lowercase().contains(&filter))
            .collect();
        let rows=functions.into_iter().filter(|(_,label)|label.to_lowercase().contains(&filter)).map(|(command,label)|{
            let id=device.id.clone();view!{<button class="activity-command" disabled=move ||app.busy.get() on:click=move |_|add(SequenceStep::Command{action:Action::new(id.clone(),command.clone())})><span>{label}</span><span aria-hidden="true">"＋"</span></button>}
        }).collect_view();
        let level_rows=levels.into_iter().map(|(kind,label)|{
            let id=device.id.clone();view!{<button class="activity-command" disabled=move ||app.busy.get() on:click=move |_|add(SequenceStep::Command{action:Action::new(id.clone(),format!("{kind}:{}",level.get_untracked()))})><span>{move ||format!("{label} · set to {}%",level.get())}</span><span aria-hidden="true">"＋"</span></button>}
        }).collect_view();
        view! {{rows}{level_rows}}.into_any()
    };
    view!{
        <section class="card activity-sequences"><h2>"On & off sequences"</h2><p class="dim">"On runs once when you start. Off runs when you end the activity. Returning to Couch leaves the activity running."</p>
            <div class="activity-tabs"><button class:selected=move ||editing.on.get() on:click=move |_|editing.on.set(true)>"On sequence"</button><button class:selected=move ||!editing.on.get() on:click=move |_|editing.on.set(false)>"Off sequence"</button></div>
            {sequence}
            <p class="dim">"Drag steps to reorder, or use the arrows. A failed command stops the sequence; completed steps are not undone."</p>
        </section>
        <aside class="card activity-command-library"><h2>"Add a command"</h2><p class="dim">{move ||if editing.on.get(){"Adding to the on sequence"}else{"Adding to the off sequence"}}</p>
            <label class="field"><span class="label">"Device"</span><select aria-label="Sequence device" prop:value=move ||selected().map(|d|d.id.to_string()).unwrap_or_default() on:change=move |ev|editing.device.set(event_target_value(&ev))>
                {config.devices().filter(|(_,d)|activity.setup.devices.contains(&d.id)).map(|(r,d)|view!{<option value=d.id.to_string()>{format!("{} · {}",d.name,r.name)}</option>}).collect_view()}
            </select></label>
            <input class="activity-command-search" type="search" aria-label="Search sequence commands" placeholder="Search commands…" prop:value=move ||editing.search.get() on:input=move |ev|editing.search.set(event_target_value(&ev))/>
            <div class="activity-command-list">{commands}</div>
            <label class="field"><span class="label">"Level (%)"</span><input type="number" aria-label="Level percent" min="0" max="100" prop:value=move ||level.get().to_string() on:input=move |ev|{if let Ok(v)=event_target_value(&ev).parse::<i32>(){level.set(v.clamp(0,100));}}/></label><p class="dim">{move ||[status.get(),ir_commands.status()].into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join(" ")}</p>
            <h3>"Add a delay"</h3><label class="field"><span class="label">"Milliseconds"</span><input type="number" aria-label="Delay milliseconds" min="1" max="30000" prop:value=move ||delay.get().to_string() on:input=move |ev|{if let Ok(ms)=event_target_value(&ev).parse(){delay.set(ms);}}/></label>
            <button class="ghost" disabled=move ||app.busy.get() || !(1..=30000).contains(&delay.get()) on:click=move |_|add(SequenceStep::Delay{ms:delay.get_untracked()})>"＋ Add delay"</button>
        </aside>
    }.into_any()
}
