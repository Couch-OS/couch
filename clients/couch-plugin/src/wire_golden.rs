//! Every protocol 1 and 2 message, byte for byte as the published packages
//! read and write it.
//!
//! The Denon, Sonos and Kodi packages in the feed were built from this crate
//! at 00ab4da2e6336a925e93702507c0b7b233012738, and every type they parse
//! refuses unknown fields. `tests/golden/wire-00ab4da.tsv` is what THAT source
//! serializes for the cases below; this tree must serialize the same bytes,
//! and must read those bytes back into the same bytes.
//!
//! The golden file was captured by this very file: at 00ab4da, drop it in as
//! `src/wire_golden.rs`, add `#[cfg(test)] mod wire_golden;` to `src/lib.rs`,
//! replace the two functions in `shim` with the literals in their comments,
//! and run
//! `COUCH_WIRE_GOLDEN_OUT=$PWD/wire-00ab4da.tsv cargo test -p couch-plugin --lib wire_golden`.
//! Add a case here and the golden file has to be captured again from there;
//! a case only this tree can build does not belong in it.

use crate::{
    protocol::{Envelope, ReplyEnvelope},
    Capability, CodeAlphabet, Component, Credential, Error, FieldKind, LocalRequest, Manifest,
    PairFailure, PairInput, PairPrompt, PairStep, Pairing, PluginActionSchema, Request, Response,
    Selectable, SettingField, Status, StatusField, TypedAction, VolumeDb,
};
use serde_json::json;

mod shim {
    use crate::{Error, Request, Response, TypedAction};
    /// 00ab4da: `Request::Command { function: function.into() }`
    pub fn command(function: &str) -> Request {
        Request::command(function)
    }
    /// 00ab4da: `Request::Status`
    pub fn status() -> Request {
        Request::status()
    }
    /// 00ab4da: `Request::Configure { settings }`
    pub fn configure(settings: serde_json::Value) -> Request {
        Request::configure(settings)
    }
    /// 00ab4da: `Request::Action { action }`
    pub fn action(action: TypedAction) -> Request {
        Request::action(action)
    }
    /// 00ab4da: `Response::Error { code }`
    pub fn error(code: Error) -> Response {
        Response::error(code)
    }
}

const GOLDEN: &str = include_str!("../tests/golden/wire-00ab4da.tsv");
/// Protocol 3, which no published package speaks. Captured by this tree, from
/// this tree, so that a frame nobody has written yet cannot change shape
/// unnoticed between one step of the train and the next. A row here can never
/// move a row in the file above: they are separate files on purpose.
const PREVIEW: &str = include_str!("../tests/golden/wire-v3-preview.tsv");

fn v1_manifest() -> Manifest {
    Manifest {
        protocol_version: 1,
        min_core_protocol_version: 1,
        actions: Vec::new(),
        id: "fixture".into(),
        label: "Fixture".into(),
        version: "1.0.0".into(),
        executable: "bin/plugin".into(),
        capabilities: vec![
            Capability {
                id: "power-on".into(),
                label: "On".into(),
            },
            Capability {
                id: "power-off".into(),
                label: "Off".into(),
            },
            Capability {
                id: "volume-up".into(),
                label: "Volume up".into(),
            },
        ],
        settings: vec![
            SettingField {
                id: "host".into(),
                label: "Host".into(),
                kind: FieldKind::Text,
                required: true,
                default: None,
            },
            SettingField {
                id: "port".into(),
                label: "Port".into(),
                kind: FieldKind::Integer,
                required: false,
                default: Some(json!(23)),
            },
            SettingField {
                id: "token".into(),
                label: "Token".into(),
                kind: FieldKind::Secret,
                required: false,
                default: None,
            },
            SettingField {
                id: "tls".into(),
                label: "Use TLS".into(),
                kind: FieldKind::Boolean,
                required: false,
                default: Some(json!(false)),
            },
        ],
        supports_inputs: false,
        presentation: Vec::new(),
        children: Vec::new(),
        pairing: None,
        keep_alive: false,
    }
}

fn v2_manifest() -> Manifest {
    let mut manifest = v1_manifest();
    manifest.protocol_version = 2;
    manifest.min_core_protocol_version = 2;
    manifest.supports_inputs = true;
    manifest.actions = vec![PluginActionSchema::SetVolumeDb {
        min_tenths: -800,
        max_tenths: 180,
        step_tenths: 5,
    }];
    manifest.presentation = vec![
        Component::CommandGroup {
            title: "Power".into(),
            commands: vec!["power-on".into(), "power-off".into()],
        },
        Component::StatusText {
            label: "Volume".into(),
            field: StatusField::VolumeDb,
        },
        Component::VolumeDbControl {
            label: "Volume".into(),
        },
        Component::Toggle {
            label: "Power".into(),
            state: StatusField::On,
            on: "power-on".into(),
            off: "power-off".into(),
        },
        Component::InputSelector {
            label: "Input".into(),
        },
    ];
    manifest
}

fn packaged(text: &str) -> Manifest {
    serde_json::from_str(text).expect("a package manifest in this tree")
}

fn frame<T: serde::Serialize>(id: u64, body: T) -> String {
    serde_json::to_string(&Envelope { id, body }).unwrap()
}

/// `(name, bytes)`. Names starting `request`, `response`, `manifest` and
/// `local` say which type reads the bytes back.
fn cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    let mut add = |name: &str, bytes: String| cases.push((name.to_owned(), bytes));

    for version in [1, 2] {
        add(
            &format!("request hello {version}"),
            frame(
                1,
                Request::Hello {
                    protocol_version: version,
                },
            ),
        );
    }
    add(
        "request configure",
        frame(
            2,
            shim::configure(json!({"host":"avr.local","port":23,"token":"private","tls":false})),
        ),
    );
    for function in [
        "power-on",
        "volume-up",
        "input:hdmi1",
        "input:Apple TV 4K",
        "play-pause",
    ] {
        add(
            &format!("request command {function}"),
            frame(3, shim::command(function)),
        );
    }
    for tenths in [-800, -345, 0, 180] {
        add(
            &format!("request action set_volume_db {tenths}"),
            frame(4, shim::action(TypedAction::SetVolumeDb { tenths })),
        );
    }
    add("request status", frame(5, shim::status()));
    add("request inputs", frame(u64::MAX, Request::Inputs));

    add(
        "response hello 1",
        frame(
            1,
            Response::Hello {
                manifest: v1_manifest(),
            },
        ),
    );
    add(
        "response hello 2",
        frame(
            1,
            Response::Hello {
                manifest: v2_manifest(),
            },
        ),
    );
    add("response ok", frame(2, Response::Ok));
    let full = Status {
        on: Some(true),
        muted: Some(false),
        volume: Some(31),
        volume_db: None,
        input: Some("hdmi1".into()),
        playing: Some(true),
        title: Some("A title, with \"quotes\" and caf\u{e9}".into()),
        ..Status::default()
    };
    for (name, status) in [
        ("empty", Status::default()),
        ("on", Status::on(true)),
        ("full", full.clone()),
        (
            "db reading",
            Status {
                volume: None,
                volume_db: Some(VolumeDb::Reading { tenths: -345 }),
                ..full.clone()
            },
        ),
        (
            "db minimum",
            Status {
                volume_db: Some(VolumeDb::Minimum),
                ..Status::default()
            },
        ),
    ] {
        add(
            &format!("response status {name}"),
            frame(5, Response::Status { status }),
        );
    }
    add(
        "response inputs none",
        frame(6, Response::Inputs { inputs: Vec::new() }),
    );
    add(
        "response inputs some",
        frame(
            6,
            Response::Inputs {
                inputs: vec![
                    Selectable::new("hdmi1", "Console"),
                    Selectable::new("Apple TV 4K", "Apple TV 4K"),
                ],
            },
        ),
    );
    for code in [
        Error::Invalid,
        Error::Unsupported,
        Error::Incompatible,
        Error::Protocol,
        Error::Transport,
        Error::Timeout,
        Error::Busy,
        Error::Expired,
        Error::Rejected,
    ] {
        add(
            &format!("response error {}", serde_json::to_string(&code).unwrap()),
            frame(7, shim::error(code)),
        );
    }

    add(
        "manifest fixture 1",
        serde_json::to_string(&v1_manifest()).unwrap(),
    );
    add(
        "manifest fixture 2",
        serde_json::to_string(&v2_manifest()).unwrap(),
    );
    add(
        "manifest echo",
        serde_json::to_string(&packaged(include_str!("../../couch-echo/plugin.json"))).unwrap(),
    );
    add(
        "manifest sonos",
        serde_json::to_string(&packaged(include_str!("../../couch-sonos/plugin.json"))).unwrap(),
    );

    // The GUI's socket to the daemon. Both ends ship in one bundle, but the
    // frames are the same types, so they are pinned too.
    for (name, request) in [
        ("command", shim::command("volume-up")),
        ("status", shim::status()),
        ("inputs", Request::Inputs),
        (
            "action",
            shim::action(TypedAction::SetVolumeDb { tenths: -345 }),
        ),
    ] {
        add(
            &format!("local {name}"),
            serde_json::to_string(&LocalRequest {
                connection_id: "living-room-avr".into(),
                request,
            })
            .unwrap(),
        );
    }

    // Protocol 3, step T2, put the level mapping in the host's gate. A level
    // sent to a connection - one that names no child - is still the plain
    // command every published package reads, and these are its bytes. Like
    // every other case here they were captured at 00ab4da, which is why the
    // mapping cannot have changed them.
    for function in ["dim:30", "position:40", "mode:heat"] {
        add(
            &format!("request level {function}"),
            frame(3, shim::command(function)),
        );
    }
    cases
}

fn golden() -> Vec<(String, String)> {
    GOLDEN
        .lines()
        .map(|line| {
            let (name, bytes) = line.split_once('\t').expect("name<TAB>bytes");
            (name.to_owned(), bytes.to_owned())
        })
        .collect()
}

#[test]
fn every_protocol_1_and_2_message_is_written_with_the_bytes_of_00ab4da() {
    let cases = cases();
    if let Ok(path) = std::env::var("COUCH_WIRE_GOLDEN_OUT") {
        let text: String = cases
            .iter()
            .map(|(name, bytes)| format!("{name}\t{bytes}\n"))
            .collect();
        std::fs::write(path, text).unwrap();
        return;
    }
    let golden = golden();
    assert_eq!(
        cases.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        golden.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "the cases and the golden file list different messages"
    );
    assert!(cases.len() >= 40, "{} cases", cases.len());
    for ((name, bytes), (_, expected)) in cases.iter().zip(&golden) {
        assert_eq!(bytes, expected, "{name}");
    }
}

#[test]
fn every_byte_00ab4da_writes_is_read_back_and_rewritten_unchanged() {
    fn again<T: serde::de::DeserializeOwned + serde::Serialize>(name: &str, bytes: &str) {
        let value: T = serde_json::from_str(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(serde_json::to_string(&value).unwrap(), bytes, "{name}");
    }
    for (name, bytes) in golden() {
        match name.split(' ').next().unwrap() {
            "request" => again::<Envelope<Request>>(&name, &bytes),
            "response" => again::<Envelope<Response>>(&name, &bytes),
            "manifest" => {
                again::<Manifest>(&name, &bytes);
                let manifest: Manifest = serde_json::from_str(&bytes).unwrap();
                assert_eq!(manifest.validate(), Ok(()), "{name}");
            }
            "local" => again::<LocalRequest>(&name, &bytes),
            other => panic!("unknown golden kind {other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol 3, step T3: pairing.
// ---------------------------------------------------------------------------
//
// Two claims, kept apart on purpose.
//
// The first is that nothing above moved. `Configure` gained a field and the
// reply envelope gained one, and every row of `wire-00ab4da.tsv` still has to
// be the bytes that revision wrote - which the test above asserts for the
// request side by construction, and `the_new_fields_reproduce_the_bytes_of_00ab4da`
// asserts again for both, explicitly, against the file's own rows.
//
// The second is that the protocol 3 frames have a shape, and that it does not
// drift between one step of the train and the next. Those cannot be captured
// at 00ab4da, because the types did not exist there, so they live in their own
// file (`wire-v3-preview.tsv`), captured from this tree:
//
//   COUCH_WIRE_V3_OUT=$PWD/couch-plugin/tests/golden/wire-v3-preview.tsv \
//     cargo test -p couch-plugin --lib wire_golden

fn credential() -> Credential {
    Credential::new(json!({"key": "0f1e2d", "id": 7})).expect("a small credential")
}

fn v3_pairing_manifest() -> Manifest {
    let mut manifest = v2_manifest();
    manifest.protocol_version = 3;
    manifest.min_core_protocol_version = 3;
    manifest.pairing = Some(Pairing {
        required: true,
        max_seconds: 120,
    });
    manifest.keep_alive = true;
    manifest
}

/// `(name, bytes)` for everything pairing put on the wire.
fn preview_cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    let mut add = |name: &str, bytes: String| cases.push((name.to_owned(), bytes));
    let settings = json!({"host":"avr.local","port":23,"token":"private","tls":false});

    add(
        "request configure with credential",
        frame(
            2,
            Request::configure_with(settings.clone(), Some(&credential())),
        ),
    );
    add(
        "request pair_start",
        frame(3, Request::pair_start(settings.clone(), None)),
    );
    add(
        "request pair_start with credential",
        frame(
            3,
            Request::pair_start(settings.clone(), Some(&credential())),
        ),
    );
    add(
        "request pair_continue",
        frame(4, Request::pair_continue("p1", None)),
    );
    add(
        "request pair_continue code",
        frame(
            4,
            Request::pair_continue("p1", Some(PairInput::code("0417"))),
        ),
    );
    add("request pair_cancel", frame(5, Request::pair_cancel("p1")));

    let pairing = |step| Response::Pairing {
        session: "p1".into(),
        step,
    };
    for (name, prompt) in [
        ("press_button", PairPrompt::press_button()),
        (
            "press_button said",
            PairPrompt::press_button().saying("Press pair on the bridge"),
        ),
        ("approve", PairPrompt::approve_on_device()),
        ("code", PairPrompt::enter_code(6, CodeAlphabet::Hex)),
        (
            "code digits",
            PairPrompt::enter_code(4, CodeAlphabet::Digits),
        ),
    ] {
        let wait = if prompt.is_code() { 0 } else { 2000 };
        add(
            &format!("response pairing waiting {name}"),
            frame(6, pairing(PairStep::waiting(prompt, wait))),
        );
    }
    add(
        "response pairing done",
        frame(
            6,
            pairing(PairStep::done(credential(), "Paired with Hall bridge")),
        ),
    );
    add(
        "response pairing done with settings",
        frame(
            6,
            pairing(
                PairStep::done(credential(), "Paired with Hall bridge")
                    .with_settings(settings.clone()),
            ),
        ),
    );
    for reason in [
        PairFailure::Unreachable,
        PairFailure::Refused,
        PairFailure::WrongCode,
        PairFailure::TimedOut,
        PairFailure::Unsupported,
    ] {
        add(
            &format!(
                "response pairing failed {}",
                serde_json::to_value(reason).unwrap().as_str().unwrap()
            ),
            frame(6, pairing(PairStep::failed(reason))),
        );
    }
    add(
        "response pairing failed said",
        frame(
            6,
            pairing(PairStep::failed(PairFailure::Refused).because("The bridge said no")),
        ),
    );

    // The reply envelope, which is the only place a rotated key travels.
    add(
        "reply ok with store_credential",
        serde_json::to_string(&ReplyEnvelope {
            id: 7,
            body: Response::Ok,
            store_credential: Some(credential()),
        })
        .unwrap(),
    );
    add(
        "reply status with store_credential",
        serde_json::to_string(&ReplyEnvelope {
            id: 7,
            body: Response::Status {
                status: Status::on(true),
            },
            store_credential: Some(credential()),
        })
        .unwrap(),
    );

    add(
        "manifest pairing 3",
        serde_json::to_string(&v3_pairing_manifest()).unwrap(),
    );
    cases
}

#[test]
fn every_protocol_3_pairing_message_keeps_the_shape_this_tree_captured() {
    let cases = preview_cases();
    if let Ok(path) = std::env::var("COUCH_WIRE_V3_OUT") {
        let text: String = cases
            .iter()
            .map(|(name, bytes)| format!("{name}\t{bytes}\n"))
            .collect();
        std::fs::write(path, text).unwrap();
        return;
    }
    let golden: Vec<(String, String)> = PREVIEW
        .lines()
        .map(|line| {
            let (name, bytes) = line.split_once('\t').expect("name<TAB>bytes");
            (name.to_owned(), bytes.to_owned())
        })
        .collect();
    assert_eq!(
        cases.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        golden.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "the preview cases and the preview golden file list different messages"
    );
    for ((name, bytes), (_, expected)) in cases.iter().zip(&golden) {
        assert_eq!(bytes, expected, "{name}");
    }
    // Every one of them reads back into the same bytes, as the older file's do.
    for (name, bytes) in &golden {
        match name.split(' ').next().unwrap() {
            "request" => {
                let value: Envelope<Request> = serde_json::from_str(bytes).unwrap();
                assert_eq!(serde_json::to_string(&value).unwrap(), *bytes, "{name}");
            }
            "response" | "reply" => {
                let value: ReplyEnvelope = serde_json::from_str(bytes).unwrap();
                assert_eq!(serde_json::to_string(&value).unwrap(), *bytes, "{name}");
            }
            "manifest" => {
                let manifest: Manifest = serde_json::from_str(bytes).unwrap();
                assert_eq!(serde_json::to_string(&manifest).unwrap(), *bytes, "{name}");
                assert_eq!(manifest.pairing.unwrap().max_seconds, 120);
                assert!(manifest.keep_alive);
                // Only a build with the switch on accepts such a manifest.
                #[cfg(feature = "protocol-3-preview")]
                assert_eq!(manifest.validate(), Ok(()), "{name}");
            }
            other => panic!("unknown preview kind {other}"),
        }
    }
}

/// The compatibility claim, stated against the file rather than left implicit:
/// a `Configure` with no key, and a reply with no rotated key, are the bytes
/// 00ab4da wrote. Neither field is ever written when it says nothing, and a
/// published package's `Request` and `Envelope` both refuse unknown fields.
#[test]
fn the_new_fields_reproduce_the_bytes_of_00ab4da() {
    let rows = golden();
    let row = |name: &str| {
        rows.iter()
            .find(|(row, _)| row == name)
            .map(|(_, bytes)| bytes.clone())
            .unwrap_or_else(|| panic!("no golden row {name}"))
    };
    // The request side, through the builder every caller now uses.
    assert_eq!(
        frame(
            2,
            Request::configure(json!({"host":"avr.local","port":23,"token":"private","tls":false})),
        ),
        row("request configure")
    );
    assert_eq!(
        frame(
            2,
            Request::configure_with(
                json!({"host":"avr.local","port":23,"token":"private","tls":false}),
                None
            ),
        ),
        row("request configure")
    );
    assert!(!row("request configure").contains("credential"));
    // The reply side: every response row, written through the envelope that
    // can carry a rotated key, with none to carry.
    let mut replies = 0;
    for (name, bytes) in &rows {
        if !name.starts_with("response ") {
            continue;
        }
        replies += 1;
        let read: ReplyEnvelope =
            serde_json::from_str(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(read.store_credential.is_none(), "{name}");
        assert_eq!(
            serde_json::to_string(&ReplyEnvelope {
                id: read.id,
                body: read.body,
                store_credential: None,
            })
            .unwrap(),
            *bytes,
            "{name}"
        );
        assert!(!bytes.contains("store_credential"), "{name}");
    }
    assert!(replies >= 15, "{replies} replies");
}
