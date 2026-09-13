use crate::{api, ui, App};
use couch_model::{Config, RemoteSettings};
use leptos::{prelude::*, task::spawn_local};

pub fn screen(app: App, config: &Config) -> AnyView {
    let timezone = RwSignal::new(config.remote.timezone.clone());
    let clock = RwSignal::new(config.remote.clock_24h);
    let dock = RwSignal::new(config.remote.dock_clock);
    let lift = RwSignal::new(config.remote.wake_on_lift);
    let zones = RwSignal::new(vec!["UTC".to_string()]);
    let error = RwSignal::new(String::new());
    spawn_local(async move {
        match api::ha("GET", "/api/remote/timezones", None).await {
            Ok(v) => {
                if let Ok(v) = serde_json::from_value::<Vec<String>>(v) {
                    zones.set(v);
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
    view! {
        {ui::page_header(app,"Remote settings".into(),None)}
        <p class="lead">"Personalize this remote’s clock, wake behavior, dock display and appearance, and change what its own Settings menu shows: display, keys, SSH, network and power."</p>
        <section class="card"><h2>"Clock & wake"</h2>
        <label class="field">"Timezone"<select aria-label="Timezone" prop:value=move ||timezone.get() on:change=move |e|timezone.set(event_target_value(&e))><option value="">"Follow system timezone"</option>{move ||zones.get().into_iter().map(|z|view!{<option selected=timezone.get_untracked()==z value=z.clone()>{z.replace('_'," ")}</option>}).collect_view()}</select></label>
        <p class="dim">"Regional timezones adjust automatically for daylight saving time."</p>
        <label class="field">"Time format"<select aria-label="Time format" prop:value=move ||if clock.get(){"24"}else{"12"} on:change=move |e|clock.set(event_target_value(&e)=="24")><option value="12">"12-hour · 9:41 PM"</option><option value="24">"24-hour · 21:41"</option></select></label>
        <label><input type="checkbox" prop:checked=move ||dock.get() on:change=move |e|dock.set(event_target_checked(&e))/>"Show a digital clock while charging"</label>
        <p class="dim">"After the normal dim timeout, show a dim clock instead of turning the display off. Tap or press a button to return. Undocking restores normal standby. The charging indicator controls this behavior, including a fully charged docked remote."</p>
        <label><input type="checkbox" prop:checked=move ||lift.get() on:change=move |e|lift.set(event_target_checked(&e))/>"Wake when picked up"</label>
        <p class="dim">"Lift the resting remote to restore the screen. Touch and button wake remain available."</p>
        <p role="alert">{move ||error.get()}</p><button class="primary" disabled=move ||app.busy.get() on:click=move |_|app.run(api::put("/api/remote",RemoteSettings{timezone:timezone.get_untracked(),clock_24h:clock.get_untracked(),dock_clock:dock.get_untracked(),wake_on_lift:lift.get_untracked()}))>"Save remote settings"</button>
        </section>
        {super::appearance::editor(app,config)}
        {super::remote_device::sections(app)}
    }.into_any()
}
