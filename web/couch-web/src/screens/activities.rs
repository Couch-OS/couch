//! Activity setup follows the task: devices, on/off sequences, then controls.
//!
//! The list and the workbench read the activity through [`App::activity`], so
//! ticking a device or renaming the activity updates the fields that show it
//! and leaves the open tab, the scroll position and the rest of the page put.
//! The three sub-editors still take a whole `&Config` and are wrapped in
//! [`super::keyed`] here, so they behave exactly as they did.
use crate::screens::keyed;
use crate::{api, route::Route, ui, App};
use couch_model::{Activity, ActivitySetup, Id};
use leptos::prelude::*;
use serde_json::json;

/// Which tab of the activity editor is open.
///
/// Provided at the root by [`super::provide_editor_state`], so it also survives
/// leaving the activity and coming back to it.
#[derive(Clone, Copy)]
pub(super) struct State {
    pub tab: RwSignal<String>,
}
impl State {
    pub(super) fn new() -> State {
        State {
            tab: RwSignal::new("setup".into()),
        }
    }
}

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

/// The activity as every editor below wants it: `setup.devices` filled in from
/// whatever the older fields mention.
fn with_members(mut activity: Activity) -> Activity {
    activity.setup.devices = members(&activity);
    activity
}

pub fn list(app: App) -> AnyView {
    let tab = expect_context::<State>().tab;
    let order = super::ids(app.activities, |a| &a.id);
    view! {
        {ui::page_header(app,"Activities",None)}
        <p class="dim pad-x">"Bring devices together for something you do. Choose what turns on, what turns off, and how the remote controls it."</p>
        <div class="activity-cards"><For each=move ||order.get() key=|id|id.clone() children=move |id|{
            let activity=app.activity(id.clone());
            let open=id.clone();
            let detail=move ||activity.get().map(|a|{
                let room=super::room_name(app,&a.room);
                format!("{room} · {} devices · {} on / {} off steps",members(&a).len(),a.setup.on.len(),a.setup.off.len())
            }).unwrap_or_default();
            view! { <button class="activity-card" on:click=move |_| {tab.set("setup".into());app.go(Route::Activity(open.clone()));}>
                <span class="eyebrow">"ACTIVITY"</span><strong>{move ||activity.get().map(|a|a.name)}</strong><span>{detail}</span><span class="activity-card-open">"Configure →"</span>
            </button> }
        }/></div>
        {move ||order.with(Vec::is_empty).then(||ui::empty("Create your first activity, such as Watch TV."))}
        {new_activity(app)}
    }.into_any()
}

/// The create dialog is built once and keeps its half-filled form: it is a
/// modal, so nothing else on the page can be saved while it is open, but a
/// rebuild used to close it outright.
fn new_activity(app: App) -> AnyView {
    let tab = expect_context::<State>().tab;
    let name = RwSignal::new(String::new());
    let room = RwSignal::new(String::new());
    let selected = RwSignal::new(Vec::<Id>::new());
    let filter = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let devices = super::ids(app.devices, |(_, d): &(Id, couch_model::Device)| &d.id);
    let open = move || {
        // The first room is the default, chosen when the form opens rather than
        // when the page was drawn, so a room created since is offered.
        if room.get_untracked().is_empty() {
            if let Some(first) = app
                .rooms
                .with_untracked(|r| r.first().map(|r| r.id.to_string()))
            {
                room.set(first);
            }
        }
        if let Some(d) = dialog.get() {
            let _ = d.show_modal();
        }
    };
    view! {
        <div class="pad">
            {move ||app.rooms.with(Vec::is_empty).then(||ui::empty("Create a room before adding an activity."))}
            <button class="primary" hidden=move ||app.rooms.with(Vec::is_empty) on:click=move |_|open()>"＋ Create activity"</button>
        </div>
        <dialog class="command-picker activity-create" node_ref=dialog aria-label="Create activity">
            <div class="command-picker-header"><h2>"New activity"</h2><button class="ghost" aria-label="Close" on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>"×"</button></div>
            <label class="field"><span class="label">"Name"</span><input aria-label="Activity name" placeholder="Watch a movie" prop:value=move ||name.get() on:input=move |ev|name.set(event_target_value(&ev))/></label>
            <label class="field"><span class="label">"Room"</span><select aria-label="Activity room" prop:value=move ||room.get() on:change=move |ev|room.set(event_target_value(&ev))>
                {move ||app.rooms.with(|rooms|rooms.iter().map(|r|view!{<option value=r.id.to_string()>{r.name.clone()}</option>}).collect_view())}
            </select></label>
            <h3>"Include devices"</h3><p class="dim">"Add the player, TV, receiver, lights, or other devices this activity will use. Devices can belong to any room."</p>
            <input type="search" aria-label="Search activity devices" placeholder="Search devices…" on:input=move |ev|filter.set(event_target_value(&ev))/>
            <div class="activity-device-list"><For each=move ||devices.get() key=|id|id.clone() children=move |id|{
                let place=super::device_place(app,id.clone());
                let label=move ||place().map(|(device,room)|format!("{device} · {room}")).unwrap_or_default();
                let shown=move ||label().to_lowercase().contains(&filter.get().to_lowercase());
                let checked=id.clone();
                view! { <label class="activity-device-choice" style:display=move ||if shown(){""}else{"none"}>
                    <input type="checkbox" prop:checked=move ||selected.get().contains(&checked) on:change=move |ev|selected.update(|ids|{if event_target_checked(&ev){if !ids.contains(&id){ids.push(id.clone());}}else{ids.retain(|v|v!=&id);}})/><span>{label}</span>
                </label> }
            }/></div>
            <button class="primary" disabled=move ||app.busy.get() || name.get().trim().is_empty() || selected.get().is_empty() on:click=move |_| {
                let setup=ActivitySetup {devices:selected.get_untracked(),..Default::default()};
                let body=json!({"name":name.get_untracked().trim(),"room":room.get_untracked(),"kind":"video","setup":setup});
                // Read now, not when the dialog was built: the new activity is
                // the one whose id was not in the house a moment ago.
                let previous:Vec<Id>=app.activities.with_untracked(|all|all.iter().map(|a|a.id.clone()).collect());
                app.run(async move {let next=api::post("/api/activities",body).await?;if let Some(a)=next.activities.iter().find(|a|!previous.contains(&a.id)){tab.set("setup".into());app.go(Route::Activity(a.id.clone()));}Ok(next)});
            }>"Create and configure"</button>
            {move ||app.error.get().map(|e|view!{<p class="error">{e}</p>})}
        </dialog>
    }.into_any()
}

pub fn detail(app: App, id: Id) -> AnyView {
    let activity = app.activity(id.clone());
    view! {
        <Show
            when=move || activity.with(Option::is_some)
            fallback=move || super::gone(app, "That activity has been deleted.")
        >
            {page(app, id.clone())}
        </Show>
    }
    .into_any()
}

/// Nothing here may read a slice while it is being built - see [`super::rooms`].
fn page(app: App, id: Id) -> AnyView {
    let tab = expect_context::<State>().tab;
    let key = StoredValue::new(id.clone());
    let current = Memo::new({
        let activity = app.activity(id);
        move |_| activity.get().map(with_members)
    });
    let save =
        move |next: Activity| app.run(api::put(format!("/api/activities/{}", next.id), next));
    // The activity as it is now, for a handler that has to send a whole one
    // back. Read on the click, so it is never a copy from an earlier render.
    let base = move || current.get_untracked();
    let devices = super::ids(app.devices, |(_, d): &(Id, couch_model::Device)| &d.id);

    let tabs=[("setup","Devices & sequences"),("buttons","Physical buttons"),("screen","Remote screen")].into_iter().map(|(id,label)|view!{
        <button class:selected=move ||tab.get()==id on:click=move |_|tab.set(id.into())>{label}</button>
    }).collect_view();

    view! {
        {ui::page_header(app,move ||current.get().map(|a|a.name),Some(Route::Activities))}
        <nav class="activity-tabs" aria-label="Activity configuration">{tabs}</nav>
        <div style:display=move ||if tab.get()=="setup"{""}else{"none"}>
            <div class="activity-workbench">
                <aside class="card activity-overview">
                    <h2>"Activity"</h2>
                    {move ||ui::text_field("Name",current.get().map(|a|a.name).unwrap_or_default(),"Watch TV",move |name|{if let Some(mut a)=base(){a.name=name;save(a);}})}
                    {move ||ui::text_field("Description",current.get().map(|a|a.setup.description).unwrap_or_default(),"TV, receiver and room lighting",move |description|{if let Some(mut a)=base(){a.setup.description=description;save(a);}})}
                    {move ||{let chosen=current.get().map(|a|a.room);view!{<label class="field"><span class="label">"Room"</span><select aria-label="Activity room" on:change=move |ev|{if let Some(mut a)=base(){a.room=Id::new(event_target_value(&ev));save(a);}}>
                        {app.rooms.with(|rooms|rooms.iter().map(|r|{let is=chosen.as_ref()==Some(&r.id);view!{<option value=r.id.to_string() selected=is>{r.name.clone()}</option>}}).collect_view())}
                    </select></label>}}}
                    <label class="activity-device-choice"><input type="checkbox" prop:checked=move ||current.get().is_some_and(|a|a.setup.keep_awake) disabled=move ||app.busy.get() on:change=move |ev|{if let Some(mut a)=base(){a.setup.keep_awake=event_target_checked(&ev);save(a);}}/><span>"Keep remote awake while running"</span></label>
                    <h3>"Included devices"</h3><p class="dim">"Only these devices appear in the command and button pickers."</p>
                    {move ||current.get().and_then(|a|{
                        // The remote holds one Bluetooth link: the main screen's
                        // device gets it. A second bonded TV with nothing else
                        // cannot be reached while this runs, which is worth
                        // saying here rather than on a key press.
                        let house=app.house();
                        let link=house.bluetooth_link_device(&a)?.name.clone();
                        let stranded:Vec<String>=house.bluetooth_conflicts(&a).iter().map(|d|d.name.clone()).collect();
                        (!stranded.is_empty()).then(||view!{<p class="notice" role="alert">{format!("{} cannot be reached while this activity runs: {link} holds the Bluetooth link and the remote keeps one link at a time. Add IR commands or a connection to {}, or make it the main screen.",stranded.join(", "),if stranded.len()==1{"it"}else{"them"})}</p>})
                    })}
                    <div class="activity-device-list"><For each=move ||devices.get() key=|id|id.clone() children=move |id|{
                        let place=super::device_place(app,id.clone());
                        let checked=id.clone();
                        view!{<label class="activity-device-choice"><input type="checkbox" prop:checked=move ||current.get().is_some_and(|a|a.setup.devices.contains(&checked)) disabled=move ||app.busy.get() on:change=move |ev|{
                            let Some(mut next)=base() else{return};
                            if event_target_checked(&ev) {if !next.setup.devices.contains(&id){next.setup.devices.push(id.clone());}}
                            else {next.setup.forget_device(&id);next.buttons.retain(|b|b.action.as_ref().is_none_or(|a|a.device!=id));next.steps.retain(|s|s.device!=id);if next.source.as_ref()==Some(&id){next.source=None;}}
                            save(next);
                        }/><span><strong>{move ||place().map(|(device,_)|device)}</strong><small>{move ||place().map(|(_,room)|room)}</small></span></label>}
                    }/></div>
                </aside>
                // Still whole-document editors: redrawn on a write, as before.
                {keyed(app,move |app,config|config.activity(&key.get_value()).cloned().map(with_members).map(|a|super::activity_sequences::editor(app,config,&a)).unwrap_or_else(||().into_any()))}
            </div>
            {move ||current.get().filter(|a|!a.steps.is_empty()).map(|a|view!{<section class="card"><h3>"Previous startup draft"</h3><p class="dim">"These older commands were never executed. Recreate the ones you want in the on sequence; they are retained here for reference."</p><ul>{a.steps.iter().map(|s|view!{<li>{format!("{} · {}",super::device_place(app,s.device.clone())().map(|(d,r)|format!("{d} · {r}")).unwrap_or_else(||format!("{} (missing)",s.device)),s.command)}</li>}).collect_view()}</ul></section>})}
        </div>
        <div style:display=move ||if tab.get()=="buttons"{""}else{"none"}>
            {keyed(app,move |app,config|{
                let Some(current)=config.activity(&key.get_value()).cloned().map(with_members) else{return ().into_any()};
                // The pickers only offer the activity's own devices.
                let mut mapped=config.clone();
                for room in &mut mapped.rooms { room.devices.retain(|d|current.setup.devices.contains(&d.id)); }
                super::activity_buttons::editor(app,&mapped,&current)
            })}
        </div>
        <div style:display=move ||if tab.get()=="screen"{""}else{"none"}>
            {keyed(app,move |app,config|config.activity(&key.get_value()).cloned().map(with_members).map(|a|screen_editor(app,config,&a)).unwrap_or_else(||().into_any()))}
        </div>
        <div class="pad">{ui::danger_button("Delete activity",move ||{let id=key.get_value();app.run(async move {let result=api::delete(format!("/api/activities/{id}")).await?;app.go(Route::Activities);Ok(result)});})}</div>
    }.into_any()
}

fn screen_editor(app: App, config: &couch_model::Config, activity: &Activity) -> AnyView {
    use couch_model::Integration;
    let base = StoredValue::new(activity.clone());
    let options=config.devices().filter(|(_,d)|activity.setup.devices.contains(&d.id)).filter_map(|(_,d)|{
        let screen=match config.resolve_integration(&d.integration) {Some(Integration::Sonos{..})=>"Sonos · now playing, transport and sources",Some(Integration::Kodi{..})=>"Kodi · artwork, playback and chapters",Some(Integration::WebOs)=>"LG TV · inputs, apps and playback",Some(Integration::AndroidTv)=>"Android TV · navigation and playback",Some(Integration::AppleTv)=>"Apple TV · apps and playback",Some(Integration::Tizen)=>"Samsung TV · sources, apps and playback",_ if d.network_integration(config).is_none() && d.bluetooth.is_some()=>"Bluetooth TV · keys over Bluetooth, no feedback",_=>return None};
        let id=d.id.clone();let selected=activity.source.as_ref()==Some(&id) && !activity.setup.custom_screen;
        Some(view!{<label class="activity-screen-choice"><input type="radio" name="activity-screen" checked=selected disabled=move ||app.busy.get() on:change=move |_|{let mut a=base.get_value();a.source=Some(id.clone());a.setup.custom_screen=false;app.run(api::put(format!("/api/activities/{}",a.id),a));}/><span><strong>{d.name.clone()}</strong><small>{screen}</small></span></label>})
    }).collect_view();
    let pages = super::activity_pages::editor(app, config, activity);
    let areas=config.areas.iter().map(|area|{let id=area.id.clone();let selected=area.activities.contains(&activity.id);let activity_id=activity.id.clone();view!{<label class="activity-device-choice"><input type="checkbox" checked=selected on:change=move |ev|{if event_target_checked(&ev){app.run(api::post(format!("/api/areas/{id}/activities"),json!({"activity":activity_id})));}else{app.run(api::delete(format!("/api/areas/{id}/activities/{activity_id}")));}}/><span>{area.name.clone()}</span></label>}}).collect_view();
    view!{<section class="card"><h2>"Choose the main control screen"</h2><p class="dim">"The source supplies the screen and default controls. Physical button mappings can control any included device. Hold Back to return to Couch without ending the activity."</p>{options}
        {activity.source.is_none().then(||view!{<p class="dim">"Select a Kodi/TV screen or create custom pages below."</p>})}
        {pages}
        <h3>"Show on areas"</h3><p class="dim">"The activity is also available in its room."</p>{areas}
    </section>}.into_any()
}
