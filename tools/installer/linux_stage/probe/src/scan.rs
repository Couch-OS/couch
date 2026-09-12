//! Bounded RAM-stage supplicant control. Scan data never enters diagnostics.
use serde::Serialize;
use std::{
    fs, io,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const CONTROL: &str = "/tmp/couch-wpa/wlan0";
const LIMIT: usize = 8192;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Control {
    socket: UnixDatagram,
    path: PathBuf,
    deadline: Instant,
}
impl Control {
    fn open(server: &Path, directory: &Path, deadline: Instant) -> io::Result<Self> {
        let path = directory.join(format!(
            "couch-scan-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = UnixDatagram::bind(&path)?;
        let result = Self {
            socket,
            path,
            deadline,
        };
        fs::set_permissions(&result.path, fs::Permissions::from_mode(0o600))?;
        result.socket.connect(server)?;
        Ok(result)
    }
    fn drain(&self) -> io::Result<()> {
        self.socket.set_nonblocking(true)?;
        let mut bytes = [0; LIMIT + 1];
        let result = loop {
            if Instant::now() >= self.deadline {
                break Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            match self.socket.recv(&mut bytes) {
                Ok(n) if n > LIMIT => break Err(io::Error::from(io::ErrorKind::InvalidData)),
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.socket.set_nonblocking(false)?;
        result
    }
    fn receive(&self) -> io::Result<String> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        self.socket
            .set_read_timeout(Some(remaining.min(Duration::from_secs(2))))?;
        let mut bytes = [0; LIMIT + 1];
        let n = self.socket.recv(&mut bytes)?;
        if n > LIMIT {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        String::from_utf8(bytes[..n].to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }
    fn command(&self, command: &str) -> io::Result<String> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        self.socket
            .set_write_timeout(Some(remaining.min(Duration::from_secs(2))))?;
        self.socket.send(command.as_bytes())?;
        self.receive()
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn reconfigure() -> io::Result<()> {
    reconfigure_at(
        Path::new(CONTROL),
        Path::new("/tmp"),
        Duration::from_secs(3),
    )
}
fn reconfigure_at(server: &Path, directory: &Path, budget: Duration) -> io::Result<()> {
    let control = Control::open(server, directory, Instant::now() + budget)?;
    if control.command("RECONFIGURE")?.trim() != "OK" {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(())
}
#[derive(Clone, Serialize, PartialEq, Debug)]
struct Network {
    ssid_hex: String,
    security: &'static str,
    dbm: i32,
}
fn ssid_bytes(value: &str) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(c) = bytes.next() {
        if c != b'\\' {
            result.push(c);
            continue;
        }
        result.push(match bytes.next()? {
            b'\\' => b'\\',
            b'"' => b'"',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'e' => 27,
            b'x' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                (high * 16 + low) as u8
            }
            _ => return None,
        });
    }
    (1..=32).contains(&result.len()).then_some(result)
}
fn parse_bss(text: &str) -> Option<Network> {
    let field = |name: &str| text.lines().find_map(|line| line.strip_prefix(name));
    let ssid = ssid_bytes(field("ssid=")?)?;
    let dbm: i32 = field("level=")?.parse().ok()?;
    if !(-127..=0).contains(&dbm) {
        return None;
    }
    let flags = field("flags=")?;
    let has_akm = |akm: &str| {
        flags
            .split('[')
            .filter_map(|p| p.strip_suffix(']'))
            .any(|p| {
                let mut parts = p.split('-');
                matches!(parts.next(), Some("WPA2" | "RSN"))
                    && parts.next().is_some_and(|v| v.split('+').any(|x| x == akm))
            })
    };
    let security = if has_akm("PSK") {
        "wpa2"
    } else if has_akm("EAP") {
        "enterprise"
    } else if has_akm("SAE") {
        "wpa3"
    } else if flags.contains("[WEP]") {
        "wep"
    } else if flags.starts_with('[')
        && flags.contains("[ESS]")
        && flags.split('[').skip(1).all(|part| {
            matches!(
                part.strip_suffix(']'),
                Some("ESS" | "WPS" | "WPS-PBC" | "WPS-PIN")
            )
        })
    {
        "open"
    } else {
        "unsupported"
    };
    Some(Network {
        ssid_hex: ssid.iter().map(|b| format!("{b:02x}")).collect(),
        security,
        dbm,
    })
}
fn merge(networks: &mut Vec<Network>, value: Network) {
    if let Some(existing) = networks
        .iter_mut()
        .find(|n| n.ssid_hex == value.ssid_hex && n.security == value.security)
    {
        existing.dbm = existing.dbm.max(value.dbm);
    } else {
        networks.push(value);
    }
}
fn collect(server: &Path, directory: &Path, budget: Duration) -> io::Result<(Vec<Network>, bool)> {
    let deadline = Instant::now() + budget;
    let events = Control::open(server, directory, deadline)?;
    if events.command("ATTACH")?.trim() != "OK" {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let control = Control::open(server, directory, deadline)?;
    // Discard cached APs and queued events before requesting our scan.
    if control.command("BSS_FLUSH 0")?.trim() != "OK" {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    events.drain()?;
    if control.command("SCAN")?.trim() != "OK" {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    let mut started = false;
    loop {
        match events.receive() {
            Ok(event) if event.contains("CTRL-EVENT-SCAN-STARTED") => started = true,
            Ok(event) if started && event.contains("CTRL-EVENT-SCAN-RESULTS") => break,
            Ok(event) if event.contains("CTRL-EVENT-SCAN-FAILED") => {
                return Err(io::Error::from(io::ErrorKind::Other))
            }
            Ok(_) => (),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) && Instant::now() < deadline =>
            {
                ()
            }
            Err(e) => return Err(e),
        }
    }
    let mut networks = Vec::new();
    let mut truncated = true;
    // Per-BSS queries avoid the supplicant's truncated SCAN_RESULTS table.
    for index in 0..128 {
        let result = control.command(&format!("BSS {index}"))?;
        if result.trim().is_empty() || result.trim() == "FAIL" {
            truncated = false;
            break;
        }
        if let Some(value) = parse_bss(&result) {
            merge(&mut networks, value);
        }
    }
    networks.sort_by(|a, b| {
        b.dbm
            .cmp(&a.dbm)
            .then(a.ssid_hex.cmp(&b.ssid_hex))
            .then(a.security.cmp(b.security))
    });
    truncated |= networks.len() > 64;
    networks.truncate(64);
    Ok((networks, truncated))
}
pub fn response() -> Vec<u8> {
    let value = if super::wifi::active() {
        serde_json::json!({"status":"unavailable","networks":[],"truncated":false})
    } else {
        match collect(
            Path::new(CONTROL),
            Path::new("/tmp"),
            Duration::from_secs(12),
        ) {
            Ok((networks, truncated)) => {
                serde_json::json!({"status":"ok","networks":networks,"truncated":truncated})
            }
            Err(_) => serde_json::json!({"status":"unavailable","networks":[],"truncated":false}),
        }
    };
    value.to_string().into_bytes()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bss(ssid: &str, flags: &str, dbm: i32) -> String {
        format!("ssid={ssid}\nflags={flags}\nlevel={dbm}\n")
    }
    #[test]
    fn escaped_and_non_utf8_ssids_keep_exact_bytes() {
        assert_eq!(
            ssid_bytes(r#"a\x00\xff\e\n\t\\\""#),
            Some(vec![b'a', 0, 255, 27, 10, 9, 92, 34])
        );
        assert_eq!(ssid_bytes(" café "), Some(" café ".as_bytes().to_vec()));
        for s in ["", r"\x0", r"\q", &"x".repeat(33)] {
            assert!(ssid_bytes(s).is_none());
        }
    }
    #[test]
    fn duplicate_networks_keep_strongest_signal_but_separate_security() {
        let mut networks = Vec::new();
        for (flags, dbm) in [
            ("[WPA2-PSK-CCMP][ESS]", -60),
            ("[WPA2-PSK-CCMP][ESS]", -40),
            ("[ESS]", -30),
        ] {
            merge(&mut networks, parse_bss(&bss("same", flags, dbm)).unwrap());
        }
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].dbm, -40);
        assert_eq!(networks[1].security, "open");
        assert_eq!(
            parse_bss(&bss("same", "[WPA2-EAP-CCMP][ESS]", -45))
                .unwrap()
                .security,
            "enterprise"
        );
        assert!(parse_bss(&bss("", "[ESS]", -40)).is_none());
        assert!(parse_bss(&bss("ssid", "[ESS]", 255)).is_none());
    }
    struct Fixture {
        directory: PathBuf,
        server: PathBuf,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
        commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl Fixture {
        fn new(mode: &'static str) -> Self {
            let directory = PathBuf::from(format!(
                "/tmp/cscan-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let server = directory.join("server");
            let socket = UnixDatagram::bind(&server).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let done = stop.clone();
            let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = commands.clone();
            let worker = std::thread::spawn(move || {
                let mut monitor = None;
                while !done.load(Ordering::Relaxed) {
                    let mut bytes = [0; 256];
                    let Ok((n, peer)) = socket.recv_from(&mut bytes) else {
                        continue;
                    };
                    let peer = peer.as_pathname().unwrap();
                    let command = std::str::from_utf8(&bytes[..n]).unwrap();
                    seen.lock().unwrap().push(command.to_owned());
                    let reply = |data: &[u8], to: &Path| {
                        let _ = socket.send_to(data, to);
                    };
                    match command {
                        "ATTACH" => {
                            monitor = Some(peer.to_owned());
                            reply(b"OK\n", peer);
                            reply(b"<3>CTRL-EVENT-SCAN-RESULTS\n", peer);
                        }
                        "BSS_FLUSH 0" => reply(b"OK\n", peer),
                        "RECONFIGURE" => match mode {
                            "reconfigure-silent" => (),
                            "reconfigure-fail" => reply(b"FAIL\n", peer),
                            _ => reply(b"OK\n", peer),
                        },
                        "SCAN" => {
                            if mode == "busy" {
                                reply(b"FAIL-BUSY\n", peer);
                                continue;
                            }
                            reply(b"OK\n", peer);
                            let monitor = monitor.as_ref().unwrap();
                            if mode == "silent" {
                                continue;
                            }
                            if mode == "oversize" {
                                reply(&vec![b'x'; LIMIT + 1], monitor);
                                continue;
                            }
                            if mode == "stale" {
                                reply(b"<3>CTRL-EVENT-SCAN-RESULTS\n", monitor);
                                continue;
                            }
                            if mode == "flood" {
                                let until = Instant::now() + Duration::from_millis(150);
                                while Instant::now() < until && !done.load(Ordering::Relaxed) {
                                    reply(b"<3>CTRL-EVENT-OTHER\n", monitor);
                                }
                                continue;
                            }
                            reply(b"<3>CTRL-EVENT-SCAN-STARTED\n", monitor);
                            reply(b"<3>CTRL-EVENT-SCAN-RESULTS\n", monitor);
                        }
                        command if command.starts_with("BSS ") => {
                            let index: usize = command[4..].parse().unwrap();
                            let count = if mode == "many" { 80 } else { 1 };
                            if index < count {
                                reply(
                                    bss(&format!("network-{index}"), "[WPA2-PSK-CCMP][ESS]", -40)
                                        .as_bytes(),
                                    peer,
                                );
                            } else {
                                reply(b"FAIL\n", peer);
                            }
                        }
                        _ => panic!("unexpected fixture operation"),
                    }
                }
            });
            Self {
                directory,
                server,
                stop,
                worker: Some(worker),
                commands,
            }
        }
        fn scan(&self, budget: Duration) -> io::Result<(Vec<Network>, bool)> {
            collect(&self.server, &self.directory, budget)
        }
        fn assert_clean(&self) {
            assert_eq!(
                fs::read_dir(&self.directory).unwrap().count(),
                1,
                "local sockets must be removed"
            );
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }
    #[test]
    fn fresh_scan_flushes_cache_and_enumerates_bss_after_start_and_completion() {
        let fixture = Fixture::new("fresh");
        let (networks, truncated) = fixture.scan(Duration::from_secs(1)).unwrap();
        assert_eq!(networks.len(), 1);
        assert!(!truncated);
        fixture.assert_clean();
        assert_eq!(
            *fixture.commands.lock().unwrap(),
            ["ATTACH", "BSS_FLUSH 0", "SCAN", "BSS 0", "BSS 1"]
        );
    }
    #[test]
    fn stale_completion_busy_silence_flood_and_oversize_never_return_cached_networks() {
        for mode in ["stale", "busy", "silent", "flood", "oversize"] {
            let fixture = Fixture::new(mode);
            let start = Instant::now();
            assert!(fixture.scan(Duration::from_millis(80)).is_err(), "{mode}");
            assert!(start.elapsed() < Duration::from_secs(1));
            fixture.assert_clean();
            assert!(!fixture
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.starts_with("BSS ")));
        }
    }
    #[test]
    fn long_results_are_bounded_and_report_partial_list() {
        let fixture = Fixture::new("many");
        let (networks, truncated) = fixture.scan(Duration::from_secs(2)).unwrap();
        assert_eq!(networks.len(), 64);
        assert!(truncated);
        fixture.assert_clean();
    }
    #[test]
    fn reconfigure_ack_failure_timeout_and_cleanup_are_bounded() {
        for mode in ["reconfigure-ok", "reconfigure-fail", "reconfigure-silent"] {
            let fixture = Fixture::new(mode);
            let result = reconfigure_at(
                &fixture.server,
                &fixture.directory,
                Duration::from_millis(50),
            );
            assert_eq!(result.is_ok(), mode == "reconfigure-ok");
            fixture.assert_clean();
        }
    }
    #[test]
    fn security_requires_exact_rsn_akm_and_never_guesses_open() {
        for (flags, expected) in [
            ("[WPA2-SAE+PSK-CCMP][ESS]", "wpa2"),
            ("[WPA2-PSK+SAE-CCMP][ESS]", "wpa2"),
            ("[RSN-PSK-CCMP][ESS]", "wpa2"),
            ("[WPA-PSK-TKIP][ESS]", "unsupported"),
            ("[WPA2-PSKLOOKALIKE-CCMP][ESS]", "unsupported"),
            ("[WPA2-SAE-CCMP][ESS]", "wpa3"),
            ("[WPA2-OWE-CCMP][ESS]", "unsupported"),
            ("[WEP][ESS]", "wep"),
            ("", "unsupported"),
            ("[WPS][ESS]", "open"),
            ("[WPS-PBC][ESS]", "open"),
            ("[WPS][PRIVACY][ESS]", "unsupported"),
        ] {
            assert_eq!(
                parse_bss(&bss("network", flags, -50)).unwrap().security,
                expected,
                "{flags}"
            );
        }
    }
}
