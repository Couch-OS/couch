//! Scene details opened from rooms or areas.
//!
//! Steps have no ids of their own, so the list is keyed by position. Editing a
//! step's command leaves every key alone and only that row's memo changes;
//! removing one shifts the rows below it, which is the one case here that
//! still rebuilds anything.

use couch_model::{Action, Id, Scene};
use leptos::prelude::*;

use crate::route::Route;
use crate::screens::{device_label, device_select, gone};
use crate::{api, ui, App};

pub fn detail(app: App, id: Id) -> AnyView {
    let scene = app.scene(id.clone());
    view! {
        <Show
            when=move || scene.with(Option::is_some)
            fallback=move || gone(app, "That scene has been deleted.")
        >
            {page(app, id.clone())}
        </Show>
    }
    .into_any()
}

/// Nothing here may read a slice while it is being built - see [`super::rooms`].
fn page(app: App, id: Id) -> AnyView {
    let scene = app.scene(id.clone());
    let key = StoredValue::new(id);
    let save = move |next: Scene| app.run(api::put(format!("/api/scenes/{}", next.id), next));
    // The scene as it is now, read on the click rather than captured: every
    // handler here sends a whole scene back.
    let base = move || scene.get_untracked();
    let name = Memo::new(move |_| scene.with(|s| s.as_ref().map(|s| s.name.clone())));
    let icon = RwSignal::new(scene.with_untracked(|s| s.as_ref().and_then(|s| s.icon)));
    let steps =
        Memo::new(move |_| scene.with(|s| s.as_ref().map(|s| s.steps.clone()).unwrap_or_default()));
    // Positions, not steps: editing a command leaves the list alone and only
    // that row's own memo changes.
    let positions = Memo::new(move |_| (0..steps.with(Vec::len)).collect::<Vec<_>>());
    // Back goes to the scene's first room, which assigning it to another room
    // changes, so the header follows it.
    let home = Memo::new(move |_| {
        scene
            .with(|s| s.as_ref().and_then(|s| s.rooms.first().cloned()))
            .map(Route::Room)
            .unwrap_or(Route::Rooms)
    });

    view! {
        {move || ui::page_header(app, name.get(), Some(home.get()))}

        <section class="card">
            {move || ui::text_field("Name", name.get().unwrap_or_default(), "Movie night", move |name| {
                if let Some(scene) = base() { save(Scene { name, ..scene }) }
            })}
            {ui::icon_select_signal(icon, move |icon| {
                if let Some(scene) = base() { save(Scene { icon, ..scene }) }
            })}
        </section>

        {room_assignment(app, scene)}
        {move || scene.get().and_then(|s|s.hue).map(|_|view!{<section class="card"><h2>"Hue scene"</h2><p>"This recalls the scene saved on your bridge. Edit its lighting in the Hue app."</p></section>})}
        // Protocol 3 (unreleased): a scene a packaged integration keeps. Like
        // a Hue scene it is recalled, not built here, so it has no steps.
        {move || scene.get().and_then(|s|s.resource).map(|_|view!{<section class="card"><h2>"Integration scene"</h2><p>"This recalls a scene saved on the integration itself. Edit what it does in that integration's own app."</p></section>})}
        <div hidden=move || scene.get().is_some_and(|s| s.hue.is_some() || s.resource.is_some())>
        {ui::section(
            "Device commands",
            Some("Add commands in the order they should run. For example, turn on the TV, then select its input. Command names depend on the integration; saving does not test or send them."),
            view! {
        {move || steps.with(Vec::is_empty).then(|| ui::empty("This scene does nothing yet. Add a step below."))}
        <ul class="rows">
            <For each=move ||positions.get() key=|index| *index
                children=move |index| step_row(app, steps, index, move |next| {
                    let Some(mut scene) = base() else { return };
                    match next {
                        Some(action) => scene.steps[index] = action,
                        None => { scene.steps.remove(index); }
                    }
                    save(scene);
                })/>
        </ul>

        {add_step(app, move |action| {
            let Some(mut scene) = base() else { return };
            scene.steps.push(action);
            save(scene);
        })}
            }
            .into_any(),
        )}
        </div>
        <div class="pad"><button class="ghost" on:click=move |_| app.go(Route::Areas)>"Add this scene to an area →"</button></div>
        <div class="pad">
            {ui::danger_button("Delete this scene", move || {
                app.go(home.get_untracked());
                app.run(api::delete(format!("/api/scenes/{}", key.get_value())));
            })}
        </div>
    }
    .into_any()
}

/// One step. `commit(None)` removes it.
fn step_row(
    app: App,
    steps: Memo<Vec<Action>>,
    index: usize,
    commit: impl Fn(Option<Action>) + Clone + Send + Sync + 'static,
) -> AnyView {
    let step = Memo::new(move |_| steps.with(|steps| steps.get(index).cloned()));
    let (for_command, for_delete) = (commit.clone(), commit);

    view! {
        <li class="row step">
            <span class="row-title">{move || step.get().map(|step| device_label(app, &step.device))}</span>
            {move || step.get().map(|base| view! {
                <input
                    class="command"
                    aria-label="Device command"
                    type="text"
                    value=base.command.clone()
                    placeholder="on"
                    on:change={
                        let for_command = for_command.clone();
                        move |ev| {
                            let command = event_target_value(&ev).trim().to_string();
                            if !command.is_empty() && command != base.command {
                                for_command(Some(Action { command, ..base.clone() }));
                            }
                        }
                    }
                />
            })}
            <button class="ghost" on:click=move |_| for_delete(None)>"Remove"</button>
        </li>
    }
    .into_any()
}

/// Picking a device is what adds the step; the command starts at "on" and is
/// edited in place, which is one interaction rather than a form.
fn add_step(app: App, commit: impl Fn(Action) + Clone + Send + Sync + 'static) -> AnyView {
    view! {
        {move || {
            if app.devices.with(Vec::is_empty) {
                return ui::empty("Add a device to a room before building a scene.");
            }
            // Tracked so a device added or renamed elsewhere shows up here; the
            // document itself is only read for the room grouping.
            app.rooms.track();
            let commit = commit.clone();
            view! {
                <div class="add-row">
                    <span class="label">"Add a step"</span>
                    {device_select(&app.house(), None, true, false, move |picked| {
                        if let Some(device) = picked {
                            commit(Action::new(device, "on"));
                        }
                    })}

                </div>
            }
            .into_any()
        }}
    }
    .into_any()
}

fn room_assignment(app: App, scene: Memo<Option<Scene>>) -> AnyView {
    let order = super::ids(app.rooms, |r| &r.id);
    view!{<section class="card"><h2>"Show in rooms"</h2><p>"Selected rooms get this scene in their bottom Scenes button. Home screen scene selection stays in Areas."</p>
        <For each=move ||order.get() key=|id|id.clone() children=move |id|{
            let room=app.room(id.clone());let checked=id.clone();
            view!{<label class="room-assignment"><input type="checkbox" prop:checked=move ||scene.get().is_some_and(|s|s.rooms.contains(&checked)) on:change=move |e|{let Some(mut next)=scene.get_untracked() else{return};if event_target_checked(&e){if !next.rooms.contains(&id){next.rooms.push(id.clone());}}else{next.rooms.retain(|r|r!=&id);}app.run(api::put(format!("/api/scenes/{}",next.id),next));}/>{move ||room.get().map(|r|r.name)}</label>}
        }/>
    </section>}.into_any()
}
