//! One-way device controls: infrared, and Bluetooth HID (the remote is the
//! peripheral, the TV paired to it gets consumer-control keys). All
//! filesystem and transmitter I/O stays on the worker.
use super::{Command, Details, Event, Work};
use couch_model::{commands::Function, Action, Integration};
use couch_webos::Button;
use std::sync::atomic::AtomicU64;

pub(super) fn function(action: &Command) -> Result<Option<String>, String> {
    Ok(Some(match action {
        Command::Retry => return Ok(None),
        Command::IrFunction(name) if Function::parse(name).is_some() => name.clone(),
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
            Button::Blue => "blue",
            Button::Yellow => "yellow",
            _ => return Err("This key has no infrared function".into()),
        }
        .into(),
        Command::Volume(true) => "volume-up".into(),
        Command::Volume(false) => "volume-down".into(),
        Command::Channel(true) => "channel-up".into(),
        Command::Channel(false) => "channel-down".into(),
        Command::Mute(_) | Command::ToggleMute => "mute".into(),
        Command::Power => "toggle".into(),
        Command::Wake => "power-on".into(),
        Command::Play(true) => "play".into(),
        Command::Play(false) => "pause".into(),
        Command::Next(true) => "next".into(),
        Command::Next(false) => "previous".into(),
        Command::Stop => "stop".into(),
        Command::Rewind(true) => "fast-forward".into(),
        Command::Rewind(false) => "rewind".into(),
        _ => return Err("This control is not available for an infrared device".into()),
    }))
}

pub(super) fn request_current(
    work: &Work,
    active: &AtomicU64,
    latest: Option<&std::sync::Arc<couch_model::Config>>,
) -> bool {
    super::android::current(work, active)
        && work
            .config
            .as_ref()
            .zip(latest)
            .is_some_and(|(expected, latest)| std::sync::Arc::ptr_eq(expected, latest))
}

pub(super) fn run(work: &Work, active: &AtomicU64) -> Result<Option<Event>, String> {
    if !super::android::current(work, active) {
        return Ok(None);
    }
    let id = work
        .connection
        .strip_prefix("ir:")
        .or_else(|| work.connection.strip_prefix("bt:"))
        .ok_or("Missing IR device")?;
    let bluetooth = work.connection.starts_with("bt:");
    let config = work.config.as_ref().ok_or("Configuration unavailable")?;
    let current = || request_current(work, active, crate::connections::config().as_ref());
    if !current() {
        return Ok(None);
    }
    let (_, device) = config
        .devices()
        .find(|(_, d)| d.id.as_str() == id)
        .ok_or("IR device was removed")?;
    // What the list offers: the codeset's own entries when the device has
    // one, then the Bluetooth keys its bond adds (those not already listed).
    // A press goes through the device's transport order either way.
    let mut choices: Vec<(String, String, String)> = Vec::new();
    if let Some(codeset) = device.effective_ir_codeset(config) {
        let integration = Integration::Ir {
            codeset: codeset.into(),
        };
        let codes = couch_ir::codeset::load(&crate::home::path("ir"), codeset)
            .map_err(|e| e.to_string())?;
        choices.extend(codes.entries.iter().filter_map(|entry| {
            let f = Function::parse(&entry.button)?;
            f.supports(&integration).then(|| {
                (
                    format!("ir:{}", f.id()),
                    entry.button.replace('-', " "),
                    "Send infrared command".into(),
                )
            })
        }));
    } else if !bluetooth {
        return Err("Selected device has no infrared commands".into());
    }
    if device.bluetooth.is_some() {
        for (id, label) in couch_model::buttons::functions(&Integration::BluetoothTv) {
            let action = format!("ir:{id}");
            if !choices.iter().any(|(a, _, _)| *a == action) {
                choices.push((action, (*label).to_owned(), "Send over Bluetooth".into()));
            }
        }
    }
    let command = function(&work.action)?;
    if !current() {
        return Ok(None);
    }
    let status = if let Some(command) = command {
        crate::activity_buttons::execute_with_input(
            &config,
            &Action::new(device.id.clone(), command),
            &mut Default::default(),
            &mut Default::default(),
            &mut Default::default(),
            &mut Default::default(),
            &crate::connections::matter(),
            work.repeat,
            &current,
        )?;
        if !current() {
            return Ok(None);
        }
        if bluetooth { "Key sent over Bluetooth · No device feedback" } else { "IR command sent · No device feedback" }
    } else if bluetooth {
        "Bluetooth · No device feedback"
    } else {
        "Infrared · No device feedback"
    };
    Ok(Some(Event {
        generation: work.generation,
        status: Ok(status.into()),
        details: Some(Details {
            source: if bluetooth { "Bluetooth controls" } else { "Infrared controls" }.into(),
            choices,
            ..Details::default()
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_ir_commands_cancel_after_close_config_change_or_expiry() {
        use std::{
            sync::{atomic::Ordering, Arc},
            time::{Duration, Instant},
        };
        let config = Arc::new(couch_model::Config::default());
        let active = AtomicU64::new(9);
        let mut work = Work {
            device: None,
            connection: "ir:test".into(),
            generation: 9,
            action: Command::Volume(true),
            at: Instant::now(),
            repeat: false,
            config: Some(config.clone()),
        };
        assert!(request_current(&work, &active, Some(&config)));
        let edited = Arc::new((*config).clone());
        assert!(!request_current(&work, &active, Some(&edited)));
        active.store(0, Ordering::SeqCst);
        assert!(!request_current(&work, &active, Some(&config)));
        active.store(9, Ordering::SeqCst);
        work.at = Instant::now() - Duration::from_secs(1);
        assert!(!request_current(&work, &active, Some(&config)));
    }
    #[test]
    fn physical_commands_use_explicit_ir_functions() {
        assert_eq!(
            function(&Command::Power).unwrap().as_deref(),
            Some("toggle")
        );
        assert_eq!(
            function(&Command::Wake).unwrap().as_deref(),
            Some("power-on")
        );
        assert_eq!(
            function(&Command::Key(Button::Enter)).unwrap().as_deref(),
            Some("ok")
        );
        assert_eq!(
            function(&Command::Rewind(true)).unwrap().as_deref(),
            Some("fast-forward")
        );
        assert!(function(&Command::App("fake".into())).is_err());
        assert!(function(&Command::Retry).unwrap().is_none());
    }
}
