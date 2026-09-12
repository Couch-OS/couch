//! Camera API proxy. The browser never receives NVR keys or stream tokens.
use super::Reply;
use couch_unifi_protect::{settings::Settings, SnapshotChannel};
use serde_json::json;
use std::path::PathBuf;

pub(super) fn route_at(method: &str, path: &[&str], body: &[u8], file: PathBuf) -> Reply {
    let lock = super::connections::lock_for(&file);
    let Ok(_guard) = lock.try_lock() else {
        return Reply::error(503, "Camera connection is busy");
    };
    if method == "GET" && path == ["connection"] {
        return Reply::json(
            200,
            &match Settings::load(&file) {
                Ok(s) => {
                    json!({"origin":s.origin,"certificate_sha256":s.certificate_sha256,"media_certificate_sha256":s.media_certificate_sha256,"media_origin":s.media_origin,"private_ca_set":s.private_ca_pem.is_some(),"stream_host":s.stream_host,"key_set":true})
                }
                Err(_) => json!({"key_set":false}),
            },
        );
    }
    if method == "PUT" && path == ["connection"] {
        let settings: Settings = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => {
                return Reply::error(
                    400,
                    "Enter the console origin, API key and explicit certificate trust",
                )
            }
        };
        let cameras = match settings.client().and_then(|c| c.cameras()) {
            Ok(c) => c,
            Err(e) => return Reply::error(502, e.to_string()),
        };
        let Some(parent) = file.parent() else {
            return Reply::error(500, "Invalid connection path");
        };
        if std::fs::create_dir_all(parent).is_err() || settings.save(&file).is_err() {
            return Reply::error(
                500,
                "Connection tested but private enrollment could not be saved",
            );
        }
        return cameras_reply(cameras);
    }
    let client = match Settings::load(&file).and_then(|s| s.client()) {
        Ok(client) => client,
        Err(_) => return Reply::error(400, "Set up the Protect connection first"),
    };
    match (method, path) {
        ("GET", ["cameras"]) => match client.cameras() {
            Ok(c) => cameras_reply(c),
            Err(e) => Reply::error(502, e.to_string()),
        },
        ("GET", ["cameras", id, "snapshot"]) => match client.snapshot(id, SnapshotChannel::Main) {
            Ok(bytes) => {
                let mut reply = Reply::json(200, &json!(null));
                reply.body = bytes;
                reply.content_type = "image/jpeg";
                reply.cache = Some("no-store");
                reply
            }
            Err(e) => Reply::error(502, e.to_string()),
        },
        _ => Reply::error(404, "Unknown camera operation"),
    }
}
fn cameras_reply(cameras: Vec<couch_unifi_protect::Camera>) -> Reply {
    Reply::json(200, &cameras.into_iter().map(|c| json!({"id":c.id,"name":c.name,"state":c.state,"has_package_camera":c.has_package_camera})).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_status_never_returns_key_and_bad_enrollment_preserves_old_file() {
        let dir = std::env::temp_dir().join(format!("protect-api-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("protect-connection.json");
        let settings: Settings = serde_json::from_value(
            json!({"origin":"https://console.test","api_key":"fixture-secret-private"}),
        )
        .unwrap();
        settings.save(&file).unwrap();
        let before = std::fs::read(&file).unwrap();
        let status = route_at("GET", &["connection"], &[], file.clone());
        assert_eq!(status.status, 200);
        let value: serde_json::Value = serde_json::from_slice(&status.body).unwrap();
        assert_eq!(value["key_set"], true);
        assert!(!String::from_utf8(status.body)
            .unwrap()
            .contains("fixture-secret-private"));
        let result = route_at(
            "PUT",
            &["connection"],
            br#"{"origin":"http://console.test","api_key":"another-private"}"#,
            file.clone(),
        );
        assert_eq!(result.status, 502);
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(!String::from_utf8(result.body)
            .unwrap()
            .contains("another-private"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
