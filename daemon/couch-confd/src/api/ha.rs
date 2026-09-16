use super::Reply;
use couch_ha::{settings::Settings, ClimateCommand, Command, CoverCommand};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

// Serialize configuration changes and commands so they cannot cross servers.

#[derive(Deserialize)]
struct Setup {
    url: String,
    #[serde(default)]
    token: String,
}
#[derive(Deserialize)]
struct Action {
    action: String,
    brightness: Option<u8>,
}
#[derive(Deserialize)]
struct CoverAction {
    action: String,
    position: Option<u8>,
}
#[derive(Deserialize)]
struct ClimateAction {
    action: String,
    temperature: Option<f64>,
    low: Option<f64>,
    high: Option<f64>,
    mode: Option<String>,
}
pub(super) fn route(method: &str, path: &[&str], body: &[u8]) -> Reply {
    let file = PathBuf::from(
        std::env::var("COUCH_HA_CONNECTION")
            .unwrap_or_else(|_| "/opt/couch/ha-connection.json".into()),
    );
    route_at(method, path, body, file)
}
pub(super) fn route_at(method: &str, path: &[&str], body: &[u8], file: PathBuf) -> Reply {
    if let Some(parent) = file.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Reply::error(500, "Cannot create private connection directory");
        }
    }
    let connection_lock = super::connections::lock_for(&file);
    let Ok(_guard) = connection_lock.try_lock() else {
        return Reply::error(503, "Home Assistant connection is busy");
    };

    let saved = Settings::load(&file);
    if method == "GET" && path == ["connection"] {
        return Reply::json(
            200,
            &match saved {
                Ok(s) => json!({"url":s.url,"token_set":!s.token.is_empty()}),
                Err(_) => json!({"url":"","token_set":false}),
            },
        );
    }
    if method == "PUT" && path == ["connection"] {
        let Ok(input) = serde_json::from_slice::<Setup>(body) else {
            return Reply::error(400, "Enter the server URL and access token");
        };
        let token = if input.token.is_empty() {
            match saved {
                Ok(s) if s.url == input.url => s.token,
                _ => return Reply::error(400, "Enter an access token for this server"),
            }
        } else {
            input.token
        };
        let setting = Settings {
            url: input.url,
            token,
        };
        let lights = match setting.client().and_then(|c| c.lights()) {
            Ok(l) => l,
            Err(e) => return Reply::error(502, e.to_string()),
        };
        if setting.save(&file).is_err() {
            return Reply::error(
                500,
                "Connection tested, but its settings could not be saved",
            );
        }
        return Reply::json(
            200,
            &json!({"url":setting.url,"token_set":true,"lights":lights}),
        );
    }
    let client = match saved.and_then(|s| s.client()) {
        Ok(c) => c,
        Err(_) => return Reply::error(400, "Set up the Home Assistant connection first"),
    };
    match (method, path) {
        ("GET", ["lights"]) => match client.lights() {
            Ok(l) => Reply::json(200, &l),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("GET", ["lights", id]) => match client.light(id) {
            Ok(l) => Reply::json(200, &l),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("POST", ["lights", id, "command"]) => {
            let Ok(action) = serde_json::from_slice::<Action>(body) else {
                return Reply::error(400, "Invalid light command");
            };
            let command = match (action.action.as_str(), action.brightness) {
                ("on", None) => Command::On,
                ("off", None) => Command::Off,
                ("brightness", Some(p)) if p <= 100 => Command::Brightness(p),
                _ => return Reply::error(400, "Choose on, off or brightness from 0 to 100"),
            };
            match client.command(id, command) {
                Ok(()) => Reply::json(200, &json!({"accepted":true})),
                Err(e) => Reply::error(502, e.to_string()),
            }
        }
        ("GET", ["covers"]) => match client.covers() {
            Ok(v) => Reply::json(200, &v),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("GET", ["covers", id]) => match client.cover(id) {
            Ok(v) => Reply::json(200, &v),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("GET", ["climates"]) => match client.climates() {
            Ok(v) => Reply::json(200, &v),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("GET", ["climates", id]) => match client.climate(id) {
            Ok(v) => Reply::json(200, &v),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("POST", ["covers", id, "command"]) => {
            let Ok(action) = serde_json::from_slice::<CoverAction>(body) else {
                return Reply::error(400, "Invalid blind command");
            };
            let Some(command) = cover_command(action) else {
                return Reply::error(
                    400,
                    "Choose open, close, stop, toggle or position from 0 to 100",
                );
            };
            match client.cover_command(id, command) {
                Ok(()) => Reply::json(200, &json!({"accepted":true})),
                Err(e) => Reply::error(502, e.to_string()),
            }
        }
        ("POST", ["climates", id, "command"]) => {
            let Ok(action) = serde_json::from_slice::<ClimateAction>(body) else {
                return Reply::error(400, "Invalid thermostat command");
            };
            let Some(command) = climate_command(action) else {
                return Reply::error(
                    400,
                    "Choose a target temperature, ordered target range or HVAC mode",
                );
            };
            match client.climate_command(id, command) {
                Ok(()) => Reply::json(200, &json!({"accepted":true})),
                Err(e) => Reply::error(502, e.to_string()),
            }
        }
        _ => Reply::error(404, "Unknown Home Assistant operation"),
    }
}

fn cover_command(action: CoverAction) -> Option<CoverCommand> {
    Some(match (action.action.as_str(), action.position) {
        ("open", None) => CoverCommand::Open,
        ("close", None) => CoverCommand::Close,
        ("toggle", None) => CoverCommand::Toggle,
        ("stop", None) => CoverCommand::Stop,
        ("position", Some(p)) if p <= 100 => CoverCommand::Position(p),
        _ => return None,
    })
}
fn climate_command(action: ClimateAction) -> Option<ClimateCommand> {
    Some(
        match (
            action.action.as_str(),
            action.temperature,
            action.low,
            action.high,
            action.mode,
        ) {
            ("temperature", Some(t), None, None, None) if t.is_finite() => {
                ClimateCommand::Temperature(t)
            }
            ("range", None, Some(low), Some(high), None)
                if low.is_finite() && high.is_finite() && low <= high =>
            {
                ClimateCommand::TemperatureRange { low, high }
            }
            ("mode", None, None, None, Some(mode)) if !mode.is_empty() => {
                ClimateCommand::HvacMode(mode)
            }
            _ => return None,
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blinds_validate_position_and_do_not_accept_light_actions() {
        for value in [
            json!({"action":"open"}),
            json!({"action":"position","position":0}),
            json!({"action":"position","position":100}),
        ] {
            assert!(cover_command(serde_json::from_value(value).unwrap()).is_some());
        }
        for value in [
            json!({"action":"position","position":101}),
            json!({"action":"position"}),
            json!({"action":"on"}),
            json!({"action":"open","position":50}),
        ] {
            assert!(cover_command(serde_json::from_value(value).unwrap()).is_none());
        }
    }
    #[test]
    fn thermostats_require_unambiguous_temperature_range_or_mode() {
        for value in [
            json!({"action":"temperature","temperature":21.5}),
            json!({"action":"range","low":18,"high":24}),
            json!({"action":"mode","mode":"heat"}),
        ] {
            assert!(climate_command(serde_json::from_value(value).unwrap()).is_some());
        }
        for value in [
            json!({"action":"temperature"}),
            json!({"action":"range","low":24,"high":18}),
            json!({"action":"mode","mode":""}),
            json!({"action":"temperature","temperature":21,"mode":"heat"}),
        ] {
            assert!(climate_command(serde_json::from_value(value).unwrap()).is_none());
        }
    }
}
