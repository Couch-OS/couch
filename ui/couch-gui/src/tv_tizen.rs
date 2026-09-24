//! Samsung Tizen adapter for the shared TV screen. Keys are fire-and-forget
//! through the control broker; no playback, mute or power state is inferred.
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
            Button::Back => "back",
            Button::Home => "home",
            Button::Menu => "menu",
            Button::Red => "red",
            Button::Green => "green",
            Button::Yellow => "yellow",
            Button::Blue => "blue",
        }
        .into(),
        Command::Volume(true) => "volume-up".into(),
        Command::Volume(false) => "volume-down".into(),
        Command::Channel(true) => "channel-up".into(),
        Command::Channel(false) => "channel-down".into(),
        Command::ToggleMute => "mute".into(),
        // The TV never reports mute or power, so a targeted state is unknowable.
        Command::Mute(_) => {
            return Err("Samsung TVs do not report mute state; use the mute toggle".into())
        }
        Command::Power => "power-off".into(),
        Command::Wake => "power-on".into(),
        Command::Play(true) => "play".into(),
        Command::Play(false) => "pause".into(),
        Command::Stop => "stop".into(),
        Command::Rewind(true) => "fast-forward".into(),
        Command::Rewind(false) => "rewind".into(),
        Command::Next(_) => return Err("Samsung remotes have no next or previous key".into()),
        Command::Input(id) if Function::Input(id.clone()).supports(&Integration::Tizen) => {
            format!("input:{id}")
        }
        Command::App(id) if Function::App(id.clone()).supports(&Integration::Tizen) => {
            format!("app:{id}")
        }
        _ => return Err("This control is not supported by the Samsung TV client".into()),
    }))
}
fn status_text(value: &serde_json::Value) -> String {
    match value["power_state"].as_str() {
        Some("on") => "TV on · Smart View",
        Some("standby") => "TV in standby · Smart View",
        _ => "Connected · Smart View",
    }
    .into()
}
pub(super) fn refresh(client: &StreamingTv, generation: u64) -> Result<Event, String> {
    let value = client.status().map_err(|e| e.to_string())?;
    Ok(Event {
        generation,
        details: None,
        status: Ok(status_text(&value)),
    })
}
fn details(status: &serde_json::Value, apps: &serde_json::Value) -> Details {
    Details {
        source: status["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("Samsung TV")
            .into(),
        choices: apps["apps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|app| {
                let id = app["id"].as_str()?;
                let title = app["title"].as_str()?;
                Function::App(id.into())
                    .supports(&Integration::Tizen)
                    .then(|| (format!("app:{id}"), title.into(), "Open on TV".into()))
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
    let settings = StreamingConnection::load(&crate::connections::file(&work.connection, "tizen"))
        .map_err(|_| "Pair this Samsung TV in Connections first".to_string())?;
    if settings.kind() != "tizen" {
        return Err("TV credentials have the wrong provider".into());
    }
    // Waking cannot need the socket a sleeping TV will not answer.
    if matches!(work.action, Command::Wake) {
        let StreamingConnection::Tizen { settings } = &settings else {
            unreachable!()
        };
        return Ok(Some(Event {
            generation: work.generation,
            details: None,
            status: couch_tizen::wake_paired(settings)
                .map(|_| "Wake requested · not confirmed by the TV".to_string())
                .map_err(|e| match e {
                    couch_tizen::Error::Unsupported => {
                        "The TV reported no MAC address; pair again with it on".to_string()
                    }
                    e => e.to_string(),
                }),
        }));
    }
    if client.is_none() || matches!(work.action, Command::Retry) {
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
                Command::Power => "Power key sent · the TV toggles",
                Command::App(_) => "App launch requested",
                Command::Input(_) => "Source key sent",
                _ => "",
            }
            .into()),
        }));
    }
    let status = c.status().map_err(|e| e.to_string())?;
    let apps = c.apps();
    if !current(work, active) {
        return Ok(None);
    }
    Ok(Some(Event {
        generation: work.generation,
        details: Some(details(
            &status,
            apps.as_ref().unwrap_or(&serde_json::Value::Null),
        )),
        status: Ok(if apps.is_ok() {
            status_text(&status)
        } else {
            format!("{} · app list unavailable", status_text(&status))
        }),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tizen_functions_never_guess_state_and_reject_unmapped_keys() {
        assert_eq!(
            function(&Command::Power).unwrap().as_deref(),
            Some("power-off")
        );
        assert_eq!(
            function(&Command::Wake).unwrap().as_deref(),
            Some("power-on")
        );
        assert_eq!(
            function(&Command::ToggleMute).unwrap().as_deref(),
            Some("mute")
        );
        assert!(function(&Command::Mute(true)).is_err());
        assert!(function(&Command::Next(true)).is_err());
        assert_eq!(
            function(&Command::Key(Button::Blue)).unwrap().as_deref(),
            Some("blue")
        );
        assert_eq!(
            function(&Command::Input("hdmi1".into()))
                .unwrap()
                .as_deref(),
            Some("input:hdmi1")
        );
        assert!(function(&Command::Input("HDMI_1".into())).is_err());
        assert_eq!(
            function(&Command::App("111299001912".into()))
                .unwrap()
                .as_deref(),
            Some("app:111299001912")
        );
        assert_eq!(function(&Command::Retry).unwrap(), None);
    }
    #[test]
    fn details_use_reported_model_and_filter_app_ids() {
        let d = details(
            &serde_json::json!({"model":"QE55Q80T","power_state":"on"}),
            &serde_json::json!({"apps":[{"id":"111299001912","title":"YouTube"},{"id":"bad id","title":"Nope"}]}),
        );
        assert_eq!(d.source, "QE55Q80T");
        assert_eq!(d.choices.len(), 1);
        assert_eq!(d.choices[0].0, "app:111299001912");
        assert_eq!(
            status_text(&serde_json::json!({"power_state":"standby"})),
            "TV in standby · Smart View"
        );
        assert_eq!(
            status_text(&serde_json::json!({})),
            "Connected · Smart View"
        );
    }
}
