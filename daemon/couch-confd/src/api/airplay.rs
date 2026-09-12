//! Optional read-only metadata pairing. Never exports keys or sends playback commands.
use super::*;
use couch_appletv::metadata::{self, StoredConnection};
fn response(c: &StoredConnection) -> Reply {
    Reply::json(
        200,
        &json!({"paired":true,"address":c.settings.address.to_string(),"port":c.settings.airplay_port,"experimental":true}),
    )
}
pub(super) fn route(method: &str, path: &[&str], body: &[u8], file: PathBuf) -> Reply {
    if (method, path) == ("GET", &["discover"][..]) {
        return discover_service(metadata::MDNS_SERVICE);
    }
    let lock = super::super::connections::lock_for(&file);
    let Ok(_guard) = lock.try_lock() else {
        return Reply::error(409, "Metadata connection is busy");
    };
    match (method, path) {
        ("GET", ["connection"]) => {
            return StoredConnection::load(&file)
                .map(|c| response(&c))
                .unwrap_or_else(|_| Reply::json(200, &json!({"paired":false,"experimental":true})))
        }
        ("DELETE", ["pairing"]) => {
            sessions().lock().unwrap().remove(&file);
            return Reply::json(200, &json!({"cancelled":true}));
        }
        ("DELETE", ["connection"]) => {
            sessions().lock().unwrap().remove(&file);
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Reply::error(500, "Cannot remove private metadata pairing"),
            }
            return Reply::json(200, &json!({"paired":false}));
        }
        ("POST", ["pair-start"]) => {
            let Ok(input) = serde_json::from_slice::<Start>(body) else {
                return Reply::error(400, "Enter the AirPlay address and port");
            };
            let Some(address) = address(&input.address) else {
                return Reply::error(400, "Enter the Apple TV LAN address");
            };
            let Some(port) = input.port.filter(|p| *p > 0) else {
                return Reply::error(
                    400,
                    "Enter the advertised AirPlay port, not the Companion port",
                );
            };
            let control_file = file.with_file_name("appletv-connection.json");
            if !StreamingConnection::load(&control_file)
                .ok()
                .is_some_and(|c| c.kind() == "appletv" && c.address() == address)
            {
                return Reply::error(
                    409,
                    "Pair Companion controls for this Apple TV address first",
                );
            }
            let Ok(token) = token() else {
                return Reply::error(500, "Cannot create pairing session");
            };
            {
                let mut sessions = sessions().lock().unwrap();
                sessions.retain(|_, v| v.deadline > Instant::now());
                if sessions.len() >= 8 && !sessions.contains_key(&file) {
                    return Reply::error(409, "Finish or cancel a pending pairing first");
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
            let settings = metadata::Settings {
                address,
                airplay_port: port,
            };
            match metadata::Pairing::begin(&settings) {
                Ok(pair) => {
                    let mut sessions = sessions().lock().unwrap();
                    let Some(p) = sessions.get_mut(&file) else {
                        return Reply::error(408, "Pairing expired");
                    };
                    p.pair = Some(Pair::AirPlay(pair, settings));
                    p.deadline = Instant::now() + Duration::from_secs(120);
                    return Reply::json(
                        200,
                        &json!({"token":token,"code_length":4,"expires_in":120}),
                    );
                }
                Err(e) => {
                    sessions().lock().unwrap().remove(&file);
                    return Reply::error(502, e.to_string());
                }
            }
        }
        ("POST", ["pair-finish"]) => {
            let Ok(input) = serde_json::from_slice::<Finish>(body) else {
                return Reply::error(400, "Enter the displayed PIN");
            };
            if !valid_code(&input.code, true) {
                return Reply::error(400, "Enter the four-digit PIN");
            }
            let pending = {
                let mut sessions = sessions().lock().unwrap();
                let Some(p) = sessions.get(&file) else {
                    return Reply::error(409, "Start metadata pairing first");
                };
                if p.deadline <= Instant::now() {
                    sessions.remove(&file);
                    return Reply::error(408, "Pairing expired");
                }
                if p.token != input.token {
                    return Reply::error(409, "Pairing session changed");
                }
                sessions.remove(&file).unwrap()
            };
            let Some(Pair::AirPlay(pair, settings)) = pending.pair else {
                return Reply::error(409, "Metadata pairing is not ready");
            };
            let credentials = match pair.finish(&input.code) {
                Ok(c) => c,
                Err(e) => return Reply::error(502, format!("{e}. Start metadata pairing again.")),
            };
            let c = StoredConnection {
                settings,
                credentials,
            };
            if c.save(&file).is_err() {
                return Reply::error(
                    500,
                    "Pairing succeeded but private credentials could not be saved",
                );
            }
            return response(&c);
        }
        ("GET", ["status"]) => {}
        _ => return Reply::error(404, "Unknown metadata operation"),
    }
    let Ok(c) = StoredConnection::load(&file) else {
        return Reply::error(409, "Pair optional AirPlay metadata first");
    };
    if !StreamingConnection::load(&file.with_file_name("appletv-connection.json"))
        .ok()
        .is_some_and(|control| {
            control.kind() == "appletv" && control.address() == c.settings.address
        })
    {
        return Reply::error(
            409,
            "Metadata pairing belongs to a different control address; pair it again",
        );
    }
    let result = (|| {
        let mut client = metadata::Client::connect(&c.settings, &c.credentials)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            client.poll()?;
        }
        let snapshot = client.now_playing();
        client.close();
        Ok::<_, couch_appletv::Error>(snapshot)
    })();
    match result {
        Ok(v) => Reply::json(200, &json!({"now_playing":v,"experimental":true})),
        Err(e) => Reply::error(502, e.to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_status_never_exports_credentials_and_requires_matching_companion() {
        let dir = std::env::temp_dir().join(format!("couch-airplay-private-{}", token().unwrap()));
        let file = dir.join("appletv-metadata-connection.json");
        let credentials = couch_appletv::Credentials {
            client_id: b"fixture-client".to_vec(),
            device_id: b"fixture-device".to_vec(),
            client_secret: [7; 32],
            device_public: [8; 32],
        };
        let stored:StoredConnection=serde_json::from_value(json!({"settings":{"address":"192.0.2.1","airplay_port":7000},"credentials":credentials})).unwrap();
        stored.save(&file).unwrap();
        let reply = route("GET", &["connection"], b"", file.clone());
        let value: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(
            value,
            json!({"paired":true,"address":"192.0.2.1","port":7000,"experimental":true})
        );
        let control_file = dir.join("appletv-connection.json");
        StreamingConnection::AppleTv {
            settings: couch_appletv::Settings {
                address: "192.0.2.2".parse().unwrap(),
                companion_port: 49152,
            },
            credentials,
        }
        .save(&control_file)
        .unwrap();
        let before = std::fs::read(&control_file).unwrap();
        assert_eq!(route("GET", &["status"], b"", file.clone()).status, 409);
        assert_eq!(
            route("DELETE", &["connection"], b"", file.clone()).status,
            200
        );
        assert_eq!(std::fs::read(control_file).unwrap(), before);
        assert!(!file.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn metadata_rejects_commands_and_invalid_pairing_without_network() {
        let file = std::env::temp_dir()
            .join(format!("couch-airplay-{}", token().unwrap()))
            .join("appletv-metadata-connection.json");
        assert_eq!(route("GET", &["connection"], b"", file.clone()).status, 200);
        for body in [
            r#"{"address":"127.0.0.1","port":7000}"#,
            r#"{"address":"192.0.2.1","port":0}"#,
            r#"{"address":"192.0.2.1"}"#,
        ] {
            assert_eq!(
                route("POST", &["pair-start"], body.as_bytes(), file.clone()).status,
                400
            );
        }
        assert_eq!(
            route(
                "POST",
                &["pair-start"],
                br#"{"address":"192.0.2.1","port":7000}"#,
                file.clone()
            )
            .status,
            409
        );
        assert_eq!(
            route("POST", &["command"], br#"{"command":"play"}"#, file.clone()).status,
            404
        );
        assert_eq!(route("GET", &["status"], b"", file.clone()).status, 409);
        sessions().lock().unwrap().insert(
            file.clone(),
            Pending {
                token: "fixture".into(),
                deadline: Instant::now() + Duration::from_secs(20),
                pair: None,
            },
        );
        assert_eq!(
            route(
                "POST",
                &["pair-finish"],
                br#"{"token":"wrong","code":"1234"}"#,
                file.clone()
            )
            .status,
            409
        );
        assert!(sessions().lock().unwrap().contains_key(&file));
        assert_eq!(
            route("DELETE", &["connection"], b"", file.clone()).status,
            200
        );
        assert!(!sessions().lock().unwrap().contains_key(&file));
    }
}
