//! Sonos I/O runs on the bounded TV worker; no network access on the UI thread.
use super::{Command, Details, Event, Work};
use std::sync::atomic::AtomicU64;
fn function(action: &Command) -> Result<Option<&'static str>, String> {
    Ok(Some(match action {
        Command::Retry => return Ok(None),
        Command::Play(true) => "play",
        Command::Play(false) => "pause",
        Command::Next(true) => "next",
        Command::Next(false) => "previous",
        Command::Stop => "stop",
        Command::Volume(true) => "volume-up",
        Command::Volume(false) => "volume-down",
        Command::ToggleMute => "mute",
        Command::Mute(true) => "mute-on",
        Command::Mute(false) => "mute-off",
        _ => return Err("This control is not available for Sonos".into()),
    }))
}
pub(super) fn run(work: &Work, active: &AtomicU64) -> Result<Option<Event>, String> {
    let current =
        || super::infrared::request_current(work, active, crate::connections::config().as_ref());
    if !current() {
        return Ok(None);
    }
    let id = work
        .connection
        .strip_prefix("sonos:")
        .ok_or("Missing Sonos device")?;
    let config = work.config.as_ref().ok_or("Configuration unavailable")?;
    let (_, device) = config
        .devices()
        .find(|(_, d)| d.id.as_str() == id)
        .ok_or("Sonos device was removed")?;
    let Some(couch_model::Integration::Sonos { host }) =
        config.resolve_integration(&device.integration)
    else {
        return Err("Selected device is no longer Sonos".into());
    };
    let action = function(&work.action)?;
    let client =
        couch_sonos::Client::connect(host.parse().map_err(|_| "Invalid Sonos IPv4 address")?)
            .map_err(|e| e.to_string())?;
    if !current() {
        return Ok(None);
    }
    if let Some(command) = action {
        match client.command_if_current(command, &current) {
            Ok(()) => {}
            Err(couch_sonos::Error::Cancelled) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        }
    }
    let state = client.status().map_err(|e| {
        if action.is_some() {
            format!("Command acknowledged; refresh failed: {e}")
        } else {
            e.to_string()
        }
    })?;
    // Display may arrive after the key's dispatch deadline. Generation/config still
    // must match, but successful command observations are not silently discarded.
    if active.load(std::sync::atomic::Ordering::SeqCst) != work.generation
        || !crate::connections::config().is_some_and(|c| std::sync::Arc::ptr_eq(&c, config))
    {
        return Ok(None);
    }
    let own = state.coordinator == state.player.uuid;
    Ok(Some(Event {
        generation: work.generation,
        status: Ok(format!(
            "Volume {}{} · {}",
            state.volume,
            if state.muted { " · Muted" } else { "" },
            if own {
                "Group playback"
            } else {
                "Member · volume only"
            }
        )),
        details: Some(Details {
            source: state.transport.replace('_', " "),
            sound: if own {
                "Playback affects this group".into()
            } else {
                format!("Select coordinator {}", state.coordinator)
            },
            ..Details::default()
        }),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sonos_screen_renders_and_touch_controls_dispatch() {
        if std::env::var_os("COUCH_TEST_SONOS_SCREEN").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tv::sonos::tests::sonos_screen_renders_and_touch_controls_dispatch",
                ])
                .env("COUCH_TEST_SONOS_SCREEN", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{
            platform::{PointerEventButton, WindowEvent},
            ComponentHandle,
        };
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_tv_action(move |name| received.borrow_mut().push(name.to_string()));
        app.set_tv_shown(true);
        app.set_tv_sonos(true);
        app.set_tv_title("Living room Sonos".into());
        app.set_tv_status("Volume 25 · Group playback".into());
        app.set_tv_source("PLAYING".into());
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        app.invoke_focus_tv();
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        if let Some(path) = std::env::var_os("COUCH_SONOS_SCREENSHOT") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
        }
        for (x, y) in [
            (84., 454.),
            (186., 454.),
            (288., 454.),
            (390., 454.),
            (100., 560.),
            (300., 560.),
            (100., 690.),
            (300., 690.),
        ] {
            let position = slint::LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        }
        assert_eq!(
            &*actions.borrow(),
            &[
                "previous",
                "play",
                "pause",
                "next",
                "volume-down",
                "volume-up",
                "toggle-mute",
                "retry",
            ]
        );
        app.hide().unwrap();
    }
    #[test]
    fn selected_sonos_device_resolves_to_its_own_target() {
        let config:couch_model::Config=serde_json::from_value(serde_json::json!({
            "schema_version":1,"connections":[
                {"id":"a","name":"A","provider":{"kind":"sonos","host":"192.0.2.1"}},
                {"id":"b","name":"B","provider":{"kind":"sonos","host":"192.0.2.2"}}],
            "rooms":[{"id":"room","name":"Room","devices":[
                {"id":"speaker-a","name":"A","kind":"speaker","integration":{"via":"connection","connection_id":"a"}},
                {"id":"speaker-b","name":"B","kind":"speaker","integration":{"via":"connection","connection_id":"b"}}]}]
        })).unwrap();
        assert_eq!(
            super::super::resolve_target(Some(&config), "device:speaker-a").unwrap(),
            ("sonos:speaker-a".into(), Some("speaker-a".into()))
        );
        assert_eq!(
            super::super::resolve_target(Some(&config), "device:speaker-b").unwrap(),
            ("sonos:speaker-b".into(), Some("speaker-b".into()))
        );
    }
    #[test]
    fn sonos_controls_do_not_invent_tv_commands() {
        assert_eq!(function(&Command::Volume(true)).unwrap(), Some("volume-up"));
        assert_eq!(function(&Command::ToggleMute).unwrap(), Some("mute"));
        assert_eq!(function(&Command::Next(false)).unwrap(), Some("previous"));
        assert_eq!(function(&Command::Retry).unwrap(), None);
        assert!(function(&Command::Power).is_err());
        assert!(function(&Command::Input("hdmi1".into())).is_err());
    }
}
