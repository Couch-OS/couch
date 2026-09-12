//! Experimental Android/Apple TV pairing. Private material never enters HTTP
//! responses or exported configuration. A short-lived socket spans PIN entry.
#[path = "airplay.rs"]
mod airplay;
use super::Reply;
use couch_control::{StreamingConnection, StreamingTv};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    io::Read,
    net::IpAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
enum Pair {
    AirPlay(
        couch_appletv::metadata::Pairing,
        couch_appletv::metadata::Settings,
    ),
    Android(couch_androidtv::Pairing, couch_androidtv::Settings),
    Apple(couch_appletv::Pairing, couch_appletv::Settings),
}
struct Pending {
    token: String,
    deadline: Instant,
    pair: Option<Pair>,
}
type Sessions = Arc<Mutex<HashMap<PathBuf, Pending>>>;
fn sessions() -> &'static Sessions {
    static SESSIONS: OnceLock<Sessions> = OnceLock::new();
    SESSIONS.get_or_init(|| {
        let map: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let worker = map.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            if let Ok(mut sessions) = worker.lock() {
                sessions.retain(|_, v| v.deadline > Instant::now());
            }
        });
        map
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    address: String,
    #[serde(default)]
    port: Option<u16>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    token: String,
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    command: String,
}
fn address(input: &str) -> Option<IpAddr> {
    let address: IpAddr = input.trim().parse().ok()?;
    (!address.is_unspecified() && !address.is_multicast() && !address.is_loopback())
        .then_some(address)
}
fn token() -> std::io::Result<String> {
    let mut random = [0; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(random.iter().map(|b| format!("{b:02x}")).collect())
}
fn response(connection: &StreamingConnection) -> Reply {
    Reply::json(
        200,
        &json!({"paired":true,"address":connection.address().to_string(),"port":connection.port(),"experimental":true}),
    )
}
fn discover(apple: bool) -> Reply {
    discover_service(if apple {
        couch_appletv::MDNS_SERVICE
    } else {
        couch_androidtv::MDNS_SERVICE
    })
}
fn discover_service(service: &str) -> Reply {
    let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
        return Reply::error(
            503,
            "Network discovery is unavailable; enter the TV address manually",
        );
    };
    let result = (|| {
        let Ok(events) = daemon.browse(service) else {
            return Reply::error(503, "Cannot search for TVs on this network");
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut found = std::collections::BTreeMap::new();
        while Instant::now() < deadline && found.len() < 64 {
            if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) =
                events.recv_timeout(Duration::from_millis(100))
            {
                if info.get_port() == 0 {
                    continue;
                }
                let name = info
                    .get_fullname()
                    .trim_end_matches(service)
                    .trim_end_matches('.')
                    .chars()
                    .take(128)
                    .collect::<String>();
                for ip in info.get_addresses_v4() {
                    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                        continue;
                    }
                    found.insert(
                        (ip.to_string(), info.get_port()),
                        json!({"name":name,"address":ip.to_string(),"port":info.get_port()}),
                    );
                }
            }
        }
        Reply::json(200, &found.into_values().collect::<Vec<_>>())
    })();
    if let Ok(done) = daemon.shutdown() {
        let _ = done.recv_timeout(Duration::from_secs(1));
    }
    result
}
pub(super) fn route(method: &str, path: &[&str], body: &[u8], file: PathBuf, apple: bool) -> Reply {
    if apple && path.first() == Some(&"metadata") {
        return airplay::route(
            method,
            &path[1..],
            body,
            file.with_file_name("appletv-metadata-connection.json"),
        );
    }
    if method == "GET" && path == ["discover"] {
        return discover(apple);
    }
    let connection_lock = super::connections::lock_for(&file);
    let Ok(_guard) = connection_lock.try_lock() else {
        return Reply::error(409, "TV connection is busy; wait for the current operation");
    };
    if method == "GET" && path == ["connection"] {
        return match StreamingConnection::load(&file) {
            Ok(c) => response(&c),
            Err(_) => Reply::json(200, &json!({"paired":false,"experimental":true})),
        };
    }
    if method == "DELETE" && path == ["pairing"] {
        sessions().lock().unwrap().remove(&file);
        return Reply::json(200, &json!({"cancelled":true}));
    }
    if method == "POST" && path == ["pair-start"] {
        let Ok(input) = serde_json::from_slice::<Start>(body) else {
            return Reply::error(400, "Enter the TV address and port");
        };
        let Some(address) = address(&input.address) else {
            return Reply::error(400, "Enter the TV’s LAN IPv4 or IPv6 address");
        };
        let port = input.port.unwrap_or(if apple { 0 } else { 6466 });
        if port == 0 {
            return Reply::error(400, "Enter the Companion port advertised by the Apple TV");
        }
        let Ok(token) = token() else {
            return Reply::error(500, "Cannot create a pairing session");
        };
        {
            let mut sessions = sessions().lock().unwrap();
            sessions.retain(|_, v| v.deadline > Instant::now());
            if sessions.len() >= 8 && !sessions.contains_key(&file) {
                return Reply::error(
                    409,
                    "Too many pending pairing sessions; finish or cancel one",
                );
            }
            sessions.insert(
                file.clone(),
                Pending {
                    token: token.clone(),
                    deadline: Instant::now() + Duration::from_secs(120),
                    pair: None,
                },
            );
        }
        let result = if apple {
            let settings = couch_appletv::Settings {
                address,
                companion_port: port,
            };
            couch_appletv::Pairing::begin(&settings)
                .map(|p| Pair::Apple(p, settings))
                .map_err(|e| e.to_string())
        } else {
            let settings = couch_androidtv::Settings {
                address,
                remote_port: port,
                pairing_port: 6467,
            };
            couch_androidtv::Identity::generate()
                .and_then(|identity| couch_androidtv::Pairing::begin(&settings, identity, "couch."))
                .map(|p| Pair::Android(p, settings))
                .map_err(|e| e.to_string())
        };
        match result {
            Ok(pair) => {
                let mut sessions = sessions().lock().unwrap();
                let Some(pending) = sessions.get_mut(&file) else {
                    return Reply::error(408, "Pairing expired; start again");
                };
                pending.pair = Some(pair);
                pending.deadline = Instant::now() + Duration::from_secs(120);
                return Reply::json(
                    200,
                    &json!({"token":token,"code_length":if apple{4}else{6},"expires_in":120}),
                );
            }
            Err(error) => {
                sessions().lock().unwrap().remove(&file);
                return Reply::error(502, error);
            }
        }
    }
    if method == "POST" && path == ["pair-finish"] {
        let Ok(input) = serde_json::from_slice::<Finish>(body) else {
            return Reply::error(400, "Enter the code displayed by the TV");
        };
        if !valid_code(&input.code, apple) {
            return Reply::error(
                400,
                if apple {
                    "Enter the four-digit PIN"
                } else {
                    "Enter the six-character hexadecimal code"
                },
            );
        }
        let pending = {
            let mut sessions = sessions().lock().unwrap();
            let Some(pending) = sessions.get(&file) else {
                return Reply::error(409, "Start pairing first");
            };
            if pending.deadline <= Instant::now() {
                sessions.remove(&file);
                return Reply::error(408, "Pairing expired; start again");
            };
            if pending.token != input.token {
                return Reply::error(409, "This pairing session changed; start again");
            };
            sessions.remove(&file).unwrap()
        };
        let result = match pending.pair {
            Some(Pair::Android(pair, settings)) => pair
                .finish(&input.code)
                .map(|credentials| StreamingConnection::AndroidTv {
                    settings,
                    credentials,
                })
                .map_err(|e| e.to_string()),
            Some(Pair::Apple(pair, settings)) => pair
                .finish(&input.code)
                .map(|credentials| StreamingConnection::AppleTv {
                    settings,
                    credentials,
                })
                .map_err(|e| e.to_string()),
            None | Some(Pair::AirPlay(..)) => return Reply::error(409, "Pairing is not ready"),
        };
        let connection = match result {
            Ok(c) => c,
            Err(error) => {
                return Reply::error(502, format!("{error}. Start pairing again to retry."))
            }
        };
        let Some(parent) = file.parent() else {
            return Reply::error(500, "Invalid private connection path");
        };
        if std::fs::create_dir_all(parent)
            .and_then(|_| std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)))
            .is_err()
            || connection.save(&file).is_err()
        {
            return Reply::error(
                500,
                "TV paired but credentials could not be saved; start pairing again",
            );
        };
        return response(&connection);
    }
    let apps = apple && method == "GET" && path == ["apps"];
    if !apps && !matches!((method, path), ("GET", ["status"]) | ("POST", ["command"])) {
        return Reply::error(404, "Unknown streaming TV operation");
    }
    let command = if method == "POST" {
        let Ok(command) = serde_json::from_slice::<Command>(body) else {
            return Reply::error(400, "Invalid TV command");
        };
        let integration = if apple {
            couch_model::Integration::AppleTv
        } else {
            couch_model::Integration::AndroidTv
        };
        if !couch_model::commands::Function::parse(&command.command)
            .is_some_and(|f| f.supports(&integration))
        {
            return Reply::error(400, "Unsupported TV command");
        };
        Some(command.command)
    } else {
        None
    };
    let connection = match StreamingConnection::load(&file) {
        Ok(c) => c,
        Err(_) => return Reply::error(400, "Pair this TV first"),
    };
    if (connection.kind() == "appletv") != apple {
        return Reply::error(400, "Saved credentials do not match this connection type");
    }
    let client = match StreamingTv::connect(&connection) {
        Ok(c) => c,
        Err(e) => return Reply::error(502, e.to_string()),
    };
    let result = match command {
        Some(command) => client.command(&command).map(|_| json!({"accepted":true})),
        None if apps => client.apps(),
        None => client.status(),
    };
    match result {
        Ok(value) => Reply::json(200, &value),
        Err(e) => Reply::error(502, e.to_string()),
    }
}
fn valid_code(code: &str, apple: bool) -> bool {
    code.len() == if apple { 4 } else { 6 }
        && code.bytes().all(|b| {
            if apple {
                b.is_ascii_digit()
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_discovery_requires_apple_pairing_and_never_falls_back_to_android() {
        let missing =
            std::env::temp_dir().join(format!("couch-unpaired-test-{}.json", token().unwrap()));
        assert!(!missing.exists());
        assert_eq!(
            route("GET", &["apps"], b"", missing.clone(), true).status,
            400
        );
        assert_eq!(
            route("GET", &["apps"], b"", missing.clone(), false).status,
            404
        );
        assert_eq!(route("POST", &["apps"], b"", missing, true).status, 404);
    }
    #[test]
    fn only_literal_lan_addresses_are_accepted() {
        for bad in [
            "localhost",
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "https://192.168.1.2",
        ] {
            assert!(address(bad).is_none());
        }
        assert!(address("192.168.1.2").is_some());
    }
    #[test]
    fn pairing_codes_are_strict_and_provider_specific() {
        assert!(valid_code("12aBcD", false));
        assert!(!valid_code("123456", true));
        assert!(valid_code("0012", true));
        assert!(!valid_code("1Z2345", false));
    }
    #[test]
    fn status_never_exports_keys_and_wrong_pairing_token_keeps_session() {
        let root = std::env::temp_dir().join(format!("couch-pairing-test-{}", token().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("appletv-connection.json");
        let connection = StreamingConnection::AppleTv {
            settings: couch_appletv::Settings {
                address: "192.168.1.2".parse().unwrap(),
                companion_port: 49152,
            },
            credentials: couch_appletv::Credentials {
                client_id: b"secret-client-id".to_vec(),
                client_secret: [7; 32],
                device_id: b"secret-device-id".to_vec(),
                device_public: [8; 32],
            },
        };
        connection.save(&file).unwrap();
        let reply = route("GET", &["connection"], &[], file.clone(), true);
        assert_eq!(reply.status, 200);
        let text = String::from_utf8(reply.body).unwrap();
        assert!(!text.contains("secret-client-id"));
        assert!(!text.contains("client_secret"));
        assert!(text.contains("192.168.1.2"));
        sessions().lock().unwrap().insert(
            file.clone(),
            Pending {
                token: "correct".into(),
                deadline: Instant::now() + Duration::from_secs(60),
                pair: None,
            },
        );
        let reply = route(
            "POST",
            &["pair-finish"],
            br#"{"token":"wrong","code":"1234"}"#,
            file.clone(),
            true,
        );
        assert_eq!(reply.status, 409);
        assert!(sessions().lock().unwrap().contains_key(&file));
        route("DELETE", &["pairing"], &[], file.clone(), true);
        assert!(!sessions().lock().unwrap().contains_key(&file));
        std::fs::remove_dir_all(root).unwrap();
    }
}
