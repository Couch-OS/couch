//! The handful of controls every screen is built from.
//!
//! Plain functions returning views rather than `#[component]`s: they are
//! called from inside closures that already own the data, and taking owned
//! values as arguments is less ceremony than a props struct for a two-line
//! widget.

use std::rc::Rc;

use couch_model::{Icon, ALL_ICONS};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::route::Route;
use crate::App;

/// A screen title, with an optional way back up the tree.
///
/// The browser's own Back does the same thing - the router pushes real history
/// entries - but a visible affordance is not optional on a phone in a kiosk-ish
/// context where the chrome may be hidden.
pub fn page_header(app: App, title: String, back: Option<Route>) -> AnyView {
    view! {
        <div class="page-head">
            {back.map(|route| view! {
                <button aria-label="Back to list" class="back" on:click=move |_| app.go(route.clone())>"‹"</button>
            })}
            <h1>{title}</h1>
        </div>
    }
    .into_any()
}

/// A text input that commits when the user leaves it or presses Enter.
///
/// Not on input: every commit is a round trip and an fsync on flash, so
/// per-keystroke saves would be both slow and a way to wear the partition out
/// spelling "Living room".
pub fn text_field(
    label: &'static str,
    value: String,
    placeholder: &'static str,
    commit: impl Fn(String) + 'static,
) -> AnyView {
    let original = value.clone();
    view! {
        <label class="field">
            <span class="label">{label}</span>
            <input
                type="text"
                value=value
                placeholder=placeholder
                on:change=move |ev| {
                    let next = event_target_value(&ev);
                    // A no-op edit would still bump the revision and write the
                    // file, which then invalidates anyone else's If-Match.
                    if next.trim() != original.trim() && !next.trim().is_empty() {
                        commit(next.trim().to_string());
                    }
                }
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        // Blur, and let the change handler above do the work.
                        let _ = ev.target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                            .map(|el| el.blur());
                    }
                }
            />
        </label>
    }
    .into_any()
}

/// A visual Lucide picker. Render a small page at a time, even for broad searches.
pub fn icon_select(current: Option<Icon>, commit: impl Fn(Option<Icon>) + 'static) -> AnyView {
    icon_select_signal(RwSignal::new(current), commit)
}
pub fn icon_select_signal(
    selected: RwSignal<Option<Icon>>,
    commit: impl Fn(Option<Icon>) + 'static,
) -> AnyView {
    let search = RwSignal::new(String::new());
    let limit = RwSignal::new(60usize);
    let commit = StoredValue::new_local(commit);
    let automatic = commit;
    let matches = move || {
        let query = search.get().to_lowercase();
        ALL_ICONS
            .iter()
            .copied()
            .filter(|icon| {
                query
                    .split_whitespace()
                    .all(|word| icon.name().contains(word))
            })
            .collect::<Vec<_>>()
    };
    view! {
        <details class="icon-picker">
            <summary><span class="label">"Icon"</span>
                <img class="icon-preview" src=move ||format!("/lucide/{}.svg", selected.get().unwrap_or_default().name()) alt=""/>
                <span>{move ||selected.get().map(|i|i.name().replace('-'," ")).unwrap_or("Automatic".into())}</span>
                <span class="dim">"Change"</span>
            </summary>
            <label class="field">"Search icons"<input type="search" placeholder="Search 2,077 Lucide icons" prop:value=move ||search.get()
                on:input=move |e|{search.set(event_target_value(&e));limit.set(60);}/></label>
            <button type="button" class="ghost" on:click=move |_|{selected.set(None);automatic.with_value(|f| f(None));}>"Use automatic icon"</button>
            <p class="dim" role="status">{move ||format!("{} icons", matches().len())}</p>
            <div class="icon-grid">{move ||matches().into_iter().take(limit.get()).map(|icon|{
                let choose=commit;let name=icon.name();
                view!{<button type="button" class="icon-option" title=name aria-label=format!("Use {name} icon")
                    aria-pressed=move ||(selected.get()==Some(icon)).to_string()
                    on:click=move |_|{selected.set(Some(icon));choose.with_value(|f| f(Some(icon)));}>
                    <img class="icon-preview" loading="lazy" src=format!("/lucide/{name}.svg") alt=""/>
                    <span>{name.replace('-'," ")}</span>
                </button>}
            }).collect_view()}</div>
            {move ||(matches().len()>limit.get()).then(||view!{<button type="button" class="ghost" on:click=move |_|limit.update(|n|*n+=60)>"Show more icons"</button>})}
        </details>
    }.into_any()
}

/// A destructive button that asks first.
///
/// Two taps on the same control rather than a modal: `window.confirm` blocks
/// the wasm event loop and looks like the browser, not the app, and a dialog
/// component is a lot of machinery for "are you sure".
pub fn danger_button(label: &'static str, confirm: impl Fn() + 'static) -> AnyView {
    confirm_button(label, "Confirm delete", confirm)
}

/// A button that asks once more: the first click arms it and shows
/// `armed_label`, the second within focus performs `confirm`; blurring
/// disarms. One control for deletes and for the remote's power actions.
pub fn confirm_button(
    label: &'static str,
    armed_label: &'static str,
    confirm: impl Fn() + 'static,
) -> AnyView {
    let armed = RwSignal::new(false);
    view! {
        <button
            type="button"
            class="danger"
            class:armed=move || armed.get()
            on:click=move |_| {
                if armed.get() {
                    armed.set(false);
                    confirm();
                } else {
                    armed.set(true);
                }
            }
            on:blur=move |_| armed.set(false)
        >
            {move || if armed.get() { armed_label.to_string() } else { label.to_string() }}
        </button>
    }
    .into_any()
}

/// A single-field "add a thing" row.
pub fn add_row(
    placeholder: &'static str,
    button: &'static str,
    submit: impl Fn(String) + 'static,
) -> AnyView {
    let draft = RwSignal::new(String::new());
    // Two handlers need the same callback, and a closure capturing a non-Clone
    // `submit` is not itself Clone.
    let submit: Rc<dyn Fn(String)> = Rc::new(submit);
    let on_enter = submit.clone();

    fn flush(draft: RwSignal<String>, submit: &dyn Fn(String)) {
        let name = draft.get().trim().to_string();
        if !name.is_empty() {
            submit(name);
        }
    }

    view! {
        <div class="add-row">
            <input
                type="text"
                placeholder=placeholder
                aria-label=placeholder
                prop:value=move || draft.get()
                on:input=move |ev| draft.set(event_target_value(&ev))
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        flush(draft, &*on_enter);
                    }
                }
            />
            <button class="primary" disabled=move || draft.get().trim().is_empty() on:click=move |_| flush(draft, &*submit)>{button}</button>
        </div>
    }
    .into_any()
}

/// A titled section: the heading and everything it introduces in one
/// bordered block, so a page of lists reads as a stack of blocks rather than
/// as one column of text.
///
/// The `.block` rules in style.css give the container its frame and the rhythm
/// between a heading, its intro line, its rows and its add-controls. Anything
/// that needs the same look without this helper can use the same markup:
/// `<section class="block"><h2 class="section">…</h2><p class="dim">…</p>…</section>`.
pub fn section(
    title: impl IntoView + 'static,
    intro: Option<&'static str>,
    body: AnyView,
) -> AnyView {
    view! {
        <section class="block">
            <h2 class="section">{title}</h2>
            {intro.map(|text| view! { <p class="dim">{text}</p> })}
            {body}
        </section>
    }
    .into_any()
}

/// An empty-state line, so a list that is legitimately empty does not read as
/// a page that failed to load.
pub fn empty(message: &'static str) -> AnyView {
    view! { <p class="dim empty">{message}</p> }.into_any()
}
