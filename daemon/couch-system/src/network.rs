//! Temporary network trials: saved credentials are written only after Save.
use crate::wifi;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    pub dbm: i32,
    pub secured: bool,
    pub supported: bool,
}

pub fn parse_scan(reply: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in reply.lines().skip(1) {
        let fields: Vec<_> = line.splitn(5, '\t').collect();
        if fields.len() != 5 || fields[4].is_empty() {
            continue;
        }
        let Ok(dbm) = fields[2].parse::<i32>() else {
            continue;
        };
        // MTK injects a zero-RSSI NVRAM diagnostic row; it is not an AP.
        if !(-127..0).contains(&dbm) {
            continue;
        }
        let ssid = wifi::Status::parse(&format!("wpa_state=COMPLETED\nssid={}\n", fields[4])).ssid;
        let flags = fields[3];
        let secured = flags.contains("WPA")
            || flags.contains("RSN")
            || flags.contains("WEP")
            || flags.contains("OWE");
        let supported = !secured || (flags.contains("PSK") && !flags.contains("EAP"));
        let network = Network {
            ssid,
            dbm,
            secured,
            supported,
        };
        if let Some(old) = networks
            .iter_mut()
            .find(|n| n.ssid == network.ssid && n.secured == secured)
        {
            if network.dbm > old.dbm {
                *old = network;
            }
        } else {
            networks.push(network);
        }
    }
    networks.sort_by(|a, b| b.dbm.cmp(&a.dbm).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

#[derive(Clone)]
struct SavedNetwork {
    id: u32,
    enabled: bool,
    current: bool,
}
fn parse_networks(reply: &str) -> Vec<SavedNetwork> {
    reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<_> = line.splitn(4, '\t').collect();
            if f.len() != 4 {
                return None;
            }
            Some(SavedNetwork {
                id: f[0].parse().ok()?,
                enabled: !f[3].contains("DISABLED"),
                current: f[3].contains("CURRENT"),
            })
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn validate(ssid: &str, password: &str) -> Result<()> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("Enter a network name of 1–32 bytes without control characters".into());
    }
    if !password.is_empty()
        && !(8..=63).contains(&password.len())
        && !(password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("Use an 8–63 byte password, or a 64-digit hexadecimal key".into());
    }
    if password.contains(['\n', '\r', '\0']) {
        return Err("Password contains an unsupported control character".into());
    }
    Ok(())
}

trait Backend {
    fn request(&mut self, command: &str) -> Result<String>;
    fn lease(&mut self, cancel: &AtomicBool) -> Result<String>;
    fn persist(&mut self, ssid: &str, block: &str) -> Result<()>;
}
fn ok(backend: &mut impl Backend, command: &str) -> Result<()> {
    if backend.request(command)? == "OK" {
        Ok(())
    } else {
        Err("Wi-Fi service rejected the configuration".into())
    }
}

struct Trial {
    id: u32,
    previous: Vec<SavedNetwork>,
    ssid: String,
    block: String,
    tested: bool,
}
impl Trial {
    fn prepare(backend: &mut impl Backend, ssid: String, psk: Option<String>) -> Result<Self> {
        let previous = parse_networks(&backend.request("LIST_NETWORKS")?);
        let id = backend
            .request("ADD_NETWORK")?
            .parse::<u32>()
            .map_err(|_| "Could not create a temporary network")?;
        let fields = vec![
            ("ssid", hex(ssid.as_bytes())),
            ("scan_ssid", "1".into()),
            (
                "key_mgmt",
                if psk.is_some() { "WPA-PSK" } else { "NONE" }.into(),
            ),
            ("priority", "10".into()),
        ];
        let mut block = format!("# net {ssid}\nnetwork={{\n");
        let configure = (|| {
            for (field, value) in &fields {
                ok(backend, &format!("SET_NETWORK {id} {field} {value}"))?;
                block.push_str(&format!("  {field}={value}\n"));
            }
            if let Some(psk) = psk {
                ok(backend, &format!("SET_NETWORK {id} psk {psk}"))?;
                block.push_str(&format!("  psk={psk}\n"));
            }
            Ok(())
        })();
        if let Err(e) = configure {
            let _ = backend.request(&format!("REMOVE_NETWORK {id}"));
            return Err(e);
        }
        block.push_str("}\n");
        Ok(Self {
            id,
            previous,
            ssid,
            block,
            tested: false,
        })
    }
    fn restore_flags(&self, backend: &mut impl Backend) -> Result<()> {
        let mut failure = None;
        for network in &self.previous {
            let cmd = if network.enabled {
                "ENABLE_NETWORK"
            } else {
                "DISABLE_NETWORK"
            };
            if let Err(e) = ok(backend, &format!("{cmd} {}", network.id)) {
                failure = Some(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn rollback(&self, backend: &mut impl Backend, renew: bool) -> Result<()> {
        let mut failure = None;
        if let Err(e) = ok(backend, &format!("REMOVE_NETWORK {}", self.id)) {
            failure = Some(e);
        }
        if let Some(previous) = self.previous.iter().find(|n| n.current) {
            if let Err(e) = ok(backend, &format!("SELECT_NETWORK {}", previous.id)) {
                failure = Some(e);
            }
        }
        if let Err(e) = self.restore_flags(backend) {
            failure = Some(e);
        }
        if renew && self.previous.iter().any(|n| n.current) {
            // DHCP may wait for association; a failure is surfaced, never called restored.
            if let Err(e) = backend.lease(&AtomicBool::new(false)) {
                failure = Some(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn save(&self, backend: &mut impl Backend) -> Result<()> {
        if !self.tested {
            return Err("Test the connection before saving".into());
        }
        backend.persist(&self.ssid, &self.block)?;
        // Re-enable fallback networks after SELECT_NETWORK disabled them.
        let _ = self.restore_flags(backend);
        Ok(())
    }
}

// A GUI restart must undo a trial even though the supplicant survives it.
const JOURNAL: &str = "/tmp/couch-wifi-trial.json";
fn socket_identity() -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/tmp/wpa/wlan0")
        .map(|m| m.ino())
        .map_err(|_| "Wi-Fi control socket is unavailable".into())
}
fn record_trial(trial: &Trial) -> Result<()> {
    let value = serde_json::json!({"socket":socket_identity()?,"id":trial.id,"ssid":trial.ssid,"block":trial.block,
        "previous":trial.previous.iter().map(|n| serde_json::json!([n.id,n.enabled,n.current])).collect::<Vec<_>>()});
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(JOURNAL)
        .map_err(|_| "A previous Wi-Fi trial still needs recovery")?;
    file.write_all(value.to_string().as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "Could not record the temporary connection".into())
}
fn clear_trial() {
    let _ = std::fs::remove_file(JOURNAL);
}
fn recover_trial(backend: &mut impl Backend) -> Result<()> {
    let raw = match std::fs::read(JOURNAL) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Could not read the pending Wi-Fi trial".into()),
    };
    let value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => {
            clear_trial();
            return Ok(());
        }
    };
    if value["socket"].as_u64() != Some(socket_identity()?) {
        clear_trial();
        return Ok(());
    }
    let id = value["id"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .ok_or("Invalid pending Wi-Fi trial")?;
    let ssid = value["ssid"]
        .as_str()
        .ok_or("Invalid pending network name")?
        .to_string();
    let block = value["block"]
        .as_str()
        .ok_or("Invalid pending network configuration")?
        .to_string();
    let root = if Path::new("/mnt/alpine/opt/couch").is_dir() {
        "/mnt/alpine/opt/couch"
    } else {
        "/opt/couch"
    };
    // Save may have completed immediately before the process exited.
    if std::fs::read_to_string(Path::new(root).join("networks.conf"))
        .is_ok_and(|s| s.contains(&block))
    {
        clear_trial();
        return Ok(());
    }
    let previous = value["previous"]
        .as_array()
        .ok_or("Invalid previous network list")?
        .iter()
        .map(|n| {
            Ok(SavedNetwork {
                id: n[0]
                    .as_u64()
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or("Invalid network id")?,
                enabled: n[1].as_bool().ok_or("Invalid network flag")?,
                current: n[2].as_bool().ok_or("Invalid current network")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let candidate = backend.request(&format!("GET_NETWORK {id} ssid"))?;
    // IDs are local to a supplicant; never remove an unrelated network.
    if candidate != hex(ssid.as_bytes()) && candidate != format!("\"{ssid}\"") {
        clear_trial();
        return Ok(());
    }
    Trial {
        id,
        previous,
        ssid,
        block,
        tested: false,
    }
    .rollback(backend, true)?;
    clear_trial();
    Ok(())
}

fn associated_with(reply: &str, id: u32, ssid: &str) -> bool {
    let status = wifi::Status::parse(reply);
    status.state == "COMPLETED"
        && status.ssid == ssid
        && reply.lines().any(|line| line == format!("id={id}"))
}

fn runtime_command(binary: &str) -> Command {
    if Path::new("/mnt/alpine/bin/busybox").exists() {
        let mut cmd = Command::new("/bin/busybox");
        cmd.args(["chroot", "/mnt/alpine", binary]);
        cmd
    } else {
        Command::new(binary)
    }
}
fn run(
    mut cmd: Command,
    input: Option<&str>,
    cancel: &AtomicBool,
    deadline: Duration,
) -> Result<std::process::Output> {
    cmd.process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| "Required Wi-Fi helper could not start")?;
    if let Some(input) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{input}");
        }
    }
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) {
                "Cancelled"
            } else {
                "Wi-Fi helper timed out"
            }
            .into());
        }
        if child
            .try_wait()
            .map_err(|_| "Could not read Wi-Fi helper status")?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|_| "Could not read Wi-Fi helper reply".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn derive_key(ssid: &str, password: &str, cancel: &AtomicBool) -> Result<Option<String>> {
    validate(ssid, password)?;
    if password.is_empty() {
        return Ok(None);
    }
    if password.len() == 64 {
        return Ok(Some(password.to_ascii_lowercase()));
    }
    let mut cmd = runtime_command("/sbin/wpa_passphrase");
    cmd.arg(ssid);
    let output = run(cmd, Some(password), cancel, Duration::from_secs(3))?;
    if !output.status.success() {
        return Err("Could not prepare the network password".into());
    }
    // Never retain/log wpa_passphrase's plaintext #psk comment.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let key = line.trim().strip_prefix("psk=")?;
            (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| Some(key.to_owned()))
        })
        .ok_or_else(|| "Could not prepare the network password".into())
}

fn merge_saved(old: &str, ssid: &str, block: &str) -> String {
    let mut output = String::new();
    let mut skip = false;
    for line in old.lines() {
        if line == format!("# net {ssid}") {
            skip = true;
            continue;
        }
        if skip {
            if line.trim() == "}" {
                skip = false;
            }
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    output.push_str(block);
    output
}
struct Live;
impl Backend for Live {
    fn request(&mut self, command: &str) -> Result<String> {
        wifi::command(command)
    }
    fn lease(&mut self, cancel: &AtomicBool) -> Result<String> {
        let mut cmd = runtime_command("/sbin/udhcpc");
        cmd.args(["-i", "wlan0", "-n", "-q", "-t", "4", "-T", "2"]);
        if !run(cmd, None, cancel, Duration::from_secs(12))?
            .status
            .success()
        {
            return Err("Password accepted, but no IP address was assigned".into());
        }
        self.request("STATUS")?
            .lines()
            .find_map(|l| {
                l.strip_prefix("ip_address=")
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
            .ok_or_else(|| "No IP address was assigned".into())
    }
    fn persist(&mut self, ssid: &str, block: &str) -> Result<()> {
        let root = if Path::new("/mnt/alpine/opt/couch").is_dir() {
            "/mnt/alpine/opt/couch"
        } else {
            "/opt/couch"
        };
        let path = Path::new(root).join("networks.conf");
        let old = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return Err("Could not read saved networks".into()),
        };
        let temporary = PathBuf::from(format!("{}.gui-{}", path.display(), std::process::id()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(merge_saved(&old, ssid, block).as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            std::fs::File::open(root)?.sync_all()
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|_: std::io::Error| "Could not save the network; retry or cancel".into())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Request {
    Hotspot,
    Scan,
    Test { ssid: String, password: String },
    Save,
    Cancel,
}
#[derive(Serialize, Deserialize)]
pub enum Event {
    Hotspot(Result<()>),
    Scanned(Result<Vec<Network>>),
    Tested(Result<(String, String)>),
    Saved(Result<()>),
    Cancelled(Result<()>),
    Expired(Result<()>),
}
pub struct Worker {
    pub tx: mpsc::Sender<Request>,
    pub rx: mpsc::Receiver<Event>,
    pub cancel: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    pub fn send(&self, request: Request) {
        self.cancel
            .store(matches!(request, Request::Cancel), Ordering::Relaxed);
        let _ = self.tx.send(request);
    }
    pub fn finish(mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        drop(self.tx);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
    pub fn start() -> Self {
        let (tx, requests) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let join = std::thread::spawn(move || {
            let mut backend = Live;
            if let Err(e) = recover_trial(&mut backend) {
                eprintln!("couch-system: Wi-Fi recovery: {e}");
            }
            let mut trial: Option<Trial> = None;
            let mut expires = None;
            loop {
                let request = match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some(t) = trial.take() {
                            if t.rollback(&mut backend, true).is_ok() {
                                clear_trial();
                            }
                        }
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if expires.is_some_and(|t| Instant::now() >= t) {
                            expires = None;
                            let result = trial
                                .take()
                                .map_or(Ok(()), |t| t.rollback(&mut backend, true));
                            if result.is_ok() {
                                clear_trial();
                            }
                            let _ = events.send(Event::Expired(result));
                        }
                        continue;
                    }
                };
                match request {
                    Request::Hotspot => {
                        let _ = events.send(Event::Hotspot(Err(
                            "Use the system hotspot endpoint".into()
                        )));
                    }
                    Request::Scan => {
                        let result = (|| {
                            ok(&mut backend, "SCAN")?;
                            for _ in 0..20 {
                                if flag.load(Ordering::Relaxed) {
                                    return Err("Cancelled".into());
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            Ok(parse_scan(&backend.request("SCAN_RESULTS")?))
                        })();
                        let _ = events.send(Event::Scanned(result));
                    }
                    Request::Test { ssid, password } => {
                        if trial.is_some() {
                            let _ = events.send(Event::Tested(Err(
                                "Save or cancel the current test first".into(),
                            )));
                            continue;
                        }
                        let result = (|| {
                            let key = derive_key(&ssid, &password, &flag)?;
                            recover_trial(&mut backend)?;
                            let mut candidate = Trial::prepare(&mut backend, ssid, key)?;
                            let result: Result<(String, String)> = (|| {
                                record_trial(&candidate)?;
                                ok(&mut backend, &format!("SELECT_NETWORK {}", candidate.id))?;
                                let start = Instant::now();
                                loop {
                                    if flag.load(Ordering::Relaxed) {
                                        return Err("Cancelled".into());
                                    }
                                    if associated_with(
                                        &backend.request("STATUS")?,
                                        candidate.id,
                                        &candidate.ssid,
                                    ) {
                                        break;
                                    }
                                    if start.elapsed() > Duration::from_secs(20) {
                                        return Err(
                                            "Could not connect. Check the password and signal."
                                                .into(),
                                        );
                                    }
                                    std::thread::sleep(Duration::from_millis(250));
                                }
                                let ip = backend.lease(&flag)?;
                                if !associated_with(
                                    &backend.request("STATUS")?,
                                    candidate.id,
                                    &candidate.ssid,
                                ) {
                                    return Err("Connection was lost during the test".into());
                                }
                                candidate.tested = true;
                                Ok((candidate.ssid.clone(), ip))
                            })();
                            match result {
                                Ok(value) => {
                                    trial = Some(candidate);
                                    expires = Some(Instant::now() + Duration::from_secs(60));
                                    Ok(value)
                                }
                                Err(error) => match candidate.rollback(&mut backend, true) {
                                    Ok(()) => {
                                        clear_trial();
                                        Err(format!("{error} Previous network restored."))
                                    }
                                    Err(_) => Err(format!(
                                        "{error} Could not restore the previous connection."
                                    )),
                                },
                            }
                        })();
                        let _ = events.send(Event::Tested(result));
                    }
                    Request::Save => {
                        let result = trial
                            .as_ref()
                            .ok_or_else(|| "Test the connection first".to_owned())
                            .and_then(|t| {
                                if !associated_with(&backend.request("STATUS")?, t.id, &t.ssid) {
                                    return Err("Connection was lost; cancel and test again".into());
                                }
                                t.save(&mut backend)
                            });
                        if result.is_ok() {
                            clear_trial();
                            trial = None;
                            expires = None;
                        }
                        let _ = events.send(Event::Saved(result));
                    }
                    Request::Cancel => {
                        expires = None;
                        let result = trial
                            .take()
                            .map_or(Ok(()), |t| t.rollback(&mut backend, true));
                        if result.is_ok() {
                            clear_trial();
                        }
                        let _ = events.send(Event::Cancelled(result));
                    }
                }
            }
        });
        Self {
            tx,
            rx,
            cancel,
            join: Some(join),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        commands: Vec<String>,
        saved: Vec<String>,
        reject: Option<String>,
    }
    impl Backend for Fake {
        fn request(&mut self, command: &str) -> Result<String> {
            self.commands.push(command.into());
            if self.reject.as_deref() == Some(command) {
                return Ok("FAIL".into());
            }
            Ok(match command { "LIST_NETWORKS" => "network id / ssid / bssid / flags\n1\tOld\tany\t[CURRENT]\n2\tDisabled\tany\t[DISABLED]\n3\tFallback\tany\t", "ADD_NETWORK" => "7", _ => "OK" }.into())
        }
        fn lease(&mut self, _: &AtomicBool) -> Result<String> {
            Ok("192.0.2.1".into())
        }
        fn persist(&mut self, _: &str, block: &str) -> Result<()> {
            self.saved.push(block.into());
            Ok(())
        }
    }
    #[test]
    fn same_ssid_on_old_network_does_not_pass_candidate_test() {
        assert!(!associated_with(
            "wpa_state=COMPLETED\nssid=Home\nid=1\n",
            7,
            "Home"
        ));
        assert!(associated_with(
            "wpa_state=COMPLETED\nssid=Home\nid=7\n",
            7,
            "Home"
        ));
    }
    #[test]
    fn trial_does_not_save_until_tested_and_explicitly_saved() {
        let mut fake = Fake::default();
        let mut trial =
            Trial::prepare(&mut fake, "Test \"wifi\"".into(), Some("a".repeat(64))).unwrap();
        assert!(fake.saved.is_empty());
        assert!(trial.save(&mut fake).is_err());
        trial.tested = true;
        trial.save(&mut fake).unwrap();
        assert_eq!(fake.saved.len(), 1);
        assert!(fake.saved[0].contains("ssid=5465737420227769666922"));
        assert!(!fake.commands.iter().any(|c| c == "SAVE_CONFIG"));
    }
    #[test]
    fn rollback_restores_selection_and_enabled_flags_without_saving() {
        let mut fake = Fake::default();
        let trial = Trial::prepare(&mut fake, "Test".into(), None).unwrap();
        trial.rollback(&mut fake, false).unwrap();
        assert!(fake.commands.ends_with(
            &[
                "REMOVE_NETWORK 7",
                "SELECT_NETWORK 1",
                "ENABLE_NETWORK 1",
                "DISABLE_NETWORK 2",
                "ENABLE_NETWORK 3"
            ]
            .map(str::to_owned)
        ));
        assert!(fake.saved.is_empty());
    }
    #[test]
    fn rejected_configuration_removes_temporary_network() {
        let mut fake = Fake {
            reject: Some("SET_NETWORK 7 scan_ssid 1".into()),
            ..Fake::default()
        };
        assert!(Trial::prepare(&mut fake, "Test".into(), None).is_err());
        assert_eq!(fake.commands.last().unwrap(), "REMOVE_NETWORK 7");
        assert!(fake.saved.is_empty());
    }
    #[test]
    fn scan_deduplicates_sorts_and_marks_unsupported_security() {
        assert!(parse_scan("header\na\t2412\t0\t[ESS]\tNVRAM WARNING: Err = 0x06\n").is_empty());
        let results = parse_scan("bssid / frequency / signal level / flags / ssid\na\t2412\t-80\t[WPA2-PSK-CCMP][ESS]\tHome\nb\t2412\t-45\t[WPA2-PSK-CCMP][ESS]\tHome\nc\t2412\t-60\t[WPA2-EAP-CCMP][ESS]\tOffice\nd\t2412\t-50\t[ESS]\tCafe\ne\t2412\t-40\t[ESS]\t\n");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].dbm, -45);
        assert!(!results[1].secured);
        assert!(!results[2].supported);
    }
    #[test]
    fn saved_network_replacement_is_exact_and_keeps_other_networks() {
        let old = "# net A.*\nnetwork={\n old\n}\n# net ABC\nnetwork={\n other\n}\n";
        assert_eq!(
            merge_saved(old, "A.*", "new\n"),
            "# net ABC\nnetwork={\n other\n}\nnew\n"
        );
        assert!(validate("Home", "short").is_err());
        assert!(validate("bad\nssid", "password").is_err());
        assert!(validate("Hidden", "password").is_ok());
    }
}
