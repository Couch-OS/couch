//! Protocol 3, end to end, with the switch on: this tree's host, the SDK's
//! `serve` loop in a real subprocess, and a fake television.
//!
//! Protocol 3 is unreleased. This file is built only with
//! `--features protocol-3-preview`; `tests/plugin.rs` holds the other half,
//! that with the switch off the same manifest is refused.

use couch_plugin::{
    testing::{self, Adapter, ConformanceCase},
    Error, Failure, KeyPhase, Reason, Request, Response,
};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::json;
use std::path::Path;

fn adapter() -> Adapter<'static> {
    Adapter {
        binary: Path::new(env!("CARGO_BIN_EXE_couch-plugin-echo-v3")),
        manifest_json: include_str!("fixtures/plugin-v3.json"),
    }
}

fn settings(device: &MockHost) -> serde_json::Value {
    json!({"host":device.host(),"port":device.port()})
}

fn because(code: Error, text: &str) -> Failure {
    Failure {
        code,
        reason: Some(Reason::Message { text: text.into() }),
    }
}

#[test]
fn the_fixture_passes_the_same_conformance_case_as_a_protocol_1_package() {
    testing::conformance(
        adapter(),
        ConformanceCase {
            offline_settings: json!({"host":"127.0.0.1","port":1,"token":"private"}),
            invalid_settings: json!({"host":"127.0.0.1","port":0}),
            device_settings: settings,
            script: Script::new()
                .terminator(b'\n')
                .on("CMD x:info", Reply::line("OK"))
                .on("GET STATUS", Reply::line("STATUS power=on;volume=31"))
                .on("LIST INPUTS", Reply::Lines(vec!["END".into()])),
            command: "x:info",
            expected_requests: &["CMD x:info", "GET STATUS", "LIST INPUTS"],
            check: |status, inputs| {
                assert_eq!(status.volume, Some(31));
                assert!(inputs.is_empty());
            },
        },
    );
}

#[test]
fn the_package_is_told_how_the_key_was_pressed_and_answers_to_its_own_button() {
    let device = MockHost::start(Script::new().terminator(b'\n').otherwise(Reply::line("OK")));
    let package = testing::Package::new(adapter());
    let mut host = package.host();
    host.configure(settings(&device)).unwrap();
    host.command("x:info").unwrap();
    host.key("x:info", KeyPhase::LongPress).unwrap();
    host.key("volume-up", KeyPhase::Repeat).unwrap();
    host.key("volume-up", KeyPhase::Tap).unwrap();
    // Undeclared, malformed, and another package's word: none costs a round
    // trip, and none reaches the television.
    for function in ["x:other", "x:Info", "mute-on"] {
        assert_eq!(
            host.key(function, KeyPhase::LongPress),
            Err(Error::Unsupported)
        );
    }
    assert_eq!(
        device.requests(),
        [
            "CMD x:info",
            "CMD x:info long_press",
            "CMD volume-up repeat",
            "CMD volume-up"
        ]
    );
}

#[test]
fn a_refusal_carries_its_reason_through_the_host_and_the_endpoint() {
    let device = MockHost::start(
        Script::new()
            .terminator(b'\n')
            // A rule answers once; each of these is asked twice per session.
            .on("CMD power-off", Reply::line("ERR the TV is locked"))
            .on("CMD power-off", Reply::line("ERR the TV is locked"))
            .on("CMD power-on", Reply::line("ERR unpaired"))
            .on("CMD power-on", Reply::line("ERR unpaired"))
            .on("CMD power-on long_press", Reply::line("ERR unpaired"))
            .on(
                "CMD mute",
                Reply::line(format!("ERR {}", "long ".repeat(40))),
            )
            .on("CMD home", Reply::line("OK")),
    );
    let package = testing::Package::new(adapter());
    let mut host = package.host();

    // A setting the package blames by name, before any device I/O.
    assert_eq!(
        host.request_detailed(Request::Configure {
            settings: json!({"host":"127.0.0.1","port":0})
        }),
        Err(Failure {
            code: Error::Invalid,
            reason: Some(Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into()
            })
        })
    );
    assert!(device.requests().is_empty());

    host.configure(settings(&device)).unwrap();
    assert_eq!(
        host.request_detailed(Request::command("power-off")),
        Err(because(Error::Rejected, "the TV is locked"))
    );
    assert_eq!(
        host.request_detailed(Request::command("power-on")),
        Err(because(Error::Unpaired, "Pair this TV again"))
    );
    // The plain calls are unchanged: the code, alone.
    assert_eq!(host.command("power-off"), Err(Error::Rejected));
    assert_eq!(host.command("power-on"), Err(Error::Unpaired));
    // Words too long to show are dropped by the SDK, so the package is not
    // retired over them: the code still arrives and the child lives on.
    assert_eq!(
        host.request_detailed(Request::command("mute")),
        Err(Error::Rejected.into())
    );
    assert!(host.is_alive());
    host.command("home").unwrap();

    let endpoint = package.endpoint(settings(&device), std::time::Duration::from_secs(5));
    assert_eq!(
        endpoint.request_detailed(Request::key("power-on", KeyPhase::LongPress)),
        Err(because(Error::Unpaired, "Pair this TV again"))
    );
    assert_eq!(
        endpoint.request(Request::command("power-on")),
        Err(Error::Unpaired)
    );
    assert_eq!(
        endpoint.request_detailed(Request::command("home")),
        Ok(Response::Ok)
    );
}

/// The panel's leg, which the daemon sits in the middle of: a key goes in over
/// the local socket with its phase, and a refusal comes back with its reason.
/// The relay here is the daemon's (`couch-confd`'s `relay`: the endpoint's
/// detailed answer, a failure sent on whole); the daemon itself is never built
/// with the preview on, so this is where that leg meets a protocol 3 package.
#[test]
fn the_panel_socket_carries_the_phase_in_and_the_reason_out() {
    use couch_plugin::LocalRequest;
    use std::{os::unix::net::UnixListener, time::Duration};
    let device = MockHost::start(
        Script::new()
            .terminator(b'\n')
            .on("CMD volume-up repeat", Reply::line("OK"))
            .on("CMD x:info long_press", Reply::line("OK"))
            .on("CMD power-off", Reply::line("ERR the TV is locked"))
            .on("CMD power-on long_press", Reply::line("ERR unpaired")),
    );
    let package = testing::Package::new(adapter());
    let endpoint = package.endpoint(settings(&device), Duration::from_secs(5));
    let directory = std::env::temp_dir().join(format!("couch-echo-panel-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let socket = directory.join("p.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon = std::thread::spawn(move || {
        // Answered streams stay open to the end: macOS will not set a deadline
        // on a socket whose peer has gone, which the asking side does. Dropping
        // one here, on this thread, as soon as the loop finishes could still
        // race the main thread's read of that very reply, so the vector is
        // handed back through the join instead of being dropped on this stack;
        // it is only closed once the caller has read every answer.
        let mut answered = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let request: LocalRequest =
                couch_plugin::read_frame_timeout(&mut stream, Duration::from_secs(2)).unwrap();
            assert_eq!(request.connection_id, "bedroom-tv");
            let response = endpoint
                .request_detailed(request.request)
                .unwrap_or_else(Response::error);
            couch_plugin::write_frame_timeout(&mut stream, &response, Duration::from_secs(2))
                .unwrap();
            answered.push(stream);
        }
        answered
    });
    let ask = |request| {
        couch_plugin::local_request_detailed(&socket, "bedroom-tv", request, Duration::from_secs(5))
    };
    assert_eq!(
        ask(Request::key("volume-up", KeyPhase::Repeat)),
        Ok(Response::Ok)
    );
    assert_eq!(
        ask(Request::key("x:info", KeyPhase::LongPress)),
        Ok(Response::Ok)
    );
    assert_eq!(
        ask(Request::command("power-off")),
        Err(because(Error::Rejected, "the TV is locked"))
    );
    assert_eq!(
        ask(Request::key("power-on", KeyPhase::LongPress)),
        Err(because(Error::Unpaired, "Pair this TV again"))
    );
    // Every read above is done; only now may the streams the daemon answered
    // on be closed.
    let answered = daemon.join().unwrap();
    assert_eq!(
        device.requests(),
        [
            "CMD volume-up repeat",
            "CMD x:info long_press",
            "CMD power-off",
            "CMD power-on long_press"
        ]
    );
    drop(answered);
    let _ = std::fs::remove_dir_all(directory);
}
