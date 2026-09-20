//! The published packages' side of the wire, frozen.
//!
//! `old` is a copy of `Error`, `Request`, `Response` and `Envelope` as they
//! stand at 00ab4da2e6336a925e93702507c0b7b233012738, the SDK revision the
//! Denon, Sonos and Kodi packages in the feed were built from. Like the
//! originals they refuse unknown fields and unknown variants, which is what
//! makes them a fair stand-in for an old child (its `Request`) and an old
//! host (its `Response`). `TypedAction` is frozen here too, since protocol 3
//! gave couch-model three more of them; `Manifest`, `Status` and `Selectable`
//! are this tree's, because a package's own bytes are pinned by the golden
//! file and none of those refuses an unknown field.
//!
//! Two directions, and both go through the same gate the host uses
//! (`host::admit`, `host::accept`) and the same shaping `serve` uses
//! (`server::refusal`), not through hand-picked messages:
//!
//! - whatever the host is willing to write to a protocol 1 or 2 package, an old
//!   child parses, and would itself have written with the same bytes;
//! - whatever an old child can write, this host parses and accepts, and
//!   whatever a protocol 1 or 2 package built with THIS SDK writes, an old
//!   host parses.

use crate::{
    host::{accept, admit},
    protocol::Envelope,
    server::refusal,
    Capability, Error, Failure, KeyPhase, Manifest, PluginActionSchema, Reason, Request, Response,
    Selectable, Status, TypedAction, VolumeDb,
};
use serde_json::json;

mod old {
    use crate::{Manifest, Selectable, Status};
    use serde::{Deserialize, Serialize};

    /// The one typed action 00ab4da knew. couch-model has three more since
    /// protocol 3, step T2, and an old child cannot read any of them.
    #[derive(Clone, Copy, Debug, Serialize, Deserialize)]
    #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
    pub enum TypedAction {
        SetVolumeDb { tenths: i16 },
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Error {
        Invalid,
        Unsupported,
        Incompatible,
        Protocol,
        Transport,
        Timeout,
        Busy,
        Expired,
        Rejected,
    }
    pub const ERRORS: [Error; 9] = [
        Error::Invalid,
        Error::Unsupported,
        Error::Incompatible,
        Error::Protocol,
        Error::Transport,
        Error::Timeout,
        Error::Busy,
        Error::Expired,
        Error::Rejected,
    ];

    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Request {
        Hello { protocol_version: u32 },
        Configure { settings: serde_json::Value },
        Command { function: String },
        Action { action: TypedAction },
        Status,
        Inputs,
    }
    /// 5e0cc20, which the protocol 1 Denon 0.1.1 package was built from: the
    /// same, before typed actions existed.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
    pub enum RequestV1 {
        Hello { protocol_version: u32 },
        Configure { settings: serde_json::Value },
        Command { function: String },
        Status,
        Inputs,
    }
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Response {
        Hello { manifest: Manifest },
        Ok,
        Status { status: Status },
        Inputs { inputs: Vec<Selectable> },
        Error { code: Error },
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Envelope<T> {
        pub id: u64,
        pub body: T,
    }
}

fn manifest(version: u32) -> Manifest {
    let mut manifest: Manifest = serde_json::from_value(json!({
        "protocol_version": 1, "id": "fixture", "label": "Fixture", "version": "1.0.0",
        "executable": "bin/plugin",
        "capabilities": [{"id":"power-on","label":"On"},{"id":"volume-up","label":"Volume up"}],
        "settings": [{"id":"host","label":"Host","kind":"text","required":true}],
        "supports_inputs": true
    }))
    .unwrap();
    manifest.protocol_version = version;
    manifest.min_core_protocol_version = version;
    if version >= 2 {
        manifest.actions = vec![PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        }];
    }
    if version >= 3 {
        manifest.capabilities.push(Capability {
            id: "x:info".into(),
            label: "Info".into(),
        });
    }
    manifest
}

/// Everything a caller can hand the host, wanted or not.
fn requests() -> Vec<Request> {
    let mut requests = vec![
        Request::Hello {
            protocol_version: 1,
        },
        Request::Hello {
            protocol_version: 2,
        },
        Request::configure(json!({"host":"avr.local"})),
        Request::configure(json!({"host":"avr.local","unknown":1})),
        Request::status(),
        Request::Inputs,
    ];
    for tenths in [-805, -800, -345, 180] {
        requests.push(Request::action(TypedAction::SetVolumeDb { tenths }));
    }
    for phase in [KeyPhase::Tap, KeyPhase::Repeat, KeyPhase::LongPress] {
        for function in [
            "power-on",
            "volume-up",
            "mute",
            "input:hdmi1",
            "input:Apple TV 4K",
            "x:info",
            "x:undeclared",
            "not a function",
        ] {
            requests.push(Request::key(function, phase));
        }
    }
    requests
}

#[test]
fn whatever_the_host_sends_a_protocol_1_or_2_package_an_old_child_reads_as_its_own_bytes() {
    for version in [1, 2] {
        let manifest = manifest(version);
        assert_eq!(manifest.validate(), Ok(()));
        let mut sent = 0;
        for (index, request) in requests().into_iter().enumerate() {
            let asked = serde_json::to_string(&request).unwrap();
            let Ok(request) = admit(&manifest, None, None, request) else {
                continue;
            };
            sent += 1;
            let bytes = serde_json::to_string(&Envelope {
                id: index as u64 + 1,
                body: &request,
            })
            .unwrap();
            assert!(!bytes.contains("phase"), "v{version} {asked}: {bytes}");
            assert!(!bytes.contains("\"x:"), "v{version} {asked}: {bytes}");
            // Protocol 3, step T2: a child of a connection, a listing of them,
            // and the three actions that drive one.
            for word in [
                "resource",
                "children",
                "cursor",
                "set_light",
                "set_cover",
                "set_climate",
            ] {
                assert!(!bytes.contains(word), "v{version} {asked}: {bytes}");
            }
            let child: old::Envelope<old::Request> = serde_json::from_str(&bytes)
                .unwrap_or_else(|e| panic!("v{version} old child refuses {bytes}: {e}"));
            assert_eq!(serde_json::to_string(&child).unwrap(), bytes);
            if version == 1 {
                serde_json::from_str::<old::Envelope<old::RequestV1>>(&bytes)
                    .unwrap_or_else(|e| panic!("5e0cc20 child refuses {bytes}: {e}"));
            }
        }
        // hello x2, one configure, status, inputs, 3 phases x 4 supported
        // functions, and for protocol 2 the three in-range actions.
        assert_eq!(sent, if version == 1 { 17 } else { 20 }, "v{version}");
    }
}

#[test]
fn a_phase_or_a_package_named_button_would_have_killed_an_old_child() {
    // Why the gate above matters: these are the bytes it never lets out.
    for body in [
        Request::key("volume-up", KeyPhase::Repeat),
        Request::key("volume-up", KeyPhase::LongPress),
    ] {
        let bytes = serde_json::to_string(&body).unwrap();
        assert!(bytes.contains("\"phase\""), "{bytes}");
        assert!(serde_json::from_str::<old::Request>(&bytes).is_err());
    }
    // A tap never writes the field, for any protocol.
    assert_eq!(
        serde_json::to_string(&Request::key("volume-up", KeyPhase::Tap)).unwrap(),
        r#"{"method":"command","function":"volume-up"}"#
    );
    // And only a protocol 3 package is sent a phase or an `x:` id.
    let v3 = manifest(3);
    for phase in [KeyPhase::Repeat, KeyPhase::LongPress] {
        for function in ["volume-up", "x:info"] {
            let sent = admit(&v3, None, None, Request::key(function, phase)).unwrap();
            assert!(matches!(sent, Request::Command { phase: kept, .. } if kept == phase));
        }
    }
    assert_eq!(
        admit(&v3, None, None, Request::command("x:undeclared")).err(),
        Some(Error::Unsupported)
    );
}

/// Everything protocol 3, step T2 added to a request: naming one child of a
/// connection, and asking for the list of them.
fn child_requests() -> Vec<Request> {
    let light = TypedAction::SetLight {
        on: Some(true),
        brightness: Some(40),
        mirek: None,
        xy: None,
    };
    let mut requests = vec![
        Request::status().at("lamp-01"),
        Request::command("toggle").at("lamp-01"),
        Request::key("toggle", KeyPhase::Repeat).at("lamp-01"),
        Request::command("dim:30").at("lamp-01"),
        Request::action(TypedAction::SetVolumeDb { tenths: -345 }).at("lamp-01"),
        Request::action(light).at("lamp-01"),
        Request::children(None),
        Request::children(Some("lamp-32".into())),
    ];
    // ...and the three actions with no resource at all, which an old package
    // could not have declared and must never be sent either.
    for action in [
        light,
        TypedAction::SetCover { position: 40 },
        TypedAction::SetClimate {
            target_tenths: Some(215),
            low_tenths: None,
            high_tenths: None,
            mode: None,
        },
    ] {
        requests.push(Request::action(action));
    }
    requests
}

/// The gate is the whole defence here, and for one of these it is the only
/// one: an old child's `Status` was a unit variant, and serde lets a unit
/// variant ignore the fields of an internally tagged frame even with
/// `deny_unknown_fields`. Such a child would read a status frame that names a
/// lamp as a status of the whole bridge and answer it.
#[test]
fn a_child_of_a_connection_is_never_named_to_a_protocol_1_or_2_package() {
    for version in [1, 2] {
        let manifest = manifest(version);
        for request in child_requests() {
            let asked = serde_json::to_string(&request).unwrap();
            // Whatever the caller claims the kind is, and whether or not the
            // package would have understood it.
            for kind in [None, Some("light"), Some("ghost")] {
                assert_eq!(
                    admit(&manifest, kind, None, request.clone()).err(),
                    Some(Error::Unsupported),
                    "v{version} {kind:?} {asked}"
                );
            }
        }
    }

    // What the gate is keeping in. Everything below would kill an old child...
    let silent = Request::status().at("lamp-01");
    for request in child_requests() {
        let bytes = serde_json::to_string(&request).unwrap();
        if serde_json::to_string(&silent).unwrap() == bytes {
            continue;
        }
        assert!(
            serde_json::from_str::<old::Request>(&bytes).is_err(),
            "an old child could read {bytes}"
        );
    }
    // ...except this one, which it reads as the status of the whole
    // connection and answers.
    let bytes = serde_json::to_string(&silent).unwrap();
    assert_eq!(bytes, r#"{"method":"status","resource":"lamp-01"}"#);
    assert!(matches!(
        serde_json::from_str::<old::Request>(&bytes).expect("an old child reads it"),
        old::Request::Status
    ));

    // And a resource-less status is still the byte-for-byte frame it was when
    // it was a unit variant here too, which is why the golden file did not
    // move.
    assert_eq!(
        serde_json::to_string(&Request::status()).unwrap(),
        r#"{"method":"status"}"#
    );
    assert!(matches!(
        serde_json::from_str::<Request>(r#"{"method":"status"}"#).unwrap(),
        Request::Status { resource: None }
    ));

    // A protocol 3 package is sent all of it, once its manifest says so.
    let mut v3 = manifest(3);
    v3.children = vec![couch_sdk::PluginChildKind {
        kind: "light".into(),
        label: "Lamp".into(),
        device_kind: couch_sdk::couch_model::DeviceKind::Light,
        component: couch_sdk::couch_model::ChildComponent::Light,
        capabilities: vec![couch_sdk::couch_model::PluginCapability {
            id: "toggle".into(),
            label: "Toggle".into(),
        }],
        actions: vec![PluginActionSchema::SetLight {}],
    }];
    v3.actions.push(PluginActionSchema::SetLight {});
    // Only a build with the switch on would accept such a manifest at all;
    // the gate itself does not depend on it.
    #[cfg(feature = "protocol-3-preview")]
    assert_eq!(v3.validate(), Ok(()));
    let sent: Vec<Request> = child_requests()
        .into_iter()
        .filter_map(|request| admit(&v3, Some("light"), None, request).ok())
        .collect();
    // status, toggle, a held toggle, dim:30 as an action, the light action,
    // two listings, and the connection's own light action.
    assert_eq!(sent.len(), 8, "{sent:?}");
    assert!(
        sent.iter().any(|request| matches!(
            request,
            Request::Action {
                action: TypedAction::SetLight {
                    brightness: Some(30),
                    ..
                },
                resource: Some(_)
            }
        )),
        "the gate did not turn dim:30 into a brightness"
    );
}

fn old_responses() -> Vec<(Request, old::Response)> {
    let status = Status {
        on: Some(true),
        muted: Some(false),
        volume: Some(31),
        input: Some("hdmi1".into()),
        playing: Some(false),
        title: Some("Title".into()),
        ..Status::default()
    };
    let mut all = vec![
        (Request::command("power-on"), old::Response::Ok),
        (
            Request::configure(json!({"host":"avr.local"})),
            old::Response::Ok,
        ),
        (
            Request::status(),
            old::Response::Status {
                status: Status::default(),
            },
        ),
        (Request::status(), old::Response::Status { status }),
        (
            Request::Inputs,
            old::Response::Inputs {
                inputs: vec![Selectable::new("hdmi1", "Console")],
            },
        ),
    ];
    for code in old::ERRORS {
        all.push((Request::command("power-on"), old::Response::Error { code }));
        all.push((Request::status(), old::Response::Error { code }));
    }
    all
}

#[test]
fn whatever_an_old_child_answers_this_host_reads_accepts_and_would_write_the_same() {
    for version in [1, 2] {
        let manifest = manifest(version);
        let mut answers = old_responses();
        answers.push((
            Request::Hello {
                protocol_version: version,
            },
            old::Response::Hello {
                manifest: manifest.clone(),
            },
        ));
        if version == 2 {
            for volume_db in [VolumeDb::Reading { tenths: -345 }, VolumeDb::Minimum] {
                answers.push((
                    Request::status(),
                    old::Response::Status {
                        status: Status {
                            volume_db: Some(volume_db),
                            ..Status::default()
                        },
                    },
                ));
            }
            answers.push((
                Request::action(TypedAction::SetVolumeDb { tenths: -345 }),
                old::Response::Ok,
            ));
        }
        for (request, answer) in answers {
            let bytes = serde_json::to_string(&old::Envelope {
                id: 7,
                body: &answer,
            })
            .unwrap();
            let read: Envelope<Response> = serde_json::from_str(&bytes)
                .unwrap_or_else(|e| panic!("v{version} host refuses {bytes}: {e}"));
            assert_eq!(serde_json::to_string(&read).unwrap(), bytes);
            assert_eq!(
                accept(&manifest, &request, &read.body),
                Ok(()),
                "v{version} {bytes}"
            );
            if let (Response::Error { code, reason }, old::Response::Error { code: sent }) =
                (&read.body, &answer)
            {
                assert_eq!(reason, &None);
                assert_eq!(
                    serde_json::to_string(code).unwrap(),
                    serde_json::to_string(sent).unwrap()
                );
            }
        }
    }
}

#[test]
fn a_protocol_1_or_2_package_built_with_this_sdk_answers_in_bytes_an_old_host_reads() {
    let reasons = [
        None,
        Some(Reason::Message {
            text: "Pair again".into(),
        }),
        Some(Reason::InvalidSetting {
            field: "host".into(),
            text: "Not an address".into(),
        }),
    ];
    let codes = [
        Error::Invalid,
        Error::Unsupported,
        Error::Incompatible,
        Error::Protocol,
        Error::Transport,
        Error::Timeout,
        Error::Busy,
        Error::Expired,
        Error::Rejected,
        Error::Unpaired,
    ];
    for version in [1, 2] {
        let manifest = manifest(version);
        for code in codes {
            for reason in &reasons {
                let response = refusal(
                    &manifest,
                    Failure {
                        code,
                        reason: reason.clone(),
                    },
                );
                let bytes = serde_json::to_string(&Envelope {
                    id: 9,
                    body: &response,
                })
                .unwrap();
                let host: old::Envelope<old::Response> = serde_json::from_str(&bytes)
                    .unwrap_or_else(|e| panic!("v{version} old host refuses {bytes}: {e}"));
                assert_eq!(serde_json::to_string(&host).unwrap(), bytes);
                let expected = if code == Error::Unpaired {
                    "rejected".to_owned()
                } else {
                    serde_json::to_value(code).unwrap().as_str().unwrap().into()
                };
                assert_eq!(
                    bytes,
                    format!(r#"{{"id":9,"body":{{"type":"error","code":"{expected}"}}}}"#)
                );
                // And this host accepts it from that package.
                assert_eq!(accept(&manifest, &Request::status(), &response), Ok(()));
            }
        }
    }
}

#[test]
fn protocol_3_words_from_a_protocol_1_or_2_package_are_a_protocol_error() {
    let message = Reason::Message {
        text: "Pair again".into(),
    };
    for version in [1, 2] {
        let manifest = manifest(version);
        for response in [
            Response::Error {
                code: Error::Unpaired,
                reason: None,
            },
            Response::Error {
                code: Error::Rejected,
                reason: Some(message.clone()),
            },
        ] {
            assert_eq!(
                accept(&manifest, &Request::status(), &response),
                Err(Error::Protocol),
                "v{version} {response:?}"
            );
        }
    }
    let v3 = manifest(3);
    let setting = |field: &str, text: String| Response::Error {
        code: Error::Invalid,
        reason: Some(Reason::InvalidSetting {
            field: field.into(),
            text,
        }),
    };
    for (response, expected) in [
        (
            Response::Error {
                code: Error::Unpaired,
                reason: Some(message.clone()),
            },
            Ok(()),
        ),
        (setting("host", "Not an address".into()), Ok(())),
        (setting("host", "a".repeat(Reason::MAX_TEXT)), Ok(())),
        (
            setting("host", "a".repeat(Reason::MAX_TEXT + 1)),
            Err(Error::Protocol),
        ),
        (setting("host", "two\nlines".into()), Err(Error::Protocol)),
        (setting("undeclared", "No".into()), Err(Error::Protocol)),
    ] {
        assert_eq!(
            accept(&v3, &Request::status(), &response),
            expected,
            "{response:?}"
        );
        // A protocol 3 package built with this SDK never sends the ones the
        // host would retire it for: the reason is dropped, the code kept.
        if let Response::Error { code, reason } = &response {
            let shaped = refusal(
                &v3,
                Failure {
                    code: *code,
                    reason: reason.clone(),
                },
            );
            assert_eq!(accept(&v3, &Request::status(), &shaped), Ok(()));
            assert!(matches!(shaped, Response::Error { code: kept, .. } if kept == *code));
        }
    }
    // A reply of protocol 3 is unreadable to an old host, which is why only a
    // protocol 3 manifest, which an old host refuses outright, may produce one.
    let bytes = serde_json::to_string(&setting("host", "No".into())).unwrap();
    assert!(serde_json::from_str::<old::Response>(&bytes).is_err());
}

// ---------------------------------------------------------------------------
// Protocol 3, step T3: pairing and the key.
// ---------------------------------------------------------------------------

use crate::{Credential, PairInput, Pairing};

fn paired(mut manifest: Manifest) -> Manifest {
    manifest.pairing = Some(Pairing {
        required: true,
        max_seconds: 120,
    });
    manifest
}

fn key() -> Credential {
    Credential::new(json!({"key": "0f1e2d"})).unwrap()
}

/// Everything pairing can put in a request, including a key ridden in on an
/// ordinary configure.
fn pairing_requests() -> Vec<Request> {
    vec![
        Request::configure_with(json!({"host":"avr.local"}), Some(&key())),
        Request::pair_start(json!({"host":"avr.local"}), None),
        Request::pair_start(json!({"host":"avr.local"}), Some(&key())),
        Request::pair_continue("p1", None),
        Request::pair_continue("p1", Some(PairInput::code("0417"))),
        Request::pair_cancel("p1"),
    ]
}

/// The host holds a key for this connection and the package is an old one.
/// Every frame it is willing to write still has to be a frame that child can
/// read, which means the key is not in it - and the configure it *does* get is
/// byte for byte the one it has always been sent.
#[test]
fn a_key_the_host_holds_never_reaches_a_protocol_1_or_2_package() {
    for version in [1, 2] {
        let manifest = manifest(version);
        assert_eq!(manifest.validate(), Ok(()));
        // Every admitted request, with a key in hand for the one that can
        // carry one.
        let mut configured = 0;
        for request in requests().into_iter().chain(pairing_requests()) {
            let asked = serde_json::to_string(&request).unwrap();
            let Ok(request) = admit(&manifest, None, None, request) else {
                continue;
            };
            let bytes = serde_json::to_string(&Envelope {
                id: 1,
                body: &request,
            })
            .unwrap();
            for word in [
                "credential",
                "pair_start",
                "pair_continue",
                "pair_cancel",
                "session",
                "0f1e2d",
            ] {
                assert!(!bytes.contains(word), "v{version} {asked}: {bytes}");
            }
            let child: old::Envelope<old::Request> = serde_json::from_str(&bytes)
                .unwrap_or_else(|e| panic!("v{version} old child refuses {bytes}: {e}"));
            assert_eq!(serde_json::to_string(&child).unwrap(), bytes);
            if matches!(request, Request::Configure { .. }) {
                configured += 1;
            }
        }
        // The three configures: two from `requests()` (one of which carries an
        // undeclared setting and is refused) and the one with the key.
        assert_eq!(configured, 2, "v{version}");
        // And it is the very same frame as the one with no key at all.
        let with = admit(
            &manifest,
            None,
            None,
            Request::configure_with(json!({"host":"avr.local"}), Some(&key())),
        )
        .unwrap();
        let without = admit(
            &manifest,
            None,
            None,
            Request::configure(json!({"host":"avr.local"})),
        )
        .unwrap();
        assert_eq!(with, without, "v{version}");
        assert_eq!(
            serde_json::to_string(&with).unwrap(),
            r#"{"method":"configure","settings":{"host":"avr.local"}}"#
        );
    }
}

/// The gate never asks such a package to pair, whatever the caller wants.
#[test]
fn a_pairing_request_is_never_sent_to_a_package_that_did_not_declare_pairing() {
    // Below protocol 3 the protocol itself refuses it...
    for version in [1, 2] {
        let manifest = manifest(version);
        for request in pairing_requests() {
            if matches!(request, Request::Configure { .. }) {
                continue;
            }
            assert_eq!(
                admit(&manifest, None, None, request.clone()).err(),
                Some(Error::Unsupported),
                "v{version} {request:?}"
            );
        }
    }
    // ...and at protocol 3 the manifest does, because it declared no pairing.
    let plain = manifest(3);
    assert!(plain.pairing.is_none());
    assert!(!plain.pairs());
    for request in pairing_requests() {
        if matches!(request, Request::Configure { .. }) {
            continue;
        }
        assert_eq!(
            admit(&plain, None, None, request.clone()).err(),
            Some(Error::Unsupported),
            "{request:?}"
        );
    }
    // ...and a step of a conversation the host is not having is refused
    // before any I/O, whatever the manifest says.
    let pairs = paired(manifest(3));
    assert_eq!(
        admit(&pairs, None, None, Request::pair_continue("p1", None)).err(),
        Some(Error::Invalid)
    );
    // Anything that has to read the settings has to validate the manifest
    // first, and a protocol 3 manifest is only valid with the switch on.
    #[cfg(feature = "protocol-3-preview")]
    {
        assert_eq!(plain.validate(), Ok(()));
        assert_eq!(pairs.validate(), Ok(()));
        // A key is stripped from a protocol 3 package too: `pairing` is the
        // whole question, not the protocol version.
        let sent = admit(
            &plain,
            None,
            None,
            Request::configure_with(json!({"host":"avr.local"}), Some(&key())),
        )
        .unwrap();
        assert!(!serde_json::to_string(&sent).unwrap().contains("credential"));
        // With pairing declared, the key rides along and a start is admitted.
        let sent = admit(
            &pairs,
            None,
            None,
            Request::configure_with(json!({"host":"avr.local"}), Some(&key())),
        )
        .unwrap();
        assert!(serde_json::to_string(&sent).unwrap().contains("credential"));
        assert!(admit(
            &pairs,
            None,
            None,
            Request::pair_start(json!({"host":"avr.local"}), None)
        )
        .is_ok());
    }
    // A pairing frame is unreadable to an old child, which is why it may only
    // ever be written to a manifest an old Couch refuses outright.
    for request in pairing_requests() {
        if matches!(request, Request::Configure { .. }) {
            continue;
        }
        let bytes = serde_json::to_string(&request).unwrap();
        assert!(
            serde_json::from_str::<old::Request>(&bytes).is_err(),
            "an old child could read {bytes}"
        );
    }
    let bytes = serde_json::to_string(&Request::configure_with(
        json!({"host":"avr.local"}),
        Some(&key()),
    ))
    .unwrap();
    assert!(
        serde_json::from_str::<old::Request>(&bytes).is_err(),
        "an old child could read {bytes}"
    );
}

/// A rotated key from a package that may not have one, and one on a reply that
/// may not carry one. Both retire the child.
#[test]
fn a_rotated_key_is_only_accepted_from_a_package_that_pairs_and_only_on_an_ordinary_reply() {
    use crate::{host::accept_reply, protocol::ReplyEnvelope};
    let reply = |body, store_credential| ReplyEnvelope {
        id: 1,
        body,
        store_credential,
    };
    for version in [1, 2, 3] {
        let manifest = manifest(version);
        assert_eq!(
            accept_reply(
                &manifest,
                &Request::status(),
                &reply(
                    Response::Status {
                        status: Status::on(true)
                    },
                    Some(key())
                )
            ),
            Err(Error::Protocol),
            "v{version}"
        );
    }
    let pairs = paired(manifest(3));
    // On an ordinary reply, from a package that pairs: accepted.
    assert_eq!(
        accept_reply(
            &pairs,
            &Request::status(),
            &reply(
                Response::Status {
                    status: Status::on(true)
                },
                Some(key())
            )
        ),
        Ok(())
    );
    assert_eq!(
        accept_reply(
            &pairs,
            &Request::command("power-on"),
            &reply(Response::Ok, Some(key()))
        ),
        Ok(())
    );
    // Never on a handshake, a configure or a pairing step: each of those has
    // its own way of saying what it means.
    for request in [
        Request::Hello {
            protocol_version: 3,
        },
        Request::configure(json!({"host":"avr.local"})),
        Request::pair_cancel("p1"),
    ] {
        assert_eq!(
            accept_reply(&pairs, &request, &reply(Response::Ok, Some(key()))),
            Err(Error::Protocol),
            "{request:?}"
        );
    }
    // And never one that does not fit.
    let oversized: Credential =
        serde_json::from_value(json!({"k": "a".repeat(Credential::MAX_BYTES)})).unwrap();
    assert_eq!(
        accept_reply(
            &pairs,
            &Request::status(),
            &reply(
                Response::Status {
                    status: Status::on(true)
                },
                Some(oversized)
            )
        ),
        Err(Error::Protocol)
    );
}

/// The one thing a credential must never do: appear in something that is
/// printed. It has a redacting `Debug`, no `Display`, and nothing that carries
/// a failure or a log line can hold one.
#[test]
fn nothing_that_formats_a_value_can_print_a_key() {
    use crate::{PairFailure, PairStep};
    let secret = "0f1e2d";
    let credential = Credential::new(json!({"key": secret, "nested": {"also": secret}})).unwrap();
    let step = PairStep::done(credential.clone(), "Paired with Hall bridge");
    let printed = [
        format!("{credential:?}"),
        format!("{step:?}"),
        format!(
            "{:?}",
            Request::configure_with(json!({"host":"avr.local"}), Some(&credential))
        ),
        format!(
            "{:?}",
            Request::pair_start(json!({"host":"avr.local"}), Some(&credential))
        ),
        format!(
            "{:?}",
            Response::Pairing {
                session: "p1".into(),
                step: step.clone()
            }
        ),
        format!("{:?}", Some(credential.clone())),
        format!("{:?}", vec![credential.clone()]),
    ];
    for text in &printed {
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains("Credential(..)"), "{text}");
    }
    // A failure is the thing the daemon logs and the browser is shown, and it
    // structurally cannot hold one: a code and a `Reason`, which is text the
    // package wrote for a person.
    let failure = Failure {
        code: Error::Unpaired,
        reason: Some(Reason::Message {
            text: "Pair this TV again".into(),
        }),
    };
    assert!(!format!("{failure:?}").contains(secret));
    assert!(!failure.to_string().contains(secret));
    // A step that failed carries words, never a key.
    let failed = PairStep::failed(PairFailure::WrongCode).because("That code was not right");
    assert!(!format!("{failed:?}").contains("Credential"));
}
