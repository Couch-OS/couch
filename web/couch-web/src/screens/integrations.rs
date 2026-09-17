//! Installing integration packages and explicitly trusting their repositories.
//!
//! Connections remain in `connections.rs`: a package operation never touches a
//! saved connection, including when a package is removed or rolled back.

use crate::{api, ui, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Default, Deserialize)]
struct Catalog {
    #[serde(default)]
    installed: Vec<Installed>,
    #[serde(default)]
    available: Vec<Available>,
    #[serde(default)]
    repositories: Vec<Repository>,
    #[serde(default)]
    catalog_error: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
struct Installed {
    id: String,
    #[serde(default, alias = "label")]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    available_version: Option<String>,
    #[serde(default)]
    status: Value,
    #[serde(default)]
    description: String,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    connection_configured: bool,
    #[serde(default)]
    can_rollback: bool,
}

#[derive(Clone, Default, Deserialize)]
struct Available {
    id: String,
    #[serde(default, alias = "label")]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
struct Repository {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    trusted: bool,
    #[serde(default)]
    official: bool,
}

#[derive(Clone, Default, Deserialize)]
struct PendingRepository {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    algorithm: String,
}

#[derive(Deserialize)]
struct StagedRepository {
    pending_confirmation: PendingRepository,
}

#[derive(Deserialize)]
struct OperationReceipt {
    operation_id: String,
}

#[derive(Default, Deserialize)]
struct OperationStatus {
    #[serde(default)]
    state: String,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    message: String,
}

#[derive(Default, Deserialize)]
struct CurrentOperation {
    id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    message: String,
}

#[derive(Default, Deserialize)]
struct CurrentOperationResponse {
    #[serde(default)]
    operation: Option<CurrentOperation>,
}

#[derive(Clone, Default, Deserialize)]
struct RecoveryStatus {
    #[serde(default)]
    recovery: Option<RecoverySnapshot>,
}

#[derive(Clone, Default, Deserialize)]
struct RecoverySnapshot {
    #[serde(default)]
    integrations_active: bool,
    #[serde(default)]
    path: Option<String>,
}

fn catalog_message(catalog: &Catalog) -> String {
    match catalog.catalog_error.as_deref() {
        Some(error) if !error.is_empty() => format!("Catalog refresh problem: {error}"),
        _ if catalog.available.is_empty() => {
            "No packages are available from your trusted repositories.".into()
        }
        _ => format!(
            "{} package{} available from trusted repositories.",
            catalog.available.len(),
            if catalog.available.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
    }
}

fn name(name: &str, id: &str) -> String {
    if name.trim().is_empty() {
        id.to_string()
    } else {
        name.to_string()
    }
}

fn status(value: &Value) -> String {
    match value {
        Value::String(value) if !value.is_empty() => value.clone(),
        Value::Object(value) => value
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.get("kind").and_then(Value::as_str))
            .unwrap_or("Installed")
            .to_string(),
        _ => "Installed".into(),
    }
}

fn load_catalog(
    app: App,
    catalog: RwSignal<Catalog>,
    message: RwSignal<String>,
    error: RwSignal<String>,
) {
    spawn_local(async move {
        match api::ha("GET", "/api/integrations/catalog", None).await {
            Ok(value) => match serde_json::from_value::<Catalog>(value) {
                Ok(next) => {
                    message.set(catalog_message(&next));
                    catalog.set(next);
                }
                Err(_) => error.set("The integration catalog was unreadable.".into()),
            },
            Err(next) => {
                if next.unauthorized {
                    app.paired.set(Some(false));
                } else if next.stale {
                    // Store::list takes the same package lock as an install.
                    // A 409 here is a short-lived view of a daemon-owned
                    // operation, not a failed catalog request; the current
                    // operation lookup below will reattach and reload when it
                    // reaches a terminal state.
                    message.set("Package list is updating…".into());
                } else {
                    error.set(next.message);
                }
            }
        }
    });
}

fn load_recovery(app: App, recovery: RwSignal<RecoveryStatus>) {
    spawn_local(async move {
        match api::ha("GET", "/api/integrations/recovery", None).await {
            Ok(value) => {
                if let Ok(next) = serde_json::from_value::<RecoveryStatus>(value) {
                    recovery.set(next);
                }
            }
            Err(next) if next.unauthorized => app.paired.set(Some(false)),
            Err(_) => {}
        }
    });
}

/// Reattach after navigation or a reload. The package worker belongs to the
/// daemon, not this page, so the catalog is only a hint; this endpoint gives
/// the live operation identity that can safely be polled.
fn restore_current_operation(
    app: App,
    catalog: RwSignal<Catalog>,
    busy: RwSignal<bool>,
    operation: RwSignal<Option<String>>,
    message: RwSignal<String>,
    error: RwSignal<String>,
    result: RwSignal<String>,
) {
    spawn_local(async move {
        match api::ha("GET", "/api/integrations/operations/current", None).await {
            Ok(value) => match serde_json::from_value::<CurrentOperationResponse>(value) {
                Ok(CurrentOperationResponse {
                    operation: Some(current),
                }) if !finished(&current.state) => {
                    busy.set(true);
                    operation.set(Some(current.id));
                    message.set(operation_text(&OperationStatus {
                        state: current.state,
                        phase: current.phase,
                        message: current.message,
                    }));
                }
                Ok(CurrentOperationResponse {
                    operation: Some(current),
                }) if current.state == "succeeded" => {
                    result.set(if current.message.is_empty() {
                        "Package operation completed.".into()
                    } else {
                        current.message
                    });
                    load_catalog(app, catalog, message, error);
                }
                Ok(CurrentOperationResponse {
                    operation: Some(current),
                }) if !current.message.is_empty() => {
                    error.set(current.message);
                    load_catalog(app, catalog, message, error);
                }
                Ok(_) => {}
                Err(_) => {
                    error.set("The remote sent an unreadable package-operation update.".into())
                }
            },
            Err(next) => {
                if next.unauthorized {
                    app.paired.set(Some(false));
                } else {
                    error.set(next.message);
                }
            }
        }
    });
}

fn begin_operation(
    app: App,
    busy: RwSignal<bool>,
    operation: RwSignal<Option<String>>,
    message: RwSignal<String>,
    error: RwSignal<String>,
    result: RwSignal<String>,
    path: String,
    body: Value,
) {
    if busy.get_untracked() {
        return;
    }
    busy.set(true);
    error.set(String::new());
    result.set(String::new());
    message.set("Starting package operation…".into());
    spawn_local(async move {
        match api::ha("POST", &path, Some(body)).await {
            Ok(value) => match serde_json::from_value::<OperationReceipt>(value) {
                Ok(receipt) => {
                    operation.set(Some(receipt.operation_id));
                    message.set("Package operation started…".into());
                }
                Err(_) => {
                    busy.set(false);
                    error.set(
                        "The remote accepted the request but did not identify its operation."
                            .into(),
                    );
                }
            },
            Err(next) => {
                busy.set(false);
                if next.unauthorized {
                    app.paired.set(Some(false));
                }
                error.set(next.message);
            }
        }
    });
}

fn operation_text(operation: &OperationStatus) -> String {
    let detail = if operation.message.is_empty() {
        &operation.phase
    } else {
        &operation.message
    };
    if detail.is_empty() {
        "Working…".into()
    } else {
        detail.to_string()
    }
}

fn finished(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled")
}

fn poll_operation(
    app: App,
    catalog: RwSignal<Catalog>,
    busy: RwSignal<bool>,
    operation: RwSignal<Option<String>>,
    polling: RwSignal<bool>,
    message: RwSignal<String>,
    error: RwSignal<String>,
    result: RwSignal<String>,
) {
    let Some(id) = operation.get_untracked() else {
        return;
    };
    if polling.get_untracked() {
        return;
    }
    polling.set(true);
    spawn_local(async move {
        match api::ha("GET", &format!("/api/integrations/operations/{id}"), None).await {
            Ok(value) => match serde_json::from_value::<OperationStatus>(value) {
                Ok(next) => {
                    message.set(operation_text(&next));
                    if finished(&next.state) {
                        operation.set(None);
                        busy.set(false);
                        if next.state == "succeeded" {
                            result.set(if next.message.is_empty() {
                                "Package operation completed.".into()
                            } else {
                                next.message.clone()
                            });
                        } else {
                            error.set(if next.message.is_empty() {
                                "The package operation did not finish.".into()
                            } else {
                                next.message
                            });
                        }
                        load_catalog(app, catalog, message, error);
                    }
                }
                Err(_) => {
                    error.set("The remote sent an unreadable package-operation update.".into())
                }
            },
            Err(next) => {
                if next.unauthorized {
                    app.paired.set(Some(false));
                }
                if next.stale {
                    operation.set(None);
                    busy.set(false);
                    error.set("The package operation is no longer available after the remote restarted. Review installed packages, then try the operation again.".into());
                    load_catalog(app, catalog, message, error);
                } else {
                    error.set(next.message);
                }
            }
        }
        polling.set(false);
    });
}

pub fn screen(app: App) -> AnyView {
    let catalog = RwSignal::new(Catalog::default());
    let busy = RwSignal::new(false);
    let operation = RwSignal::new(None::<String>);
    let polling = RwSignal::new(false);
    let message = RwSignal::new("Loading integration catalog…".to_string());
    let error = RwSignal::new(String::new());
    let result = RwSignal::new(String::new());
    let pending = RwSignal::new(None::<PendingRepository>);
    let recovery = RwSignal::new(RecoveryStatus::default());
    let repo_id = RwSignal::new(String::new());
    let repo_name = RwSignal::new(String::new());
    let repo_url = RwSignal::new(String::new());
    let public_key = RwSignal::new(String::new());
    let fingerprint_checked = RwSignal::new(false);

    load_catalog(app, catalog, message, error);
    load_recovery(app, recovery);
    restore_current_operation(app, catalog, busy, operation, message, error, result);
    let timer = set_interval_with_handle(
        move || {
            poll_operation(
                app, catalog, busy, operation, polling, message, error, result,
            )
        },
        std::time::Duration::from_millis(750),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });

    let refresh = move || {
        begin_operation(
            app,
            busy,
            operation,
            message,
            error,
            result,
            "/api/integrations/refresh".into(),
            json!({}),
        )
    };
    let stage_repository = move || {
        let id = repo_id.get_untracked().trim().to_string();
        let name = repo_name.get_untracked().trim().to_string();
        let url = repo_url.get_untracked().trim().to_string();
        let key = public_key.get_untracked().trim().to_string();
        if id.is_empty() || name.is_empty() || url.is_empty() || key.is_empty() {
            error.set(
                "Enter a repository ID, name, URL and public key before checking its fingerprint."
                    .into(),
            );
            return;
        }
        error.set(String::new());
        message.set("Checking the pasted public key…".into());
        spawn_local(async move {
            match api::ha(
                "POST",
                "/api/integrations/repositories",
                Some(json!({
                    "id": id, "name": name, "url": url, "public_key": key,
                })),
            )
            .await
            {
                Ok(value) => match serde_json::from_value::<StagedRepository>(value) {
                    Ok(next) => {
                        fingerprint_checked.set(false);
                        pending.set(Some(next.pending_confirmation));
                        message
                            .set("Compare the fingerprint before trusting this repository.".into());
                    }
                    Err(_) => {
                        error.set("The remote did not return a public-key fingerprint.".into())
                    }
                },
                Err(next) => {
                    if next.unauthorized {
                        app.paired.set(Some(false));
                    }
                    error.set(next.message);
                }
            }
        });
    };
    let confirm_repository = move || {
        let Some(repository) = pending.get_untracked() else {
            return;
        };
        if !fingerprint_checked.get_untracked() {
            return;
        }
        error.set(String::new());
        spawn_local(async move {
            let path = format!("/api/integrations/repositories/{}/confirm", repository.id);
            match api::ha(
                "POST",
                &path,
                Some(json!({"fingerprint": repository.fingerprint})),
            )
            .await
            {
                Ok(_) => {
                    pending.set(None);
                    repo_id.set(String::new());
                    repo_name.set(String::new());
                    repo_url.set(String::new());
                    public_key.set(String::new());
                    result.set("Repository trusted. Refresh packages to load its catalog.".into());
                    load_catalog(app, catalog, message, error);
                }
                Err(next) => {
                    if next.unauthorized {
                        app.paired.set(Some(false));
                    }
                    error.set(next.message);
                }
            }
        });
    };

    view! {
        {ui::page_header(app, "Integrations", None)}
        <p class="lead">"Install signed packages from trusted repositories. Removing or changing a package keeps its saved connection settings so it can be set up again later."</p>
        {move || recovery.get().recovery.and_then(|snapshot| (!snapshot.integrations_active && snapshot.path.is_some()).then_some(view! {
            <section class="notice" role="alert"><strong>"Saved integration configuration found"</strong><p>"Couch kept configuration from an earlier runtime integration setup. Download a copy before any deliberate import; importing it replaces the current house configuration."</p><a href="/api/integrations/recovery/config" download="couch-integration-recovery.json">"Download saved integration configuration"</a></section>
        }))}
        <section class="card integration-operation">
            <h2>"Catalog"</h2>
            <p role="status" aria-live="polite">{move || message.get()}</p>
            <p role="status" aria-live="polite">{move || result.get()}</p>
            <p role="alert">{move || error.get()}</p>
            <button class="ghost" disabled=move || busy.get() on:click=move |_| refresh()>"Refresh packages"</button>
        </section>
        <section class="card">
            <h2>"Installed packages" <span class="count">{move || catalog.get().installed.len()}</span></h2>
            {move || catalog.get().installed.is_empty().then(|| view! { <p class="dim">"No external integration packages are installed. Connection settings already saved on the remote stay intact."</p> })}
            <div class="integration-grid">{move || catalog.get().installed.into_iter().map(|item| installed_card(app, item, busy, operation, message, error, result)).collect_view()}</div>
        </section>
        <section class="card">
            <h2>"Available packages" <span class="count">{move || catalog.get().available.len()}</span></h2>
            <div class="integration-grid">{move || catalog.get().available.into_iter().map(|item| available_card(app, item, busy, operation, message, error, result)).collect_view()}</div>
        </section>
        {super::integration_migrations::section(app, busy)}
        <section class="creation integration-repositories">
            <h2>"Trusted repositories"</h2>
            <p class="dim">"Official repositories are built in. To add a custom repository, paste its public signing key from a source you trust; Couch does not fetch or trust a key automatically."</p>
            <div class="integration-grid">{move || catalog.get().repositories.into_iter().map(|repository| repository_card(app, repository, catalog, message, error)).collect_view()}</div>
            <h3>"Add a custom repository"</h3>
            <label class="field"><span class="label">"Repository ID"</span><input type="text" aria-label="Repository ID" placeholder="living-room-packages" prop:value=move || repo_id.get() on:input=move |event| repo_id.set(event_target_value(&event)) /></label>
            <label class="field"><span class="label">"Repository name"</span><input type="text" aria-label="Repository name" placeholder="Living room packages" prop:value=move || repo_name.get() on:input=move |event| repo_name.set(event_target_value(&event)) /></label>
            <label class="field"><span class="label">"Repository URL"</span><input type="url" aria-label="Repository URL" placeholder="https://packages.example.com/couch" prop:value=move || repo_url.get() on:input=move |event| repo_url.set(event_target_value(&event)) /></label>
            <label class="field"><span class="label">"Repository public key (PEM)"</span><textarea aria-label="Repository public key" rows="5" placeholder="-----BEGIN PUBLIC KEY-----" prop:value=move || public_key.get() on:input=move |event| public_key.set(event_target_value(&event))></textarea></label>
            <button class="primary" type="button" on:click=move |_| stage_repository()>"Check public-key fingerprint"</button>
            {move || pending.get().map(|repository| pending_confirmation(repository, fingerprint_checked, confirm_repository))}
        </section>
        <section class="card">
            <h2>"Developer installs"</h2>
            <p class="dim">"For local development, install a package over SSH with the Couch integration tooling. The web UI accepts only packages advertised by a trusted repository."</p>
        </section>
    }.into_any()
}

fn installed_card(
    app: App,
    item: Installed,
    busy: RwSignal<bool>,
    operation: RwSignal<Option<String>>,
    message: RwSignal<String>,
    error: RwSignal<String>,
    result: RwSignal<String>,
) -> AnyView {
    let id = item.id.clone();
    let repository = item.repository.clone().unwrap_or_default();
    let update = item.available_version.clone();
    let has_update = update.is_some();
    let update_version = update.unwrap_or_default();
    let install = move |action: &'static str| {
        begin_operation(
            app,
            busy,
            operation,
            message,
            error,
            result,
            format!("/api/integrations/{action}"),
            json!({"id": id, "repository": repository, "preserve_connection_config": true}),
        )
    };
    let update_action = install.clone();
    let rollback_action = install.clone();
    let remove_action = install;
    view! { <article class="card integration-card">
        <h3>{name(&item.name, &item.id)}</h3>
        <p class="mono">{format!("{} · {}", item.id, item.version)}</p>
        <p class="dim">{status(&item.status)}</p>
        {(!item.description.is_empty()).then(|| view! { <p>{item.description}</p> })}
        {item.connection_configured.then(|| view! { <p class="notice small">"Saved connection settings are retained."</p> })}
        <div class="actions">
            {has_update.then(|| view! { <button class="primary" disabled=move || busy.get() on:click=move |_| update_action("update")>{format!("Update to {update_version}")}</button> })}
            {item.can_rollback.then(|| view! { <button class="ghost" disabled=move || busy.get() on:click=move |_| rollback_action("rollback")>"Restore previous version"</button> })}
            {ui::danger_button("Remove package", move || remove_action("remove"))}
        </div>
    </article> }.into_any()
}

fn available_card(
    app: App,
    item: Available,
    busy: RwSignal<bool>,
    operation: RwSignal<Option<String>>,
    message: RwSignal<String>,
    error: RwSignal<String>,
    result: RwSignal<String>,
) -> AnyView {
    let id = item.id.clone();
    let repository = item.repository.unwrap_or_default();
    view! { <article class="card integration-card">
        <h3>{name(&item.name, &item.id)}</h3>
        <p class="mono">{format!("{} · {}", item.id, item.version)}</p>
        {(!item.description.is_empty()).then(|| view! { <p>{item.description}</p> })}
        <button class="primary" disabled=move || busy.get() on:click=move |_| begin_operation(app, busy, operation, message, error, result, "/api/integrations/install".into(), json!({"id": id, "repository": repository, "preserve_connection_config": true}))>"Install"</button>
    </article> }.into_any()
}

fn repository_card(
    app: App,
    repository: Repository,
    catalog: RwSignal<Catalog>,
    message: RwSignal<String>,
    error: RwSignal<String>,
) -> AnyView {
    let id = repository.id.clone();
    let official = repository.official;
    view! { <article class="card integration-card repository-card">
        <h3>{name(&repository.name, &repository.id)}</h3>
        <p class="mono">{repository.url}</p>
        <p class="mono">{repository.fingerprint}</p>
        <p class="dim">{if official { "Official repository" } else if repository.trusted { "Custom repository · trusted" } else { "Not trusted" }}</p>
        {(!official).then(|| ui::danger_button("Remove repository", move || {
            let id = id.clone();
            spawn_local(async move {
                match api::ha("DELETE", &format!("/api/integrations/repositories/{id}"), None).await {
                    Ok(_) => { message.set("Repository removed. Installed packages and their saved connections remain.".into()); load_catalog(app, catalog, message, error); }
                    Err(next) => { if next.unauthorized { app.paired.set(Some(false)); } error.set(next.message); }
                }
            });
        }))}
    </article> }.into_any()
}

fn pending_confirmation(
    repository: PendingRepository,
    checked: RwSignal<bool>,
    confirm: impl Fn() + 'static,
) -> AnyView {
    let name = name(&repository.name, &repository.id);
    let algorithm = if repository.algorithm.is_empty() {
        "SHA-256 (PEM)".to_string()
    } else {
        repository.algorithm
    };
    view! { <section class="notice integration-confirmation" role="status">
        <h3>"Confirm this public key"</h3>
        <p>{format!("{} · {}", name, repository.url)}</p>
        <p class="mono">{format!("{}: {}", algorithm, repository.fingerprint)}</p>
        <label><input type="checkbox" prop:checked=move || checked.get() on:change=move |event| checked.set(event_target_checked(&event))/>"I compared this fingerprint with the repository owner’s published fingerprint."</label>
        <button class="primary" type="button" disabled=move || !checked.get() on:click=move |_| confirm()>"Trust repository"</button>
    </section> }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_messages_do_not_hide_a_refresh_problem() {
        let mut catalog = Catalog::default();
        catalog.catalog_error = Some("offline".into());
        assert_eq!(
            catalog_message(&catalog),
            "Catalog refresh problem: offline"
        );
        catalog.catalog_error = None;
        catalog.available.push(Available::default());
        assert_eq!(
            catalog_message(&catalog),
            "1 package available from trusted repositories."
        );
    }

    #[test]
    fn only_terminal_operation_states_finish_polling() {
        assert!(finished("succeeded"));
        assert!(finished("failed"));
        assert!(!finished("running"));
    }
}
