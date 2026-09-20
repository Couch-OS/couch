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

// ---------------------------------------------------------------------------
// A connection with children: the fake bridge.
// ---------------------------------------------------------------------------

use couch_echo::v3::{catalogue, Hostile, CHILDREN, GROUPS, LAMPS, SCENES};
use couch_plugin::{
    list_children,
    testing::FakeDevice,
    testing_v3::{self, ChildrenCase},
    ChildPage, Host, TypedAction, MAX_PAGE,
};

fn bridge() -> Adapter<'static> {
    Adapter {
        binary: Path::new(env!("CARGO_BIN_EXE_couch-plugin-echo-bridge")),
        manifest_json: include_str!("fixtures/plugin-bridge-v3.json"),
    }
}

/// The bridge has no device to address: its "settings" say only whether it
/// should misbehave, and its request log is empty because nothing is asked of
/// anything outside the process.
struct NoDevice(serde_json::Value);
impl FakeDevice for NoDevice {
    fn settings(&self) -> serde_json::Value {
        self.0.clone()
    }
    fn requests(&self) -> Vec<String> {
        Vec::new()
    }
}

fn hostile(mode: &str) -> serde_json::Value {
    json!({ "hostile": mode })
}

#[test]
fn the_bridge_lists_its_children_pages_them_and_answers_for_one_at_a_time() {
    testing_v3::children(
        bridge(),
        ChildrenCase {
            device: Box::new(NoDevice(hostile("none"))),
            expect: CHILDREN,
            kind: "light",
            write: TypedAction::SetLight {
                on: None,
                brightness: Some(40),
                mirek: None,
                xy: None,
            },
            unknown: "no-such-lamp",
        },
    );
}

#[test]
fn eighty_children_are_three_pages_and_the_catalogue_is_what_it_says_it_is() {
    assert_eq!(CHILDREN, LAMPS + GROUPS + SCENES + 2);
    assert_eq!(CHILDREN, 80);
    assert_eq!(CHILDREN.div_ceil(MAX_PAGE), 3);
    let all = catalogue();
    assert_eq!(all.len(), CHILDREN);
    assert!(all.iter().all(|child| child.is_well_formed()));
    let mut pages = 0;
    let mut cursor = None;
    let mut seen = Vec::new();
    loop {
        let page = ChildPage::fill(all.clone(), cursor.as_deref()).unwrap();
        pages += 1;
        assert!(!page.children.is_empty());
        assert!(page.children.len() <= MAX_PAGE);
        seen.extend(page.children.iter().map(|child| child.id.clone()));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), CHILDREN);
    assert_eq!(
        seen,
        all.iter().map(|child| child.id.clone()).collect::<Vec<_>>()
    );
}

/// A write is acknowledged with the state the child is in, and a read agrees.
/// This is the part a panel leans on: it moves a slider, sends one frame, and
/// draws what comes back rather than asking again.
#[test]
fn a_write_to_a_child_is_acknowledged_with_the_state_it_left_behind() {
    use couch_plugin::{ClimateMode, CoverState, LightState, Request, Response};
    let package = testing::Package::new(bridge());
    let mut host = package.host();
    host.configure(hostile("none")).unwrap();
    let state = |response: Response| match response {
        Response::Status { status } => status,
        other => panic!("{other:?}"),
    };

    let mut lamp =
        |action| host.request_child_detailed(Some("light"), Request::action(action).at("lamp-01"));
    let after = state(
        lamp(TypedAction::SetLight {
            on: None,
            brightness: Some(40),
            mirek: None,
            xy: None,
        })
        .unwrap(),
    );
    assert_eq!(
        after.light,
        Some(LightState {
            on: Some(true),
            brightness: Some(40),
            mirek: Some(366),
            xy: None,
        })
    );
    // Brightness 0 is off, and an absent field is left alone.
    let off = state(
        lamp(TypedAction::SetLight {
            on: None,
            brightness: Some(0),
            mirek: None,
            xy: None,
        })
        .unwrap(),
    );
    assert_eq!(off.light.unwrap().on, Some(false));
    assert_eq!(off.light.unwrap().mirek, Some(366));
    assert_eq!(
        state(
            host.request_child_detailed(Some("light"), Request::status().at("lamp-01"))
                .unwrap()
        ),
        off
    );

    // A level is a command everywhere in Couch and a typed action on the
    // wire: the host's gate is the only place that knows.
    let dimmed = state(
        host.request_child_detailed(Some("light"), Request::command("dim:70").at("lamp-01"))
            .unwrap(),
    );
    assert_eq!(dimmed.light.unwrap().brightness, Some(70));
    let raised = state(
        host.request_child_detailed(Some("blind"), Request::command("position:60").at("blind-1"))
            .unwrap(),
    );
    assert_eq!(
        raised.cover,
        Some(CoverState {
            open: Some(true),
            position: Some(60)
        })
    );
    let warmed = state(
        host.request_child_detailed(
            Some("thermostat"),
            Request::command("mode:cool").at("thermostat-1"),
        )
        .unwrap(),
    );
    assert_eq!(warmed.climate.unwrap().mode, Some(ClimateMode::Cool));

    // A scene is recalled and reports nothing.
    assert_eq!(
        host.request_child_detailed(Some("scene"), Request::command("on").at("scene/2")),
        Ok(Response::Ok)
    );
    assert!(host.is_alive());
}

/// The gate refuses before any I/O: a level a child's kind cannot take, an
/// action of the wrong kind, a resource that climbs, and a kind the manifest
/// never declared.
#[test]
fn the_gate_refuses_what_a_child_of_that_kind_cannot_be_told() {
    use couch_plugin::{Error, Request};
    let package = testing::Package::new(bridge());
    let mut host = package.host();
    host.configure(hostile("none")).unwrap();
    let refused = |host: &mut Host, kind: &str, request: Request| {
        host.request_child_detailed(Some(kind), request)
            .map_err(|failure| failure.code)
    };
    for (kind, request, expected) in [
        // A blind is not dimmed and a lamp has no position.
        (
            "blind",
            Request::command("dim:30").at("blind-1"),
            Error::Unsupported,
        ),
        (
            "light",
            Request::command("position:30").at("lamp-01"),
            Error::Unsupported,
        ),
        (
            "light",
            Request::action(TypedAction::SetCover { position: 30 }).at("lamp-01"),
            Error::Unsupported,
        ),
        // A scene takes `on` and nothing else, and has no action at all.
        (
            "scene",
            Request::command("off").at("scene/1"),
            Error::Unsupported,
        ),
        (
            "scene",
            Request::action(TypedAction::SetLight {
                on: Some(true),
                brightness: None,
                mirek: None,
                xy: None,
            })
            .at("scene/1"),
            Error::Unsupported,
        ),
        // A word that is no mode at all is no function, so it is no
        // capability of that kind either. (A mode the *particular* thermostat
        // does not have is the model's business, not the wire's: the binding
        // never validates, so the gate never sees it.)
        (
            "thermostat",
            Request::command("mode:eco").at("thermostat-1"),
            Error::Unsupported,
        ),
        // A resource that climbs out of the connection, and one too long.
        ("light", Request::status().at("../secrets"), Error::Invalid),
        (
            "light",
            Request::status().at("a".repeat(129)),
            Error::Invalid,
        ),
        // A kind the manifest does not declare.
        ("ghost", Request::status().at("lamp-01"), Error::Unsupported),
    ] {
        assert_eq!(
            refused(&mut host, kind, request.clone()),
            Err(expected),
            "{kind} {request:?}"
        );
    }
    // A resource with no kind never leaves the host.
    assert_eq!(
        host.request_detailed(Request::status().at("lamp-01"))
            .map_err(|failure| failure.code),
        Err(Error::Invalid)
    );
    // None of that cost the package its life: the gate is before any I/O.
    assert!(host.is_alive());
    assert_eq!(
        host.request_child_detailed(Some("light"), Request::status().at("lamp-01"))
            .map(|_| ()),
        Ok(())
    );
}

/// Four ways a bridge can fail to end a listing. Each one is a protocol error
/// and each one costs the package its child process, whether the host sees it
/// in a single answer or only the caller sees it across the whole listing.
#[test]
fn a_hostile_listing_is_a_protocol_error_and_the_package_is_retired() {
    use couch_plugin::Error;
    for (mode, host_sees_it) in [
        (Hostile::Oversized, true),
        (Hostile::Undeclared, true),
        (Hostile::Cycle, false),
        (Hostile::Duplicate, false),
    ] {
        let name = match mode {
            Hostile::Oversized => "oversized",
            Hostile::Undeclared => "undeclared",
            Hostile::Cycle => "cycle",
            Hostile::Duplicate => "duplicate",
            Hostile::None => unreachable!(),
        };
        let package = testing::Package::new(bridge());
        let mut host = package.host();
        host.configure(hostile(name)).unwrap();
        let mut ask = |request| host.request_detailed(request);
        assert_eq!(list_children(&mut ask), Err(Error::Protocol), "{name}");
        if host_sees_it {
            // The answer itself was nonsense, so the host retired the child
            // the moment it read it.
            assert!(!host.is_alive(), "{name}: the child survived its answer");
        } else {
            // Every page was well formed; only the listing as a whole was not,
            // which is the caller's to notice and the caller's to act on.
            assert!(host.is_alive(), "{name}: retired too early");
            host.retire();
            assert!(!host.is_alive(), "{name}");
        }
    }
}

/// With the switch off the bridge's manifest is a package that needs a newer
/// Couch. That half lives in `tests/plugin.rs` for the television; this is the
/// same assertion for a connection with children, and it is what keeps
/// `children` out of every shipped build.
#[test]
fn a_manifest_that_declares_children_is_a_protocol_3_manifest() {
    use couch_plugin::{Error, Manifest};
    let manifest: Manifest =
        serde_json::from_str(include_str!("fixtures/plugin-bridge-v3.json")).unwrap();
    assert_eq!(manifest.protocol_version, 3);
    assert_eq!(manifest.children.len(), 5);
    assert_eq!(manifest.validate(), Ok(()));
    for version in [1, 2] {
        let mut older = manifest.clone();
        older.protocol_version = version;
        older.min_core_protocol_version = version;
        assert_eq!(older.validate(), Err(Error::Invalid), "protocol {version}");
        older.children.clear();
        assert_eq!(older.validate(), Ok(()), "protocol {version}");
    }
}
