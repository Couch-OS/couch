use crate::{api, App};
use couch_model::{Appearance, Config};
use leptos::prelude::*;

pub fn editor(app: App, config: &Config) -> AnyView {
    let saved = config.appearance.accent.clone();
    let draft = RwSignal::new(saved.clone());
    let error = RwSignal::new(String::new());
    let valid = move || {
        Appearance {
            accent: draft.get(),
        }
        .rgb()
        .is_some()
    };
    let preview = move || {
        if valid() {
            draft.get()
        } else {
            Appearance::default().accent
        }
    };
    view!{<section class="card appearance"><h2>"Appearance"</h2><p>"Choose the accent for your physical remote’s focus outlines, headings and controls."</p>
        <div class="accent-presets">{[("White","#FFFFFF"),("Purple","#B794F4"),("Teal","#4FD1C5"),("Blue","#63B3ED"),("Rose","#F687B3"),("Orange","#E8703A")].into_iter().map(|(name,color)|view!{<button type="button" class="ghost" aria-label=format!("{name} accent") aria-pressed=move ||draft.get().eq_ignore_ascii_case(color) on:click=move |_|draft.set(color.into())><span class="color-swatch" style:background=color></span>{name}</button>}).collect_view()}</div>
        <label class="field"><span class="label">"Custom color"</span><input type="color" aria-label="Custom color" prop:value=preview on:input=move |e|draft.set(event_target_value(&e))/></label>
        {super::connections::field("Hex color",draft,"#FFFFFF")}
        <div class="accent-preview" style:color=preview style:border-color=preview><span>"REMOTE PREVIEW"</span><strong>"Living room"</strong><span>"Selected control"</span></div>
        <p class="dim">"Use a bright color to keep focus outlines visible. Changes apply after saving and persist after reboot."</p>
        <p role="alert">{move ||error.get()}</p><div class="actions"><button class="primary" disabled=move ||app.busy.get() on:click=move |_|{
            let value=Appearance{accent:draft.get_untracked().trim().to_uppercase()};if value.rgb().is_none(){error.set("Use a color in #RRGGBB format".into());return}error.set(String::new());app.run(api::put("/api/appearance",value));
        }>"Save appearance"</button><button class="ghost" on:click=move |_|{draft.set(saved.clone());error.set(String::new());}>"Discard color changes"</button></div>
    </section>}.into_any()
}
