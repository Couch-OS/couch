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
