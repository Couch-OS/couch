//! Building a client without the device, and proving it without the device.
//!
//! Two things live here. [`MockHost`] is a scripted TCP peer: it answers the
//! line-oriented request/response pattern that the Denon, Kodi and webOS
//! transports all use, and it can also refuse to answer or hang up, which are
//! the two failure paths a client gets wrong. [`contract_findings`] runs a
//! client against whatever host you point it at and reports what it got wrong.
//!
//! Neither needs hardware, a credential, or a network beyond loopback, which is
//! the whole point: a contributor with no Astrion HA100 on their desk can still
//! write a client and know it is correct up to the wire format.
//!
//! Enable with `features = ["testing"]` in `[dev-dependencies]`.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use couch_model::commands::Function;

use crate::{ClientSettings, DeviceClient, Error};

/// How the fake host answers one request.
#[derive(Clone, Debug)]
pub enum Reply {
    /// One line, plus the terminator.
    Line(String),
    /// Several lines, written in one `write_all` so the client also sees the
    /// case where two replies arrive in a single read.
    Lines(Vec<String>),
    /// Say nothing at all. The client must time out rather than block forever.
    Silence,
    /// Hang up mid-conversation.
    Close,
}

impl Reply {
    pub fn line(text: impl Into<String>) -> Self {
        Self::Line(text.into())
    }
}

/// What the fake host says, and to what.
#[derive(Clone, Debug)]
pub struct Script {
    terminator: u8,
    rules: Vec<(String, Reply)>,
    fallback: Reply,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            terminator: b'\r',
            rules: Vec::new(),
            fallback: Reply::Silence,
        }
    }
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    /// The byte that ends a request and each reply line. `\r` for a Denon AVR,
    /// `\n` for a newline protocol.
    pub fn terminator(mut self, byte: u8) -> Self {
        self.terminator = byte;
        self
    }

    /// Answer `request` with `reply`. Rules are matched in the order added;
    /// adding the same request twice answers it differently on each occurrence,
    /// which is how you script a state change.
    pub fn on(mut self, request: impl Into<String>, reply: Reply) -> Self {
        self.rules.push((request.into(), reply));
        self
    }

    /// What to do with a request no rule matched. Defaults to [`Reply::Silence`].
    pub fn otherwise(mut self, reply: Reply) -> Self {
        self.fallback = reply;
        self
    }
}

/// A scripted device on loopback.
///
/// Stops when dropped. Every request it received is available from
/// [`MockHost::requests`]. Pass this host to [`contract_findings`] and it will
/// observe those requests directly while it checks that a refused command
/// costs no round trip.
pub struct MockHost {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl MockHost {
    pub fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let port = listener.local_addr().expect("local address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let host = Self {
            port,
            requests: requests.clone(),
            stop: stop.clone(),
        };
        std::thread::spawn(move || {
            let mut sessions = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let script = script.clone();
                        let requests = requests.clone();
                        let stop = stop.clone();
                        sessions.push(std::thread::spawn(move || {
                            serve(socket, script, requests, stop)
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => return,
                }
            }
            for session in sessions {
                let _ = session.join();
            }
        });
        host
    }

    pub fn host(&self) -> &'static str {
        "127.0.0.1"
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every complete request received so far, oldest first.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log").clone()
    }
}

impl Drop for MockHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn serve(
    mut socket: TcpStream,
    script: Script,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
    let mut pending: VecDeque<(String, Reply)> = script.rules.into_iter().collect();
    let mut buffer: Vec<u8> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let mut bytes = [0; 256];
        match socket.read(&mut bytes) {
            Ok(0) => return,
            Ok(n) => buffer.extend_from_slice(&bytes[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return,
        }
        while let Some(at) = buffer.iter().position(|b| *b == script.terminator) {
            let line: Vec<u8> = buffer.drain(..=at).collect();
            let Ok(request) = String::from_utf8(line[..at].to_vec()) else {
                return;
            };
            requests.lock().expect("request log").push(request.clone());
            let reply = match pending.iter().position(|(want, _)| *want == request) {
                Some(index) => pending.remove(index).expect("scripted reply").1,
                None => script.fallback.clone(),
            };
            let lines = match reply {
                Reply::Line(line) => vec![line],
                Reply::Lines(lines) => lines,
                Reply::Silence => continue,
                Reply::Close => return,
            };
            let mut out = Vec::new();
            for line in lines {
                out.extend_from_slice(line.as_bytes());
                out.push(script.terminator);
            }
            if socket.write_all(&out).is_err() {
                return;
            }
        }
    }
}

/// Everything wrong with a client, as a list of sentences.
///
/// Empty means the client keeps the contract: its declarations parse, it
/// refuses what it never declared, it refuses nonsense *without touching the
/// network*, and its settings survive being written to disk and read back.
///
/// `settings` must address a reachable host - in a test, a [`MockHost`].
///
/// `host` is the [`MockHost`] addressed by `settings`. Checking the return
/// value alone cannot prove the gate held: a client that pings the device and
/// *then* refuses returns the right error and is still wrong, because a stale
/// button mapping would reach a device it was never allowed to touch. Taking
/// the host itself, rather than an arbitrary counter callback, makes the
/// no-round-trip assertion part of this checker rather than an optional claim.
pub fn contract_findings<C: DeviceClient>(settings: &C::Settings, host: &MockHost) -> Vec<String> {
    let mut findings = Vec::new();
    if C::KIND.is_empty()
        || !C::KIND
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        findings.push(format!(
            "KIND {:?} must be a lowercase kebab-case provider slug, matching Provider::kind()",
            C::KIND
        ));
    }
    if C::LABEL.is_empty() {
        findings.push("LABEL is empty; the connections UI has nothing to show".into());
    }
    if C::capabilities().is_empty() {
        findings
            .push("capabilities() is empty; no button could ever be mapped to this device".into());
    }
    for (index, (id, label)) in C::capabilities().iter().enumerate() {
        if label.is_empty() {
            findings.push(format!("capability `{id}` has no label"));
        }
        if C::capabilities()[..index]
            .iter()
            .any(|(seen, _)| seen == id)
        {
            findings.push(format!("capability `{id}` is declared twice"));
        }
        if id.starts_with("input:") || id.starts_with("app:") {
            findings.push(format!(
                "capability `{id}` is a dynamic function; declare it with supports_input/supports_app instead"
            ));
            continue;
        }
        match Function::parse(id) {
            Some(function) if function.id() == *id => {
                if !C::supports(&function) {
                    findings.push(format!(
                        "capability `{id}` is declared but supports() refuses it"
                    ));
                }
            }
            Some(function) => findings.push(format!(
                "capability `{id}` is not canonical; couch_model spells it `{}`",
                function.id()
            )),
            None => findings.push(format!(
                "capability `{id}` is not a couch_model::commands::Function"
            )),
        }
    }
    if C::actions().len() > 1 || C::actions().iter().any(|schema| !schema.is_valid()) {
        findings.push("typed action declarations are invalid or duplicated".into());
    }
    if let Err(e) = settings.validate() {
        findings.push(format!("the settings given to this check are invalid: {e}"));
        return findings;
    }
    match roundtrip_settings::<C>(settings) {
        Ok(()) => {}
        Err(e) => findings.push(e),
    }
    let mut client = match C::connect(settings) {
        Ok(client) => client,
        Err(e) => {
            findings.push(format!("connect failed against the given host: {e}"));
            return findings;
        }
    };
    // The gate is checked twice over: the answer, and the silence.
    let quiet = |client: &mut C, command: &str, findings: &mut Vec<String>| {
        let before = host.requests().len();
        let answer = client.command(command);
        if answer != Err(Error::Unsupported) {
            findings.push(format!(
                "command({command:?}) must answer Unsupported; it answered {answer:?}"
            ));
        }
        // The host counts requests on its own thread, so a probe sent a
        // microsecond ago may not be logged yet. Wait for the count to move
        // rather than reading it once and calling the client innocent.
        let deadline = std::time::Instant::now() + Duration::from_millis(100);
        let mut reached = 0;
        while std::time::Instant::now() < deadline {
            reached = host.requests().len().saturating_sub(before);
            if reached > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if reached > 0 {
            findings.push(format!(
                "command({command:?}) was refused but still sent {reached} request(s) to the device; the gate must come first"
            ));
        }
    };
    quiet(&mut client, "this-is-not-a-function", &mut findings);
    if let Some(undeclared) = ["power-off", "mute", "stop", "home", "yellow"]
        .into_iter()
        .find(|id| !C::capabilities().iter().any(|(name, _)| name == id))
    {
        quiet(&mut client, undeclared, &mut findings);
    }
    // Skipped entirely for a client that does support apps: this check is about
    // the gate, not about launching something on a real device.
    if !C::supports_app("definitely-not-installed") {
        quiet(&mut client, "app:definitely-not-installed", &mut findings);
    }
    // Extreme values are outside every valid schema; observe the mock peer
    // as well as the return value so a transport-first gate cannot pass.
    let before = host.requests().len();
    for tenths in [i16::MIN, i16::MAX] {
        let expected = if C::actions().is_empty() {
            Error::Unsupported
        } else {
            Error::Invalid
        };
        let answer = client.action(crate::TypedAction::SetVolumeDb { tenths });
        if answer != Err(expected) {
            findings.push(format!(
                "out-of-range typed action was not refused: {answer:?}"
            ));
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    if host.requests().len() != before {
        findings.push("refused typed action reached the device".into());
    }
    findings
}

fn roundtrip_settings<C: DeviceClient>(settings: &C::Settings) -> std::result::Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "couch-sdk-contract-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let connection = "contract-check";
    std::fs::create_dir_all(dir.join("connections").join(connection))
        .map_err(|e| format!("cannot create a temporary connection directory: {e}"))?;
    let path = C::Settings::path_in(&dir, connection)
        .map_err(|e| format!("cannot construct a temporary credential path: {e}"))?;
    let result = (|| {
        settings
            .save(&path)
            .map_err(|e| format!("settings could not be saved to {}: {e}", path.display()))?;
        let loaded = C::Settings::load(&path)
            .map_err(|e| format!("settings could not be read back from disk: {e}"))?;
        if serde_json::to_value(&loaded).ok() != serde_json::to_value(settings).ok() {
            return Err("settings did not survive a save/load round trip unchanged".into());
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// [`contract_findings`], as an assertion. Panics listing everything at once.
pub fn assert_contract<C: DeviceClient>(settings: &C::Settings, host: &MockHost) {
    let findings = contract_findings::<C>(settings, host);
    assert!(
        findings.is_empty(),
        "{} does not keep the client contract:\n- {}",
        C::KIND,
        findings.join("\n- ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, Result, Status};
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Serialize, Deserialize)]
    struct Settings {
        host: String,
        port: u16,
    }
    impl ClientSettings for Settings {
        const FILE_PREFIX: &'static str = "prober";
        fn validate(&self) -> Result<()> {
            if self.host.is_empty() || self.port == 0 {
                return Err(Error::Invalid);
            }
            Ok(())
        }
    }

    /// A client that keeps every rule the harness used to check and is still
    /// wrong: it touches the device *before* the gate, so a stale button
    /// mapping reaches a device it was never allowed to touch. It returns the
    /// correct error afterwards, which is exactly why the return value alone
    /// cannot catch it.
    struct Prober {
        socket: std::net::TcpStream,
    }
    impl DeviceClient for Prober {
        type Settings = Settings;
        const KIND: &'static str = "prober";
        const LABEL: &'static str = "Prober";
        fn capabilities() -> &'static [Capability] {
            &[("on", "On"), ("off", "Off")]
        }
        fn connect(settings: &Settings) -> Result<Self> {
            settings.validate()?;
            Ok(Self {
                socket: std::net::TcpStream::connect((settings.host.as_str(), settings.port))?,
            })
        }
        fn execute(&mut self, _: &Function) -> Result<()> {
            Ok(())
        }
        fn status(&mut self) -> Result<Status> {
            Ok(Status::on(true))
        }
        fn command(&mut self, command: &str) -> Result<()> {
            use std::io::Write;
            let _ = self
                .socket
                .write_all(format!("PROBE {command}\r").as_bytes());
            let function = Function::parse(command).ok_or(Error::Unsupported)?;
            if !Self::supports(&function) {
                return Err(Error::Unsupported);
            }
            self.execute(&function)
        }
    }

    #[test]
    fn a_client_that_pings_before_refusing_is_caught_even_though_it_answers_correctly() {
        let host = MockHost::start(Script::new().otherwise(Reply::line("OK")));
        let settings = Settings {
            host: host.host().into(),
            port: host.port(),
        };
        let findings = contract_findings::<Prober>(&settings, &host);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("still sent") && f.contains("this-is-not-a-function")),
            "the harness must notice the probe: {findings:?}"
        );
        assert!(
            host.requests()
                .iter()
                .any(|r| r.starts_with("PROBE this-is-not-a-function")),
            "the device really did see it: {:?}",
            host.requests()
        );
    }
}
