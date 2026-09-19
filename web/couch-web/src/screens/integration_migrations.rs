//! Connections saved while their integration was built into the OS.
//!
//! The remote hands such a connection to its package by itself (it installs
//! the package from the feed if it has to); this page only says where that
//! stands. Until it is done the connection reads "Needs the Denon package",
//! with the reason the last attempt gave and a way to try again now.
use crate::{api, App};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;

#[derive(Clone, Default, PartialEq, Deserialize)]
pub(super) struct Waiting {
    #[serde(default)]
    pub connections: Vec<WaitingConnection>,
    #[serde(default)]
    pub working: bool,
    #[serde(default)]
    pub retry_in_seconds: Option<u64>,
}

#[derive(Clone, Default, PartialEq, Deserialize)]
pub(super) struct WaitingConnection {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub package_name: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// When the remote will next try by itself, in words.
pub(super) fn retry_text(waiting: &Waiting) -> String {
    if waiting.working {
        return "Trying now…".into();
    }
    match waiting.retry_in_seconds {
        None | Some(0) => "The remote is about to try again.".into(),
        Some(seconds) if seconds < 90 => {
            format!("The remote tries again by itself in {seconds} s.")
        }
        Some(seconds) => format!(
            "The remote tries again by itself in about {} min.",
            (seconds + 30) / 60
        ),
    }
}

/// What one waiting connection says under its name.
pub(super) fn reason_text(connection: &WaitingConnection) -> String {
    match connection.reason.as_deref() {
        Some(reason) if !reason.is_empty() => format!("Last attempt: {reason}"),
        _ => "Couch installs the package from the package feed and switches this connection over by itself; rooms, activities and buttons stay attached.".into(),
    }
}

/// Keeps `waiting` current while the view that owns it is alive. A connection
/// that stops waiting was converted by the daemon, so the configuration this
/// page holds is out of date: reload it.
pub(super) fn watch(app: App, waiting: RwSignal<Waiting>, error: RwSignal<String>) {
    let load = move || {
        spawn_local(async move {
            match api::ha("GET", "/api/integrations/legacy", None).await {
                Ok(value) => {
                    let next: Waiting = serde_json::from_value(value).unwrap_or_default();
                    let before = waiting.get_untracked().connections.len();
                    let converted = next.connections.len() < before;
                    if waiting.get_untracked() != next {
                        waiting.set(next);
                    }
                    if converted {
                        if let Ok(config) = api::load().await {
                            app.config.set(Some(config));
                        }
                    }
                }
                Err(next) => {
                    if next.unauthorized {
                        app.paired.set(Some(false));
                    } else {
                        error.set(next.message);
                    }
                }
            }
        });
    };
    load();
    let timer = set_interval_with_handle(
        move || {
            // Nothing waits on most remotes; do not poll for them.
            if !waiting.get_untracked().connections.is_empty() {
                load();
            }
        },
        std::time::Duration::from_secs(2),
    )
    .ok();
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
    });
}

pub(super) fn try_again(app: App, waiting: RwSignal<Waiting>, error: RwSignal<String>) {
    error.set(String::new());
    waiting.update(|waiting| waiting.working = true);
    spawn_local(async move {
        if let Err(next) = api::ha("POST", "/api/integrations/legacy/retry", None).await {
            if next.unauthorized {
                app.paired.set(Some(false));
            }
            error.set(next.message);
        }
    });
}

/// The Integrations page: nothing at all unless something is waiting.
pub(super) fn section(app: App, busy: RwSignal<bool>) -> AnyView {
    let waiting = RwSignal::new(Waiting::default());
    let error = RwSignal::new(String::new());
    watch(app, waiting, error);
    view! {
        {move || (!waiting.get().connections.is_empty()).then(|| view! {
            <section class="card integration-legacy">
                <h2>"Connections waiting for a package"</h2>
                <p>"These connections were set up when their integration was part of Couch. It is now a package, and the remote installs it and switches them over by itself."</p>
                <p role="alert">{move || error.get()}</p>
                <div class="integration-grid">{move || waiting.get().connections.into_iter().map(|connection| {
                    let reason = reason_text(&connection);
                    view! { <article class="card integration-card">
                        <h3>{connection.name}</h3>
                        <p class="notice small">{connection.message}</p>
                        <p class="dim">{reason}</p>
                    </article> }
                }).collect_view()}</div>
                <p class="dim" role="status" aria-live="polite">{move || retry_text(&waiting.get())}</p>
                <button class="primary" disabled=move || busy.get() || waiting.get().working on:click=move |_| try_again(app, waiting, error)>"Try again"</button>
            </section>
        })}
    }
    .into_any()
}

/// The same state on the connection's own page and on its devices' cards.
pub(super) fn connection_notice(app: App, id: String, package_name: String) -> AnyView {
    let waiting = RwSignal::new(Waiting::default());
    let error = RwSignal::new(String::new());
    watch(app, waiting, error);
    let key = StoredValue::new(id);
    let mine = Memo::new(move |_| {
        waiting.with(|waiting| {
            key.with_value(|id| waiting.connections.iter().find(|c| &c.id == id).cloned())
        })
    });
    let heading = format!("Needs the {package_name} package");
    view! {
        <section class="card integration-legacy">
            <h2>{heading}</h2>
            <p>{format!("This connection was set up when {package_name} support was part of Couch. It is now an integration package. The remote installs it from the package feed and switches this connection over by itself; its rooms, activities and buttons stay attached and its address is carried over.")}</p>
            <p class="dim">{move || mine.get().map(|connection| reason_text(&connection))}</p>
            <p role="alert">{move || error.get()}</p>
            <p class="dim" role="status" aria-live="polite">{move || retry_text(&waiting.get())}</p>
            <button class="primary" disabled=move || waiting.get().working on:click=move |_| try_again(app, waiting, error)>"Try again"</button>
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_said_in_words_and_the_reason_is_never_hidden() {
        let mut waiting = Waiting::default();
        assert_eq!(retry_text(&waiting), "The remote is about to try again.");
        waiting.retry_in_seconds = Some(28);
        assert_eq!(
            retry_text(&waiting),
            "The remote tries again by itself in 28 s."
        );
        waiting.retry_in_seconds = Some(870);
        assert_eq!(
            retry_text(&waiting),
            "The remote tries again by itself in about 15 min."
        );
        waiting.working = true;
        assert_eq!(retry_text(&waiting), "Trying now…");
        let mut connection = WaitingConnection::default();
        assert!(reason_text(&connection).contains("by itself"));
        connection.reason = Some("cannot download repository index over HTTPS".into());
        assert_eq!(
            reason_text(&connection),
            "Last attempt: cannot download repository index over HTTPS"
        );
    }

    #[test]
    fn the_status_the_remote_sends_is_read() {
        let waiting: Waiting = serde_json::from_value(serde_json::json!({
            "connections":[{"id":"receiver","name":"Receiver","kind":"denon","package":"denon",
                "package_name":"Denon","message":"Needs the Denon package","reason":null}],
            "working":false,"retry_in_seconds":30}))
        .unwrap();
        assert_eq!(waiting.connections[0].message, "Needs the Denon package");
        assert_eq!(waiting.connections[0].reason, None);
        assert_eq!(waiting.retry_in_seconds, Some(30));
    }
}
