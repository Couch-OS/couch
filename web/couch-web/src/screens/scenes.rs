//! Scene details opened from rooms or areas.

use couch_model::{Action, Config, Icon, Id, Scene};
use leptos::prelude::*;

use crate::route::Route;
use crate::screens::{device_name, device_select, gone};
use crate::{api, ui, App};

pub fn detail(app: App, config: &Config, id: &Id) -> AnyView {
    let Some(scene) = config.scene(id) else {
        return gone(app, "That scene has been deleted.");
    };
    let id = id.clone();

    let save = {
        let id = id.clone();
        move |next: Scene| app.run(api::put(format!("/api/scenes/{id}"), next))
    };
    let base = scene.clone();
    let (for_name, for_icon, for_steps, for_add) =
        (save.clone(), save.clone(), save.clone(), save.clone());
    let (name_base, icon_base, steps_base, add_base) =
        (base.clone(), base.clone(), base.clone(), base.clone());
    let delete_id = id.clone();
    let back = scene
        .rooms
        .first()
        .cloned()
        .map(Route::Room)
        .unwrap_or(Route::Rooms);
    let after_delete = back.clone();

    view! {
        {ui::page_header(app, scene.name.clone(), Some(back))}

        <section class="card">
            {ui::text_field("Name", scene.name.clone(), "Movie night", move |name| {
                for_name(Scene { name, ..name_base.clone() })
            })}
            {ui::icon_select(scene.icon, move |icon: Option<Icon>| {
                for_icon(Scene { icon, ..icon_base.clone() })
            })}
        </section>

        {room_assignment(app,config,scene)}
        {scene.hue.as_ref().map(|_|view!{<section class="card"><h2>"Hue scene"</h2><p>"This recalls the scene saved on your bridge. Edit its lighting in the Hue app."</p></section>})}
        <div hidden=scene.hue.is_some()>
        {ui::section(
            "Device commands",
            Some("Add commands in the order they should run. For example, turn on the TV, then select its input. Command names depend on the integration; saving does not test or send them."),
            view! {
        {scene.steps.is_empty().then(|| {
            ui::empty("This scene does nothing yet. Add a step below.")
        })}
        <ul class="rows">
            {scene.steps
                .iter()
                .enumerate()
                .map(|(index, step)| {
                    let commit = for_steps.clone();
                    let base = steps_base.clone();
                    step_row(config, step, move |next| {
                        let mut scene = base.clone();
                        match next {
                            Some(action) => scene.steps[index] = action,
                            None => {
                                scene.steps.remove(index);
                            }
                        }
                        commit(scene);
                    })
                })
                .collect_view()}
        </ul>

        {add_step(config, move |action| {
            let mut scene = add_base.clone();
            scene.steps.push(action);
            for_add(scene);
        })}
            }
            .into_any(),
        )}
        </div>
        <div class="pad"><button class="ghost" on:click=move |_| app.go(Route::Areas)>"Add this scene to an area →"</button></div>
        <div class="pad">
            {ui::danger_button("Delete this scene", move || {
                app.go(after_delete.clone());
                app.run(api::delete(format!("/api/scenes/{delete_id}")));
            })}
        </div>
    }
    .into_any()
}

/// One step. `commit(None)` removes it.
fn step_row(
    config: &Config,
    step: &Action,
    commit: impl Fn(Option<Action>) + Clone + 'static,
) -> AnyView {
    let label = device_name(config, &step.device);
    let (for_command, for_delete) = (commit.clone(), commit);
    let base = step.clone();

    view! {
        <li class="row step">
            <span class="row-title">{label}</span>
            <input
                class="command"
                aria-label="Device command"
                type="text"
                value=step.command.clone()
                placeholder="on"
                on:change=move |ev| {
                    let command = event_target_value(&ev).trim().to_string();
                    if !command.is_empty() && command != base.command {
                        for_command(Some(Action { command, ..base.clone() }));
                    }
                }
            />
            <button class="ghost" on:click=move |_| for_delete(None)>"Remove"</button>
        </li>
    }
    .into_any()
}

/// Picking a device is what adds the step; the command starts at "on" and is
/// edited in place, which is one interaction rather than a form.
fn add_step(config: &Config, commit: impl Fn(Action) + 'static) -> AnyView {
    if config.devices().next().is_none() {
        return ui::empty("Add a device to a room before building a scene.");
    }
    view! {
        <div class="add-row">
            <span class="label">"Add a step"</span>
            {device_select(config, None, true, false, move |picked| {
                if let Some(device) = picked {
                    commit(Action::new(device, "on"));
                }
            })}

        </div>
    }
    .into_any()
}

fn room_assignment(app: App, config: &Config, scene: &Scene) -> AnyView {
    let scene = scene.clone();
    view!{<section class="card"><h2>"Show in rooms"</h2><p>"Selected rooms get this scene in their bottom Scenes button. Home screen scene selection stays in Areas."</p>
        {config.rooms.iter().map(|r|{let id=r.id.clone();let name=r.name.clone();let base=scene.clone();let checked=scene.rooms.contains(&id);view!{<label class="room-assignment"><input type="checkbox" checked=checked on:change=move |e|{let mut next=base.clone();if event_target_checked(&e){if !next.rooms.contains(&id){next.rooms.push(id.clone());}}else{next.rooms.retain(|r|r!=&id);}app.run(api::put(format!("/api/scenes/{}",next.id),next));}/>{name}</label>}}).collect_view()}
    </section>}.into_any()
}
