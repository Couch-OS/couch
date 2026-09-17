//! Actual production subprocess and native adapter, against simulated receivers.
use couch_plugin::{
    testing::{Adapter, Package},
    Error, Request, TypedAction, VolumeDb,
};
use couch_sdk::{
    testing::{MockHost, Reply, Script},
    DeviceClient,
};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

fn package() -> Package {
    Package::new(Adapter {
        binary: Path::new(env!("CARGO_BIN_EXE_couch-plugin-denon")),
        manifest_json: include_str!("../plugin.json"),
    })
}
fn settings(device: &MockHost) -> Value {
    json!({"host":device.host(),"port":device.port()})
}
fn state_script(volume: &str) -> Script {
    Script::new()
        .terminator(b'\r')
        .on("ZM?", Reply::line("ZMON"))
        .on("MV?", Reply::line(volume))
        .on("MU?", Reply::line("MUOFF"))
        .on("SI?", Reply::line("SIHD RADIO"))
}

#[test]
fn native_and_packaged_measurements_match_including_minimum_and_spaced_sources() {
    let package = package();
    for (wire, expected) in [
        ("MV455", VolumeDb::Reading { tenths: -345 }),
        ("MV00", VolumeDb::Minimum),
    ] {
        let native_device = MockHost::start(state_script(wire));
        let mut native = <couch_denon::Client as DeviceClient>::connect(
            &serde_json::from_value(settings(&native_device)).unwrap(),
        )
        .unwrap();
        let native_state = DeviceClient::status(&mut native).unwrap();
        let device = MockHost::start(state_script(wire));
        let mut host = package.host();
        host.configure(settings(&device)).unwrap();
        let state = host.status().unwrap();
        assert_eq!(state, native_state);
        assert_eq!(state.volume_db, Some(expected));
        assert_eq!(state.volume, None);
        assert_eq!(state.input.as_deref(), Some("HD RADIO"));
        assert_eq!(device.requests(), ["ZM?", "MV?", "MU?", "SI?"]);
    }
}

#[test]
fn absolute_half_step_and_minimum_are_confirmed_once_and_spaced_binding_is_exact() {
    let package = package();
    let device = MockHost::start(
        Script::new()
            .terminator(b'\r')
            .on("MV455", Reply::line("MV455"))
            .on("MV?", Reply::line("MV455"))
            .on("MV00", Reply::line("MV00"))
            .on("MV?", Reply::line("MV00"))
            .on("SIHD RADIO", Reply::line("SIHD RADIO"))
            .on("SI?", Reply::line("SIHD RADIO")),
    );
    let mut host = package.host();
    host.configure(settings(&device)).unwrap();
    for tenths in [-805, -344, 185] {
        assert_eq!(
            host.action(TypedAction::SetVolumeDb { tenths }),
            Err(Error::Invalid)
        );
    }
    assert!(device.requests().is_empty());
    host.action(TypedAction::SetVolumeDb { tenths: -345 })
        .unwrap();
    host.action(TypedAction::SetVolumeDb { tenths: -800 })
        .unwrap();
    host.command("input:HD RADIO").unwrap();
    assert_eq!(
        device.requests(),
        ["MV455", "MV?", "MV00", "MV?", "SIHD RADIO", "SI?"]
    );
}

#[test]
fn typed_disconnect_malformed_confirmation_and_timeout_never_retry() {
    let package = package();
    for (script, error, expected) in [
        (
            Script::new().on("MV455", Reply::Close),
            Error::Transport,
            vec!["MV455"],
        ),
        (
            Script::new()
                .on("MV455", Reply::Silence)
                .on("MV?", Reply::line("X".repeat(1025))),
            Error::Protocol,
            vec!["MV455", "MV?"],
        ),
        (
            Script::new().otherwise(Reply::Silence),
            Error::Timeout,
            vec!["MV455", "MV?"],
        ),
    ] {
        let device = MockHost::start(script.terminator(b'\r'));
        let endpoint = package.endpoint(settings(&device), Duration::from_millis(600));
        assert_eq!(
            endpoint.request(Request::Action {
                action: TypedAction::SetVolumeDb { tenths: -345 }
            }),
            Err(error)
        );
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(device.requests(), expected);
    }
}

/// Raw framed peer deliberately bypasses Host's own admission/action guards.
/// The server must independently refuse before even opening the AVR socket.
struct Raw(std::process::Child);
impl Raw {
    fn start() -> Self {
        Self(
            std::process::Command::new(env!("CARGO_BIN_EXE_couch-plugin-denon"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
    fn exchange(&mut self, id: u64, body: Value) -> Value {
        couch_plugin::write_frame(
            self.0.stdin.as_mut().unwrap(),
            &json!({"id":id,"body":body}),
        )
        .unwrap();
        couch_plugin::read_frame::<_, Value>(self.0.stdout.as_mut().unwrap()).unwrap()["body"]
            .clone()
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn v2_server_refuses_downgrade_missing_handshake_bad_values_and_ambiguous_input_before_connect() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut raw = Raw::start();
    assert_eq!(
        raw.exchange(1, json!({"method":"status"})),
        json!({"type":"error","code":"incompatible"})
    );
    assert_eq!(
        raw.exchange(2, json!({"method":"hello","protocol_version":1})),
        json!({"type":"error","code":"incompatible"})
    );
    assert_eq!(
        raw.exchange(3, json!({"method":"hello","protocol_version":2}))["type"],
        "hello"
    );
    assert_eq!(raw.exchange(4,json!({"method":"configure","settings":{"host":"127.0.0.1","port":listener.local_addr().unwrap().port()}})),json!({"type":"ok"}));
    for (id, tenths) in [(5, -805), (6, -344), (7, 185)] {
        assert_eq!(
            raw.exchange(
                id,
                json!({"method":"action","action":{"action":"set_volume_db","tenths":tenths}})
            ),
            json!({"type":"error","code":"invalid"})
        );
    }
    assert_eq!(
        raw.exchange(8, json!({"method":"command","function":"input: HD RADIO"})),
        json!({"type":"error","code":"unsupported"})
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    // Malformed typed JSON terminates the framed session, before any socket.
    couch_plugin::write_frame(raw.0.stdin.as_mut().unwrap(),&json!({"id":9,"body":{"method":"action","action":{"action":"set_volume_db","tenths":-34.5}}})).unwrap();
    assert!(couch_plugin::read_frame::<_, Value>(raw.0.stdout.as_mut().unwrap()).is_err());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
