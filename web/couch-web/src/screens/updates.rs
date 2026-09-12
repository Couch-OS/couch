use crate::{api, route::Route, ui, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
#[derive(Clone, Default, Deserialize)]
struct Status {
    installed: String,
    channel: String,
    available: Option<String>,
    notes: String,
    phase: String,
    message: String,
    can_install: bool,
    automatic_checks: bool,
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
    view! { {move ||value.get().available.map(|version|view!{
        <div class="banner" role="status"><span>{format!("Couch {version} is available")}</span><button class="link" on:click=move |_|app.go(Route::Updates)>"Review update"</button></div>
    })} }.into_any()
}
pub fn screen(app: App) -> AnyView {
    let value = RwSignal::new(Status::default());
    let error = RwSignal::new(String::new());
    let confirm = RwSignal::new(false);
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
        {ui::page_header(app,"Software updates".into(),None)}
        <p class="lead">"Review new Couch builds and choose when to install them."</p>
        <section class="card"><h2>"Installed build"</h2><p>{move ||value.get().installed}</p>
        <p class="dim">"This updater installs Couch apps and services. Kernel and Alpine upgrades use the OS installer."</p>
        <label class="field">"Release channel"<select aria-label="Release channel" prop:value=move ||value.get().channel on:change=move |e|request(app,value,error,"PUT","/api/updates/settings",serde_json::json!({"channel":event_target_value(&e),"automatic_checks":value.get_untracked().automatic_checks}))><option value="stable">"Stable"</option><option value="alpha">"Alpha · testing builds"</option></select></label>
        <label><input type="checkbox" prop:checked=move ||value.get().automatic_checks on:change=move |e|request(app,value,error,"PUT","/api/updates/settings",serde_json::json!({"channel":value.get_untracked().channel,"automatic_checks":event_target_checked(&e)}))/>"Check for updates when I open the web UI"</label>
        <p class="dim">"Checks run at most once every six hours. Updates are installed only when you choose."</p>
        <button class="ghost" disabled=move ||matches!(value.get().phase.as_str(),"checking"|"downloading"|"verifying"|"ready") on:click=move |_|request(app,value,error,"POST","/api/updates/check",serde_json::json!({"automatic":false}))>"Check now"</button>
        </section>
        <section class="card"><h2>{move ||value.get().available.map(|v|format!("Available: {v}")).unwrap_or_else(||"Update status".into())}</h2>
        <p role="status" aria-live="polite">{move ||value.get().message}</p><p>{move ||value.get().notes}</p>
        {move ||value.get().can_install.then(||view!{<button class="primary" on:click=move |_|request(app,value,error,"POST","/api/updates/install",serde_json::json!({"version":value.get_untracked().available}))>"Download & verify update"</button>})}
        {move ||(value.get().phase=="ready").then(||view!{
            <p>"The update is ready. Your connections, Wi-Fi and settings will be kept. Keep the remote charged while it restarts."</p>
            <label><input type="checkbox" prop:checked=move ||confirm.get() on:change=move |e|confirm.set(event_target_checked(&e))/>"Restart the remote and apply this update"</label>
            <button class="primary" disabled=move ||!confirm.get() on:click=move |_|{confirm.set(false);request(app,value,error,"POST","/api/updates/restart",serde_json::json!({"confirm":true}));}>"Install & restart"</button>
        })}
        <p role="alert">{move ||error.get()}</p>
        </section>
    }.into_any()
}
