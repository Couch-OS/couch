//! Fixed-grid custom activity pages; all widgets dispatch typed device commands.
use crate::{api, ui, App};
use couch_model::{Action, Activity, ActivityPage, ActivityWidget, Config, Id};
use leptos::prelude::*;
use std::sync::Arc;

pub fn editor(app: App, config: &Config, activity: &Activity) -> AnyView {
    let ir_commands = super::device_commands::Commands::new(config);
    let base = StoredValue::new(activity.clone());
    // One snapshot, shared with every page's picker, instead of two copies of
    // the document per page.
    let house = Arc::new(config.clone());
    let save =
        move |next: Activity| app.run(api::put(format!("/api/activities/{}", next.id), next));
    let pages=activity.setup.pages.iter().enumerate().map(|(index,page)| {
        let label_base=base.get_value();
        let title=page.title.clone();
        let items=page.widgets.iter().enumerate().map(|(widget_index,widget)| {
            let label=widget.label.clone();let action=widget.action.clone();
            let device=config.devices().find(|(_,d)|d.id==action.device).map(|(_,d)|d.name.clone()).unwrap_or_default();
            let function=config.devices().find(|(_,d)|d.id==action.device).and_then(|(_,d)|config.resolve_integration(&d.integration)).and_then(|i|couch_model::buttons::function_choices(&i).into_iter().find(|f|f.0==action.command).map(|f|f.1)).or_else(||super::device_commands::value_label(&action.command)).unwrap_or(action.command);
            view!{<div class="custom-widget-editor">
                <div class="custom-widget-heading"><strong>{format!("Button {}",widget_index+1)}</strong><span>{format!("{device} · {function}")}</span></div>
                {ui::text_field("Button label",label,"Play / pause",move |label|{let mut a=base.get_value();a.setup.pages[index].widgets[widget_index].label=label;save(a);})}
                {ui::icon_select(widget.icon,move |icon|{let mut a=base.get_value();a.setup.pages[index].widgets[widget_index].icon=icon;save(a);})}
                <div class="custom-page-actions">
                    <button class="ghost" aria-label=format!("Move button {} up on page {}",widget_index+1,index+1) disabled={move ||app.busy.get()||widget_index==0} on:click=move |_|{let mut a=base.get_value();a.setup.pages[index].widgets.swap(widget_index,widget_index-1);save(a);}>"↑"</button>
                    <button class="ghost" aria-label=format!("Move button {} down on page {}",widget_index+1,index+1) disabled={move ||app.busy.get()||widget_index+1>=base.get_value().setup.pages[index].widgets.len()} on:click=move |_|{let mut a=base.get_value();a.setup.pages[index].widgets.swap(widget_index,widget_index+1);save(a);}>"↓"</button>
                    <button class="danger" aria-label=format!("Remove button {} from page {}",widget_index+1,index+1) disabled={move ||app.busy.get()} on:click=move |_|{let mut a=base.get_value();a.setup.pages[index].widgets.remove(widget_index);save(a);}>"Remove"</button>
                </div>
            </div>}
        }).collect_view();
        let picker=add_widget(app,house.clone(),activity,index,ir_commands);
        view!{<section class="card custom-page-editor" aria-label=format!("Custom page {}",index+1)>
            <div class="custom-page-heading"><h3>{format!("Page {}",index+1)}</h3><div class="custom-page-actions">
                <button class="ghost" aria-label=format!("Move page {} up",index+1) disabled={move ||app.busy.get()||index==0} on:click=move |_|{let mut a=base.get_value();a.setup.pages.swap(index,index-1);save(a);}>"↑"</button>
                <button class="ghost" aria-label=format!("Move page {} down",index+1) disabled={move ||app.busy.get()||index+1>=base.get_value().setup.pages.len()} on:click=move |_|{let mut a=base.get_value();a.setup.pages.swap(index,index+1);save(a);}>"↓"</button>
                <button class="danger" aria-label=format!("Delete page {}",index+1) disabled={move ||app.busy.get()} on:click=move |_|{let mut a=base.get_value();a.setup.pages.remove(index);if a.setup.pages.is_empty(){a.setup.custom_screen=false;}save(a);}>"Delete page"</button>
            </div></div>
            {ui::text_field("Page title",title,"Playback",move |title|{let mut a=label_base.clone();a.setup.pages[index].title=title;save(a);})}
            <div class="custom-widget-grid">{items}</div>
            {picker}
        </section>}
    }).collect_view();
    view!{<section class="custom-pages">
        <h3>"Custom pages"</h3><p class="dim">"Build up to 8 pages with 6 command buttons each. Buttons appear left to right, then top to bottom. Use the page arrows or D-pad edges to switch pages on the remote."</p>
        <label class="activity-screen-choice"><input type="radio" name="activity-screen" checked=activity.setup.custom_screen disabled={move ||app.busy.get()||base.get_value().setup.pages.is_empty()} on:change=move |_|{let mut a=base.get_value();a.setup.custom_screen=true;save(a);}/><span><strong>"Open custom pages first"</strong><small>"Keep a Kodi or TV source to switch to its media controls. Physical button mappings still take precedence."</small></span></label>
        {pages}
        <button class="ghost" disabled={move ||app.busy.get()||base.get_value().setup.pages.len()>=8} on:click=move |_|{let mut a=base.get_value();a.setup.pages.push(ActivityPage{title:format!("Page {}",a.setup.pages.len()+1),widgets:vec![]});save(a);}>"＋ Add page"</button>
    </section>}.into_any()
}
fn add_widget(
    app: App,
    house: Arc<Config>,
    activity: &Activity,
    page: usize,
    ir_commands: super::device_commands::Commands,
) -> AnyView {
    if activity.setup.pages[page].widgets.len() >= 6 {
        return view!{<p class="dim">"This page has 6 buttons. Add another page for more controls."</p>}.into_any();
    }
    let base = StoredValue::new(activity.clone());
    let cfg = StoredValue::new(house);
    let device = RwSignal::new(
        activity
            .setup
            .devices
            .first()
            .map(ToString::to_string)
            .unwrap_or_default(),
    );
    let command = RwSignal::new(String::new());
    let functions = move || {
        let c = cfg.get_value();
        let rows = c
            .devices()
            .find(|(_, d)| d.id.as_str() == device.get())
            .map(|(_, d)| ir_commands.choices(&c, d, Vec::new()))
            .unwrap_or_default();
        rows
    };
    let options = move || {
        functions()
            .into_iter()
            .map(|(id, label)| view! {<option value=id>{label}</option>})
            .collect_view()
    };
    view!{<div class="custom-add-widget"><h4>"Add a command button"</h4>
        <label class="field"><span class="label">"Device"</span><select aria-label=format!("Button device for page {}",page+1) prop:value=move ||device.get() on:change=move |ev|{device.set(event_target_value(&ev));command.set(String::new());}>
            {cfg.get_value().devices().filter(|(_,d)|activity.setup.devices.contains(&d.id)).map(|(_,d)|view!{<option value=d.id.to_string()>{d.name.clone()}</option>}).collect_view()}
        </select></label>
        <label class="field"><span class="label">"Function"</span><select aria-label=format!("Button function for page {}",page+1) prop:value=move ||command.get() on:change=move |ev|command.set(event_target_value(&ev))><option value="">"Choose a function…"</option>{options}</select></label>
        <p class="dim" role="status">{move ||ir_commands.status()}</p>
        <button class="ghost" disabled={move ||app.busy.get()||command.get().is_empty()} on:click=move |_|{
            let selected=command.get_untracked();
            let Some((_,label))=functions().into_iter().find(|f|f.0==selected) else{return};
            let mut a=base.get_value();a.setup.pages[page].widgets.push(ActivityWidget{label,icon:None,action:Action::new(Id::new(device.get_untracked()),selected)});app.run(api::put(format!("/api/activities/{}",a.id),a));
        }>"＋ Add button"</button>
    </div>}.into_any()
}
