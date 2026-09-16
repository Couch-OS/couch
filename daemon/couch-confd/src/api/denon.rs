use super::{parse, Reply};
use couch_control::Denon as Client;
use couch_denon::{Command, Settings};
use serde::Deserialize;
#[derive(Deserialize)]
struct Input {
    command: String,
    value: Option<serde_json::Value>,
}
pub(super) fn route(method: &str, path: &[&str], body: &[u8], settings: Settings) -> Reply {
    let command = match (method, path) {
        ("GET", ["status"] | ["sources"]) => None,
        ("POST", ["command"]) => {
            let input: Input = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let value = input.value.unwrap_or_default();
            Some(match input.command.as_str() {
                "power-on" => Command::Power(true),
                "power-off" => Command::Power(false),
                "mute" => Command::Mute(true),
                "unmute" => Command::Mute(false),
                "volume-up" => Command::VolumeUp,
                "volume-down" => Command::VolumeDown,
                "volume" => match value.as_f64() {
                    Some(db) => Command::VolumeDb(db as f32),
                    None => return Reply::error(400, "Enter a volume in dB"),
                },
                "input" => match value.as_str() {
                    Some(id) => Command::Input(id.into()),
                    None => return Reply::error(400, "Choose an input"),
                },
                _ => return Reply::error(400, "Unsupported AVR command"),
            })
        }
        _ => return Reply::error(404, "Not found"),
    };
    let result = (|| {
        let mut c = Client::connect(&settings)?;
        if path == ["sources"] {
            return c.sources().map(|s| serde_json::json!(s));
        }
        match command {
            Some(command) => c.command(command),
            None => c.status(),
        }
        .map(|s| serde_json::json!(s))
    })();
    match result {
        Ok(v) => Reply::json(200, &v),
        Err(e) => Reply::error(502, e.to_string()),
    }
}
