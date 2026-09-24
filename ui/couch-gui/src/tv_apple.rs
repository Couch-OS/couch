//! Companion controls on the shared TV screen. No inferred playback or power state.
use super::{android::current, Button, Command, Details, Event, Work};
use couch_control::{StreamingConnection, StreamingTv};
use couch_model::{commands::Function, Integration};
use std::sync::atomic::{AtomicU64, Ordering};

fn function(action: &Command) -> Result<Option<String>, String> {
    Ok(Some(match action {
        Command::Retry => return Ok(None),
        Command::Key(key) => match key {
            Button::Up => "up",
            Button::Down => "down",
            Button::Left => "left",
            Button::Right => "right",
            Button::Enter => "ok",
            Button::Back | Button::Menu => "back",
            Button::Home => "home",
            _ => return Err("Apple TV does not support color keys".into()),
        }
        .into(),
        Command::Volume(true) => "volume-up".into(),
        Command::Volume(false) => "volume-down".into(),
        Command::Channel(true) => "channel-up".into(),
        Command::Channel(false) => "channel-down".into(),
        // Companion has explicit sleep/wake, but no verified state subscription
        // here. Power is deliberately Sleep; never guess an on/off toggle.
        Command::Power => "power-off".into(),
        Command::Wake => "power-on".into(),
        Command::Play(true) => "play".into(),
        Command::Play(false) => "pause".into(),
        Command::Next(true) => "next".into(),
        Command::Next(false) => "previous".into(),
        Command::App(id) if Function::App(id.clone()).supports(&Integration::AppleTv) => {
            format!("app:{id}")
        }
        _ => return Err("This control is not supported by Apple TV Companion".into()),
    }))
}
pub(super) fn refresh(client: &StreamingTv, generation: u64) -> Result<Event, String> {
    client.status().map_err(|e| e.to_string())?;
    Ok(Event {
        generation,
        details: None,
        status: Ok("Connected · Companion".into()),
    })
}
fn details(value: &serde_json::Value) -> Details {
    Details {
        source: "Apple TV".into(),
        choices: value["apps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|app| {
                let id = app["id"].as_str()?;
                let title = app["title"].as_str()?;
                Function::App(id.into())
                    .supports(&Integration::AppleTv)
                    .then(|| (format!("app:{id}"), title.into(), "Open on Apple TV".into()))
            })
            .collect(),
        ..Details::default()
    }
}
pub(super) fn run(
    client: &mut Option<StreamingTv>,
    work: &Work,
    active: &AtomicU64,
) -> Result<Option<Event>, String> {
    if !current(work, active) {
        return Ok(None);
    }
    let action = match function(&work.action) {
        Ok(action) => action,
        Err(error) => {
            return Ok(Some(Event {
                generation: work.generation,
                details: None,
                status: Err(error),
            }))
        }
    };
    if client.is_none() || matches!(work.action, Command::Retry) {
        let settings =
            StreamingConnection::load(&crate::connections::file(&work.connection, "appletv"))
                .map_err(|_| "Pair this Apple TV in Connections first".to_string())?;
        if settings.kind() != "appletv" {
            return Err("TV credentials have the wrong provider".into());
        }
        *client = Some(StreamingTv::connect(&settings).map_err(|e| e.to_string())?);
    }
    if !current(work, active) {
        if active.load(Ordering::SeqCst) != work.generation {
            *client = None;
        }
        return Ok(None);
    }
    let c = client.as_ref().unwrap();
    if let Some(action) = action {
        c.command(&action).map_err(|e| e.to_string())?;
        return Ok(Some(Event {
            generation: work.generation,
            details: None,
            status: Ok(match work.action {
                Command::Power => "Sleep requested",
                Command::Wake => "Wake requested",
                Command::App(_) => "App launch requested",
                _ => "",
            }
            .into()),
        }));
    }
    c.status().map_err(|e| e.to_string())?;
    let apps = c.apps();
    if !current(work, active) {
        return Ok(None);
    }
    Ok(Some(Event {
        generation: work.generation,
        details: Some(details(apps.as_ref().unwrap_or(&serde_json::Value::Null))),
        status: Ok(if apps.is_ok() {
            "Connected · Companion"
        } else {
            "Connected · app list unavailable"
        }
        .into()),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apple_screen_takes_physical_keys_and_exposes_only_supported_touch_actions() {
        if std::env::var_os("COUCH_TEST_APPLE_SCREEN").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tv::apple::tests::apple_screen_takes_physical_keys_and_exposes_only_supported_touch_actions"])
                .env("COUCH_TEST_APPLE_SCREEN", "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use slint::{
            platform::{Key, PointerEventButton, WindowEvent},
            ComponentHandle,
        };
        use std::{cell::RefCell, rc::Rc};
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_tv_action(move |action| received.borrow_mut().push(action.to_string()));
        app.set_tv_apple(true);
        app.set_tv_shown(true);
        app.set_tv_title("Living room Apple TV".into());
        app.set_tv_source("Apple TV".into());
        app.set_tv_status("Connected · Companion".into());
        app.show().unwrap();
        app.invoke_focus_tv();
        for key in [
            Key::UpArrow,
            Key::Return,
            Key::Escape,
            Key::Home,
            Key::F13,
            Key::F23,
            Key::F21,
            Key::F14,
            Key::F15,
            Key::F16,
            Key::F17,
            Key::F18,
        ] {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: char::from(key).to_string().into(),
            });
        }
        assert_eq!(
            &*actions.borrow(),
            &[
                "up",
                "ok",
                "back",
                "home",
                "power",
                "volume-up",
                "channel-up"
            ]
        );
        actions.borrow_mut().clear();
        for (x, y) in [
            (84., 456.),
            (186., 456.),
            (288., 456.),
            (390., 456.),
            (100., 570.),
            (350., 570.),
        ] {
            window.dispatch_event(WindowEvent::PointerPressed {
                position: slint::LogicalPosition::new(x, y),
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: PointerEventButton::Left,
            });
        }
        assert_eq!(
            &*actions.borrow(),
            &["previous", "play", "pause", "next", "wake", "apps"]
        );
        if let Some(path) = std::env::var_os("COUCH_APPLE_SCREENSHOT") {
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
                renderer.render(&mut pixels, 480);
                let bytes: Vec<u8> = pixels.into_iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    std::path::Path::new(&path),
                    &bytes,
                    480,
                    800,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            });
        }
    }
    #[test]
    fn only_supported_commands_reach_companion() {
        for (input, expected) in [
            ("ok", "ok"),
            ("back", "back"),
            ("home", "home"),
            ("menu", "back"),
            ("power", "power-off"),
            ("wake", "power-on"),
            ("volume-up", "volume-up"),
            ("channel-down", "channel-down"),
            ("previous", "previous"),
            ("next", "next"),
            ("play", "play"),
            ("pause", "pause"),
            ("app:com.example.video", "app:com.example.video"),
        ] {
            assert_eq!(
                function(&super::super::command(input).unwrap())
                    .unwrap()
                    .as_deref(),
                Some(expected)
            );
        }
        for input in [
            "toggle-mute",
            "mute",
            "stop",
            "rewind",
            "forward",
            "red",
            "input:HDMI_1",
            "app:bad id",
        ] {
            assert!(function(&super::super::command(input).unwrap()).is_err());
        }
    }
    #[test]
    fn app_choices_use_validated_ids_without_now_playing_fields() {
        let d = details(
            &serde_json::json!({"apps":[{"id":"com.example.video","title":"Video"},{"id":"bad id","title":"Bad"}]}),
        );
        assert_eq!(d.source, "Apple TV");
        assert_eq!(
            d.choices,
            vec![(
                "app:com.example.video".into(),
                "Video".into(),
                "Open on Apple TV".into()
            )]
        );
        assert!(d.sound.is_empty() && d.picture.is_empty());
    }
}
