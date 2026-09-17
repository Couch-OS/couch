//! Explicit migration pilot; package installation alone never changes a connection.
use crate::{api, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Default, Deserialize)]
struct MigrationList {
    revision: u64,
    connections: Vec<MigrationConnection>,
    package_available: bool,
    #[serde(default)]
    supports_volume_db: bool,
    #[serde(default)]
    supports_absolute_volume: bool,
}
#[derive(Clone, Deserialize)]
struct MigrationConnection {
    id: String,
    name: String,
    state: String,
}

fn load(app: App, list: RwSignal<MigrationList>, error: RwSignal<String>) {
    spawn_local(async move {
        match api::ha("GET", "/api/integrations/migrations/denon", None).await {
            Ok(value) => match serde_json::from_value(value) {
                Ok(next) => list.set(next),
                Err(_) => error.set("The remote sent an unreadable migration status.".into()),
            },
            Err(next) => {
                if next.unauthorized {
                    app.paired.set(Some(false));
                }
                error.set(next.message);
            }
        }
    });
}

pub(super) fn section(app: App, busy: RwSignal<bool>) -> AnyView {
    let list = RwSignal::new(MigrationList::default());
    let error = RwSignal::new(String::new());
    let result = RwSignal::new(String::new());
    let pending = RwSignal::new(None::<MigrationConnection>);
    Effect::new(move |_| {
        if !busy.get() {
            load(app, list, error);
        }
    });
    let confirm = move || {
        if busy.get_untracked() {
            return;
        }
        let Some(connection) = pending.get_untracked() else {
            return;
        };
        let restore = connection.state == "migrated";
        let revision = list.get_untracked().revision;
        busy.set(true);
        error.set(String::new());
        result.set(String::new());
        pending.set(None);
        spawn_local(async move {
            let path = format!("/api/integrations/migrations/denon/{}", connection.id);
            match api::ha("POST", &path, Some(json!({
                "action": if restore { "restore-native" } else { "migrate" }, "revision":revision,
            }))).await {
                Ok(_) => {
                    result.set(if restore { "Built-in Denon control restored." } else { "This connection now uses the Denon package." }.into());
                    match api::load().await {
                        Ok(config) => app.config.set(Some(config)),
                        Err(next) => {
                            if next.unauthorized { app.paired.set(Some(false)); }
                            error.set(format!("Connection changed, but configuration could not be refreshed: {}", next.message));
                        }
                    }
                }
                Err(next) => {
                    if next.unauthorized { app.paired.set(Some(false)); }
                    error.set(next.message);
                }
            }
            busy.set(false);
        });
    };
    view! {
        <section class="card integration-migration">
            <h2>"Denon migration pilot"</h2>
            <p>"Try the package with an existing named Denon connection. Your devices, activities and button assignments stay attached. You can restore built-in control here."</p>
            {move || (!(list.get().supports_volume_db && list.get().supports_absolute_volume)).then(|| view! {<p class="notice">"Preview limitation: this package does not provide full dB reading and absolute-volume support. Keep built-in control if you need either feature."</p>})}
            <p role="alert">{move || error.get()}</p>
            <p role="status" aria-live="polite">{move || result.get()}</p>
            {move || (!list.get().package_available).then(|| view! { <p class="dim">"Install the Denon package before switching a connection."</p> })}
            {move || list.get().connections.is_empty().then(|| view! { <p class="dim">"No named Denon connections are available for this pilot."</p> })}
            <div class="integration-grid">{move || list.get().connections.into_iter().map(|connection| {
                let restored = connection.state == "migrated";
                let name = connection.name.clone();
                view! { <article class="integration-package">
                    <h3>{name}</h3>
                    <p>{if restored { "Using the Denon package" } else { "Using built-in Denon control" }}</p>
                    <button class="ghost" disabled=move || busy.get() || (!restored && !list.get().package_available)
                        on:click=move |_| pending.set(Some(connection.clone()))>
                        {if restored { "Restore built-in control" } else { "Switch to Denon package" }}
                    </button>
                </article> }
            }).collect_view()}</div>
            {move || pending.get().map(|connection| {
                let restore = connection.state == "migrated";
                view! { <div class="notice" role="alert">
                    <strong>{format!("{} for {}?", if restore { "Restore built-in control" } else { "Use the preview package" }, connection.name)}</strong>
                    <p>{if restore { "The package connection will stop before built-in control resumes. The installed package remains available." } else { "The current connection will stop before the package takes over. Review any limitations above. Existing settings are retained for restoration." }}</p>
                    <button class="primary" disabled=move || busy.get() on:click=move |_| confirm()>{if restore { "Confirm restore" } else { "Confirm switch" }}</button>
                    <button class="ghost" disabled=move || busy.get() on:click=move |_| pending.set(None)>"Cancel"</button>
                </div> }
            })}
        </section>
    }.into_any()
}
