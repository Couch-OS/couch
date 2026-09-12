use super::Reply;
use couch_system::{
    client,
    protocol::{Reply as SystemReply, Request},
};
pub(super) fn route(method: &str, path: &[&str], body: &[u8]) -> Reply {
    let value = if body.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(v) => v,
            Err(_) => return Reply::error(400, "Invalid update request"),
        }
    };
    let request = match (method, path) {
        ("GET", []) => Request::UpdateStatus,
        ("POST", ["check"]) => Request::UpdateCheck {
            automatic: value["automatic"].as_bool().unwrap_or(false),
        },
        ("POST", ["install"]) => {
            let Some(version) = value["version"].as_str() else {
                return Reply::error(400, "Select a build version");
            };
            Request::UpdateInstall {
                version: version.to_owned(),
            }
        }
        ("POST", ["restart"]) if value["confirm"] == true => Request::UpdateRestart,
        ("PUT", ["settings"]) => {
            let channel = match serde_json::from_value(value["channel"].clone()) {
                Ok(v) => v,
                Err(_) => return Reply::error(400, "Choose stable or alpha"),
            };
            let Some(automatic_checks) = value["automatic_checks"].as_bool() else {
                return Reply::error(400, "Choose automatic update checks");
            };
            Request::UpdateSettings {
                channel,
                automatic_checks,
            }
        }
        _ => return Reply::error(400, "Unsupported update operation"),
    };
    match client::call(request) {
        Ok(SystemReply::Update(status)) => Reply::json(200, &status),
        Ok(SystemReply::Done(Ok(()))) => Reply::json(202, &serde_json::json!({"accepted":true})),
        Ok(SystemReply::Done(Err(error))) => Reply::error(409, &error),
        _ => Reply::error(503, "System update service is unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_requires_explicit_boolean_confirmation_before_ipc() {
        for body in [
            b"{}".as_slice(),
            br#"{"confirm":false}"#,
            br#"{"confirm":"true"}"#,
        ] {
            assert_eq!(route("POST", &["restart"], body).status, 400);
        }
    }

    #[test]
    fn invalid_version_settings_and_json_are_rejected_before_ipc() {
        for (path, body) in [
            ("install", b"{}".as_slice()),
            ("install", br#"{"version":42}"#),
            ("check", b"{"),
        ] {
            assert_eq!(route("POST", &[path], body).status, 400);
        }
        assert_eq!(
            route(
                "PUT",
                &["settings"],
                br#"{"channel":"beta","automatic_checks":true}"#
            )
            .status,
            400
        );
        assert_eq!(
            route("PUT", &["settings"], br#"{"channel":"stable"}"#).status,
            400
        );
    }
}
