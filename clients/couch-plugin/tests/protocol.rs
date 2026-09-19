use couch_plugin::{
    read_frame, write_frame, Error, Host, HostPolicy, Manifest, Request, Response, MAX_FRAME,
};
use serde_json::json;
use std::{
    io::Cursor,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Package {
    root: PathBuf,
    manifest: Manifest,
}
impl Package {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "couch-plugin-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let manifest = serde_json::from_value(json!({"protocol_version":1,"id":"fixture","label":"Fixture","version":"1.0.0","executable":"bin/plugin","capabilities":[{"id":"power-on","label":"On"}],"settings":[{"id":"host","label":"Host","kind":"text","required":true},{"id":"port","label":"Port","kind":"integer","default":23}]})).unwrap();
        Self { root, manifest }
    }
    fn script(&self, text: &str) {
        let path = self.root.join("bin/plugin");
        std::fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn hello(&self) -> String {
        print_frame(&json!({"id":1,"body":{"type":"hello","manifest":self.manifest}}))
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn print_bytes(bytes: &[u8]) -> String {
    let escaped: String = bytes.iter().map(|b| format!("\\{:03o}", b)).collect();
    format!("printf '{escaped}'\n")
}
fn print_frame(value: &serde_json::Value) -> String {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, value).unwrap();
    print_bytes(&bytes)
}

#[test]
fn frames_are_bounded_before_allocating_and_validate_json() {
    for data in [
        vec![0, 0, 0, 0],
        ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0, 0, 1, b'{'],
    ] {
        assert_eq!(
            read_frame::<_, serde_json::Value>(&mut Cursor::new(data)),
            Err(Error::Protocol)
        );
    }
    assert_eq!(
        write_frame(&mut Vec::new(), &"a".repeat(MAX_FRAME)),
        Err(Error::Protocol)
    );
    let mut data = Vec::new();
    write_frame(&mut data, &json!({"ok":true})).unwrap();
    assert_eq!(
        read_frame::<_, serde_json::Value>(&mut Cursor::new(data)).unwrap(),
        json!({"ok":true})
    );
}

#[test]
fn curated_components_only_bind_declared_actions_and_compatible_state() {
    use couch_plugin::{Component, StatusField};
    let p = Package::new();
    let mut manifest = p.manifest.clone();
    assert!(
        manifest.presentation.is_empty(),
        "older manifests remain valid"
    );
    manifest.presentation = vec![
        Component::CommandGroup {
            title: "Power".into(),
            commands: vec!["power-on".into()],
        },
        Component::StatusText {
            label: "Power".into(),
            field: StatusField::On,
        },
    ];
    assert!(manifest.validate().is_ok());
    for component in [
        Component::CommandGroup {
            title: "Invalid".into(),
            commands: vec!["mute".into()],
        },
        Component::CommandGroup {
            title: "Invalid".into(),
            commands: vec!["power-on".into(), "power-on".into()],
        },
        Component::CommandGroup {
            title: "Empty".into(),
            commands: vec![],
        },
        Component::StatusText {
            label: "Bad\nlabel".into(),
            field: StatusField::Title,
        },
        Component::Toggle {
            label: "Wrong state".into(),
            state: StatusField::Volume,
            on: "power-on".into(),
            off: "power-on".into(),
        },
        Component::Toggle {
            label: "Same commands".into(),
            state: StatusField::On,
            on: "power-on".into(),
            off: "power-on".into(),
        },
        Component::Toggle {
            label: "Undeclared".into(),
            state: StatusField::On,
            on: "power-on".into(),
            off: "power-off".into(),
        },
        Component::InputSelector {
            label: "Inputs".into(),
        },
    ] {
        manifest.presentation = vec![component];
        assert_eq!(manifest.validate(), Err(Error::Invalid));
    }
    manifest.supports_inputs = true;
    assert!(manifest.validate().is_ok());
    manifest.presentation = vec![
        Component::StatusText {
            label: "State".into(),
            field: StatusField::On
        };
        17
    ];
    assert_eq!(manifest.validate(), Err(Error::Invalid));
    for field in ["arbitrary", "subtitle", "secret"] {
        assert!(serde_json::from_value::<Component>(
            json!({"kind":"status_text","label":"No","field":field})
        )
        .is_err());
    }
}

#[test]
fn manifest_settings_reject_unknowns_types_and_secrets_in_defaults() {
    let p = Package::new();
    assert!(p.manifest.validate().is_ok());
    assert_eq!(
        p.manifest
            .with_defaults(json!({"host":"avr.local"}))
            .unwrap(),
        json!({"host":"avr.local","port":23})
    );
    for value in [
        json!({}),
        json!({"host":""}),
        json!({"host":"avr","port":"23"}),
        json!({"host":"avr","unexpected":true}),
        json!({"host":"avr\n"}),
    ] {
        assert_eq!(p.manifest.validate_settings(&value), Err(Error::Invalid));
    }
    let mut bad = p.manifest.clone();
    for version in [
        "../../escape",
        "/tmp/plugin",
        ".",
        "..",
        "",
        "1/2",
        "version 1",
    ] {
        bad.version = version.into();
        assert_eq!(bad.validate(), Err(Error::Invalid));
    }
    bad = p.manifest.clone();
    bad.protocol_version = 2;
    assert_eq!(bad.validate(), Err(Error::Incompatible));
    bad = p.manifest.clone();
    bad.capabilities.push(bad.capabilities[0].clone());
    assert_eq!(bad.validate(), Err(Error::Invalid));
    bad = p.manifest.clone();
    bad.settings[1].kind = couch_plugin::FieldKind::Secret;
    bad.settings[1].default = Some(json!("private"));
    assert_eq!(bad.validate(), Err(Error::Invalid));
}

#[test]
fn only_a_protocol_3_manifest_may_name_buttons_of_its_own() {
    let p = Package::new();
    for manifest in [p.manifest.clone(), v2_manifest(p.manifest.clone())] {
        assert!(manifest.validate().is_ok());
        assert!(!manifest.supports("x:info"));
        let mut named = manifest.clone();
        named.capabilities.push(couch_plugin::Capability {
            id: "x:info".into(),
            label: "Info".into(),
        });
        assert_eq!(
            named.validate(),
            Err(Error::Invalid),
            "protocol {}",
            manifest.protocol_version
        );
        // Even a manifest that slipped through is never sent one.
        assert!(!named.supports("x:info"));
    }
}

/// Protocol 3 is unreleased. Without the `protocol-3-preview` feature, which
/// no shipped crate enables, its manifest is a package that needs a newer
/// Couch, whatever else it says, and nothing is executed.
#[cfg(not(feature = "protocol-3-preview"))]
#[test]
fn a_protocol_3_manifest_is_refused_as_needing_a_newer_couch() {
    assert_eq!(
        couch_plugin::accepted_protocol_version(),
        couch_plugin::PROTOCOL_VERSION
    );
    assert_eq!(couch_plugin::PROTOCOL_VERSION, 2);
    let mut p = Package::new();
    p.manifest = v3_manifest(p.manifest.clone());
    assert_eq!(p.manifest.validate(), Err(Error::Incompatible));
    let mut plain = v2_manifest(p.manifest.clone());
    plain.capabilities.pop();
    plain.protocol_version = 3;
    plain.min_core_protocol_version = 3;
    assert_eq!(plain.validate(), Err(Error::Incompatible));
    let marker = p.root.join("ran");
    p.script(&format!(": > {}\nexec /bin/sleep 10", marker.display()));
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Incompatible)
    ));
    assert!(matches!(
        couch_plugin::Endpoint::start(&p.root, p.manifest.clone(), json!({"host":"example"})),
        Err(Error::Incompatible)
    ));
    assert!(!marker.exists(), "a refused package was executed");
}

#[cfg(feature = "protocol-3-preview")]
#[test]
fn the_preview_switch_admits_protocol_3_and_nothing_newer() {
    assert_eq!(couch_plugin::accepted_protocol_version(), 3);
    assert_eq!(
        couch_plugin::PROTOCOL_VERSION,
        2,
        "the release constant never moves"
    );
    let p = Package::new();
    let manifest = v3_manifest(p.manifest.clone());
    assert_eq!(manifest.validate(), Ok(()));
    assert!(manifest.supports("x:info"));
    assert!(!manifest.supports("x:other"));
    let wire = serde_json::to_value(&manifest).unwrap();
    assert_eq!(wire["protocol_version"], 3);
    assert_eq!(wire["min_core_protocol_version"], 3);

    let mut next = manifest.clone();
    next.protocol_version = 4;
    next.min_core_protocol_version = 4;
    assert_eq!(next.validate(), Err(Error::Incompatible));

    for id in ["x:", "x:Info", "x:in fo", "x:info!"] {
        let mut bad = manifest.clone();
        bad.capabilities[1].id = id.into();
        assert_eq!(bad.validate(), Err(Error::Invalid), "{id}");
    }
    let mut many = manifest.clone();
    many.capabilities.truncate(1);
    for index in 0..32 {
        many.capabilities.push(couch_plugin::Capability {
            id: format!("x:button-{index}"),
            label: format!("Button {index}"),
        });
    }
    assert_eq!(many.validate(), Ok(()));
    many.capabilities.push(couch_plugin::Capability {
        id: "x:one-too-many".into(),
        label: "One too many".into(),
    });
    assert_eq!(many.validate(), Err(Error::Invalid));

    // Schemas are found by kind and no kind is declared twice. Only one kind
    // exists, so a second schema is still a duplicate.
    let mut twice = manifest.clone();
    twice.actions.push(twice.actions[0]);
    assert_eq!(twice.validate(), Err(Error::Invalid));
    let mut grouped = manifest.clone();
    grouped.presentation = vec![couch_plugin::Component::CommandGroup {
        title: "More".into(),
        commands: vec!["x:info".into()],
    }];
    assert_eq!(grouped.validate(), Ok(()));
}

/// What a scripted child saw on its stdin: the hello frame and `requests`
/// more, read by exact length so the child can answer in between.
fn seen(p: &Package) -> Vec<u8> {
    std::fs::read(p.root.join("seen")).unwrap_or_default()
}
fn read_exactly(p: &Package, bytes: usize) -> String {
    format!(
        "/bin/dd bs=1 count={bytes} >> {} 2>/dev/null\n",
        p.root.join("seen").display()
    )
}
/// The exact text, in the host's own field order: `json!` would sort it.
fn frame_bytes(text: &str) -> Vec<u8> {
    let mut bytes = (text.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes
}
fn hello_frame(manifest: &Manifest) -> Vec<u8> {
    frame_bytes(&format!(
        r#"{{"id":1,"body":{{"method":"hello","protocol_version":{}}}}}"#,
        manifest.protocol_version
    ))
}

#[test]
fn a_key_phase_never_reaches_a_protocol_1_or_2_package() {
    use couch_plugin::KeyPhase;
    // The bytes 00ab4da writes for a tap; see src/wire_golden.rs.
    let tap = frame_bytes(r#"{"id":2,"body":{"method":"command","function":"power-on"}}"#);
    for use_v2 in [false, true] {
        for phase in [KeyPhase::Tap, KeyPhase::Repeat, KeyPhase::LongPress] {
            let mut p = Package::new();
            if use_v2 {
                p.manifest = v2_manifest(p.manifest.clone());
            }
            let hello = hello_frame(&p.manifest);
            p.script(&format!(
                "{}{}{}exec /bin/sleep 10",
                p.hello(),
                read_exactly(&p, hello.len() + tap.len()),
                print_frame(&json!({"id":2,"body":{"type":"ok"}}))
            ));
            let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
            assert_eq!(host.key("power-on", phase), Ok(()), "{phase:?}");
            assert_eq!(seen(&p), [hello, tap.clone()].concat(), "{phase:?}");
        }
    }
}

#[cfg(feature = "protocol-3-preview")]
#[test]
fn a_protocol_3_package_is_told_the_phase_and_a_tap_is_still_not_written() {
    use couch_plugin::KeyPhase;
    for (function, phase, body) in [
        (
            "power-on",
            KeyPhase::Tap,
            r#"{"method":"command","function":"power-on"}"#,
        ),
        (
            "power-on",
            KeyPhase::Repeat,
            r#"{"method":"command","function":"power-on","phase":"repeat"}"#,
        ),
        (
            "x:info",
            KeyPhase::LongPress,
            r#"{"method":"command","function":"x:info","phase":"long_press"}"#,
        ),
    ] {
        let mut p = Package::new();
        p.manifest = v3_manifest(p.manifest.clone());
        let hello = hello_frame(&p.manifest);
        let expected = frame_bytes(&format!(r#"{{"id":2,"body":{body}}}"#));
        p.script(&format!(
            "{}{}{}exec /bin/sleep 10",
            p.hello(),
            read_exactly(&p, hello.len() + expected.len()),
            print_frame(&json!({"id":2,"body":{"type":"ok"}}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        assert_eq!(host.key(function, phase), Ok(()));
        assert_eq!(
            String::from_utf8_lossy(&seen(&p)),
            String::from_utf8_lossy(&[hello, expected].concat())
        );
    }
}

#[test]
fn a_package_named_button_is_refused_before_any_io_unless_declared() {
    let mut manifests = vec![Package::new().manifest.clone()];
    manifests.push(v2_manifest(manifests[0].clone()));
    #[cfg(feature = "protocol-3-preview")]
    manifests.push(v3_manifest(manifests[0].clone()));
    for manifest in manifests {
        let mut p = Package::new();
        p.manifest = manifest;
        // The child answers exactly one request after hello, with id 2. A
        // refused command that had reached it would have used that id up.
        p.script(&format!(
            "{}{}exec /bin/sleep 10",
            p.hello(),
            print_frame(&json!({"id":2,"body":{"type":"status","status":{"on":true}}}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        let declared = p.manifest.protocol_version >= 3;
        for function in ["x:undeclared", "x:Bad", "x:"] {
            assert_eq!(host.command(function), Err(Error::Unsupported));
        }
        if !declared {
            assert_eq!(host.command("x:info"), Err(Error::Unsupported));
            assert_eq!(couch_plugin::requires(&Request::command("x:info")), 3);
        }
        assert!(host.is_alive());
        assert_eq!(host.status().unwrap().on, Some(true));
    }
    assert_eq!(couch_plugin::requires(&Request::command("power-on")), 1);
    assert_eq!(couch_plugin::requires(&Request::Status), 1);
    assert_eq!(
        couch_plugin::requires(&Request::Action {
            action: couch_plugin::TypedAction::SetVolumeDb { tenths: 0 }
        }),
        2
    );
}

#[test]
fn a_reason_or_unpaired_from_a_protocol_1_or_2_package_retires_it() {
    for use_v2 in [false, true] {
        for body in [
            json!({"type":"error","code":"unpaired"}),
            json!({"type":"error","code":"rejected","reason":{"kind":"message","text":"Pair again"}}),
            json!({"type":"error","code":"invalid","reason":{"kind":"invalid_setting","field":"host","text":"No"}}),
        ] {
            let mut p = Package::new();
            if use_v2 {
                p.manifest = v2_manifest(p.manifest.clone());
            }
            p.script(&format!(
                "{}{}exec /bin/sleep 10",
                p.hello(),
                print_frame(&json!({"id":2,"body":body}))
            ));
            let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
            let pid = host.pid();
            assert_eq!(
                host.request_detailed(Request::Status),
                Err(Error::Protocol.into()),
                "{body}"
            );
            assert!(!host.is_alive());
            assert_eq!(
                unsafe { libc::kill(pid as i32, 0) },
                -1,
                "child must be reaped"
            );
        }
        // The nine codes it has always been allowed still arrive as they are.
        let mut p = Package::new();
        if use_v2 {
            p.manifest = v2_manifest(p.manifest.clone());
        }
        p.script(&format!(
            "{}{}exec /bin/sleep 10",
            p.hello(),
            print_frame(&json!({"id":2,"body":{"type":"error","code":"rejected"}}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        assert_eq!(
            host.request_detailed(Request::Status),
            Err(couch_plugin::Failure {
                code: Error::Rejected,
                reason: None
            })
        );
        assert!(host.is_alive());
    }
}

#[cfg(feature = "protocol-3-preview")]
#[test]
fn a_protocol_3_reason_is_kept_and_one_couch_cannot_show_retires_the_package() {
    use couch_plugin::{Failure, Reason};
    for (body, expected) in [
        (
            json!({"type":"error","code":"unpaired","reason":{"kind":"message","text":"Pair this TV again"}}),
            Some(Failure {
                code: Error::Unpaired,
                reason: Some(Reason::Message {
                    text: "Pair this TV again".into(),
                }),
            }),
        ),
        (
            json!({"type":"error","code":"invalid","reason":{"kind":"invalid_setting","field":"host","text":"Not an address"}}),
            Some(Failure {
                code: Error::Invalid,
                reason: Some(Reason::InvalidSetting {
                    field: "host".into(),
                    text: "Not an address".into(),
                }),
            }),
        ),
        (
            json!({"type":"error","code":"unpaired"}),
            Some(Error::Unpaired.into()),
        ),
        (
            json!({"type":"error","code":"rejected","reason":{"kind":"message","text":"a".repeat(161)}}),
            None,
        ),
        (
            json!({"type":"error","code":"rejected","reason":{"kind":"message","text":"two\nlines"}}),
            None,
        ),
        (
            json!({"type":"error","code":"invalid","reason":{"kind":"invalid_setting","field":"undeclared","text":"No"}}),
            None,
        ),
        (
            json!({"type":"error","code":"rejected","reason":{"kind":"link","text":"No"}}),
            None,
        ),
        (
            json!({"type":"error","code":"rejected","reason":{"kind":"message","text":"No","more":1}}),
            None,
        ),
    ] {
        let mut p = Package::new();
        p.manifest = v3_manifest(p.manifest.clone());
        p.script(&format!(
            "{}{}exec /bin/sleep 10",
            p.hello(),
            print_frame(&json!({"id":2,"body":body}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        let pid = host.pid();
        match expected {
            Some(failure) => {
                assert_eq!(
                    host.request_detailed(Request::Status),
                    Err(failure),
                    "{body}"
                );
                assert!(host.is_alive(), "{body}");
            }
            None => {
                assert_eq!(
                    host.request_detailed(Request::Status),
                    Err(Error::Protocol.into()),
                    "{body}"
                );
                assert!(!host.is_alive(), "{body}");
                assert_eq!(
                    unsafe { libc::kill(pid as i32, 0) },
                    -1,
                    "child must be reaped"
                );
            }
        }
    }
    // The plain call still answers with the code alone.
    let mut p = Package::new();
    p.manifest = v3_manifest(p.manifest.clone());
    p.script(&format!(
        "{}{}{}exec /bin/sleep 10",
        p.hello(),
        print_frame(&json!({"id":2,"body":{"type":"ok"}})),
        print_frame(&json!({"id":3,"body":{"type":"error","code":"unpaired","reason":{"kind":"message","text":"Pair again"}}}))
    ));
    let endpoint =
        couch_plugin::Endpoint::start(&p.root, p.manifest.clone(), json!({"host":"example"}))
            .unwrap();
    assert_eq!(
        endpoint.request_detailed(Request::Status),
        Err(Failure {
            code: Error::Unpaired,
            reason: Some(Reason::Message {
                text: "Pair again".into()
            })
        })
    );
}

#[test]
fn executable_must_be_contained_and_not_writable_by_other_users() {
    let mut p = Package::new();
    p.script("exit 1");
    std::fs::set_permissions(
        p.root.join("bin/plugin"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Invalid)
    ));
    symlink("/bin/sh", p.root.join("bin/outside")).unwrap();
    p.manifest.executable = "bin/outside".into();
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Invalid)
    ));
    for path in ["/bin/sh", "../plugin", "bin/../plugin", ""] {
        p.manifest.executable = path.into();
        assert_eq!(p.manifest.validate(), Err(Error::Invalid));
    }
}

#[test]
fn incompatible_handshake_and_wrong_identity_never_activate() {
    let p = Package::new();
    // Keep the peer alive until the host rejects the reply. Exiting with the
    // unread Hello request can reset a Unix socket before Linux delivers it.
    p.script(&format!(
        "{}exec /bin/sleep 10",
        print_frame(&json!({"id":1,"body":{"type":"error","code":"incompatible"}}),)
    ));
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Incompatible)
    ));
    let mut other = p.manifest.clone();
    other.version = "2.0.0".into();
    p.script(&format!(
        "{}exec /bin/sleep 10",
        print_frame(&json!({"id":1,"body":{"type":"hello","manifest":other}}),)
    ));
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Incompatible)
    ));
}

#[test]
fn wrong_ids_malformed_and_oversized_replies_retire_and_reap_child() {
    for payload in [
        print_frame(&json!({"id":99,"body":{"type":"status","status":{}}})),
        print_bytes(&[0, 0, 0, 1, b'{']),
        print_bytes(&((MAX_FRAME + 1) as u32).to_be_bytes()),
    ] {
        let p = Package::new();
        p.script(&format!("{}{}exec /bin/sleep 10", p.hello(), payload));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        let pid = host.pid();
        assert_eq!(host.request(Request::Status), Err(Error::Protocol));
        assert!(!host.is_alive());
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            -1,
            "child must be reaped"
        );
        assert_eq!(host.request(Request::Status), Err(Error::Transport));
    }
}

#[test]
fn timeout_is_absolute_including_partial_frames_and_kills_descendants() {
    let p = Package::new();
    // Each byte arrives inside a per-read timeout. Only an absolute deadline
    // prevents an indefinitely dribbling child from occupying its endpoint.
    p.script(&format!("{}printf '\\000'; /bin/sleep 0.06; printf '\\000'; /bin/sleep 0.06; printf '\\000'; /bin/sleep 0.06; printf '\\100'; exec /bin/sleep 10",p.hello()));
    let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
    host.set_timeout(Duration::from_millis(100)).unwrap();
    let pid = host.pid();
    let start = Instant::now();
    assert_eq!(host.request(Request::Status), Err(Error::Timeout));
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
}

#[test]
fn unsupported_commands_never_reach_child_and_drop_reaps_it() {
    let p = Package::new();
    p.script(&format!(
        "{}{}exec /bin/sleep 10",
        p.hello(),
        print_frame(&json!({"id":2,"body":{"type":"status","status":{"on":true}}}))
    ));
    let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
    let pid = host.pid();
    assert_eq!(host.command("play-pause"), Err(Error::Unsupported));
    assert_eq!(
        host.request(Request::Status),
        Ok(Response::Status {
            status: couch_plugin::Status::on(true)
        })
    );
    drop(host);
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
}

#[test]
fn final_endpoint_drop_synchronously_reaps_its_process() {
    let p = Package::new();
    p.script(&format!(
        "{}{}{}exec /bin/sleep 10",
        p.hello(),
        print_frame(&json!({"id":2,"body":{"type":"ok"}})),
        dynamic_status(3, "$$"),
    ));
    let endpoint =
        couch_plugin::Endpoint::start(&p.root, p.manifest.clone(), json!({"host":"example"}))
            .unwrap();
    let response = endpoint.request(Request::Status).unwrap();
    let Response::Status { status } = response else {
        panic!("expected status")
    };
    let pid: i32 = status.title.unwrap().parse().unwrap();
    let clone = endpoint.clone();
    drop(endpoint);
    let start = Instant::now();
    // If drop failed to close the queue before joining, this would deadlock.
    drop(clone);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn bridge_frame_deadline_cannot_be_extended_by_partial_writes() {
    use std::{io::Write, os::unix::net::UnixStream};
    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    let child = std::thread::spawn(move || {
        for byte in [0, 0, 0, 20] {
            if writer.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
    });
    let start = Instant::now();
    assert_eq!(
        couch_plugin::read_frame_timeout::<serde_json::Value>(
            &mut reader,
            Duration::from_millis(100)
        ),
        Err(Error::Timeout)
    );
    assert!(start.elapsed() < Duration::from_millis(500));
    drop(reader);
    child.join().unwrap();
}

#[test]
fn child_environment_is_cleared_and_linux_root_is_dropped() {
    let p = Package::new();
    p.script(&format!(
        "{}uid=$(/usr/bin/id -u)\n{}exec /bin/sleep 10",
        p.hello(),
        dynamic_status(2, "$uid:${USER-unset}")
    ));
    let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
    let uid = unsafe { libc::geteuid() };
    let expected = if cfg!(target_os = "linux") && uid == 0 {
        65534
    } else {
        uid
    };
    assert_eq!(
        host.status().unwrap().title,
        Some(format!("{expected}:unset"))
    );
}

#[test]
fn default_policy_selects_only_the_ha100_network_group() {
    #[cfg(all(target_os = "linux", target_arch = "arm", target_env = "musl"))]
    assert_eq!(HostPolicy::default().supplementary_gids(), &[3003]);
    #[cfg(not(all(target_os = "linux", target_arch = "arm", target_env = "musl")))]
    assert!(HostPolicy::default().supplementary_gids().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn root_spawn_installs_only_the_declared_supplementary_groups() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let p = Package::new();
    p.script(&format!(
        "{}groups=$(/usr/bin/id -G)\n{}exec /bin/sleep 10",
        p.hello(),
        dynamic_status(2, "$groups")
    ));
    let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
    let mut actual: Vec<u32> = host
        .status()
        .unwrap()
        .title
        .unwrap()
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .unwrap();
    let mut expected = vec![65534];
    expected.extend(HostPolicy::default().supplementary_gids());
    actual.sort_unstable();
    actual.dedup();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(actual, expected);
}

// Values in this helper are fixed test shell expressions, never user inputs.
fn dynamic_status(id: u64, value: &str) -> String {
    format!(
        r#"printf '\000\000\000'
body="{{\"id\":{id},\"body\":{{\"type\":\"status\",\"status\":{{\"title\":\"{value}\"}}}}}}"
length=${{#body}}
octal=$(printf '%03o' "$length")
printf "\\$octal"
printf '%s' "$body"
"#
    )
}

fn v3_manifest(manifest: Manifest) -> Manifest {
    let mut manifest = v2_manifest(manifest);
    manifest.protocol_version = 3;
    manifest.min_core_protocol_version = 3;
    manifest.capabilities.push(couch_plugin::Capability {
        id: "x:info".into(),
        label: "Info".into(),
    });
    manifest
}

fn v2_manifest(mut manifest: Manifest) -> Manifest {
    manifest.protocol_version = 2;
    manifest.min_core_protocol_version = 2;
    manifest.actions = vec![couch_plugin::PluginActionSchema::SetVolumeDb {
        min_tenths: -800,
        max_tenths: 180,
        step_tenths: 5,
    }];
    manifest
}

#[test]
fn v2_is_explicit_and_headless_actions_do_not_depend_on_presentation() {
    use couch_plugin::{Component, PluginActionSchema, StatusField, TypedAction};
    let package = Package::new();
    let v1 = &package.manifest;
    let wire = serde_json::to_value(v1).unwrap();
    assert!(wire.get("actions").is_none());
    assert!(wire.get("min_core_protocol_version").is_none());
    assert_eq!(
        v1.validate_action(TypedAction::SetVolumeDb { tenths: -345 }),
        Err(Error::Unsupported)
    );
    let mut manifest = v2_manifest(v1.clone());
    assert!(manifest.presentation.is_empty());
    assert_eq!(manifest.validate(), Ok(()));
    assert_eq!(
        manifest.validate_action(TypedAction::SetVolumeDb { tenths: -345 }),
        Ok(())
    );
    for tenths in [-805, -344, 185] {
        assert_eq!(
            manifest.validate_action(TypedAction::SetVolumeDb { tenths }),
            Err(Error::Invalid)
        );
    }
    manifest.presentation = vec![Component::VolumeDbControl {
        label: "Volume".into(),
    }];
    assert_eq!(manifest.validate(), Ok(()));
    manifest.actions.clear();
    assert_eq!(manifest.validate(), Err(Error::Invalid));
    manifest.presentation = vec![Component::StatusText {
        label: "Volume".into(),
        field: StatusField::VolumeDb,
    }];
    assert_eq!(
        manifest.validate(),
        Ok(()),
        "a readout does not authorize writes"
    );
    assert_eq!(
        manifest.validate_action(TypedAction::SetVolumeDb { tenths: -345 }),
        Err(Error::Unsupported)
    );
    manifest.protocol_version = 1;
    manifest.min_core_protocol_version = 1;
    assert_eq!(
        manifest.validate(),
        Err(Error::Invalid),
        "no v2 controls in v1"
    );
    // (3, 3) unless the protocol 3 preview is switched on, which no shipped
    // build does; then (4, 4).
    let next = couch_plugin::accepted_protocol_version() + 1;
    for (version, minimum) in [(0, 0), (1, 2), (2, 1), (2, 3), (next, next)] {
        let mut manifest = v2_manifest(v1.clone());
        manifest.protocol_version = version;
        manifest.min_core_protocol_version = minimum;
        assert_eq!(manifest.validate(), Err(Error::Incompatible));
    }
    for actions in [
        vec![PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 0,
        }],
        vec![
            PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5
            };
            2
        ],
    ] {
        manifest = v2_manifest(v1.clone());
        manifest.actions = actions;
        assert_eq!(manifest.validate(), Err(Error::Invalid));
    }
}

#[test]
fn typed_action_refusals_do_not_consume_a_child_request_and_bad_measurements_retire_it() {
    use couch_plugin::TypedAction;
    for use_v2 in [false, true] {
        let mut p = Package::new();
        if use_v2 {
            p.manifest = v2_manifest(p.manifest.clone());
        }
        p.script(&format!(
            "{}{}exec /bin/sleep 10",
            p.hello(),
            print_frame(&json!({"id":2,"body":{"type":"status","status":{"on":true}}}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        assert_eq!(
            host.action(TypedAction::SetVolumeDb { tenths: -805 }),
            Err(if use_v2 {
                Error::Invalid
            } else {
                Error::Unsupported
            })
        );
        assert_eq!(host.status().unwrap().on, Some(true));
    }
    for reading in [
        json!({"kind":"reading","tenths":301}),
        json!({"kind":"minimum","tenths":0}),
        json!({"kind":"reading","tenths":-34.5}),
        json!({"kind":"reading"}),
    ] {
        let mut p = Package::new();
        p.manifest = v2_manifest(p.manifest.clone());
        p.script(&format!(
            "{}{}exec /bin/sleep 10",
            p.hello(),
            print_frame(&json!({"id":2,"body":{"type":"status","status":{"volume_db":reading}}}))
        ));
        let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
        assert_eq!(host.status(), Err(Error::Protocol));
        assert!(!host.is_alive());
    }
    let p = Package::new();
    p.script(&format!(
        "{}{}exec /bin/sleep 10",
        p.hello(),
        print_frame(
            &json!({"id":2,"body":{"type":"status","status":{"volume_db":{"kind":"minimum"}}}})
        )
    ));
    let mut host = Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)).unwrap();
    assert_eq!(
        host.status(),
        Err(Error::Protocol),
        "v1 cannot smuggle a v2 reading"
    );
}
