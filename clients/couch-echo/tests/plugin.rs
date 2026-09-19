use couch_plugin::{Endpoint, Error, Host, Manifest, Request, Response};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Package {
    root: PathBuf,
    manifest: Manifest,
}
impl Package {
    fn new() -> Self {
        // `tools/tests/old-package-wire.sh` points this at an executable built
        // from the source the published packages were built from.
        let binary = std::env::var_os("COUCH_ADMISSION_BINARY_ECHO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_couch-plugin-echo")));
        #[cfg(target_os = "linux")]
        let parent = binary.parent().unwrap().to_path_buf();
        #[cfg(not(target_os = "linux"))]
        let parent = std::env::temp_dir();
        let root = loop {
            let candidate = parent.join(format!(
                ".couch-echo-plugin-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create echo package: {error}"),
            }
        };
        std::fs::create_dir(root.join("bin")).unwrap();
        // A concurrent fork can inherit a copy destination's writable descriptor
        // and cause Linux execve to fail with ETXTBSY. Link the immutable Cargo
        // artifact on its own filesystem, just like the shared admission fixture.
        #[cfg(target_os = "linux")]
        std::fs::hard_link(&binary, root.join("bin/couch-plugin-echo")).unwrap();
        #[cfg(not(target_os = "linux"))]
        std::fs::copy(&binary, root.join("bin/couch-plugin-echo")).unwrap();
        let manifest = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        Self { root, manifest }
    }
    fn host(&self) -> Host {
        Host::spawn(&self.root, &self.manifest, Duration::from_secs(5)).unwrap()
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn settings(device: &MockHost) -> serde_json::Value {
    json!({"host":device.host(),"port":device.port()})
}

#[test]
fn real_subprocess_handshake_and_configuration_succeed_with_device_offline() {
    let p = Package::new();
    let mut host = p.host();
    host.configure(json!({"host":"127.0.0.1","port":1,"token":"private-token"}))
        .unwrap();
    assert_eq!(
        host.configure(json!({"host":"127.0.0.1","port":0})),
        Err(Error::Invalid)
    );
    assert_eq!(host.command("undeclared"), Err(Error::Unsupported));
    assert_eq!(host.command("power-on"), Err(Error::Transport));
}

#[test]
fn real_echo_subprocess_preserves_connection_order_status_inputs_and_refusals() {
    let device = MockHost::start(
        Script::new()
            .terminator(b'\n')
            .on("CMD volume-up", Reply::line("OK"))
            .on("CMD power-off", Reply::line("ERR locked"))
            .on(
                "GET STATUS",
                Reply::line("STATUS power=on;mute=off;volume=31;input=hdmi1"),
            )
            .on(
                "LIST INPUTS",
                Reply::Lines(vec!["INPUT hdmi1 Console".into(), "END".into()]),
            ),
    );
    let p = Package::new();
    let mut host = p.host();
    host.configure(settings(&device)).unwrap();
    assert!(device.requests().is_empty());
    assert_eq!(host.command("mute-on"), Err(Error::Unsupported));
    assert_eq!(
        host.command("input:hdmi1\nCMD power-off"),
        Err(Error::Unsupported)
    );
    host.command("volume-up").unwrap();
    assert_eq!(host.command("power-off"), Err(Error::Rejected));
    assert_eq!(host.status().unwrap().volume, Some(31));
    assert_eq!(host.inputs().unwrap()[0].id, "hdmi1");
    assert_eq!(
        device.requests(),
        [
            "CMD volume-up",
            "CMD power-off",
            "GET STATUS",
            "LIST INPUTS"
        ]
    );
}

#[test]
fn timeout_does_not_retry_and_a_later_explicit_request_can_reconnect() {
    let device = MockHost::start(
        Script::new()
            .terminator(b'\n')
            .on("GET STATUS", Reply::line("STATUS power=on")),
    );
    let p = Package::new();
    let endpoint = Endpoint::start_with_timeout(
        &p.root,
        p.manifest.clone(),
        settings(&device),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        endpoint.request(Request::command("volume-up")),
        Err(Error::Timeout)
    );
    assert_eq!(device.requests(), ["CMD volume-up"]);
    assert!(matches!(
        endpoint.request(Request::Status),
        Ok(Response::Status { .. })
    ));
    assert_eq!(device.requests(), ["CMD volume-up", "GET STATUS"]);
}

#[test]
fn endpoint_queue_is_bounded_and_stale_commands_are_never_sent() {
    let device = MockHost::start(Script::new().terminator(b'\n'));
    let p = Package::new();
    let endpoint = Endpoint::start_with_timeout(
        &p.root,
        p.manifest.clone(),
        settings(&device),
        Duration::from_secs(3),
    )
    .unwrap();
    let first = endpoint.clone();
    let first = std::thread::spawn(move || first.request(Request::command("volume-up")));
    let start = Instant::now();
    while device.requests().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(5));
    }
    let barrier = Arc::new(Barrier::new(13));
    let requests: Vec<_> = (0..12)
        .map(|_| {
            let endpoint = endpoint.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                endpoint.request(Request::command("power-on"))
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(first.join().unwrap(), Err(Error::Timeout));
    let replies: Vec<_> = requests.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        replies.iter().filter(|r| **r == Err(Error::Busy)).count(),
        4
    );
    assert_eq!(
        replies
            .iter()
            .filter(|r| **r == Err(Error::Expired))
            .count(),
        8
    );
    assert_eq!(device.requests(), ["CMD volume-up"]);
}

/// A key held down or long-pressed still reaches a protocol 1 package, as the
/// tap it has always been sent. Run against the published SDK's executable by
/// `tools/tests/old-package-wire.sh`, this is the proof that the phase never
/// leaves the host: that child would refuse the frame and exit.
#[test]
fn a_held_or_long_pressed_key_reaches_a_protocol_1_package_as_a_tap() {
    use couch_plugin::KeyPhase;
    let device = MockHost::start(Script::new().terminator(b'\n').otherwise(Reply::line("OK")));
    let p = Package::new();
    let mut host = p.host();
    host.configure(settings(&device)).unwrap();
    for phase in [KeyPhase::Tap, KeyPhase::Repeat, KeyPhase::LongPress] {
        assert_eq!(host.key("volume-up", phase), Ok(()), "{phase:?}");
    }
    assert_eq!(host.command("x:info"), Err(Error::Unsupported));
    assert!(host.is_alive());
    assert_eq!(
        device.requests(),
        ["CMD volume-up", "CMD volume-up", "CMD volume-up"]
    );
    let endpoint = Endpoint::start(&p.root, p.manifest.clone(), settings(&device)).unwrap();
    assert_eq!(
        endpoint.request_detailed(Request::key("volume-up", KeyPhase::LongPress)),
        Ok(Response::Ok)
    );
}

/// The protocol 3 fixture's manifest, with the switch off, which is how every
/// shipped Couch is built: the host refuses it as needing a newer Couch
/// without running anything, and the SDK's `serve` refuses to speak it.
#[cfg(not(feature = "protocol-3-preview"))]
#[test]
fn the_protocol_3_fixture_manifest_is_refused_while_the_switch_is_off() {
    let manifest: Manifest = serde_json::from_str(include_str!("fixtures/plugin-v3.json")).unwrap();
    assert_eq!(manifest.protocol_version, 3);
    assert_eq!(manifest.validate(), Err(Error::Incompatible));
    let mut p = Package::new();
    p.manifest = manifest.clone();
    // The executable exists and would answer; it must not be asked.
    std::fs::rename(
        p.root.join("bin/couch-plugin-echo"),
        p.root.join("bin/couch-plugin-echo-v3"),
    )
    .unwrap();
    assert!(matches!(
        Host::spawn(&p.root, &p.manifest, Duration::from_secs(5)),
        Err(Error::Incompatible)
    ));
    assert!(matches!(
        Endpoint::start(
            &p.root,
            manifest.clone(),
            json!({"host":"127.0.0.1","port":1})
        ),
        Err(Error::Incompatible)
    ));
    // Child side: returns before reading a byte of stdin.
    assert_eq!(
        couch_plugin::serve::<couch_echo::EchoTv>(manifest),
        Err(Error::Incompatible)
    );
}

/// Not a test of this tree: a control for `tools/tests/old-package-wire.sh`,
/// which runs it against an executable built from the published SDK. That child
/// refuses unknown fields, so a frame with a key phase, which this tree's host
/// never sends it, makes it exit without answering. If this ever passed
/// against this tree's executable the control would prove nothing, so it is
/// ignored everywhere else.
#[test]
#[ignore = "run by tools/tests/old-package-wire.sh against an old executable"]
fn control_a_child_built_from_the_published_sdk_exits_on_a_key_phase() {
    use std::process::{Command, Stdio};
    let p = Package::new();
    let mut child = Command::new(p.root.join("bin/couch-plugin-echo"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    couch_plugin::write_frame(
        &mut input,
        &json!({"id":1,"body":{"method":"hello","protocol_version":1}}),
    )
    .unwrap();
    let hello: serde_json::Value = couch_plugin::read_frame(&mut output).unwrap();
    assert_eq!(hello["body"]["type"], "hello");
    // The same frame without the phase is answered (unsupported before
    // configure is fine: it is an answer).
    couch_plugin::write_frame(
        &mut input,
        &json!({"id":2,"body":{"method":"command","function":"volume-up"}}),
    )
    .unwrap();
    let answer: serde_json::Value = couch_plugin::read_frame(&mut output).unwrap();
    assert_eq!(answer["id"], 2);
    couch_plugin::write_frame(
        &mut input,
        &json!({"id":3,"body":{"method":"command","function":"volume-up","phase":"repeat"}}),
    )
    .unwrap();
    assert!(
        couch_plugin::read_frame::<_, serde_json::Value>(&mut output).is_err(),
        "the child answered a frame it should not have been able to read"
    );
    assert_eq!(child.wait().unwrap().code(), Some(1));
}
