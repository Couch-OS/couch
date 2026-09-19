//! A packaged device on the core control screen.
//!
//! The screen - header, current source, tiles, the slide-up list - is the one
//! every device uses. What goes in it comes from what the package declares:
//! its status, its inputs and its commands. This module only asks the package
//! and says what it answered; like `tv_sonos`, all I/O runs on the bounded TV
//! worker, never on the UI thread.
use super::{Command, Details, Event, Work};
use couch_model::{Integration, PluginComponent, PluginStatusField};
use couch_plugin::{Request, Response, Status, VolumeDb};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Commands the remote's own keys send (activity_buttons' row bindings), and
/// the discrete power pair the Power tile stands for. None of them is listed
/// under Commands.
const KEYED: &[&str] = &[
    "volume-up",
    "volume-down",
    "mute",
    "mute-on",
    "mute-off",
    "power-on",
    "power-off",
];

/// What a packaged device puts on the screen, from its declaration alone.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Layout {
    pub kind: String,
    pub power: bool,
    pub inputs: bool,
    /// (function id, label) for the Commands list.
    pub commands: Vec<(String, String)>,
}

pub(crate) fn layout(config: &couch_model::Config, device: &couch_model::Device) -> Option<Layout> {
    let Integration::Plugin {
        connection_id,
        capabilities,
        supports_inputs,
        presentation,
        ..
    } = config.resolve_integration(&device.integration)?
    else {
        return None;
    };
    let has = |id: &str| capabilities.iter().any(|c| c.id == id);
    let toggles_power = presentation.iter().any(|component| {
        matches!(component, PluginComponent::Toggle { state, .. } if *state == PluginStatusField::On)
    });
    Some(Layout {
        kind: config
            .connection(&connection_id)
            .map(|c| c.provider.label().to_uppercase())
            .unwrap_or_default(),
        power: toggles_power || has("power-on") && has("power-off"),
        inputs: supports_inputs,
        commands: capabilities
            .iter()
            .filter(|c| !KEYED.contains(&c.id.as_str()))
            .map(|c| (c.id.clone(), c.label.clone()))
            .collect(),
    })
}

fn ask(connection: &str, request: Request) -> Result<Response, String> {
    couch_plugin::local_request(
        &crate::home::path("plugin.sock"),
        connection,
        request,
        couch_plugin::REQUEST_TIMEOUT + Duration::from_secs(1),
    )
    .map_err(|e| e.to_string())
}

/// The function a screen action means for this device, given its last status.
fn function(action: &Command, status: Option<&Status>) -> Result<Option<String>, String> {
    Ok(Some(match action {
        Command::Retry => return Ok(None),
        Command::Power => match status.and_then(|s| s.on) {
            Some(true) => "power-off".into(),
            Some(false) => "power-on".into(),
            None => return Err("This device has not said whether it is on".into()),
        },
        Command::Wake => "power-on".into(),
        Command::Volume(true) => "volume-up".into(),
        Command::Volume(false) => "volume-down".into(),
        Command::ToggleMute => "mute".into(),
        Command::Mute(true) => "mute-on".into(),
        Command::Mute(false) => "mute-off".into(),
        Command::Play(true) => "play".into(),
        Command::Play(false) => "pause".into(),
        Command::Next(true) => "next".into(),
        Command::Next(false) => "previous".into(),
        Command::Stop => "stop".into(),
        Command::Input(id) => format!("input:{id}"),
        Command::Function(id) => id.clone(),
        _ => return Err("This control is not available for this device".into()),
    }))
}

/// "−39.5 dB", "37%", "Muted", or nothing the device reports.
pub(crate) fn level(status: &Status) -> String {
    if status.muted == Some(true) {
        return "Muted".into();
    }
    match (&status.volume_db, status.volume) {
        (Some(VolumeDb::Reading { tenths }), _) => format!("{:.1} dB", f32::from(*tenths) / 10.0),
        (Some(VolumeDb::Minimum), _) => "Minimum".into(),
        (None, Some(percent)) => format!("{percent}%"),
        (None, None) => String::new(),
    }
}

/// What one status reading puts in the header and on the big line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Shown {
    /// Under the device's name: "Off" wins, then whether it is playing, then
    /// "On", and "Connected" for a device that reports none of them.
    pub status: &'static str,
    /// The Power tile: "On", "Off", or nothing the device reports.
    pub power: &'static str,
    /// The big line: what is playing when the device says, else its input.
    pub source: String,
    /// The Input tile's line when a title has taken the input's place.
    pub input: String,
}

pub(crate) fn shown(status: &Status, inputs: &[couch_plugin::Selectable]) -> Shown {
    let input = status
        .input
        .as_deref()
        .map(|id| {
            inputs
                .iter()
                .find(|input| input.id == id)
                .map_or_else(|| id.to_owned(), |input| input.name.clone())
        })
        .unwrap_or_default();
    let power = match status.on {
        Some(true) => "On",
        Some(false) => "Off",
        None => "",
    };
    let title = status.title.as_deref().map(str::trim).unwrap_or_default();
    Shown {
        status: match (status.on, status.playing) {
            // A device that is off is not playing, whatever it last said.
            (Some(false), _) => "Off",
            (_, Some(true)) => "Playing",
            (_, Some(false)) => "Paused",
            (Some(true), None) => "On",
            (None, None) => "Connected",
        },
        power,
        source: if title.is_empty() {
            input.clone()
        } else {
            title.to_owned()
        },
        input: if title.is_empty() {
            String::new()
        } else {
            input
        },
    }
}

pub(super) fn run(work: &Work, active: &AtomicU64) -> Result<Option<Event>, String> {
    let current =
        || super::infrared::request_current(work, active, crate::connections::config().as_ref());
    if !current() {
        return Ok(None);
    }
    let id = work
        .connection
        .strip_prefix("plugin:")
        .ok_or("Missing device")?;
    let config = work.config.as_ref().ok_or("Configuration unavailable")?;
    let (_, device) = config
        .devices()
        .find(|(_, d)| d.id.as_str() == id)
        .ok_or("Device was removed")?;
    let Some(Integration::Plugin {
        connection_id,
        supports_inputs,
        ..
    }) = config.resolve_integration(&device.integration)
    else {
        return Err("Selected device is no longer a packaged integration".into());
    };
    let connection = connection_id.as_str();
    let read = || match ask(connection, Request::Status)? {
        Response::Status { status } => Ok(status),
        Response::Error { code } => Err(code.to_string()),
        _ => Err("The integration returned an invalid status".to_string()),
    };
    // Power is decided against what the device says now, not what the screen
    // last showed.
    let before = matches!(work.action, Command::Power)
        .then(|| read())
        .transpose()?;
    if let Some(function) = function(&work.action, before.as_ref())? {
        if !current() {
            return Ok(None);
        }
        match ask(connection, Request::Command { function })? {
            Response::Ok => {}
            Response::Error { code } => return Err(code.to_string()),
            _ => return Err("The integration returned an invalid response".into()),
        }
    }
    let status = read()?;
    let inputs = if supports_inputs {
        match ask(connection, Request::Inputs) {
            Ok(Response::Inputs { inputs }) => inputs,
            _ => vec![],
        }
    } else {
        vec![]
    };
    if active.load(std::sync::atomic::Ordering::SeqCst) != work.generation
        || !crate::connections::config().is_some_and(|c| std::sync::Arc::ptr_eq(&c, config))
    {
        return Ok(None);
    }
    let shown = shown(&status, &inputs);
    let mut choices: Vec<(String, String, String)> = inputs
        .iter()
        .map(|input| {
            (
                format!("input:{}", input.id),
                input.name.clone(),
                if status.input.as_deref() == Some(input.id.as_str()) {
                    "Current input".into()
                } else {
                    String::new()
                },
            )
        })
        .collect();
    if let Some(layout) = layout(config, device) {
        choices.extend(
            layout
                .commands
                .into_iter()
                .map(|(id, label)| (format!("fn:{id}"), label, String::new())),
        );
    }
    Ok(Some(Event {
        generation: work.generation,
        // The header says whether it is on or playing; the level has the line
        // under the title or input to itself, and the volume card keeps that
        // line fresh.
        status: Ok(shown.status.into()),
        details: Some(Details {
            source: shown.source,
            input: shown.input,
            sound: shown.power.into(),
            picture: level(&status),
            choices,
            ..Details::default()
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denon() -> couch_model::Config {
        serde_json::from_value(serde_json::json!({
            "schema_version":1,
            "connections":[{"id":"theater-avr","name":"Theater AVR","provider":{
                "kind":"plugin","id":"denon","label":"Denon AVR","supports_inputs":true,
                "capabilities":[
                    {"id":"power-on","label":"Main zone on"},{"id":"power-off","label":"Main zone off"},
                    {"id":"volume-up","label":"Volume up (0.5 dB)"},{"id":"volume-down","label":"Volume down (0.5 dB)"},
                    {"id":"mute","label":"Mute"},{"id":"mute-on","label":"Mute on"},{"id":"mute-off","label":"Mute off"}],
                "presentation":[
                    {"kind":"toggle","label":"Power","state":"on","on":"power-on","off":"power-off"},
                    {"kind":"status_text","label":"Volume","field":"volume_db"},
                    {"kind":"volume_db_control","label":"Set volume"},
                    {"kind":"input_selector","label":"Input"}],
                "actions":[{"action":"set_volume_db","min_tenths":-800,"max_tenths":180,"step_tenths":5}]}}],
            "rooms":[{"id":"theater","name":"Theater","devices":[
                {"id":"avr","name":"Theater AVR","kind":"speaker",
                 "integration":{"via":"connection","connection_id":"theater-avr","resource_id":""}}]}]
        }))
        .unwrap()
    }

    #[test]
    fn a_packaged_receiver_declares_power_and_inputs_and_keeps_keyed_commands_off_screen() {
        let config = denon();
        let (_, device) = config.devices().next().unwrap();
        let declared = layout(&config, device).unwrap();
        assert_eq!(declared.kind, "DENON AVR");
        assert!(declared.power && declared.inputs);
        // Volume, mute and the power pair belong to the remote's keys and the
        // Power tile: nothing is left for a Commands list.
        assert!(declared.commands.is_empty());
        assert_eq!(
            super::super::resolve_target(Some(&config), "device:avr").unwrap(),
            ("plugin:avr".into(), Some("avr".into()))
        );
        assert_eq!(
            crate::lights::tv_connection(&config, "avr").as_deref(),
            Some("plugin:avr")
        );
    }

    #[test]
    fn power_is_decided_from_the_observed_state_and_never_guessed() {
        let status = |json: serde_json::Value| -> Status { serde_json::from_value(json).unwrap() };
        let on = status(serde_json::json!({"on":true}));
        let off = status(serde_json::json!({"on":false}));
        assert_eq!(
            function(&Command::Power, Some(&on)).unwrap().as_deref(),
            Some("power-off")
        );
        assert_eq!(
            function(&Command::Power, Some(&off)).unwrap().as_deref(),
            Some("power-on")
        );
        assert!(function(&Command::Power, Some(&status(serde_json::json!({})))).is_err());
        assert!(function(&Command::Power, None).is_err());
        assert_eq!(
            function(&Command::Input("SAT/CBL".into()), None)
                .unwrap()
                .as_deref(),
            Some("input:SAT/CBL")
        );
        assert_eq!(
            function(&Command::Function("sound-mode-movie".into()), None)
                .unwrap()
                .as_deref(),
            Some("sound-mode-movie")
        );
        assert_eq!(function(&Command::Retry, None).unwrap(), None);
        assert!(function(&Command::Channel(true), None).is_err());
        assert_eq!(
            level(&status(
                serde_json::json!({"volume_db":{"kind":"reading","tenths":-395}})
            )),
            "-39.5 dB"
        );
        assert_eq!(
            level(&status(
                serde_json::json!({"muted":true,"volume_db":{"kind":"reading","tenths":-395}})
            )),
            "Muted"
        );
        assert_eq!(level(&status(serde_json::json!({"volume":37}))), "37%");
        assert_eq!(level(&status(serde_json::json!({}))), "");
    }

    fn reading(json: serde_json::Value) -> Status {
        serde_json::from_value(json).unwrap()
    }

    fn receiver_inputs() -> Vec<couch_plugin::Selectable> {
        vec![
            couch_plugin::Selectable::new("BD", "CoreELEC"),
            couch_plugin::Selectable::new("NET", "Network"),
        ]
    }

    #[test]
    fn the_header_says_playing_or_paused_and_a_title_takes_the_big_line() {
        // A receiver that reports neither: exactly what the screen said before.
        let denon = shown(
            &reading(serde_json::json!({"on":true,"input":"BD",
                "volume_db":{"kind":"reading","tenths":-395}})),
            &receiver_inputs(),
        );
        assert_eq!(
            denon,
            Shown {
                status: "On",
                power: "On",
                source: "CoreELEC".into(),
                input: String::new(),
            }
        );
        assert_eq!(
            shown(&reading(serde_json::json!({})), &[]).status,
            "Connected"
        );
        assert_eq!(
            shown(&reading(serde_json::json!({"input":"AUX"})), &[]).source,
            "AUX",
            "an input the device did not list is shown by its ID"
        );
        // Kodi: no power, no inputs, a title while something is loaded.
        let kodi = shown(
            &reading(serde_json::json!({"volume":64,"muted":false,"playing":true,
                "title":"Breaking Bad S5E14 - Ozymandias"})),
            &[],
        );
        assert_eq!(
            (
                kodi.status,
                kodi.power,
                kodi.source.as_str(),
                kodi.input.as_str()
            ),
            ("Playing", "", "Breaking Bad S5E14 - Ozymandias", "")
        );
        // Sonos 0.1.0: whether it plays, and no title.
        let sonos = shown(
            &reading(serde_json::json!({"volume":22,"muted":false,"playing":false})),
            &[],
        );
        assert_eq!(
            (sonos.status, sonos.source.as_str(), sonos.input.as_str()),
            ("Paused", "", "")
        );
        // Both: the title has the big line and the Input tile keeps the input.
        let streaming = shown(
            &reading(
                serde_json::json!({"on":true,"input":"NET","playing":true,"title":"  Blue in Green  "}),
            ),
            &receiver_inputs(),
        );
        assert_eq!(
            (
                streaming.status,
                streaming.power,
                streaming.source.as_str(),
                streaming.input.as_str()
            ),
            ("Playing", "On", "Blue in Green", "Network")
        );
        // Off wins over a stale play state; an empty title is no title.
        let off = shown(
            &reading(serde_json::json!({"on":false,"input":"BD","playing":true,"title":" "})),
            &receiver_inputs(),
        );
        assert_eq!(
            (
                off.status,
                off.power,
                off.source.as_str(),
                off.input.as_str()
            ),
            ("Off", "Off", "CoreELEC", "")
        );
    }

    #[test]
    fn the_core_screen_shows_a_receiver_and_its_keys_move_between_tiles_and_rows() {
        if std::env::var_os("COUCH_TEST_CORE_SCREEN").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tv::plugin::tests::the_core_screen_shows_a_receiver_and_its_keys_move_between_tiles_and_rows",
                ])
                .env("COUCH_TEST_CORE_SCREEN", "1")
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
        use slint::{platform::WindowEvent, ComponentHandle, Model};
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let received = actions.clone();
        app.on_tv_action(move |name| received.borrow_mut().push(name.to_string()));
        // One status reading, put on the screen the way the controller does.
        let fill = |kind: &str,
                    (power, input, command): (bool, bool, bool),
                    name: &str,
                    status: serde_json::Value| {
            let status = reading(status);
            let shown = shown(&status, &receiver_inputs());
            app.set_tv_shown(false);
            slint::platform::update_timers_and_animations();
            app.set_tv_generic(true);
            app.set_tv_kind_label(kind.into());
            app.set_tv_can_power(power);
            app.set_tv_can_input(input);
            app.set_tv_can_command(command);
            app.set_tv_title(name.into());
            app.set_tv_status(shown.status.into());
            app.set_tv_source(shown.source.into());
            app.set_tv_input(shown.input.into());
            app.set_tv_sound(
                if shown.power.is_empty() {
                    "Unavailable"
                } else {
                    shown.power
                }
                .into(),
            );
            app.set_tv_picture(level(&status).into());
            app.set_tv_shown(true);
            slint::platform::update_timers_and_animations();
        };
        // A receiver reports neither a title nor whether it plays: its screen
        // is the one it always had.
        fill(
            "DENON AVR",
            (true, true, false),
            "Theater AVR",
            serde_json::json!({"on":true,"input":"BD","volume_db":{"kind":"reading","tenths":-395}}),
        );
        assert_eq!(
            (
                app.get_tv_status().as_str(),
                app.get_tv_source().as_str(),
                app.get_tv_input().as_str(),
                app.get_tv_sound().as_str(),
                app.get_tv_picture().as_str()
            ),
            ("On", "CoreELEC", "", "On", "-39.5 dB")
        );
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        app.invoke_focus_tv();
        let shot = |name: &str| {
            slint::platform::update_timers_and_animations();
            let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            if let Some(dir) = std::env::var_os("COUCH_CORE_SCREENSHOTS") {
                let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                let path = std::path::Path::new(&dir).join(name);
                image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
            }
        };
        let key = |key: slint::platform::Key| {
            let text = slint::SharedString::from(char::from(key));
            window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(WindowEvent::KeyReleased { text });
        };
        shot("core-1-power.png");
        // OK on the first tile is Power; Right moves to Input; OK opens its list.
        key(slint::platform::Key::Return);
        key(slint::platform::Key::RightArrow);
        key(slint::platform::Key::Return);
        assert_eq!(&*actions.borrow(), &["power", "inputs"]);
        shot("core-2-input.png");
        // The list, as the controller would fill it, with the D-pad choosing.
        let rows = [
            ("BD", "CoreELEC", "Current input"),
            ("TV", "TV Audio", ""),
            ("SAT/CBL", "Fios", ""),
        ];
        app.set_tv_choices(slint::ModelRc::new(slint::VecModel::from(
            rows.iter()
                .map(|(id, title, detail)| crate::TvChoice {
                    action: format!("input:{id}").into(),
                    title: (*title).into(),
                    detail: (*detail).into(),
                })
                .collect::<Vec<_>>(),
        )));
        app.set_tv_tray_selected(0);
        app.set_tv_panel(1);
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        key(slint::platform::Key::DownArrow);
        key(slint::platform::Key::DownArrow);
        key(slint::platform::Key::DownArrow);
        assert_eq!(app.get_tv_tray_selected(), 2, "stops at the last row");
        shot("core-3-tray.png");
        key(slint::platform::Key::Return);
        assert_eq!(
            actions.borrow().last().map(String::as_str),
            Some("input:SAT/CBL")
        );
        key(slint::platform::Key::Escape);
        assert_eq!(actions.borrow().last().map(String::as_str), Some("dismiss"));
        assert_eq!(app.get_tv_choices().row_count(), 3);
        app.set_tv_panel(0);
        key(slint::platform::Key::Escape);
        assert_eq!(actions.borrow().last().map(String::as_str), Some("close"));
        // What a package says is playing: the header has the state, the big
        // line the title.
        fill(
            "KODI",
            (false, false, true),
            "Living room Kodi",
            serde_json::json!({"volume":64,"muted":false,"playing":true,
                "title":"Breaking Bad S5E14 - Ozymandias"}),
        );
        assert_eq!(app.get_tv_status(), "Playing");
        assert_eq!(app.get_tv_source(), "Breaking Bad S5E14 - Ozymandias");
        // Let the input list finish sliding away before the first picture.
        for _ in 0..30 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        shot("core-4-playing-title.png");
        fill(
            "SONOS",
            (false, false, true),
            "Kitchen",
            serde_json::json!({"volume":22,"muted":false,"playing":false}),
        );
        assert_eq!(app.get_tv_status(), "Paused");
        assert_eq!(app.get_tv_source(), "");
        shot("core-5-paused-no-title.png");
        // A title longer than two lines ends in an ellipsis, and the Input
        // tile still names the input.
        fill(
            "DENON AVR",
            (true, true, false),
            "Theater AVR",
            serde_json::json!({"on":true,"input":"NET","playing":true,
                "volume_db":{"kind":"reading","tenths":-395},
                "title":"The Lord of the Rings: The Fellowship of the Ring (Extended Edition)"}),
        );
        assert_eq!(app.get_tv_input(), "Network");
        shot("core-6-title-and-input.png");
        app.hide().unwrap();
    }
}
