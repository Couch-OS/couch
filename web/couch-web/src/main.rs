//! The Couch config UI.
//!
//! A client-side Leptos app, served by `couch-confd` off the remote itself.
//! It shares `couch-model` with that daemon, so the shapes it edits are the
//! same types the device stores - the pickers for icons and device kinds are
//! built from the model's own lists rather than from a copy that can go stale.
//!
//! Two things shape the whole design:
//!
//! * **The config is one signal.** Every mutating call answers with the
//!   complete document, so there is no local patching and no way for the screen
//!   to disagree with the file. A house is a few KB; this is cheaper than the
//!   bugs the alternative buys.
//!
//! * **Edits commit on `change`, not on every keystroke.** Each commit is an
//!   fsync and a rename on a phone flash partition, and a per-character write
//!   would be both slow and pointless.

mod api;
mod route;
mod screens;
mod ui;

use std::future::Future;

use couch_model::Config;
use leptos::prelude::*;
use leptos::task::spawn_local;

use api::ApiError;
use route::{Route, Router};

/// Everything the screens share, passed by context.
///
/// `Copy` because Leptos signals are handles into an arena, so this can be
/// captured by every closure in the tree without a clone dance.
#[derive(Clone, Copy)]
pub struct App {
    pub config: RwSignal<Option<Config>>,
    pub error: RwSignal<Option<String>>,
    pub busy: RwSignal<bool>,
    pub router: Router,
    pub activity_sequence: RwSignal<bool>,
    pub activity_device: RwSignal<String>,
    pub activity_search: RwSignal<String>,
    pub activity_tab: RwSignal<String>,
    pub button_selection: RwSignal<(couch_model::buttons::Button,couch_model::buttons::Gesture)>,
    pub device_source: RwSignal<String>,
    pub ir_device: RwSignal<String>,
    pub device_filter: RwSignal<String>,
    pub hue_category: RwSignal<String>,
    pub hue_room_filter: RwSignal<String>,
    /// `None` until the first status call answers, so the app shows neither the
    /// house nor a PIN box while it does not yet know which is right.
    pub paired: RwSignal<Option<bool>>,
}

impl App {
    /// Run one API call: mark the app busy, then either adopt the config it
    /// returns or show why it did not happen.
    ///
    /// Nothing is applied optimistically. On a device where a write can fail
    /// for real - a full partition, a config another client just changed -
    /// showing the edit and taking it back is worse than a half-second wait.
    pub fn run<F>(&self, call: F)
    where
        F: Future<Output = Result<Config, ApiError>> + 'static,
    {
        if self.busy.get_untracked() {
            return;
        }
        let (config, error, busy, paired) = (self.config, self.error, self.busy, self.paired);
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            match call.await {
                Ok(next) => config.set(Some(next)),
                Err(e) if e.unauthorized => {
                    // Not an edit that failed - the session went away. Sending
                    // them to the PIN box says what to do; an error banner over
                    // a stale house does not.
                    paired.set(Some(false));
                }
                Err(e) => {
                    // A failed validation or network request must leave the
                    // local device draft intact. Reload only stale references.
                    if e.stale {
                        error.set(Some(format!("{} Your saved configuration has been reloaded. Review it before trying again.", e.message)));
                        if let Ok(fresh) = api::load().await {
                            config.set(Some(fresh));
                        }
                    } else {
                        error.set(Some(e.message));
                    }
                }
            }
            busy.set(false);
        });
    }

    pub fn go(&self, route: Route) {
        self.router.go(route);
    }
}

fn main() {
    install_panic_hook();
    mount_to_body(Shell);
}

/// A panic in wasm is otherwise an "unreachable executed" with no location.
/// Hooking it costs nothing at runtime and is the difference between a
/// debuggable report from someone's phone and a shrug.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&format!("couch-web panic: {info}").into());
    }));
}

#[component]
fn Shell() -> impl IntoView {
    let app = App {
        config: RwSignal::new(None),
        error: RwSignal::new(None),
        busy: RwSignal::new(false),
        router: Router::install(),
        activity_sequence: RwSignal::new(true),
        activity_device: RwSignal::new(String::new()),
        activity_search: RwSignal::new(String::new()),
        activity_tab: RwSignal::new("setup".into()),
        button_selection: RwSignal::new((couch_model::buttons::Button::VolumeUp,couch_model::buttons::Gesture::Short)),
        device_source: RwSignal::new(String::new()),
        ir_device: RwSignal::new(String::new()),
        device_filter: RwSignal::new(String::new()),
        hue_category: RwSignal::new("lights".into()),
        hue_room_filter: RwSignal::new(String::new()),
        paired: RwSignal::new(None),
    };
    provide_context(app);

    // Ask whether this browser is already paired before anything else. A
    // session lives a week, so the common case is that it is and the PIN box
    // never appears.
    spawn_local(async move {
        let known = api::auth_status()
            .await
            .map(|s| s.authenticated)
            .unwrap_or(false);
        app.paired.set(Some(known));
        if known {
            app.run(api::load());
        }
    });

    let route = app.router.current;

    view! {
        <header class="bar">
            <a class="brand" href="/">"couch."</a>
            <span class="spacer"></span>
            <span class="status" role="status" aria-live="polite">
                {move || if app.busy.get() { "Saving…" } else if app.error.get().is_some() { "Not saved" } else if app.config.get().is_some() { "Saved" } else { "" }}
            </span>
            <Show when=move || app.paired.get() == Some(true)>
                <button
                    class="link"
                    on:click=move |_| spawn_local(async move {
                        let _ = api::log_out().await;
                        app.config.set(None);
                        app.paired.set(Some(false));
                    })
                >
                    "Unpair"
                </button>
            </Show>
        </header>

        <main>
            {move || (app.paired.get() == Some(true)).then(|| screens::updates::notification(app))}
            // Inside main rather than over it: a fixed banner covered the
            // heading of whatever screen raised the error, which is the one
            // thing on the page that says what the message is about. It is
            // sticky instead, so it still follows a list being scrolled.
            {move || app.error.get().map(|message| view! {
                <div class="banner" role="alert">
                    <span>{message}</span>
                    <button class="link" on:click=move |_| app.error.set(None)>"dismiss"</button>
                </div>
            })}

            {move || match app.paired.get() {
                None => view! { <p class="dim pad">"Connecting…"</p> }.into_any(),
                Some(false) => view! { <Pair/> }.into_any(),
                Some(true) => match app.config.get() {
                    None => view! { <p class="dim pad">"Loading your configuration…"</p> }.into_any(),
                    Some(config) => view! { <fieldset class="editor" disabled=move || app.busy.get()>{screens::render(app, &config, route.get())}</fieldset> }.into_any(),
                },
            }}
        </main>

        <nav aria-label="Configuration" class="tabs" class:hidden=move || app.paired.get() != Some(true)>
            {[
                (Route::Overview, "Overview"),
                (Route::Rooms, "Rooms & devices"),
                (Route::Connections, "Connections"),
                (Route::Settings, "Remote settings"),
                (Route::Updates, "Updates"),
                (Route::Areas, "Areas"),
                (Route::Activities, "Activities"),
            ]
                .into_iter()
                .map(|(target, label)| {
                    let for_class = target.clone();
                    let for_click = target.clone();
                    view! {
                        <button
                            class="tab"
                            class:on=move || route.get().tab() == for_class
                            on:click=move |_| app.go(for_click.clone())
                        >
                            {label}
                        </button>
                    }
                })
                .collect_view()}
        </nav>
    }
}

/// The pairing screen.
///
/// Asking for a challenge is what puts the PIN on the remote, so it happens
/// when this screen mounts rather than behind a button: by the time anyone has
/// read this far, the digits are already up.
#[component]
fn Pair() -> impl IntoView {
    let app = expect_context::<App>();
    let pin = RwSignal::new(String::new());
    let message = RwSignal::new(Option::<String>::None);
    let asking = RwSignal::new(true);
    let status = RwSignal::new(api::AuthStatus::default());

    let ask = move || {
        asking.set(true);
        message.set(None);
        spawn_local(async move {
            match api::auth_challenge().await {
                // --no-auth: there is nothing to pair with, so do not sit here
                // asking for a PIN that will never appear.
                Ok(s) if s.disabled => {
                    app.paired.set(Some(true));
                    app.run(api::load());
                }
                Ok(s) => status.set(s),
                Err(e) => message.set(Some(e.message)),
            }
            asking.set(false);
        });
    };
    ask();

    let submit = move || {
        let offered = pin.get_untracked().trim().to_string();
        if offered.len() != 4 {
            message.set(Some("Four digits.".into()));
            return;
        }
        spawn_local(async move {
            match api::auth_verify(offered).await {
                Ok(result) if result.paired => {
                    pin.set(String::new());
                    message.set(None);
                    app.paired.set(Some(true));
                    app.run(api::load());
                }
                Ok(result) => {
                    pin.set(String::new());
                    message.set(Some(result.message));
                    // The count comes back from the daemon, which is the only
                    // thing that knows how many of these a PIN has left.
                    if let Ok(s) = api::auth_status().await {
                        status.set(s);
                    }
                }
                Err(e) => message.set(Some(e.message)),
            }
        });
    };

    view! {
        <section class="pair">
            <h1>"Look at your remote"</h1>
            <p class="dim">
                "It is showing four digits. Type them here to let this browser \
                 edit your house."
            </p>

            <input
                class="pin"
                type="text"
                inputmode="numeric"
                autocomplete="off"
                maxlength="4"
                placeholder="••••"
                prop:value=move || pin.get()
                on:input=move |e| {
                    // Digits only, so a stray character cannot make a
                    // four-character entry that can never match.
                    let cleaned: String =
                        event_target_value(&e).chars().filter(char::is_ascii_digit).take(4).collect();
                    pin.set(cleaned);
                }
                on:keydown=move |e| if e.key() == "Enter" { submit() }
            />

            <button class="primary" on:click=move |_| submit()>"Pair"</button>

            {move || message.get().map(|m| view! { <p class="wrong" role="alert">{m}</p> })}

            <p class="dim small">
                {move || {
                    let s = status.get();
                    if asking.get() {
                        "Asking the remote for a PIN…".to_string()
                    } else if !s.pairing {
                        "No PIN on the screen? Wake the remote and ask again.".to_string()
                    } else if s.expires_in > 0 {
                        format!("It stops working in about {} seconds.", s.expires_in)
                    } else {
                        "That PIN has expired.".to_string()
                    }
                }}
            </p>
            {move || {
                let left = status.get().tries_left;
                // Only worth saying once some have been spent: a fresh PIN
                // announcing five attempts reads as a challenge.
                (status.get().pairing && left < 5).then(|| view! {
                    <p class="dim small">
                        {match left {
                            0 => "Ask for a new PIN.".to_string(),
                            1 => "One try left, then you will need a new PIN.".to_string(),
                            n => format!("{n} tries left."),
                        }}
                    </p>
                })
            }}
            <button class="link" on:click=move |_| ask()>"Show a new PIN"</button>
        </section>
    }
}
