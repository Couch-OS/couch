//! Shared IR catalog selection. Previewing and saving never transmit a command.
use crate::{api, App};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

// A catalog is immutable for the lifetime of the served bundle. Coalesce the
// first request when several device editors mount together; retry after errors.
type CatalogCallback = Box<dyn FnOnce(Result<Value, api::ApiError>)>;
thread_local! {
    static CATALOG: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
    static CATALOG_WAITERS: std::cell::RefCell<Vec<CatalogCallback>> = const { std::cell::RefCell::new(Vec::new()) };
}
fn load_catalog(callback: impl FnOnce(Result<Value, api::ApiError>) + 'static) {
    let cached = CATALOG.with(|c| c.borrow().clone());
    if let Some(value) = cached {
        callback(Ok(value));
        return;
    }
    let start = CATALOG_WAITERS.with(|waiters| {
        let mut waiters = waiters.borrow_mut();
        waiters.push(Box::new(callback));
        waiters.len() == 1
    });
    if start {
        spawn_local(async move {
            let result = api::ha("GET", "/api/ir/catalog", None).await;
            if let Ok(value) = &result {
                CATALOG.with(|c| *c.borrow_mut() = Some(value.clone()));
            }
            let waiters = CATALOG_WAITERS.with(|w| std::mem::take(&mut *w.borrow_mut()));
            for callback in waiters {
                callback(result.clone());
            }
        });
    }
}

pub fn library(app: App, output: RwSignal<String>, power_only: bool) -> AnyView {
    let catalog = RwSignal::new(Vec::<Value>::new());
    let brand = RwSignal::new(String::new());
    let kind = RwSignal::new(String::new());
    let selected = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let commands = RwSignal::new(Vec::<Value>::new());
    let busy = RwSignal::new(true);
    let message = RwSignal::new(String::new());
    let source = RwSignal::new(String::new());
    let import_text = RwSignal::new(String::new());
    let import_format = RwSignal::new("flipper".to_string());
    let import_name = RwSignal::new("Imported remote".to_string());
    load_catalog(move |result| {
        if busy.try_get_untracked().is_none() {
            return;
        }
        match result {
            Ok(v) => {
                catalog.set(v["codesets"].as_array().cloned().unwrap_or_default());
                source.set(format!(
                    "{} · {}",
                    v["source"]["name"].as_str().unwrap_or("IR library"),
                    v["source"]["license"]
                        .as_str()
                        .unwrap_or("See source license")
                ));
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
    let fetch = move |path: String, body: Option<Value>| {
        busy.set(true);
        commands.set(Vec::new());
        message.set(String::new());
        spawn_local(async move {
            let result = api::ha(if body.is_some() { "POST" } else { "GET" }, &path, body).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(v) => {
                    commands.set(v["commands"].as_array().cloned().unwrap_or_default());
                    message.set("Choose a function for each command you want to use. No command has been sent.".into());
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
    view! {
        <div class="ir-library"><h4>"Choose remote codes"</h4>
            <p class="dim">{move ||source.get()}</p>
            <label class="field">"Brand"<select aria-label="IR brand" disabled=move ||busy.get() prop:value=move ||brand.get() on:change=move |e|{brand.set(event_target_value(&e));kind.set(String::new());selected.set(String::new());commands.set(Vec::new());}>
                <option value="">"Choose a brand"</option>{move ||catalog.get().iter().filter_map(|v|v["brand"].as_str()).map(str::to_string).collect::<std::collections::BTreeSet<_>>().into_iter().map(|s|view!{<option value=s.clone()>{s.clone()}</option>}).collect_view()}
            </select></label>
            <label class="field">"Device type"<select aria-label="IR library device type" prop:value=move ||kind.get() disabled=move ||busy.get()||brand.get().is_empty() on:change=move |e|{kind.set(event_target_value(&e));selected.set(String::new());commands.set(Vec::new());}>
                <option value="">"All device types"</option>{move ||catalog.get().iter().filter(|v|v["brand"].as_str()==Some(brand.get().as_str())).filter_map(|v|v["device_type"].as_str()).map(str::to_string).collect::<std::collections::BTreeSet<_>>().into_iter().map(|s|view!{<option value=s.clone()>{s.clone()}</option>}).collect_view()}
            </select></label>
            {super::connections::field("Search models",search,"Filter this brand’s models")}
            <label class="field">"Model / codeset"<select aria-label="IR model" disabled=move ||busy.get()||brand.get().is_empty() prop:value=move ||selected.get() on:change=move |e|{let id=event_target_value(&e);selected.set(id.clone());if !id.is_empty(){fetch(format!("/api/ir/catalog/{id}"),None);}}>
                <option value="">"Choose a model"</option>{move ||catalog.get().into_iter().filter(|v|v["brand"].as_str()==Some(brand.get().as_str())&&(kind.get().is_empty()||v["device_type"].as_str()==Some(kind.get().as_str())) && v["model"].as_str().unwrap_or("").to_lowercase().contains(&search.get().to_lowercase())).map(|v|{let id=v["id"].as_str().unwrap_or("").to_string();let label=format!("{} — {} supported commands",v["model"].as_str().unwrap_or(&id),v["supported_commands"]);view!{<option value=id>{label}</option>}}).collect_view()}
            </select></label>
            <details><summary>"Import your own remote codes"</summary>
                {super::connections::field("Import name",import_name,"Remote model")}
                <label class="field">"File format"<select aria-label="IR import format" on:change=move |e|import_format.set(event_target_value(&e))><option value="flipper">"Flipper .ir"</option><option value="couch">"Couch codeset"</option></select></label>
                <label class="field">"Paste file contents"<textarea aria-label="IR import contents" rows="5" maxlength="262144" prop:value=move ||import_text.get() on:input=move |e|import_text.set(event_target_value(&e)) /></label>
                <button type="button" class="primary" disabled=move ||busy.get()||import_text.get().trim().is_empty() on:click=move |_|fetch("/api/ir/import".into(),Some(json!({"name":import_name.get_untracked(),"format":import_format.get_untracked(),"text":import_text.get_untracked()})))>"Preview imported commands"</button>
            </details>
            <p role="status">{move ||message.get()}</p>
            <Show when=move ||!commands.get().is_empty()><div class="ir-match-actions"><button type="button" class="ghost" disabled=move ||busy.get()||!commands.get().iter().any(|v|v["supported"]==true&&suggested_function(v["name"].as_str().unwrap_or(""),power_only).is_some()) on:click=move |_|{
                let entries=commands.get_untracked();output.update(|text|{for value in &entries {if value["supported"]==true{if let (Some(function),Some(code))=(suggested_function(value["name"].as_str().unwrap_or(""),power_only),value["code"].as_str()){assign(text,&function,code);}}}});
                message.set("Matching functions assigned. Review them before saving; no command was sent.".into());
            }>"Assign matching functions"</button><p class="dim">"Recognizes common button names and replaces their assignments."</p></div></Show>
            <div class="ir-command-list" aria-label="Available IR commands">{move ||commands.get().into_iter().enumerate().map(|(index,v)|{
                let name=v["name"].as_str().unwrap_or("Unnamed command").to_string();
                let supported=v["supported"]==true && v["code"].is_string();
                let code=v["code"].as_str().unwrap_or("").to_string();
                let reason=v["reason"].as_str().unwrap_or("Protocol is not supported by Couch").to_string();
                let label=format!("Assign {name} ({})",index+1);
                view!{<div class="ir-command"><strong>{name}</strong>{if supported {
                    view!{<label class="field">"Remote function"<select aria-label=label on:change=move |e|{let function=event_target_value(&e);if !function.is_empty(){output.update(|text|assign(text,&function,&code));}}><option value="">"Choose a function"</option>{(if power_only {vec!["power","power-on","power-off"]} else {vec!["toggle","power-on","power-off","up","down","left","right","ok","back","home","menu","volume-up","volume-down","mute","channel-up","channel-down","play","pause","play-pause","stop","next","previous","rewind","fast-forward","red","green","yellow","blue"]}).into_iter().map(|s|view!{<option value=s>{s.replace('-'," ")}</option>}).collect_view()}</select></label>}.into_any()
                }else{view!{<p class="dim">{format!("Unavailable: {reason}")}</p>}.into_any()}}</div>}
            }).collect_view()}</div>
            <p class="dim">"Library codes are candidates, not proof of compatibility. Saving does not send IR. Discrete power on/off must use their own codes; a toggle is never substituted."</p>
        </div>
    }.into_any()
}

fn assign(text: &mut String, function: &str, code: &str) {
    let fields: Vec<_> = code.split_whitespace().collect();
    if fields.len() < 2 {
        return;
    }
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| {
            !line
                .split_whitespace()
                .next()
                .is_some_and(|key| key.eq_ignore_ascii_case(function))
        })
        .map(str::to_string)
        .collect();
    lines.push(format!("{function} {}", fields[1..].join(" ")));
    *text = lines.join("\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn common_names_preserve_discrete_power_and_filter_power_only_picker() {
        assert_eq!(
            suggested_function("Power", false).as_deref(),
            Some("toggle")
        );
        assert_eq!(
            suggested_function("Power_off", false).as_deref(),
            Some("power-off")
        );
        assert_eq!(suggested_function("Power", true).as_deref(), Some("power"));
        assert_eq!(
            suggested_function("Vol_up", false).as_deref(),
            Some("volume-up")
        );
        assert_eq!(suggested_function("Vol_up", true), None);
        assert_eq!(suggested_function("Unknown key", false), None);
    }
    #[test]
    fn remove_is_case_insensitive_and_preserves_other_commands() {
        let mut text = "# custom commands\nToggle nec 4 8\nvolume-up nec 4 2".to_string();
        remove_assignment(&mut text, "toggle");
        assert_eq!(assigned_functions(&text), vec!["volume-up"]);
        assert!(text.contains("# custom commands"));
    }
    #[test]
    fn raw_assignment_preserves_pulses() {
        let mut text = String::new();
        assign(&mut text, "power-off", "Off raw 38000 9000,4500,560,560");
        assert_eq!(text, "power-off raw 38000 9000,4500,560,560");
    }
    #[test]
    fn assignment_replaces_only_requested_function() {
        let mut text = "power nec 4 8\npower-on nec 4 9".to_string();
        assign(&mut text, "power", "source nec 4 10");
        assert_eq!(text, "power-on nec 4 9\npower nec 4 10");
        assign(&mut text, "power-off", "invalid");
        assert!(!text.contains("power-off"));
    }
}

/// Which device's IR editor is open.
///
/// Provided at the root by [`super::provide_editor_state`]: saving a command
/// replaces the config signal and rebuilds the room, and this keeps the editor
/// open across that rebuild instead of closing it after every save.
#[derive(Clone, Copy)]
pub(super) struct State {
    pub open: RwSignal<String>,
}
impl State {
    pub(super) fn new() -> State {
        State {
            open: RwSignal::new(String::new()),
        }
    }
}

/// The IR section of a device row, open across a successful revisioned save.
pub fn device_commands(
    app: App,
    config: &couch_model::Config,
    room: &couch_model::Id,
    device: &couch_model::Device,
) -> AnyView {
    let open = expect_context::<State>().open;
    let configured = device.effective_ir_codeset(config).is_some();
    let id = StoredValue::new(device.id.to_string());
    let room = StoredValue::new(room.clone());
    let device = StoredValue::new(device.clone());
    view! {<section class="device-ir">
        <div class="device-ir-heading"><div><h4>"IR commands"</h4><p class="dim">{if configured {"Commands assigned to this device"} else {"Use the remote’s built-in infrared transmitter"}}</p></div>
        <button type="button" class="ghost" aria-expanded=move ||open.get()==id.get_value() on:click=move |_|open.update(|current|*current=if *current==id.get_value(){String::new()}else{id.get_value()})>{move ||if open.get()==id.get_value(){"Close IR commands"}else if configured{"Manage IR commands"}else{"Add IR commands"}}</button></div>
        {move ||(open.get()==id.get_value()).then(||device_setup(app,room.get_value(),Some(device.get_value())))}
    </section>}.into_any()
}

pub fn device_setup(
    app: App,
    room: couch_model::Id,
    existing: Option<couch_model::Device>,
) -> AnyView {
    let open = expect_context::<State>().open;
    let is_new = existing.is_none();
    let name = RwSignal::new(String::new());
    let kind = RwSignal::new("tv".to_string());
    let saved = StoredValue::new(existing);
    let room = StoredValue::new(room);
    let text = RwSignal::new(String::new());
    let original = RwSignal::new(String::new());
    let codeset = RwSignal::new(String::new());
    let loading = RwSignal::new(!is_new);
    let testing = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let loaded = RwSignal::new(is_new);
    if let Some(device) = saved.get_value() {
        spawn_local(async move {
            let result = api::ha(
                "GET",
                &format!("/api/rooms/{}/devices/{}/ir", room.get_value(), device.id),
                None,
            )
            .await;
            if loading.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(v) => {
                    let body = v["text"].as_str().unwrap_or("").to_string();
                    text.set(body.clone());
                    original.set(body);
                    codeset.set(v["codeset"].as_str().unwrap_or("").into());
                    if let Some(error) = v["error"].as_str() {
                        message.set(error.into());
                    }
                    loaded.set(true);
                }
                Err(e) => {
                    if e.unauthorized {
                        app.paired.set(Some(false));
                    }
                    message.set(e.message);
                }
            }
            loading.set(false);
        });
    }
    view! {<div class="ir-device-editor">
        {is_new.then(||view!{<h3>"Add a device"</h3><p class="dim">"Name the device, then assign its infrared commands. No connection setup is needed."</p>
            {super::connections::field("Device name",name,"Living room TV")}
            <label class="field">"Device type"<select aria-label="Manual device type" prop:value=move ||kind.get() on:change=move |e|kind.set(event_target_value(&e))><option value="tv">"TV"</option><option value="speaker">"Speaker / receiver"</option><option value="media-player">"Media player"</option><option value="other">"Other"</option></select></label>})}
        {(!is_new).then(||view!{<p class="dim">"Assigned functions use infrared. Other controls keep using this device’s connection. You do not need a second device."</p>})}
        {move ||loading.get().then(||view!{<p role="status">"Loading saved commands…"</p>})}
        <Show when=move ||loaded.get()>
            <div class="ir-assignments"><h4>"Assigned functions"</h4>
            {move ||if assigned_functions(&text.get()).is_empty(){view!{<p class="dim">"No IR commands assigned. Choose a remote below to get started."</p>}.into_any()}else{
                assigned_functions(&text.get()).into_iter().map(|function|{
                    let title=function_label(&function);let remove=function.clone();let test=function.clone();
                    view!{<div class="ir-assignment"><span>{title}</span><div class="ir-assignment-actions">
                        <button type="button" class="ghost" aria-label=format!("Test {}",function_label(&test)) disabled=move ||testing.get()||app.busy.get()||codeset.get().is_empty()||text.get()!=original.get() on:click=move |_|{
                            testing.set(true);message.set(String::new());let command=test.clone();
                            spawn_local(async move {let result=api::ha("POST",&format!("/api/ir/codesets/{}/test",codeset.get_untracked()),Some(json!({"command":command}))).await;
                                if testing.try_get_untracked().is_none(){return;}
                                match result{Ok(_)=>message.set("IR command sent. Check that the device responded.".into()),Err(e)=>{if e.unauthorized{app.paired.set(Some(false));}message.set(e.message);}}testing.set(false);
                            });
                        }>"Test"</button>
                        <button type="button" class="ghost" aria-label=format!("Remove {}",function_label(&remove)) disabled=move ||testing.get()||app.busy.get() on:click=move |_|text.update(|body|remove_assignment(body,&remove))>"Remove"</button>
                    </div></div>}
                }).collect_view().into_any()
            }}
            <p class="dim">"Save changes before testing. Test sends one command using the remote’s built-in blaster."</p></div>
            <details class="ir-library-picker" open=move ||original.get().is_empty()><summary>"Find commands in the library or import a remote"</summary>{library(app,text,false)}</details>
            <details class="ir-advanced"><summary>"Advanced: edit command definitions"</summary><label class="field">"Command definitions"<textarea aria-label="Assigned IR commands" rows="6" maxlength="262144" prop:value=move ||text.get() on:input=move |e|text.set(event_target_value(&e)) /></label></details>
            <div class="ir-editor-actions"><button type="button" class="primary" disabled=move ||loading.get()||testing.get()||app.busy.get()||(is_new&&(text.get().trim().is_empty()||name.get().trim().is_empty())) on:click=move |_|{
                let body=text.get_untracked();
                if let Some(device)=saved.get_value(){let path=format!("/api/rooms/{}/devices/{}/ir",room.get_value(),device.id);if body.trim().is_empty(){app.run(api::delete(path));}else{app.run(api::put(path,json!({"text":body})));}}
                else {
                    let old_ids: Vec<_>=app.config.get_untracked().unwrap_or_default().devices().map(|(_,d)|d.id.clone()).collect();
                    let payload=json!({"name":name.get_untracked().trim(),"kind":kind.get_untracked(),"text":body});
                    app.run(async move {let next=api::post(format!("/api/rooms/{}/devices/ir",room.get_value()),payload).await?;
                        if let Some((_,device))=next.devices().find(|(_,d)|!old_ids.contains(&d.id)){open.set(device.id.to_string());}
                        Ok(next)
                    });
                }
            }>{if is_new{"Add device to room"}else{"Save IR commands"}}</button>
            {(!is_new).then(||view!{<button type="button" class="ghost" disabled=move ||loading.get()||testing.get()||app.busy.get()||codeset.get().is_empty() on:click=move |_|{if let Some(device)=saved.get_value(){app.run(api::delete(format!("/api/rooms/{}/devices/{}/ir",room.get_value(),device.id)));}}>"Remove all IR commands"</button>})}</div>
        </Show>
        <p role="status">{move ||message.get()}</p>
    </div>}.into_any()
}

fn assigned_functions(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                None
            } else {
                line.split_whitespace().next().map(str::to_string)
            }
        })
        .collect()
}
fn remove_assignment(text: &mut String, function: &str) {
    *text = text
        .lines()
        .filter(|line| {
            !line
                .split_whitespace()
                .next()
                .is_some_and(|key| key.eq_ignore_ascii_case(function))
        })
        .collect::<Vec<_>>()
        .join("\n");
}
fn function_label(function: &str) -> String {
    match function {
        "toggle" | "power" => "Power toggle".into(),
        "power-on" => "Power on".into(),
        "power-off" => "Power off".into(),
        "ok" => "OK / select".into(),
        _ => {
            let label = function.replace('-', " ");
            let mut chars = label.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

fn suggested_function(name: &str, power_only: bool) -> Option<String> {
    let key = name.to_ascii_lowercase().replace([' ', '_'], "-");
    let function = match key.as_str() {
        "power" | "power-toggle" | "toggle" => {
            if power_only {
                "power"
            } else {
                "toggle"
            }
        }
        "on" | "power-on" | "poweron" => "power-on",
        "off" | "power-off" | "poweroff" => "power-off",
        "vol+" | "vol-up" | "volume-up" | "volume+" => "volume-up",
        "vol-" | "vol-down" | "volume-down" | "volume-" => "volume-down",
        "ch+" | "ch-up" | "channel-up" => "channel-up",
        "ch-" | "ch-down" | "channel-down" => "channel-down",
        "enter" | "select" | "ok" => "ok",
        "return" | "back" => "back",
        "play/pause" | "playpause" | "play-pause" => "play-pause",
        "up" | "down" | "left" | "right" | "home" | "menu" | "mute" | "play" | "pause" | "stop"
        | "next" | "previous" | "rewind" | "fast-forward" | "red" | "green" | "yellow" | "blue" => {
            key.as_str()
        }
        _ => return None,
    };
    if power_only && !matches!(function, "power" | "power-on" | "power-off") {
        None
    } else {
        Some(function.into())
    }
}
