/// Use the same tzdata files as the GUI, including Alpine when launched by stage2.
pub(super) fn timezones() -> Vec<String> {
    let mut zones = vec!["UTC".to_string()];
    for root in [
        "/usr/share/zoneinfo",
        "/mnt/alpine/usr/share/zoneinfo",
        "/var/db/timezone/zoneinfo",
    ] {
        if let Ok(text) = std::fs::read_to_string(format!("{root}/zone.tab")) {
            zones.extend(
                text.lines()
                    .filter(|l| !l.starts_with('#'))
                    .filter_map(|l| l.split_whitespace().nth(2))
                    .map(str::to_string),
            );
        }
    }
    zones.sort();
    zones.dedup();
    zones
}

// --- the remote's own settings, mirrored from its Settings menu ---------------
//
// Display, key backlight, standby timeouts and SSH live in the settings file
// couch-system's `ui_settings` owns; the GUI applies a change it notices
// there within a second. Network is read from the kernel's tables the way
// the remote's Network section reads them; Power asks the system service.
use super::Reply;
use couch_system::{
    bluetooth::PairAction,
    client, netinfo,
    power::Action,
    protocol::{Reply as SystemReply, Request},
    ui_settings::{self, Settings, DIM_LABELS, OFF_LABELS},
};

fn ssh_available() -> bool {
    matches!(
        client::call(Request::SshAvailable),
        Ok(SystemReply::Available(true))
    )
}

/// The page's picture of the device settings, choices included so the page
/// needs no copy of the tables.
fn device_view(settings: &Settings, ssh_available: bool) -> serde_json::Value {
    let state = ui_settings::bluetooth_state();
    let pairing = ui_settings::bluetooth_pairing();
    serde_json::json!({
        "brightness": settings.brightness,
        "keys": settings.keys,
        "dim_index": settings.dim_index,
        "off_index": settings.off_index,
        "dim_choices": DIM_LABELS,
        "off_choices": OFF_LABELS,
        "ssh": {
            "available": ssh_available,
            "enabled": settings.ssh,
            "running": ui_settings::sshd_running(),
        },
        "bluetooth": {
            "available": ui_settings::bluetooth_available(),
            "enabled": settings.bluetooth,
            "running": state == ui_settings::BluetoothState::On,
            "state": state.word(),
            "detail": match &state {
                ui_settings::BluetoothState::Error(error) => error.as_str(),
                _ => "",
            },
            // Pairing mode, from the HID daemon's state file: the phase and
            // its detail (the TV's name, or why the window closed), and the
            // TV on the link right now, window or not.
            "pairing": {
                "phase": pairing.phase.word(),
                "detail": pairing.detail,
            },
            "peer": pairing.peer,
        },
    })
}

pub(super) fn device(method: &str, body: &[u8]) -> Reply {
    let available = ssh_available();
    let current = ui_settings::load(Settings::defaults(available));
    match method {
        "GET" => Reply::json(200, &device_view(&current, available)),
        "PUT" => {
            let Ok(wanted) = serde_json::from_slice::<Settings>(body) else {
                return Reply::error(
                    400,
                    "Send brightness, keys, dim_index, off_index, ssh and bluetooth",
                );
            };
            if wanted.brightness % 10 != 0 || !(10..=100).contains(&wanted.brightness) {
                return Reply::error(400, "Brightness is 10 to 100, in tens");
            }
            if wanted.clone().clamped() != wanted {
                return Reply::error(400, "Choose a listed dim or screen-off timeout");
            }
            if wanted.ssh && !available {
                return Reply::error(
                    409,
                    "SSH needs a key or password enrolled from the setup page first",
                );
            }
            if wanted.ssh != current.ssh {
                if let Err(error) = client::action(Request::Ssh {
                    enabled: wanted.ssh,
                }) {
                    return Reply::error(409, &error);
                }
            }
            if wanted.bluetooth != current.bluetooth {
                if let Err(error) = client::action(Request::Bluetooth {
                    enabled: wanted.bluetooth,
                }) {
                    return Reply::error(409, &error);
                }
            }
            if let Err(error) = ui_settings::save(&wanted) {
                return Reply::error(
                    500,
                    format!("Could not save the remote's settings: {error}"),
                );
            }
            Reply::json(200, &device_view(&wanted, available))
        }
        _ => Reply::error(405, "Use GET or PUT"),
    }
}

pub(super) fn network() -> Reply {
    let info = netinfo::current();
    Reply::json(
        200,
        &serde_json::json!({
            "address": info.address,
            "gateway": info.gateway,
            "dns": info.dns,
            "mac": info.mac,
            "web": info.web(),
            "host": netinfo::WEB_HOST,
        }),
    )
}

/// `{"action": "off" | "restart" | "recovery", "confirm": true}`. The page
/// confirms every action; recovery's button says what it means first.
pub(super) fn power(body: &[u8]) -> Reply {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Reply::error(400, "Invalid power request");
    };
    if value["confirm"] != true {
        return Reply::error(400, "Confirm the power action");
    }
    let Ok(action) = serde_json::from_value::<Action>(value["action"].clone()) else {
        return Reply::error(400, "Choose off, restart or recovery");
    };
    match client::call(Request::Power { action }) {
        Ok(SystemReply::Done(Ok(()))) => Reply::json(
            202,
            &serde_json::json!({"accepted": true, "action": action}),
        ),
        Ok(SystemReply::Done(Err(error))) => Reply::error(409, &error),
        _ => Reply::error(503, "System service is unavailable"),
    }
}

/// `{"action": "pair" | "stop" | "forget" | "enter"}`: pairing mode on the
/// HID daemon, through the system service. 202 once the word is on its way;
/// the outcome shows up in `GET /api/remote/device`'s `bluetooth.pairing`.
pub(super) fn bluetooth(body: &[u8]) -> Reply {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Reply::error(400, "Invalid Bluetooth request");
    };
    let Ok(action) = serde_json::from_value::<PairAction>(value["action"].clone()) else {
        return Reply::error(400, "Choose pair, stop, forget or enter");
    };
    match client::call(Request::BluetoothPair { action }) {
        Ok(SystemReply::Done(Ok(()))) => Reply::json(
            202,
            &serde_json::json!({"accepted": true, "action": action}),
        ),
        Ok(SystemReply::Done(Err(error))) => Reply::error(409, &error),
        _ => Reply::error(503, "System service is unavailable"),
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    fn device_put_rejects_out_of_range_and_unknown_fields_before_touching_anything() {
        for body in [
            r#"{"brightness":55,"keys":true,"dim_index":1,"off_index":3,"ssh":false}"#,
            r#"{"brightness":50,"keys":true,"dim_index":9,"off_index":3,"ssh":false}"#,
            r#"{"brightness":50,"keys":true,"dim_index":1,"off_index":3,"ssh":false,"extra":1}"#,
            r#"not json"#,
        ] {
            let reply = device("PUT", body.as_bytes());
            assert_eq!(reply.status, 400, "{body}");
        }
    }

    #[test]
    fn power_requires_confirmation_and_a_known_action() {
        assert_eq!(power(br#"{"action":"restart"}"#).status, 400);
        assert_eq!(power(br#"{"action":"halt","confirm":true}"#).status, 400);
        assert_eq!(power(b"{").status, 400);
    }

    #[test]
    fn bluetooth_pairing_takes_only_the_named_actions() {
        // The page cannot type a word for the daemon's socket: a raw key
        // name, an unknown action and no action at all stop here, before
        // the system service is asked anything.
        for body in [
            br#"{"action":"kbd:28"}"#.as_slice(),
            br#"{"action":"vol+"}"#,
            br#"{"action":"Pair"}"#,
            br#"{}"#,
            b"{",
        ] {
            assert_eq!(bluetooth(body).status, 400, "{}", String::from_utf8_lossy(body));
        }
    }
}
