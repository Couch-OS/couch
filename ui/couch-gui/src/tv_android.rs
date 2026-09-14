//! Android TV adapter for the shared TV screen; all calls run on its worker.
use super::{Command, Details, Event, Work};
use couch_control::{StreamingConnection, StreamingTv};
use couch_webos::Button;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(super) fn current(work: &Work, active: &AtomicU64) -> bool {
    work.generation != 0
        && active.load(Ordering::SeqCst) == work.generation
        && (matches!(work.action, Command::Retry)
            || work.at.elapsed() <= Duration::from_millis(750))
}
fn function(action: &Command, state: &Value) -> Result<Option<&'static str>, String> {
    Ok(Some(match action {
        Command::IrFunction(_) => return Err("IR commands are unavailable on Android TV".into()),
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
            _ => return Err("This Android TV client does not support color keys yet".into()),
        },
        Command::Volume(true) => "volume-up",
        Command::Volume(false) => "volume-down",
        Command::Channel(true) => "channel-up",
        Command::Channel(false) => "channel-down",
        Command::ToggleMute => "mute",
        Command::Mute(wanted) => match state["volume"][2].as_bool() {
            Some(value) if value == *wanted => return Ok(None),
            Some(_) => "mute",
            None => return Err("TV has not reported its mute state".into()),
        },
        Command::Power => match state["on"].as_bool() {
            Some(true) => "power-off",
            Some(false) => "power-on",
            None => return Err("TV has not reported its power state; try again shortly".into()),
        },
        Command::Next(true) => "next",
        Command::Next(false) => "previous",
        Command::Stop => "stop",
        Command::Play(true) => "play",
        Command::Play(false) => "pause",
        Command::Rewind(true) => "fast-forward",
        Command::Rewind(false) => "rewind",
        Command::Wake
        | Command::SetVolume(_)
        | Command::Input(_)
        | Command::App(_)
        | Command::Sound(_) => return Err("This control is only available for LG webOS TVs".into()),
    }))
}
fn event(generation: u64, state: &Value) -> Event {
    let power = match state["on"].as_bool() {
        Some(true) => "TV on",
        Some(false) => "TV in standby",
        None => "Connected",
    };
    let mut status = power.to_string();
    if let Some(volume) = state["volume"][0].as_u64() {
        status.push_str(&format!(" · Volume {volume}"));
        if state["volume"][2] == true {
            status.push_str(" · Muted");
        }
    }
    Event {
        generation,
        status: Ok(status),
        details: Some(Details {
            source: state["model"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("Android / Google TV")
                .into(),
            ..Details::default()
        }),
    }
}
pub(super) fn refresh(client: &StreamingTv, generation: u64) -> Result<Event, String> {
    client
        .status()
        .map(|state| event(generation, &state))
        .map_err(|e| e.to_string())
}
pub(super) fn run(
    client: &mut Option<StreamingTv>,
    work: &Work,
    active: &AtomicU64,
) -> Result<Option<Event>, String> {
    if !current(work, active) {
        return Ok(None);
    }
    if client.is_none() || matches!(work.action, Command::Retry) {
        let settings =
            StreamingConnection::load(&crate::connections::file(&work.connection, "androidtv"))
                .map_err(|_| "Pair this Android TV in Connections first".to_string())?;
        if settings.kind() != "androidtv" {
            return Err("TV credentials have the wrong provider".into());
        }
        *client = Some(StreamingTv::connect(&settings).map_err(|e| e.to_string())?);
    }
    // A slow handshake must not replay keys from a screen that was closed or
    // from an earlier key press. Dropping this lease releases its broker owner.
    if !current(work, active) {
        if active.load(Ordering::SeqCst) != work.generation {
            *client = None;
        }
        return Ok(None);
    }
    let c = client.as_ref().unwrap();
    // Only state-dependent operations need a synchronous status read. Ordinary
    // navigation stays on the immediate command path.
    let state = if matches!(
        work.action,
        Command::Power | Command::Mute(_) | Command::Retry
    ) {
        c.status().map_err(|e| e.to_string())?
    } else {
        Value::Null
    };
    if !current(work, active) {
        return Ok(None);
    }
    if let Command::App(url) = &work.action {
        if !couch_model::valid_app_url(url) {
            return Ok(Some(Event {
                generation: work.generation,
                details: None,
                status: Err("Use an absolute Android app link in Connections".into()),
            }));
        }
        c.command(&format!("app:{url}"))
            .map_err(|e| e.to_string())?;
        return Ok(Some(Event {
            generation: work.generation,
            details: None,
            status: Ok("App launch requested".into()),
        }));
    }
    match function(&work.action, &state) {
        Ok(Some(command)) => c.command(command).map_err(|e| e.to_string())?,
        Ok(None) => {}
        Err(error) => {
            return Ok(Some(Event {
                generation: work.generation,
                details: None,
                status: Err(error),
            }))
        }
    }
    Ok(Some(if matches!(work.action, Command::Retry) {
        event(work.generation, &state)
    } else {
        Event {
            generation: work.generation,
            details: None,
            status: Ok(String::new()),
        }
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    #[test]
    fn android_physical_and_playback_routes_are_explicit() {
        for (input, output) in [
            ("up", "up"),
            ("down", "down"),
            ("left", "left"),
            ("right", "right"),
            ("ok", "ok"),
            ("back", "back"),
            ("home", "home"),
            ("menu", "menu"),
            ("volume-up", "volume-up"),
            ("volume-down", "volume-down"),
            ("channel-up", "channel-up"),
            ("channel-down", "channel-down"),
            ("toggle-mute", "mute"),
            ("next", "next"),
            ("previous", "previous"),
            ("stop", "stop"),
            ("play", "play"),
            ("pause", "pause"),
            ("rewind", "rewind"),
            ("forward", "fast-forward"),
        ] {
            assert_eq!(
                function(&super::super::command(input).unwrap(), &Value::Null).unwrap(),
                Some(output)
            );
        }
        for input in [
            "red",
            "green",
            "blue",
            "yellow",
            "input:HDMI_1",
            "app:com.lg.settings",
            "sound:tv_speaker",
        ] {
            assert!(function(&super::super::command(input).unwrap(), &Value::Null).is_err());
        }
    }
    #[test]
    fn power_and_explicit_mute_require_known_state() {
        assert_eq!(
            function(&Command::Power, &serde_json::json!({"on":true})).unwrap(),
            Some("power-off")
        );
        assert_eq!(
            function(&Command::Power, &serde_json::json!({"on":false})).unwrap(),
            Some("power-on")
        );
        assert!(function(&Command::Power, &Value::Null).is_err());
        let state = serde_json::json!({"volume":[12,100,true]});
        assert_eq!(function(&Command::Mute(true), &state).unwrap(), None);
        assert_eq!(
            function(&Command::Mute(false), &state).unwrap(),
            Some("mute")
        );
    }
    #[test]
    fn closed_changed_and_expired_work_never_routes_a_key() {
        let active = AtomicU64::new(2);
        let mut work = Work {
            device: None,
            repeat: false,
            config: None,
            connection: "tv".into(),
            generation: 2,
            action: Command::Key(Button::Enter),
            at: Instant::now(),
        };
        assert!(current(&work, &active));
        active.store(0, Ordering::SeqCst);
        assert!(!current(&work, &active));
        active.store(3, Ordering::SeqCst);
        assert!(!current(&work, &active));
        active.store(2, Ordering::SeqCst);
        work.at -= Duration::from_secs(1);
        assert!(!current(&work, &active));
        work.action = Command::Retry;
        assert!(current(&work, &active));
    }
    #[test]
    fn status_reports_real_state_without_now_playing_claims() {
        let e = event(
            4,
            &serde_json::json!({"on":true,"model":"MiTV-AFMU0","volume":[12,100,false]}),
        );
        assert_eq!(e.status.unwrap(), "TV on · Volume 12");
        assert_eq!(e.details.unwrap().source, "MiTV-AFMU0");
    }
}
