//! Reusable admission checks for independently installed integration binaries.
//!
//! Catalog packages run these cases against their real subprocess and a fake
//! device. The package supplies only its wire dialogue and expected readings;
//! process isolation, capability gating, timeout recovery and queue pressure
//! are asserted here once.
//!
//! # The fake device is the integration's own
//!
//! What these cases assert is protocol-level and transport-agnostic: configure
//! performs no device I/O, invalid settings are refused offline, an undeclared
//! command costs no round trip, a timeout sends one command and is never
//! replayed, a burst does not queue stale work, and startup is race-free and
//! offline. None of that needs the harness to understand the wire format, so it
//! does not: an integration supplies a [`FakeDevice`], and the harness only
//! asks it for the settings that address it and for the list of requests it
//! saw. An HTTP server, a TLS endpoint or a WebSocket peer is as admissible as
//! a line protocol, and the pinned-certificate and explicit-API-root
//! integrations reach their fixture through ordinary settings rather than a
//! test-only trust bypass in shipping code.
//!
//! [`MockHost`] stays the default. A line-protocol integration keeps writing
//! [`ConformanceCase`], [`FailureCase`], [`TimeoutCase`] and [`SpikeCase`]
//! unchanged; each converts into the transport-agnostic case the harness runs.
//!
//! ```no_run
//! # use couch_plugin::testing::{self, Adapter, Conformance, FakeDevice, Fixture};
//! # use serde_json::{json, Value};
//! # struct FakeSonos;
//! # impl FakeSonos { fn start() -> Self { Self } fn api_root(&self) -> String { String::new() } }
//! # impl FakeDevice for FakeSonos {
//! #     fn settings(&self) -> Value { json!({}) }
//! #     fn requests(&self) -> Vec<String> { Vec::new() }
//! # }
//! # fn adapter() -> Adapter<'static> { unimplemented!() }
//! testing::conformance(
//!     adapter(),
//!     Conformance {
//!         offline_settings: json!({"host": "192.0.2.10"}),
//!         invalid_settings: json!({"host": "not-an-address"}),
//!         device: Fixture::new(FakeSonos::start),
//!         command: "play-pause",
//!         expected_requests: &["GET /api/v1/players/local/info"],
//!         check: |status, _inputs| assert_eq!(status.playing, Some(true)),
//!     },
//! );
//! ```
//!
//! The concurrent-startup case the curated feed also requires is deliberately
//! not a function here: it needs no device at all, and the feed checks that an
//! integration's own `tests/admission.rs` calls [`Package::new`] by name. Write
//! it as `couch-denon` and `couch-sonos` do - several threads, each building a
//! [`Package`] and an [`Endpoint`] against an address nothing answers. How many
//! is the integration's call: the executable is copied per package, so a large
//! adapter pays a real per-exec cost and a number chosen for a small one turns
//! the case into a timing test.

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
        // On Linux, fork can inherit another test thread's still write-open
        // copy destination. CLOEXEC is applied too late to prevent execve from
        // rejecting that executable with ETXTBSY. The Cargo artifact is
        // already immutable and on this same filesystem, so link it without a
        // writable window, matching production's immutable package slots.
        #[cfg(target_os = "linux")]
        std::fs::hard_link(adapter.binary, &executable).expect("link immutable real plugin binary");
        // Concurrent execution through one inode timed out on macOS; separate
        // copies do not have Linux's inherited-writer failure there.
        #[cfg(not(target_os = "linux"))]
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

/// A fake device one admission case addresses and observes.
///
/// The harness neither builds nor parses a request. It starts the device, hands
/// the packaged adapter the settings that point at it, and compares the
/// device's own log with what the case declared. That is why the log is
/// `Vec<String>` in the integration's own spelling - `MUON` for a Denon line,
/// `GET /api/v1/players/local/info` for a Sonos player - and why nothing here
/// mentions a terminator, a port or a scheme.
///
/// A device must stop serving when it is dropped: the harness holds it only for
/// the duration of one case.
pub trait FakeDevice {
    /// Settings that point a configured adapter at this device.
    ///
    /// Passed through `configure` unchanged, so every field must be declared in
    /// the manifest. This is also where a fixture's own certificate or plain
    /// HTTP origin arrives, through the same settings a user would supply.
    fn settings(&self) -> Value;

    /// Every request the device has seen, oldest first.
    ///
    /// Read repeatedly while a case runs, including while other threads drive
    /// the endpoint, so an implementation must not consume the log.
    fn requests(&self) -> Vec<String>;
}

/// How a case starts one fake device.
///
/// A factory rather than a device because [`failure`] needs three, each with a
/// fresh log, and because a case that reuses a device cannot tell the request
/// that arrived from the request that was left over.
pub struct Fixture(Box<dyn Fn() -> Box<dyn FakeDevice>>);

impl Fixture {
    /// A fake device of the integration's own making. The closure runs once per
    /// device the case needs.
    pub fn new<D: FakeDevice + 'static>(start: impl Fn() -> D + 'static) -> Self {
        Self(Box::new(move || Box::new(start()) as Box<dyn FakeDevice>))
    }

    /// The default: a scripted [`MockHost`] line protocol, with `settings`
    /// naming its loopback address the way the manifest spells it.
    pub fn line(script: Script, settings: DeviceSettings) -> Self {
        Self::new(move || LineDevice::start(script.clone(), settings))
    }

    fn start(&self) -> Box<dyn FakeDevice> {
        (self.0)()
    }
}

/// The line-protocol default: a [`MockHost`] and the settings addressing it.
pub struct LineDevice {
    host: MockHost,
    settings: Value,
}

impl LineDevice {
    pub fn start(script: Script, settings: DeviceSettings) -> Self {
        let host = MockHost::start(script);
        let settings = settings(&host);
        Self { host, settings }
    }

    /// The scripted peer, for a case that wants more than the request log.
    pub fn host(&self) -> &MockHost {
        &self.host
    }
}

impl FakeDevice for LineDevice {
    fn settings(&self) -> Value {
        self.settings.clone()
    }
    fn requests(&self) -> Vec<String> {
        self.host.requests()
    }
}

/// How a line-protocol case turns its [`MockHost`] into settings.
pub type DeviceSettings = fn(&MockHost) -> Value;

/// Handshake, offline validation, one command, one reading, one enumeration.
pub struct Conformance {
    pub offline_settings: Value,
    pub invalid_settings: Value,
    pub device: Fixture,
    pub command: &'static str,
    pub expected_requests: &'static [&'static str],
    pub check: fn(&Status, &[Selectable]),
}

/// [`Conformance`] for a line protocol, unchanged since the first packages.
pub struct ConformanceCase {
    pub offline_settings: Value,
    pub invalid_settings: Value,
    pub device_settings: DeviceSettings,
    pub script: Script,
    pub command: &'static str,
    pub expected_requests: &'static [&'static str],
    pub check: fn(&Status, &[Selectable]),
}

impl From<ConformanceCase> for Conformance {
    fn from(case: ConformanceCase) -> Self {
        Self {
            offline_settings: case.offline_settings,
            invalid_settings: case.invalid_settings,
            device: Fixture::line(case.script, case.device_settings),
            command: case.command,
            expected_requests: case.expected_requests,
            check: case.check,
        }
    }
}

/// Proves handshake and validation are offline, then observes a real command,
/// status read and input enumeration through the package subprocess.
pub fn conformance(adapter: Adapter<'_>, case: impl Into<Conformance>) {
    let case = case.into();
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

    let device = case.device.start();
    let mut host = package.host();
    host.configure(device.settings())
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

/// The capability gate, plus the two ways a device can answer badly.
pub struct Failure {
    /// A device that answers nothing: it only has to observe the silence the
    /// capability gate owes it.
    pub idle: Fixture,
    pub unknown_command: &'static str,
    pub request: Request,
    /// Answers `request` with something the adapter cannot use.
    pub malformed: Fixture,
    pub malformed_requests: &'static [&'static str],
    /// Hangs up on `request` instead of answering it.
    pub disconnected: Fixture,
    pub disconnected_requests: &'static [&'static str],
}

/// [`Failure`] for a line protocol. The idle device is an empty [`Script`].
pub struct FailureCase {
    pub device_settings: DeviceSettings,
    pub unknown_command: &'static str,
    pub request: Request,
    pub malformed_requests: &'static [&'static str],
    pub disconnected_requests: &'static [&'static str],
    pub malformed: Script,
    pub disconnected: Script,
}

impl From<FailureCase> for Failure {
    fn from(case: FailureCase) -> Self {
        Self {
            idle: Fixture::line(Script::new(), case.device_settings),
            unknown_command: case.unknown_command,
            request: case.request,
            malformed: Fixture::line(case.malformed, case.device_settings),
            malformed_requests: case.malformed_requests,
            disconnected: Fixture::line(case.disconnected, case.device_settings),
            disconnected_requests: case.disconnected_requests,
        }
    }
}

/// Proves the manifest gate costs no device I/O and malformed or disconnected
/// devices fail closed through the real adapter process.
pub fn failure(adapter: Adapter<'_>, case: impl Into<Failure>) {
    let case = case.into();
    let package = Package::new(adapter);
    let idle = case.idle.start();
    let mut host = package.host();
    host.configure(idle.settings()).unwrap();
    assert_eq!(host.command(case.unknown_command), Err(Error::Unsupported));
    assert!(
        idle.requests().is_empty(),
        "an undeclared command reached the device"
    );

    for (fixture, expected, requests) in [
        (case.malformed, Error::Protocol, case.malformed_requests),
        (
            case.disconnected,
            Error::Transport,
            case.disconnected_requests,
        ),
    ] {
        let device = fixture.start();
        let mut host = package.host();
        host.configure(device.settings()).unwrap();
        assert_eq!(host.request(case.request.clone()), Err(expected));
        assert_eq!(device.requests(), requests);
    }
}

/// One ambiguous timeout, then one explicit recovery.
pub struct TimeoutNoRetry {
    /// Must leave `command` unanswered past the harness's 600 ms deadline, and
    /// must still answer `recovery_request` afterwards.
    pub device: Fixture,
    pub command: &'static str,
    pub timed_out_requests: &'static [&'static str],
    pub recovery_request: Request,
    pub recovery_requests: &'static [&'static str],
}

/// [`TimeoutNoRetry`] for a line protocol. See [`silence_script`].
pub struct TimeoutCase {
    pub device_settings: DeviceSettings,
    pub script: Script,
    pub command: &'static str,
    pub timed_out_requests: &'static [&'static str],
    pub recovery_request: Request,
    pub recovery_requests: &'static [&'static str],
}

impl From<TimeoutCase> for TimeoutNoRetry {
    fn from(case: TimeoutCase) -> Self {
        Self {
            device: Fixture::line(case.script, case.device_settings),
            command: case.command,
            timed_out_requests: case.timed_out_requests,
            recovery_request: case.recovery_request,
            recovery_requests: case.recovery_requests,
        }
    }
}

/// A timed-out request is sent once. A later explicit request replaces the
/// failed child and can recover; the failed command is never replayed.
pub fn timeout_no_retry(adapter: Adapter<'_>, case: impl Into<TimeoutNoRetry>) {
    let case = case.into();
    let device = case.device.start();
    let package = Package::new(adapter);
    let endpoint = package.endpoint(device.settings(), Duration::from_millis(600));
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

/// Queue pressure behind one request that never completes.
pub struct Spike {
    /// Must accept `command`'s first request and never answer it.
    pub device: Fixture,
    pub command: &'static str,
    pub initial_requests: &'static [&'static str],
}

/// [`Spike`] for a line protocol: an empty [`Script`] on this terminator
/// answers nothing, which is exactly what this case needs.
pub struct SpikeCase {
    pub device_settings: DeviceSettings,
    pub command: &'static str,
    pub initial_requests: &'static [&'static str],
    pub terminator: u8,
}

impl From<SpikeCase> for Spike {
    fn from(case: SpikeCase) -> Self {
        Self {
            device: Fixture::line(
                Script::new().terminator(case.terminator),
                case.device_settings,
            ),
            command: case.command,
            initial_requests: case.initial_requests,
        }
    }
}

/// Holds one real request in flight, releases more callers than the bounded
/// endpoint can queue, and proves old queued commands expire without reaching
/// the device. Assertions deliberately avoid scheduler-dependent exact counts.
pub fn spike(adapter: Adapter<'_>, case: impl Into<Spike>) {
    let case = case.into();
    let command = case.command;
    let device = case.device.start();
    let package = Package::new(adapter);
    let endpoint = package.endpoint(device.settings(), Duration::from_secs(3));
    let first_endpoint = endpoint.clone();
    let first = std::thread::spawn(move || {
        first_endpoint.request(Request::Command {
            function: command.to_owned(),
        })
    });
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
            std::thread::spawn(move || {
                barrier.wait();
                endpoint.request(Request::Command {
                    function: command.to_owned(),
                })
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(first.join().unwrap(), Err(Error::Timeout));
    let replies = requests
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert!(replies.contains(&Err(Error::Busy)));
    assert!(replies.contains(&Err(Error::Expired)));
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
