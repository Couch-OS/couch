use super::{parse, Reply};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: String,
    value: Option<serde_json::Value>,
}
fn valid(input: &Input) -> bool {
    match input.command.as_str() {
        "volume" => input
            .value
            .as_ref()
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v <= 100),
        "play" | "pause" | "play-pause" | "stop" | "next" | "previous" | "volume-up"
        | "volume-down" | "mute" | "mute-on" | "mute-off" => {
            input.value.as_ref().is_none_or(|v| v.is_null())
        }
        _ => false,
    }
}
pub(super) fn route(method: &str, path: &[&str], body: &[u8], host: &str) -> Reply {
    let input = match (method, path) {
        ("GET", ["status"]) => None,
        ("POST", ["command"]) => {
            let input: Input = match parse(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            if !valid(&input) {
                return Reply::error(
                    400,
                    "Choose a Sonos playback command or volume from 0 to 100",
                );
            }
            Some(input)
        }
        _ => return Reply::error(404, "Unknown Sonos operation"),
    };
    let Ok(address) = host.parse() else {
        return Reply::error(400, "Enter the Sonos IPv4 address");
    };
    let result = (|| {
        let client = couch_sonos::Client::connect(address)?;
        if let Some(input) = input {
            if input.command == "volume" {
                client.set_volume(input.value.unwrap().as_u64().unwrap() as u8)?;
            } else {
                client.command(&input.command)?;
            }
            // Acknowledgement stays separate from later observations: a failed status
            // request must not make a successful write look retryable.
            Ok(serde_json::json!({"acknowledged": true}))
        } else {
            client.status().map(|s| serde_json::json!(s))
        }
    })();
    match result {
        Ok(v) => Reply::json(200, &v),
        Err(couch_sonos::Error::NotCoordinator{coordinator}) => Reply::error(409, format!("Playback belongs to coordinator {coordinator}. Select that speaker explicitly; volume and mute still control this speaker.")),
        Err(e) => Reply::error(502,e.to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_validated_before_network_access() {
        for body in [
            r#"{"command":"power-on"}"#,
            r#"{"command":"volume","value":101}"#,
            r#"{"command":"volume","value":-1}"#,
            r#"{"command":"volume","value":1.5}"#,
            r#"{"command":"play","value":50}"#,
        ] {
            assert_eq!(
                route("POST", &["command"], body.as_bytes(), "invalid").status,
                400
            );
            let input: Input = serde_json::from_str(body).unwrap();
            assert!(!valid(&input));
        }
        for body in [
            r#"{"command":"volume","value":0}"#,
            r#"{"command":"volume","value":100}"#,
            r#"{"command":"pause"}"#,
        ] {
            assert!(valid(&serde_json::from_str(body).unwrap()));
        }
        assert_eq!(route("DELETE", &["command"], b"", "invalid").status, 404);
    }
}
