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
use couch_webos::Button;
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

/// A television-profile package receives these from the panel's native
/// transport row and physical keys. They must not be duplicated in Commands.
const TV_KEYED: &[&str] = &[
    "up",
    "down",
    "left",
    "right",
    "ok",
    "back",
    "home",
    "menu",
    "red",
    "green",
    "yellow",
    "blue",
    "channel-up",
    "channel-down",
    "play",
    "pause",
    "stop",
    "rewind",
    "fast-forward",
];

/// The complete standard vocabulary that makes a package a television rather
/// than a receiver with a few cursor commands. This is deliberately derived
/// from already-released manifest data: adding a new protocol-3 presentation
/// tag would make the package unreadable by existing protocol-3 cores.
const TELEVISION_PROFILE: &[&str] = &[
    "power-off",
    "volume-up",
    "volume-down",
    "mute",
    "up",
    "down",
    "left",
    "right",
    "ok",
    "back",
    "home",
    "play",
    "pause",
    "stop",
    "rewind",
    "fast-forward",
];

/// What a packaged device puts on the screen, from its declaration alone.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Layout {
    pub kind: String,
    pub television: bool,
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
    let television = supports_inputs
        && presentation
            .iter()
            .any(|component| matches!(component, PluginComponent::InputSelector { .. }))
        && TELEVISION_PROFILE.iter().all(|id| has(id));
    Some(Layout {
        kind: config
            .connection(&connection_id)
            .map(|c| c.provider.label().to_uppercase())
            .unwrap_or_default(),
        television,
        power: toggles_power
            || has("power-on") && has("power-off")
            || television && has("power-off"),
        inputs: supports_inputs,
        commands: capabilities
            .iter()
            .filter(|c| {
                !KEYED.contains(&c.id.as_str())
                    && !(television && TV_KEYED.contains(&c.id.as_str()))
            })
            .map(|c| (c.id.clone(), c.label.clone()))
            .collect(),
    })
}

/// The panel does no pairing of its own (T3): this is the whole of what it
/// offers instead, on the toast, the packaged control screen's status line
/// and a packaged light row's detail.
pub(crate) const PAIRING_HINT: &str = "Open Couch in a browser to pair";

/// What the panel says when a packaged device's request fails: Couch's own
/// sentence for the code, and under it the package's line when it gave one
/// (protocol 3; the host has already held it to 160 bytes of printable text).
/// The sentence comes first because it is the part that is always there, and
/// the part a one-line surface keeps.
///
/// `Unpaired` is the one code that never shows the package's own line:
/// rendering the sentence, the package's line and [`PAIRING_HINT`] together
/// takes three lines, which do not fit the 72px toast bar (checked by
/// rendering it - the card grows past its own border rather than wrapping).
/// Two lines do, so `Unpaired` keeps Couch's sentence and the hint, the same
/// pair on every surface that shows it.
pub(crate) fn refusal(failure: &couch_plugin::Failure) -> String {
    if failure.code == couch_plugin::Error::Unpaired {
        return format!("{}\n{PAIRING_HINT}", failure.code);
    }
    let said = failure
        .reason
        .as_ref()
        .map(|reason| reason.text().trim())
        .filter(|text| !text.is_empty());
    match said {
        Some(text) => format!("{}\n{text}", failure.code),
        None => failure.code.to_string(),
    }
}

/// One request to the daemon's panel socket. A refusal is an `Err`, with its
/// reason; `Ok` is never `Response::Error`.
pub(crate) fn ask_detailed(
    connection: &str,
    request: Request,
) -> Result<Response, couch_plugin::Failure> {
    ask_within(
        connection,
        request,
        couch_plugin::REQUEST_TIMEOUT + Duration::from_secs(1),
    )
}

/// [`ask_detailed`] with a deadline of its own: the room list gives a status
/// read far less than a command's, because a room full of rows is read one
/// after another on one worker.
pub(crate) fn ask_within(
    connection: &str,
    request: Request,
    timeout: Duration,
) -> Result<Response, couch_plugin::Failure> {
    couch_plugin::local_request_detailed(
        &crate::home::path("plugin.sock"),
        connection,
        request,
        timeout,
    )
}

/// Which child of its connection a packaged device is, if it is one.
///
/// A device that *is* the connection - a receiver, a TV - names nothing, and
/// every frame sent for it is the one a protocol 1 or 2 package has always
/// read, byte for byte. A saved `resource_id` alone does not make a child:
/// `zone1` on a receiver is part of the connection's own settings.
pub(crate) fn resource(integration: &Integration) -> Option<&str> {
    match integration {
        Integration::Plugin {
            resource_id,
            child: Some(_),
            ..
        } => Some(resource_id),
        _ => None,
    }
}

/// `request`, aimed at the child this device is; unchanged for a device that
/// is the connection itself.
pub(crate) fn aimed(request: Request, integration: &Integration) -> Request {
    match resource(integration) {
        Some(child) => request.at(child),
        None => request,
    }
}

/// One request for a packaged device, aimed at the child it is.
///
/// The connection is taken from the device's own resolved integration, so a
/// child's name can never be carried to another connection's package.
pub(crate) fn ask_device(
    integration: &Integration,
    request: Request,
) -> Result<Response, couch_plugin::Failure> {
    let Integration::Plugin { connection_id, .. } = integration else {
        return Err(couch_plugin::Error::Invalid.into());
    };
    ask_detailed(connection_id.as_str(), aimed(request, integration))
}

/// The function a screen action means for this device, given its last status.
fn function(
    action: &Command,
    status: Option<&Status>,
    capabilities: &[couch_model::PluginCapability],
) -> Result<Option<String>, String> {
    let has = |id: &str| capabilities.iter().any(|capability| capability.id == id);
    let function = match action {
        Command::Retry => return Ok(None),
        Command::Power => match status.and_then(|s| s.on) {
            Some(true) if has("power-off") => "power-off",
            Some(false) if has("power-on") => "power-on",
            Some(false) => return Err("This device does not support power on".into()),
            None => return Err("This device has not said whether it is on".into()),
            Some(true) => return Err("This device does not support power off".into()),
        },
        Command::Wake => "power-on",
        Command::Volume(true) => "volume-up",
        Command::Volume(false) => "volume-down",
        Command::ToggleMute => "mute",
        Command::Mute(true) => "mute-on",
        Command::Mute(false) => "mute-off",
        Command::Play(true) => "play",
        Command::Play(false) => "pause",
        Command::Next(true) => "next",
        Command::Next(false) => "previous",
        Command::Stop => "stop",
        Command::Rewind(false) => "rewind",
        Command::Rewind(true) => "fast-forward",
        Command::Channel(true) => "channel-up",
        Command::Channel(false) => "channel-down",
        Command::Key(Button::Up) => "up",
        Command::Key(Button::Down) => "down",
        Command::Key(Button::Left) => "left",
        Command::Key(Button::Right) => "right",
        Command::Key(Button::Enter) => "ok",
        Command::Key(Button::Back) => "back",
        Command::Key(Button::Home) => "home",
        Command::Key(Button::Menu) => "menu",
        Command::Key(Button::Red) => "red",
        Command::Key(Button::Green) => "green",
        Command::Key(Button::Yellow) => "yellow",
        Command::Key(Button::Blue) => "blue",
        Command::Input(id) => return Ok(Some(format!("input:{id}"))),
        Command::Function(id) if has(id) => return Ok(Some(id.clone())),
        Command::Function(_) => return Err("This control is not available for this device".into()),
        _ => return Err("This control is not available for this device".into()),
    };
    if has(function) {
        Ok(Some(function.into()))
    } else {
        Err("This control is not available for this device".into())
    }
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
    let integration = config
        .resolve_integration(&device.integration)
        .ok_or("Selected device is no longer a packaged integration")?;
    let Integration::Plugin {
        capabilities,
        supports_inputs,
        ..
    } = &integration
    else {
        return Err("Selected device is no longer a packaged integration".into());
    };
    let supports_inputs = *supports_inputs;
    let ask = |request| ask_device(&integration, request).map_err(|f| refusal(&f));
    let read = || match ask(Request::status())? {
        Response::Status { status } => Ok(status),
        _ => Err("The integration returned an invalid status".to_string()),
    };
    // Power is decided against what the device says now, not what the screen
    // last showed.
    let before = matches!(work.action, Command::Power)
        .then(|| read())
        .transpose()?;
    if let Some(function) = function(&work.action, before.as_ref(), capabilities)? {
        if !current() {
            return Ok(None);
        }
        let phase = if work.repeat {
            couch_model::KeyPhase::Repeat
        } else {
            couch_model::KeyPhase::Tap
        };
        match ask(Request::key(function, phase))? {
            Response::Ok => {}
            // A write to a child may be acknowledged with the state it left
            // it in; this screen reads the status straight after anyway.
            Response::Status { .. } => {}
            _ => return Err("The integration returned an invalid response".into()),
        }
    }
    let status = read()?;
    let inputs = if supports_inputs {
        match ask(Request::Inputs) {
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
    fn a_television_profile_keeps_native_remote_keys_out_of_commands() {
        let mut config = denon();
        let couch_model::Provider::Plugin { capabilities, .. } =
            &mut config.connections[0].provider
        else {
            unreachable!()
        };
        for id in [
            "up",
            "down",
            "left",
            "right",
            "ok",
            "back",
            "home",
            "play",
            "pause",
            "stop",
            "rewind",
            "fast-forward",
        ] {
            capabilities.push(couch_model::PluginCapability {
                id: id.into(),
                label: id.into(),
            });
        }
        config.validate().unwrap();
        let (_, device) = config.devices().next().unwrap();
        let declared = layout(&config, device).unwrap();
        assert!(declared.television && declared.power && declared.inputs);
        assert!(declared.commands.is_empty());
    }

    /// `refusal` is what both the toast (`activity_buttons::plugin_failure`)
    /// and this screen's own status line are built from, so what it does for
    /// `Unpaired` reaches both: Couch's sentence and the browser hint, never
    /// the package's own line, because the panel does no pairing of its own.
    #[test]
    fn an_unpaired_refusal_gives_the_status_line_the_browser_hint_not_the_reason() {
        use couch_plugin::{Error, Failure, Reason};
        assert_eq!(
            refusal(&Failure::from(Error::Unpaired)),
            format!("{}\n{PAIRING_HINT}", Error::Unpaired)
        );
        assert_eq!(
            refusal(&Failure {
                code: Error::Unpaired,
                reason: Some(Reason::Message {
                    text: "Pair this TV again".into()
                }),
            }),
            format!("{}\n{PAIRING_HINT}", Error::Unpaired)
        );
        // Every other code keeps carrying the package's own line, unchanged.
        assert_eq!(
            refusal(&Failure {
                code: Error::Rejected,
                reason: Some(Reason::Message {
                    text: "The TV is locked".into()
                }),
            }),
            format!("{}\nThe TV is locked", Error::Rejected)
        );
    }

    /// Which frames name a child and which do not. This is the one place the
    /// panel decides it, so every request it makes - a row key, a volume
    /// write, a status read, the packaged screen - inherits the answer.
    #[test]
    fn only_a_child_is_named_and_a_receivers_frames_are_unchanged() {
        let config = denon();
        let (_, device) = config.devices().next().unwrap();
        let receiver = config.resolve_integration(&device.integration).unwrap();
        assert_eq!(resource(&receiver), None);
        // A receiver with a zone saved on it is still the connection itself:
        // `zone1` is part of its settings, not a child, and its frames are
        // the bytes a protocol 2 package has always read.
        let mut zoned = config.clone();
        zoned.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: "theater-avr".into(),
            resource_id: "zone1".into(),
            child: None,
        };
        zoned.validate().unwrap();
        let (_, device) = zoned.devices().next().unwrap();
        let zoned = zoned.resolve_integration(&device.integration).unwrap();
        assert_eq!(resource(&zoned), None);
        for request in [
            Request::status(),
            Request::key("volume-up", couch_model::KeyPhase::Repeat),
            Request::action(couch_model::TypedAction::SetVolumeDb { tenths: -395 }),
            Request::Inputs,
        ] {
            assert_eq!(aimed(request.clone(), &zoned), request);
            assert_eq!(
                serde_json::to_string(&aimed(request.clone(), &receiver)).unwrap(),
                serde_json::to_string(&request).unwrap()
            );
        }

        // A child of a bridge: every frame that can name one does.
        let bridge: couch_model::Config = serde_json::from_value(serde_json::json!({
            "schema_version":1,
            "connections":[{"id":"bridge","name":"Hue bridge","provider":{"kind":"plugin",
                "id":"hue","label":"Philips Hue","children":[
                    {"kind":"light","label":"Light","device_kind":"light","component":"light",
                     "capabilities":[{"id":"toggle","label":"Toggle"}],
                     "actions":[{"action":"set_light"}]}]}}],
            "rooms":[{"id":"living-room","name":"Living room","devices":[
                {"id":"desk","name":"Desk lamp","kind":"light","integration":{"via":"connection",
                    "connection_id":"bridge","resource_id":"lamp/1",
                    "child":{"kind":"light","light":{"dimmable":true}}}}]}]
        }))
        .unwrap();
        bridge.validate().unwrap();
        let (_, device) = bridge.devices().next().unwrap();
        let child = bridge.resolve_integration(&device.integration).unwrap();
        assert_eq!(resource(&child), Some("lamp/1"));
        assert_eq!(
            aimed(Request::status(), &child),
            Request::status().at("lamp/1")
        );
        // A level on a child goes as the plain command it is; the host gate
        // is what turns it into the typed action.
        assert_eq!(
            aimed(Request::key("dim:30", couch_model::KeyPhase::Tap), &child),
            Request::Command {
                function: "dim:30".into(),
                phase: couch_model::KeyPhase::Tap,
                resource: Some("lamp/1".into())
            }
        );
        // Nothing that cannot name a child is changed by aiming it.
        assert_eq!(aimed(Request::Inputs, &child), Request::Inputs);
    }

    #[test]
    fn power_is_decided_from_the_observed_state_and_never_guessed() {
        let capabilities = |ids: &[&str]| {
            ids.iter()
                .map(|id| couch_model::PluginCapability {
                    id: (*id).into(),
                    label: (*id).into(),
                })
                .collect::<Vec<_>>()
        };
        let commands = capabilities(&[
            "power-on",
            "power-off",
            "channel-up",
            "up",
            "fast-forward",
            "x:sound-mode-movie",
        ]);
        let status = |json: serde_json::Value| -> Status { serde_json::from_value(json).unwrap() };
        let on = status(serde_json::json!({"on":true}));
        let off = status(serde_json::json!({"on":false}));
        assert_eq!(
            function(&Command::Power, Some(&on), &commands)
                .unwrap()
                .as_deref(),
            Some("power-off")
        );
        assert_eq!(
            function(&Command::Power, Some(&off), &commands)
                .unwrap()
                .as_deref(),
            Some("power-on")
        );
        assert!(function(
            &Command::Power,
            Some(&status(serde_json::json!({}))),
            &commands
        )
        .is_err());
        assert!(function(&Command::Power, None, &commands).is_err());
        assert_eq!(
            function(&Command::Input("SAT/CBL".into()), None, &commands)
                .unwrap()
                .as_deref(),
            Some("input:SAT/CBL")
        );
        assert_eq!(
            function(
                &Command::Function("x:sound-mode-movie".into()),
                None,
                &commands
            )
            .unwrap()
            .as_deref(),
            Some("x:sound-mode-movie")
        );
        assert_eq!(function(&Command::Retry, None, &commands).unwrap(), None);
        assert_eq!(
            function(&Command::Channel(true), None, &commands)
                .unwrap()
                .as_deref(),
            Some("channel-up")
        );
        assert_eq!(
            function(&Command::Key(Button::Up), None, &commands)
                .unwrap()
                .as_deref(),
            Some("up")
        );
        assert_eq!(
            function(&Command::Rewind(true), None, &commands)
                .unwrap()
                .as_deref(),
            Some("fast-forward")
        );
        assert!(function(&Command::Key(Button::Down), None, &commands).is_err());
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
            app.set_tv_native(false);
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
        // A capability-profiled television keeps the same native transport row
        // and sends the physical D-pad to the package instead of moving a
        // selection border between generic tiles.
        fill(
            "LG WEBOS TV",
            (true, true, false),
            "Living room TV",
            serde_json::json!({"on":true,"input":"NET","volume":12,"title":"Netflix"}),
        );
        app.set_tv_native(true);
        actions.borrow_mut().clear();
        key(slint::platform::Key::UpArrow);
        key(slint::platform::Key::Return);
        assert_eq!(&*actions.borrow(), &["up", "ok"]);
        shot("core-7-native-tv.png");
        app.hide().unwrap();
    }
}
