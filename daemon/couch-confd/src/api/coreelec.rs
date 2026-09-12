use super::Reply;
use couch_coreelec::{Client, OsAction, SshConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
#[derive(Serialize, Deserialize)]
struct Saved {
    host: String,
    user: String,
    port: u16,
    directory: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enroll {
    user: String,
    port: u16,
    private_key: String,
    known_hosts: String,
}
fn client(host: &str, port: u16, saved: &Saved) -> Result<Client, String> {
    if saved.host != host {
        return Err("Address changed; enroll SSH again for this CoreELEC device".into());
    }
    let config = SshConfig::new(
        host.parse()
            .map_err(|_| "OS access requires an IP address")?,
        saved.port,
        saved.user.clone(),
        saved.directory.join("key"),
        saved.directory.join("known_hosts"),
    )
    .map_err(|e| e.to_string())?;
    Ok(Client::new(couch_kodi::Kodi::tcp(host, port)).with_ssh(config))
}
fn private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
pub(super) fn route(
    method: &str,
    path: &[&str],
    body: &[u8],
    file: PathBuf,
    host: &str,
    port: u16,
) -> Reply {
    let lock = super::connections::lock_for(&file);
    let Ok(_guard) = lock.try_lock() else {
        return Reply::error(503, "CoreELEC OS connection is busy");
    };
    let saved = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Saved>(&b).ok());
    match (method, path) {
        ("GET", ["connection"]) => Reply::json(
            200,
            &json!({"configured":saved.as_ref().is_some_and(|s|s.host==host),"user":saved.as_ref().map(|s|s.user.as_str()).unwrap_or("root"),"port":saved.as_ref().map(|s|s.port).unwrap_or(22)}),
        ),
        ("DELETE", ["connection"]) => {
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Reply::error(500, "Could not disable OS access"),
            };
            if let Some(s) = saved {
                let _ = std::fs::remove_dir_all(s.directory);
            }
            Reply::json(200, &json!({"configured":false}))
        }
        ("PUT", ["connection"]) => {
            let Ok(input) = serde_json::from_slice::<Enroll>(body) else {
                return Reply::error(
                    400,
                    "Enter SSH user, port, private key and verified known_hosts entry",
                );
            };
            if input.private_key.len() > 16384
                || input.private_key.is_empty()
                || input.known_hosts.is_empty()
                || input.known_hosts.len() > 16384
            {
                return Reply::error(
                    400,
                    "Enter a private SSH key and verified known_hosts entry (maximum 16 KiB each)",
                );
            }
            let Some(parent) = file.parent() else {
                return Reply::error(500, "Missing credential directory");
            };
            let suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = parent.join(format!("coreelec-ssh-{suffix}"));
            let mut staged = Saved {
                host: host.into(),
                user: input.user,
                port: input.port,
                directory: directory.clone(),
            };
            let prepare = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(parent)?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
                std::fs::create_dir(&directory)?;
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
                private_write(&directory.join("key"), input.private_key.as_bytes())?;
                private_write(&directory.join("known_hosts"), input.known_hosts.as_bytes())
            })();
            if prepare.is_err() {
                let _ = std::fs::remove_dir_all(&directory);
                return Reply::error(500, "Could not stage private SSH credentials");
            }
            staged.directory = match std::fs::canonicalize(&directory) {
                Ok(p) => p,
                Err(_) => {
                    let _ = std::fs::remove_dir_all(&directory);
                    return Reply::error(500, "Could not resolve private credential directory");
                }
            };
            let identity = match client(host, port, &staged)
                .and_then(|c| c.identity().map_err(|e| e.to_string()))
            {
                Ok(i) => i,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&directory);
                    return Reply::error(502, &e);
                }
            };
            let temporary = parent.join(format!("coreelec-config-{suffix}.tmp"));
            if private_write(&temporary, &serde_json::to_vec(&staged).unwrap())
                .and_then(|_| std::fs::rename(&temporary, &file))
                .is_err()
            {
                let _ = std::fs::remove_dir_all(&directory);
                let _ = std::fs::remove_file(temporary);
                return Reply::error(500, "SSH test passed but credentials could not be saved");
            }
            if let Some(old) = saved {
                let _ = std::fs::remove_dir_all(old.directory);
            }
            Reply::json(200, &json!({"configured":true,"identity":identity}))
        }
        ("GET", ["status"]) => {
            let Some(saved) = saved else {
                return Reply::error(409, "Enroll optional SSH access first");
            };
            match client(host, port, &saved).and_then(|c| {
                let identity = c.identity().map_err(|e| e.to_string())?;
                let service = c.kodi_service().map_err(|e| e.to_string())?;
                Ok(json!({"identity":identity,"service":service}))
            }) {
                Ok(value) => Reply::json(200, &value),
                Err(e) => Reply::error(502, &e),
            }
        }
        ("POST", ["action"]) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                action: String,
                confirm: bool,
            }
            let Ok(input) = serde_json::from_slice::<Input>(body) else {
                return Reply::error(400, "Choose an action and confirm it");
            };
            if !input.confirm {
                return Reply::error(400, "Confirm this disruptive OS action");
            };
            let action = match input.action.as_str() {
                "restart-kodi" => OsAction::RestartKodi,
                "reboot" => OsAction::Reboot,
                "poweroff" => OsAction::PowerOff,
                _ => return Reply::error(400, "Unknown CoreELEC OS action"),
            };
            let Some(saved) = saved else {
                return Reply::error(409, "Enroll optional SSH access first");
            };
            match client(host, port, &saved)
                .and_then(|c| c.action(action).map_err(|e| e.to_string()))
            {
                Ok(()) => Reply::json(
                    202,
                    &json!({"accepted":true,"message":"Request accepted; refresh status after the device restarts"}),
                ),
                Err(e) => Reply::error(502, &e),
            }
        }
        _ => Reply::error(404, "Unknown CoreELEC operation"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_credentials_are_not_exposed_or_reused_after_address_change() {
        let dir =
            std::env::temp_dir().join(format!("couch-coreelec-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("connection.json");
        let saved = Saved {
            host: "192.0.2.1".into(),
            user: "root".into(),
            port: 22,
            directory: dir.join("private-secret-location"),
        };
        private_write(&file, &serde_json::to_vec(&saved).unwrap()).unwrap();
        let original = std::fs::read(&file).unwrap();
        assert_eq!(
            route(
                "PUT",
                &["connection"],
                br#"{"user":"root","port":22,"private_key":"invalid","known_hosts":""}"#,
                file.clone(),
                "192.0.2.1",
                9090
            )
            .status,
            400
        );
        assert_eq!(std::fs::read(&file).unwrap(), original);
        let response = route("GET", &["connection"], &[], file.clone(), "192.0.2.2", 9090);
        let body = String::from_utf8(response.body).unwrap();
        assert!(!body.contains("private-secret"));
        assert!(
            !serde_json::from_str::<serde_json::Value>(&body).unwrap()["configured"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            route("GET", &["status"], &[], file, "192.0.2.2", 9090).status,
            502
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn enrollment_is_optional_and_mutations_require_confirmation() {
        let file = std::env::temp_dir()
            .join(format!("couch-coreelec-api-{}", std::process::id()))
            .join("coreelec.json");
        let status = route("GET", &["connection"], &[], file.clone(), "192.0.2.1", 9090);
        assert_eq!(status.status, 200);
        assert!(
            !serde_json::from_slice::<serde_json::Value>(&status.body).unwrap()["configured"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            route(
                "POST",
                &["action"],
                br#"{"action":"reboot","confirm":false}"#,
                file.clone(),
                "192.0.2.1",
                9090
            )
            .status,
            400
        );
        assert_eq!(
            route(
                "POST",
                &["action"],
                br#"{"action":"reboot","confirm":true}"#,
                file,
                "192.0.2.1",
                9090
            )
            .status,
            409
        );
    }
}
