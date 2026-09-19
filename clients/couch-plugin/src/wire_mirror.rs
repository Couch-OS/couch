//! The published packages' side of the wire, frozen.
//!
//! `old` is a copy of `Error`, `Request`, `Response` and `Envelope` as they
//! stand at 00ab4da2e6336a925e93702507c0b7b233012738, the SDK revision the
//! Denon, Sonos and Kodi packages in the feed were built from. Like the
//! originals they refuse unknown fields and unknown variants, which is what
//! makes them a fair stand-in for an old child (its `Request`) and an old
//! host (its `Response`). `Manifest`, `Status`, `Selectable` and `TypedAction`
//! are this tree's: this change does not touch them.
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
        Action { action: couch_sdk::TypedAction },
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
        Request::Configure {
            settings: json!({"host":"avr.local"}),
        },
        Request::Configure {
            settings: json!({"host":"avr.local","unknown":1}),
        },
        Request::Status,
        Request::Inputs,
    ];
    for tenths in [-805, -800, -345, 180] {
        requests.push(Request::Action {
            action: TypedAction::SetVolumeDb { tenths },
        });
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
            let Ok(request) = admit(&manifest, request) else {
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
            let sent = admit(&v3, Request::key(function, phase)).unwrap();
            assert!(matches!(sent, Request::Command { phase: kept, .. } if kept == phase));
        }
    }
    assert_eq!(
        admit(&v3, Request::command("x:undeclared")).err(),
        Some(Error::Unsupported)
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
            Request::Configure {
                settings: json!({"host":"avr.local"}),
            },
            old::Response::Ok,
        ),
        (
            Request::Status,
            old::Response::Status {
                status: Status::default(),
            },
        ),
        (Request::Status, old::Response::Status { status }),
        (
            Request::Inputs,
            old::Response::Inputs {
                inputs: vec![Selectable::new("hdmi1", "Console")],
            },
        ),
    ];
    for code in old::ERRORS {
        all.push((Request::command("power-on"), old::Response::Error { code }));
        all.push((Request::Status, old::Response::Error { code }));
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
                    Request::Status,
                    old::Response::Status {
                        status: Status {
                            volume_db: Some(volume_db),
                            ..Status::default()
                        },
                    },
                ));
            }
            answers.push((
                Request::Action {
                    action: TypedAction::SetVolumeDb { tenths: -345 },
                },
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
                assert_eq!(accept(&manifest, &Request::Status, &response), Ok(()));
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
                accept(&manifest, &Request::Status, &response),
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
            accept(&v3, &Request::Status, &response),
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
            assert_eq!(accept(&v3, &Request::Status, &shaped), Ok(()));
            assert!(matches!(shaped, Response::Error { code: kept, .. } if kept == *code));
        }
    }
    // A reply of protocol 3 is unreadable to an old host, which is why only a
    // protocol 3 manifest, which an old host refuses outright, may produce one.
    let bytes = serde_json::to_string(&setting("host", "No".into())).unwrap();
    assert!(serde_json::from_str::<old::Response>(&bytes).is_err());
}
