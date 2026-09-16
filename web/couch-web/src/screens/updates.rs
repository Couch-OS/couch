use crate::{api, route::Route, ui, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
#[derive(Clone, Default, Deserialize)]
struct Status {
    installed: String,
    channel: String,
    available: Option<String>,
    #[serde(default)]
    kind: String,
    notes: String,
    phase: String,
    message: String,
    can_install: bool,
    automatic_checks: bool,
    /// The kernel commit the installed boot payload's notes named, and whether
    /// the image it replaced is still saved on the remote.
    #[serde(default)]
    boot_kernel: String,
    #[serde(default)]
    boot_previous: bool,
    /// The release the kernel on the partition came from, whether it has
    /// fallen behind the software, and whether the second step of a two-step
    /// update is still outstanding.
    #[serde(default)]
    boot_release: String,
    #[serde(default)]
    boot_behind: bool,
    #[serde(default)]
    boot_pending: bool,
    /// The one sentence
    /// the remote's own Updates panel shows for the same state.
    #[serde(default)]
    guidance: String,
}
async fn status(app: App, value: RwSignal<Status>, error: RwSignal<String>) {
    match api::ha("GET", "/api/updates", None).await {
        Ok(v) => match serde_json::from_value(v) {
            Ok(s) => value.set(s),
            Err(_) => error.set("Could not read update status".into()),
        },
        Err(e) => {
            if e.unauthorized {
                app.paired.set(Some(false));
            }
            error.set(e.message);
        }
    }
}
fn request(
    app: App,
    value: RwSignal<Status>,
    error: RwSignal<String>,
    method: &'static str,
    path: &'static str,
    body: serde_json::Value,
) {
    error.set(String::new());
    spawn_local(async move {
        match api::ha(method, path, Some(body)).await {
            Ok(_) => status(app, value, error).await,
            Err(e) => {
                if e.unauthorized {
                    app.paired.set(Some(false));
                }
                error.set(e.message);
            }
        }
    });
}
pub fn notification(app: App) -> AnyView {
    let value = RwSignal::new(Status::default());
    let error = RwSignal::new(String::new());
    spawn_local(async move {
        let _ = api::ha(
            "POST",
            "/api/updates/check",
            Some(serde_json::json!({"automatic":true})),
        )
        .await;
        status(app, value, error).await;
    });
    let timer = leptos::prelude::set_interval_with_handle(
        move || {
            if app.paired.get_untracked() == Some(true) {
                spawn_local(status(app, value, error));
            }
        },
        std::time::Duration::from_secs(15),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    view! { {move ||{
        let v = value.get();
        // An unfinished two-step update is worth a banner of its own: the user
        // who stopped after step 1 is not looking for "an update", they think
        // they already installed it.
        let (text, label) = match (&v.available, v.boot_pending) {
            (Some(version), _) if v.kind == "boot" => (format!("Finish updating Couch {version}: kernel and boot image"), "Finish update"),
            (Some(version), _) if v.kind == "combined" => (format!("Couch {version} is available: software and kernel, one restart"), "Review update"),
            (Some(version), _) => (format!("Couch {version} is available"), "Review update"),
            (None, true) => ("Your last Couch update is not finished: its kernel and boot image are still to install".into(), "Finish update"),
            (None, false) => return None,
        };
        Some(view!{
        <div class="banner" role="status"><span>{text}</span><button class="link" on:click=move |_|app.go(Route::Updates)>{label}</button></div>
    })}} }.into_any()
}
pub fn screen(app: App) -> AnyView {
    let value = RwSignal::new(Status::default());
    let error = RwSignal::new(String::new());
    let confirm = RwSignal::new(false);
    let undo = RwSignal::new(false);
    spawn_local(status(app, value, error));
    let timer = leptos::prelude::set_interval_with_handle(
        move || spawn_local(status(app, value, error)),
        std::time::Duration::from_secs(2),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
    view! {
        {ui::page_header(app,"Software updates",None)}
        <p class="lead">"Review new Couch builds and choose when to install them."</p>
        <section class="card"><h2>"What is installed"</h2>
        <p>"Software "{move ||value.get().installed}</p>
        {move ||{let v=value.get(); (!v.boot_release.is_empty()).then(||{
            // Plain words for the row the remote also shows: which release the
            // kernel came from, and whether it kept up with the software.
            let state = if v.boot_pending { " — older than the software, and its update is still to install".to_string() }
                else if v.boot_release == v.installed { " — up to date".to_string() }
                else if v.boot_behind { " — from an earlier build; no newer kernel is published for this one".to_string() }
                else { String::new() };
            let commit = if v.boot_kernel.is_empty() { String::new() } else { format!(" · kernel source {}", v.boot_kernel) };
            view!{<p>"Kernel and boot image "{v.boot_release}{state}{commit}</p>}
        })}}
        {move ||value.get().boot_previous.then(||view!{
            <p class="dim">"The boot image this replaced is saved on the remote as /opt/couch/boot/previous.img. Writing it back verifies it against its record first, and does not restart: use Power afterwards. A kernel that boots but never brings the GUI up puts the remote into recovery on its own; one that dies earlier needs the physical route, holding Back while powering on."</p>
            <label><input type="checkbox" prop:checked=move ||undo.get() on:change=move |e|undo.set(event_target_checked(&e))/>"Write the saved previous boot image back to the boot partition"</label>
            <button class="ghost" disabled=move ||!undo.get() on:click=move |_|{undo.set(false);request(app,value,error,"POST","/api/updates/boot-rollback",serde_json::json!({"confirm":true}));}>"Restore previous boot image"</button>
        })}
        <p class="dim">"Couch software and its matching kernel and boot image download and verify together. Install the update with one restart. Your connections, Wi-Fi and settings are kept. Alpine upgrades use the OS installer."</p>
        <label class="field">"Release channel"<select aria-label="Release channel" prop:value=move ||value.get().channel on:change=move |e|request(app,value,error,"PUT","/api/updates/settings",serde_json::json!({"channel":event_target_value(&e),"automatic_checks":value.get_untracked().automatic_checks}))><option value="stable">"Stable"</option><option value="alpha">"Alpha · testing builds"</option><option value="dev">"Dev · every build from the dev branch"</option></select></label>
        <label><input type="checkbox" prop:checked=move ||value.get().automatic_checks on:change=move |e|request(app,value,error,"PUT","/api/updates/settings",serde_json::json!({"channel":value.get_untracked().channel,"automatic_checks":event_target_checked(&e)}))/>"Check for updates when I open the web UI"</label>
        <p class="dim">"Checks run at most once every six hours. Updates are installed only when you choose."</p>
        <button class="ghost" disabled=move ||matches!(value.get().phase.as_str(),"checking"|"downloading"|"verifying"|"ready") on:click=move |_|request(app,value,error,"POST","/api/updates/check",serde_json::json!({"automatic":false}))>"Check now"</button>
        </section>
        <section class="card"><h2>{move ||{let v=value.get(); match (&v.available,v.boot_pending) {
            (Some(_),_) if v.kind=="boot" => "Finish update: kernel and boot image".to_string(),
            (Some(version),_) if v.kind=="combined" => format!("Couch software and kernel {version}"),
            (Some(version),_) => format!("Available: {version}"),
            (None,true) => "This update is not finished".to_string(),
            (None,false) => "Update status".to_string(),
        }}}</h2>
        // The same sentence the remote's own Updates panel shows, so the two
        // screens never tell different stories about the same state.
        {move ||(!value.get().guidance.is_empty()).then(||view!{<div class="notice" role="status"><p>{value.get().guidance}</p></div>})}
        <p role="status" aria-live="polite">{move ||value.get().message}</p><p>{move ||value.get().notes}</p>
        {move ||(value.get().boot_pending&&value.get().available.is_none()).then(||view!{<button class="primary" disabled=move ||matches!(value.get().phase.as_str(),"checking"|"downloading"|"verifying"|"ready") on:click=move |_|request(app,value,error,"POST","/api/updates/check",serde_json::json!({"automatic":false}))>"Find the rest of this update"</button>})}
        {move ||value.get().can_install.then(||view!{<button class="primary" on:click=move |_|request(app,value,error,"POST","/api/updates/install",serde_json::json!({"version":value.get_untracked().available}))>{if value.get_untracked().kind=="boot"{"Finish update: download the kernel and boot image"}else{"Download & verify update"}}</button>})}
        {move ||(value.get().phase=="ready").then(||view!{
            {if value.get_untracked().kind=="boot" { view!{<p>"The boot image is ready. Installing saves the previous boot image and restarts the remote. Keep the remote charged until it is back."</p>}.into_any() } else if value.get_untracked().kind=="combined" { view!{<p>"The software and boot image are ready. Both install together with one restart. Your connections, Wi-Fi and settings will be kept. Keep the remote charged until it is back."</p>}.into_any() } else { view!{<p>"The update is ready. Your connections, Wi-Fi and settings will be kept. Keep the remote charged while it restarts."</p>}.into_any() }}
            <label><input type="checkbox" prop:checked=move ||confirm.get() on:change=move |e|confirm.set(event_target_checked(&e))/>{if value.get_untracked().kind=="boot"{"Write the boot image and restart the remote"}else{"Restart the remote and apply this update"}}</label>
            <button class="primary" disabled=move ||!confirm.get() on:click=move |_|{confirm.set(false);request(app,value,error,"POST","/api/updates/restart",serde_json::json!({"confirm":true}));}>{if value.get_untracked().kind=="boot"{"Finish update & restart"}else{"Install & restart"}}</button>
        })}
        <p role="alert">{move ||error.get()}</p>
        </section>
    }.into_any()
}
