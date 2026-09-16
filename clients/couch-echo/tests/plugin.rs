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
        let root = std::env::temp_dir().join(format!(
            "couch-echo-plugin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_couch-plugin-echo"),
            root.join("bin/couch-plugin-echo"),
        )
        .unwrap();
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
        endpoint.request(Request::Command {
            function: "volume-up".into()
        }),
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
    let first = std::thread::spawn(move || {
        first.request(Request::Command {
            function: "volume-up".into(),
        })
    });
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
                endpoint.request(Request::Command {
                    function: "power-on".into(),
                })
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
