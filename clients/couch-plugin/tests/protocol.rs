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
    p.script(&print_frame(
        &json!({"id":1,"body":{"type":"error","code":"incompatible"}}),
    ));
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Incompatible)
    ));
    let mut other = p.manifest.clone();
    other.version = "2.0.0".into();
    p.script(&print_frame(
        &json!({"id":1,"body":{"type":"hello","manifest":other}}),
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
