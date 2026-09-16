//! Reusable admission checks for independently installed integration binaries.
//!
//! Catalog packages run these cases against their real subprocess and a
//! scripted device. The package supplies only its wire dialogue and expected
//! readings; process isolation, capability gating, timeout recovery and queue
//! pressure are asserted here once.

use crate::{Endpoint, Error, Host, Manifest, Request, Selectable, Status, QUEUE_CAPACITY};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

static NEXT_PACKAGE: AtomicUsize = AtomicUsize::new(0);

/// The two package artifacts every admission case executes.
#[derive(Clone, Copy)]
pub struct Adapter<'a> {
    pub binary: &'a Path,
    pub manifest_json: &'a str,
}

/// A package directory with the production binary at its manifest path.
pub struct Package {
    root: PathBuf,
    pub manifest: Manifest,
}

impl Package {
    pub fn new(adapter: Adapter<'_>) -> Self {
        let manifest: Manifest =
            serde_json::from_str(adapter.manifest_json).expect("valid embedded plugin manifest");
        manifest.validate().expect("valid plugin manifest");
        // Reserve the package root atomically. create_dir_all would silently
        // reuse debris if the operating system reused a test process ID.
        let parent = adapter.binary.parent().expect("test binary parent");
        let root = loop {
            let candidate = parent.join(format!(
                ".couch-plugin-admission-{}-{}-{}",
                manifest.id,
                std::process::id(),
                NEXT_PACKAGE.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create admission package: {error}"),
            }
        };
        let executable = root.join(&manifest.executable);
        std::fs::create_dir_all(executable.parent().expect("binary parent"))
            .expect("create admission package");
        std::fs::copy(adapter.binary, &executable).expect("copy real plugin binary");
        Self { root, manifest }
    }

    pub fn host(&self) -> Host {
        Host::spawn(&self.root, &self.manifest, Duration::from_secs(5))
            .expect("real plugin handshake")
    }

    pub fn endpoint(&self, settings: Value, timeout: Duration) -> Endpoint {
        Endpoint::start_with_timeout(&self.root, self.manifest.clone(), settings, timeout)
            .expect("configured plugin endpoint")
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub type DeviceSettings = fn(&MockHost) -> Value;

pub struct ConformanceCase {
    pub offline_settings: Value,
    pub invalid_settings: Value,
    pub device_settings: DeviceSettings,
    pub script: Script,
    pub command: &'static str,
    pub expected_requests: &'static [&'static str],
    pub check: fn(&Status, &[Selectable]),
}

/// Proves handshake and validation are offline, then observes a real command,
/// status read and input enumeration through the package subprocess.
pub fn conformance(adapter: Adapter<'_>, case: ConformanceCase) {
    let package = Package::new(adapter);
    let mut offline = package.host();
    offline
        .configure(case.offline_settings)
        .expect("valid settings configure without contacting the device");
    assert_eq!(
        offline.configure(case.invalid_settings),
        Err(Error::Invalid),
        "invalid settings must be rejected before device I/O"
    );

    let device = MockHost::start(case.script);
    let mut host = package.host();
    host.configure((case.device_settings)(&device))
        .expect("configure live fixture");
    assert!(
        device.requests().is_empty(),
        "configure must not contact the device"
    );
    host.command(case.command).expect("declared command");
    let status = host.status().expect("status response");
    let inputs = if package.manifest.supports_inputs {
        host.inputs().expect("input response")
    } else {
        Vec::new()
    };
    (case.check)(&status, &inputs);
    assert_eq!(device.requests(), case.expected_requests);
}

pub struct FailureCase {
    pub device_settings: DeviceSettings,
    pub unknown_command: &'static str,
    pub request: Request,
    pub malformed_requests: &'static [&'static str],
    pub disconnected_requests: &'static [&'static str],
    pub malformed: Script,
    pub disconnected: Script,
}

/// Proves the manifest gate costs no device I/O and malformed or disconnected
/// devices fail closed through the real adapter process.
pub fn failure(adapter: Adapter<'_>, case: FailureCase) {
    let package = Package::new(adapter);
    let idle = MockHost::start(Script::new());
    let mut host = package.host();
    host.configure((case.device_settings)(&idle)).unwrap();
    assert_eq!(host.command(case.unknown_command), Err(Error::Unsupported));
    assert!(
        idle.requests().is_empty(),
        "an undeclared command reached the device"
    );

    for (script, expected, requests) in [
        (case.malformed, Error::Protocol, case.malformed_requests),
        (
            case.disconnected,
            Error::Transport,
            case.disconnected_requests,
        ),
    ] {
        let device = MockHost::start(script);
        let mut host = package.host();
        host.configure((case.device_settings)(&device)).unwrap();
        assert_eq!(host.request(case.request.clone()), Err(expected));
        assert_eq!(device.requests(), requests);
    }
}

pub struct TimeoutCase {
    pub device_settings: DeviceSettings,
    pub script: Script,
    pub command: &'static str,
    pub timed_out_requests: &'static [&'static str],
    pub recovery_request: Request,
    pub recovery_requests: &'static [&'static str],
}

/// A timed-out request is sent once. A later explicit request replaces the
/// failed child and can recover; the failed command is never replayed.
pub fn timeout_no_retry(adapter: Adapter<'_>, case: TimeoutCase) {
    let device = MockHost::start(case.script);
    let package = Package::new(adapter);
    let endpoint = package.endpoint((case.device_settings)(&device), Duration::from_millis(600));
    assert_eq!(
        endpoint.request(Request::Command {
            function: case.command.into(),
        }),
        Err(Error::Timeout)
    );
    assert_eq!(device.requests(), case.timed_out_requests);
    assert!(
        endpoint.request(case.recovery_request).is_ok(),
        "a later explicit request did not recover"
    );
    let mut expected = case.timed_out_requests.to_vec();
    expected.extend_from_slice(case.recovery_requests);
    assert_eq!(device.requests(), expected);
}

pub struct SpikeCase {
    pub device_settings: DeviceSettings,
    pub command: &'static str,
    pub initial_requests: &'static [&'static str],
    pub terminator: u8,
}

/// Holds one real request in flight, releases more callers than the bounded
/// endpoint can queue, and proves old queued commands expire without reaching
/// the device. Assertions deliberately avoid scheduler-dependent exact counts.
pub fn spike(adapter: Adapter<'_>, case: SpikeCase) {
    let device = MockHost::start(Script::new().terminator(case.terminator));
    let package = Package::new(adapter);
    let endpoint = package.endpoint((case.device_settings)(&device), Duration::from_secs(3));
    let first_endpoint = endpoint.clone();
    let command = case.command.to_owned();
    let first =
        std::thread::spawn(move || first_endpoint.request(Request::Command { function: command }));
    let start = Instant::now();
    while device.requests().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "first request stalled"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let callers = QUEUE_CAPACITY * 3;
    let barrier = Arc::new(Barrier::new(callers + 1));
    let requests = (0..callers)
        .map(|_| {
            let endpoint = endpoint.clone();
            let barrier = barrier.clone();
            let command = case.command.to_owned();
            std::thread::spawn(move || {
                barrier.wait();
                endpoint.request(Request::Command { function: command })
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(first.join().unwrap(), Err(Error::Timeout));
    let replies = requests
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert!(replies.iter().any(|reply| *reply == Err(Error::Busy)));
    assert!(replies.iter().any(|reply| *reply == Err(Error::Expired)));
    assert!(replies
        .iter()
        .all(|reply| matches!(reply, Err(Error::Busy | Error::Expired))));
    assert_eq!(
        device.requests(),
        case.initial_requests,
        "queued or refused commands reached the device"
    );
}

/// Convenience constructors keep adapter test files focused on wire fixtures.
pub fn silence_script(terminator: u8, request: &str) -> Script {
    Script::new()
        .terminator(terminator)
        .on(request, Reply::Silence)
}
