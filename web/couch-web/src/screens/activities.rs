//! Activity setup follows the task: devices, on/off sequences, then controls.
use crate::{api, route::Route, ui, App};
use couch_model::{Activity, ActivitySetup, Config, Id, Integration};
use leptos::prelude::*;
use serde_json::json;

pub(super) fn members(activity: &Activity) -> Vec<Id> {
    if !activity.setup.devices.is_empty() {
        return activity.setup.devices.clone();
    }
    let mut ids = Vec::new();
    for id in activity
        .source
        .iter()
        .chain(
            activity
                .buttons
                .iter()
                .filter_map(|b| b.action.as_ref().map(|a| &a.device)),
        )
        .chain(activity.steps.iter().map(|a| &a.device))
    {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}
pub fn list(app: App, config: &Config) -> AnyView {
    let rows = config.activities.iter().map(|a| {
        let id=a.id.clone(); let room=config.room(&a.room).map(|r|r.name.clone()).unwrap_or_default();
        let detail=format!("{room} · {} devices · {} on / {} off steps",members(a).len(),a.setup.on.len(),a.setup.off.len());
        view! { <button class="activity-card" on:click=move |_| {app.activity_tab.set("setup".into());app.go(Route::Activity(id.clone()));}>
            <span class="eyebrow">"ACTIVITY"</span><strong>{a.name.clone()}</strong><span>{detail}</span><span class="activity-card-open">"Configure →"</span>
        </button> }
    }).collect_view();
    view! {
        {ui::page_header(app,"Activities".into(),None)}
        <p class="dim pad-x">"Bring devices together for something you do. Choose what turns on, what turns off, and how the remote controls it."</p>
        <div class="activity-cards">{rows}</div>
        {config.activities.is_empty().then(||ui::empty("Create your first activity, such as Watch TV."))}
        {new_activity(app,config)}
    }.into_any()
}
fn new_activity(app: App, config: &Config) -> AnyView {
    if config.rooms.is_empty() {
        return ui::empty("Create a room before adding an activity.");
    }
    let name = RwSignal::new(String::new());
    let room = RwSignal::new(config.rooms[0].id.to_string());
    let selected = RwSignal::new(Vec::<Id>::new());
    let filter = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let previous = config
        .activities
        .iter()
        .map(|a| a.id.clone())
        .collect::<Vec<_>>();
    let devices=config.devices().map(|(r,d)| {
        let id=d.id.clone();let label=format!("{} · {}",d.name,r.name);let search=label.to_lowercase();let checked=id.clone();
        view! { <label class="activity-device-choice" style:display=move ||if search.contains(&filter.get().to_lowercase()){""}else{"none"}>
            <input type="checkbox" prop:checked=move ||selected.get().contains(&checked) on:change=move |ev|selected.update(|ids|{if event_target_checked(&ev){if !ids.contains(&id){ids.push(id.clone());}}else{ids.retain(|v|v!=&id);}})/><span>{label}</span>
        </label> }
    }).collect_view();
    view! {
        <div class="pad"><button class="primary" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}>"＋ Create activity"</button></div>
        <dialog class="command-picker activity-create" node_ref=dialog aria-label="Create activity">
            <div class="command-picker-header"><h2>"New activity"</h2><button class="ghost" aria-label="Close" on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>"×"</button></div>
            <label class="field"><span class="label">"Name"</span><input aria-label="Activity name" placeholder="Watch a movie" prop:value=move ||name.get() on:input=move |ev|name.set(event_target_value(&ev))/></label>
            <label class="field"><span class="label">"Room"</span><select aria-label="Activity room" prop:value=move ||room.get() on:change=move |ev|room.set(event_target_value(&ev))>
                {config.rooms.iter().map(|r|view!{<option value=r.id.to_string()>{r.name.clone()}</option>}).collect_view()}
            </select></label>
            <h3>"Include devices"</h3><p class="dim">"Add the player, TV, receiver, lights, or other devices this activity will use. Devices can belong to any room."</p>
            <input type="search" aria-label="Search activity devices" placeholder="Search devices…" on:input=move |ev|filter.set(event_target_value(&ev))/>
            <div class="activity-device-list">{devices}</div>
            <button class="primary" disabled=move ||app.busy.get() || name.get().trim().is_empty() || selected.get().is_empty() on:click=move |_| {
                let setup=ActivitySetup {devices:selected.get_untracked(),..Default::default()};
                let body=json!({"name":name.get_untracked().trim(),"room":room.get_untracked(),"kind":"video","setup":setup});
                let previous=previous.clone();
                app.run(async move {let next=api::post("/api/activities",body).await?;if let Some(a)=next.activities.iter().find(|a|!previous.contains(&a.id)){app.activity_tab.set("setup".into());app.go(Route::Activity(a.id.clone()));}Ok(next)});
            }>"Create and configure"</button>
            {move ||app.error.get().map(|e|view!{<p class="error">{e}</p>})}
        </dialog>
    }.into_any()
}
pub fn detail(app: App, config: &Config, id: &Id) -> AnyView {
    let Some(activity) = config.activity(id) else {
        return super::gone(app, "That activity has been deleted.");
    };
    let mut activity = activity.clone();
    activity.setup.devices = members(&activity);
    let activity = StoredValue::new(activity);
    let cfg = StoredValue::new(config.clone());
    let save =
        move |next: Activity| app.run(api::put(format!("/api/activities/{}", next.id), next));
    let name_base = activity.get_value();
    let description_base = name_base.clone();
    let room_base = name_base.clone();
    let awake_base = name_base.clone();
    let devices=config.devices().map(|(r,d)| {
        let id=d.id.clone();let label=d.name.clone();let room=r.name.clone();let checked=activity.get_value().setup.devices.contains(&id);
        view!{<label class="activity-device-choice"><input type="checkbox" checked=checked disabled=move ||app.busy.get() on:change=move |ev|{
            let mut next=activity.get_value();
            if event_target_checked(&ev) {if !next.setup.devices.contains(&id){next.setup.devices.push(id.clone());}}
            else {next.setup.forget_device(&id);next.buttons.retain(|b|b.action.as_ref().is_none_or(|a|a.device!=id));next.steps.retain(|s|s.device!=id);if next.source.as_ref()==Some(&id){next.source=None;}}
            save(next);
        }/><span><strong>{label}</strong><small>{room}</small></span></label>}
    }).collect_view();
    let tabs=[("setup","Devices & sequences"),("buttons","Physical buttons"),("screen","Remote screen")].into_iter().map(|(id,label)|view!{
        <button class:selected=move ||app.activity_tab.get()==id on:click=move |_|app.activity_tab.set(id.into())>{label}</button>
    }).collect_view();
    let screen = screen_editor(app, config, &activity.get_value());
    let mut mapped_config = config.clone();
    for room in &mut mapped_config.rooms {
        room.devices
            .retain(|d| activity.get_value().setup.devices.contains(&d.id));
    }
    let mappings = super::activity_buttons::editor(app, &mapped_config, &activity.get_value());
    let sequence = super::activity_sequences::editor(app, config, &activity.get_value());
    view! {
        {ui::page_header(app,activity.get_value().name,Some(Route::Activities))}
        <nav class="activity-tabs" aria-label="Activity configuration">{tabs}</nav>
        <div style:display=move ||if app.activity_tab.get()=="setup"{""}else{"none"}>
            <div class="activity-workbench">
                <aside class="card activity-overview">
                    <h2>"Activity"</h2>
                    {ui::text_field("Name",name_base.name.clone(),"Watch TV",move |name|save(Activity{name,..name_base.clone()}))}
                    {ui::text_field("Description",description_base.setup.description.clone(),"TV, receiver and room lighting",move |description|{let mut a=description_base.clone();a.setup.description=description;save(a);})}
                    <label class="field"><span class="label">"Room"</span><select aria-label="Activity room" on:change=move |ev|{let mut a=room_base.clone();a.room=Id::new(event_target_value(&ev));save(a);}>
                        {config.rooms.iter().map(|r|view!{<option value=r.id.to_string() selected=r.id==activity.get_value().room>{r.name.clone()}</option>}).collect_view()}
                    </select></label>
                    <label class="activity-device-choice"><input type="checkbox" checked=awake_base.setup.keep_awake disabled=move ||app.busy.get() on:change=move |ev|{let mut a=awake_base.clone();a.setup.keep_awake=event_target_checked(&ev);save(a);}/><span>"Keep remote awake while running"</span></label>
                    <h3>"Included devices"</h3><p class="dim">"Only these devices appear in the command and button pickers."</p>
                    <div class="activity-device-list">{devices}</div>
                </aside>
                {sequence}
            </div>
            {(!activity.get_value().steps.is_empty()).then(||view!{<section class="card"><h3>"Previous startup draft"</h3><p class="dim">"These older commands were never executed. Recreate the ones you want in the on sequence; they are retained here for reference."</p><ul>{activity.get_value().steps.into_iter().map(|s|view!{<li>{format!("{} · {}",super::device_name(&cfg.get_value(),&s.device),s.command)}</li>}).collect_view()}</ul></section>})}
        </div>
        <div style:display=move ||if app.activity_tab.get()=="buttons"{""}else{"none"}>{mappings}</div>
        <div style:display=move ||if app.activity_tab.get()=="screen"{""}else{"none"}>{screen}</div>
        <div class="pad">{ui::danger_button("Delete activity",move ||{let id=activity.get_value().id;app.run(async move {let result=api::delete(format!("/api/activities/{id}")).await?;app.go(Route::Activities);Ok(result)});})}</div>
    }.into_any()
}
fn screen_editor(app: App, config: &Config, activity: &Activity) -> AnyView {
    let base = StoredValue::new(activity.clone());
    let options=config.devices().filter(|(_,d)|activity.setup.devices.contains(&d.id)).filter_map(|(_,d)|{
        let screen=match config.resolve_integration(&d.integration) {Some(Integration::Sonos{..})=>"Sonos · group playback and speaker volume",Some(Integration::Kodi{..})=>"Kodi · artwork, playback and chapters",Some(Integration::WebOs)=>"LG TV · inputs, apps and playback",Some(Integration::AndroidTv)=>"Android TV · navigation and playback",Some(Integration::AppleTv)=>"Apple TV · apps and playback",_=>return None};
        let id=d.id.clone();let selected=activity.source.as_ref()==Some(&id) && !activity.setup.custom_screen;
        Some(view!{<label class="activity-screen-choice"><input type="radio" name="activity-screen" checked=selected disabled=move ||app.busy.get() on:change=move |_|{let mut a=base.get_value();a.source=Some(id.clone());a.setup.custom_screen=false;app.run(api::put(format!("/api/activities/{}",a.id),a));}/><span><strong>{d.name.clone()}</strong><small>{screen}</small></span></label>})
    }).collect_view();
    let pages=super::activity_pages::editor(app,config,activity);
    let areas=config.areas.iter().map(|area|{let id=area.id.clone();let selected=area.activities.contains(&activity.id);let activity_id=activity.id.clone();view!{<label class="activity-device-choice"><input type="checkbox" checked=selected on:change=move |ev|{if event_target_checked(&ev){app.run(api::post(format!("/api/areas/{id}/activities"),json!({"activity":activity_id})));}else{app.run(api::delete(format!("/api/areas/{id}/activities/{activity_id}")));}}/><span>{area.name.clone()}</span></label>}}).collect_view();
    view!{<section class="card"><h2>"Choose the main control screen"</h2><p class="dim">"The source supplies the screen and default controls. Physical button mappings can control any included device. Hold Back to return to Couch without ending the activity."</p>{options}
        {activity.source.is_none().then(||view!{<p class="dim">"Select a Kodi/TV screen or create custom pages below."</p>})}
        {pages}
        <h3>"Show on areas"</h3><p class="dim">"The activity is also available in its room."</p>{areas}
    </section>}.into_any()
}
