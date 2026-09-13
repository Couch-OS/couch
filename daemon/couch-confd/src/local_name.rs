//! `couch.local`: the config server's name on the LAN, and its plain-port
//! listener.
//!
//! Browsers, phones and laptops resolve `.local` names over mDNS without any
//! setup, so the daemon answers for `couch.local` itself rather than depending
//! on a router's DNS or on anyone learning the DHCP address. The same crate
//! the streaming-TV discovery already uses does the responding: registering
//! an `_http._tcp` service under that host name makes it answer A and AAAA
//! queries for the name, and lets service browsers see "Couch" too.
//!
//! Only a wildcard bind advertises: a daemon on 127.0.0.1 is a developer's or
//! a test's, and must not claim the name on their network.
//!
//! The crate skips an interface whose multicast socket cannot be set up and
//! says so only through the `log` facade at debug level, so a daemon can
//! report "answering" while nothing listens. Two things make that visible on
//! a device nobody has a shell on: the crate's own log lines are kept (the
//! last few dozen) and every IPv4 interface is probed with the same multicast
//! join the crate performs; both are reported by `/api/health`.
use std::{
    collections::VecDeque,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Mutex, OnceLock},
};

pub const HOST: &str = "couch.local.";
pub const SERVICE: &str = "_http._tcp.local.";
pub const INSTANCE: &str = "Couch";
/// The port a browser assumes when the URL names none.
pub const PLAIN_PORT: u16 = 80;
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// The crate's first lines say which sockets it could set up; its later ones
/// are announcements. Keep both ends: a bind failure at startup must not be
/// pushed out by an afternoon of re-announcing.
const KEPT_HEAD_LINES: usize = 24;
const KEPT_TAIL_LINES: usize = 24;

/// Whether `addr` is the device's real listener rather than a local one.
pub fn is_device_listener(addr: &SocketAddr) -> bool {
    addr.ip().is_unspecified()
}

/// The address to also listen on so `http://couch.local` needs no port.
/// `None` when the main listener already is that, or is not the device's.
pub fn plain_listener(addr: &SocketAddr) -> Option<SocketAddr> {
    (is_device_listener(addr) && addr.port() != PLAIN_PORT)
        .then(|| SocketAddr::new(addr.ip(), PLAIN_PORT))
}

/// For the log line: the host name without its trailing dot.
pub fn display_host() -> &'static str {
    HOST.trim_end_matches('.')
}

/// One IPv4 interface as the probe saw it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct InterfaceProbe {
    pub name: String,
    pub ip: String,
    /// `/sys/class/net/<name>/flags` as the kernel prints it (hex), when readable.
    pub flags: Option<String>,
    /// IFF_MULTICAST (0x1000) in those flags, when known.
    pub multicast_capable: Option<bool>,
    /// Result of `IP_ADD_MEMBERSHIP` for 224.0.0.251 on this address.
    pub join: String,
}

/// What `/api/health` reports under `local_name`.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Status {
    pub host: String,
    pub advertised: bool,
    pub port: u16,
    /// Why registration failed, when it did.
    pub error: Option<String>,
    pub interfaces: Vec<InterfaceProbe>,
    /// The mDNS crate's own log lines, oldest first.
    pub log: Vec<String>,
}

static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
static LOG_HEAD: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOG_TAIL: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// Lines dropped between head and tail, so a reader knows there is a gap.
static LOG_DROPPED: Mutex<usize> = Mutex::new(0);

/// The kept lines, oldest first, with a marker where lines were dropped.
fn kept_log() -> Vec<String> {
    let mut lines: Vec<String> = LOG_HEAD.lock().map(|h| h.clone()).unwrap_or_default();
    let dropped = LOG_DROPPED.lock().map(|d| *d).unwrap_or(0);
    if dropped > 0 {
        lines.push(format!("… {dropped} lines not kept …"));
    }
    if let Ok(tail) = LOG_TAIL.lock() {
        lines.extend(tail.iter().cloned());
    }
    lines
}

fn keep_log_line(line: String) {
    if let Ok(mut head) = LOG_HEAD.lock() {
        if head.len() < KEPT_HEAD_LINES {
            head.push(line);
            return;
        }
    }
    if let Ok(mut tail) = LOG_TAIL.lock() {
        if tail.len() >= KEPT_TAIL_LINES {
            tail.pop_front();
            if let Ok(mut dropped) = LOG_DROPPED.lock() {
                *dropped += 1;
            }
        }
        tail.push_back(line);
    }
}

fn status_cell() -> &'static Mutex<Status> {
    STATUS.get_or_init(|| Mutex::new(Status::default()))
}

/// The current picture, for the health endpoint.
pub fn status() -> Status {
    let mut status = status_cell().lock().map(|s| s.clone()).unwrap_or_default();
    status.log = kept_log();
    status
}

/// Forward the mDNS crate's log records to stdout (the daemon's log file)
/// and keep the newest ones for the health endpoint. Other crates' records
/// are dropped, so this changes nothing else about the daemon's output.
struct Bridge;
impl log::Log for Bridge {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().starts_with("mdns_sd")
    }
    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {}", record.level(), record.args());
        println!("couch-confd: mdns: {line}");
        keep_log_line(line);
    }
    fn flush(&self) {}
}

fn install_bridge() {
    static BRIDGE: Bridge = Bridge;
    if log::set_logger(&BRIDGE).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

fn sys_flags(name: &str) -> Option<String> {
    std::fs::read_to_string(format!("/sys/class/net/{name}/flags"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// The same multicast join the crate performs on each address, reported
/// rather than swallowed. Loopback is skipped like the crate skips it.
pub fn probe_interfaces() -> Vec<InterfaceProbe> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|intf| !intf.is_loopback())
        .filter_map(|intf| match intf.ip() {
            std::net::IpAddr::V4(ip) => Some((intf.name, ip)),
            _ => None,
        })
        .map(|(name, ip)| {
            let flags = sys_flags(&name);
            let multicast_capable = flags
                .as_deref()
                .and_then(|f| u32::from_str_radix(f.trim_start_matches("0x"), 16).ok())
                .map(|f| f & 0x1000 != 0);
            let join = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
                .and_then(|socket| socket.join_multicast_v4(&MDNS_GROUP, &ip))
                .map(|()| "ok".to_owned())
                .unwrap_or_else(|e| e.to_string());
            InterfaceProbe {
                name,
                ip: ip.to_string(),
                flags,
                multicast_capable,
                join,
            }
        })
        .collect()
}

/// Answer for `couch.local` on every interface, advertising `port`. The
/// daemon is leaked on purpose: it must live as long as the process. The
/// outcome and the interface probe are recorded for `/api/health`.
pub fn advertise(port: u16) -> Result<(), String> {
    install_bridge();
    let interfaces = probe_interfaces();
    for probe in &interfaces {
        println!(
            "couch-confd: mdns: interface {} {} flags {} join {}",
            probe.name,
            probe.ip,
            probe.flags.as_deref().unwrap_or("?"),
            probe.join
        );
    }
    let result = (|| {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
        let info = mdns_sd::ServiceInfo::new(
            SERVICE,
            INSTANCE,
            HOST,
            "" as &str,
            port,
            [("path", "/")].as_slice(),
        )
        .map_err(|e| e.to_string())?
        .enable_addr_auto();
        daemon.register(info).map_err(|e| e.to_string())?;
        std::mem::forget(daemon);
        Ok::<(), String>(())
    })();
    if let Ok(mut status) = status_cell().lock() {
        *status = Status {
            host: display_host().to_owned(),
            advertised: result.is_ok(),
            port,
            error: result.as_ref().err().cloned(),
            interfaces,
            log: Vec::new(),
        };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wildcard_binds_are_the_device_and_get_a_plain_listener() {
        let device: SocketAddr = "0.0.0.0:8090".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:18092".parse().unwrap();
        let already: SocketAddr = "0.0.0.0:80".parse().unwrap();
        assert!(is_device_listener(&device));
        assert!(!is_device_listener(&local));
        assert_eq!(plain_listener(&device), Some("0.0.0.0:80".parse().unwrap()));
        assert_eq!(plain_listener(&local), None);
        assert_eq!(plain_listener(&already), None);
        assert_eq!(display_host(), "couch.local");
    }

    #[test]
    fn the_probe_lists_no_loopback_and_answers_for_each_v4_interface() {
        for probe in probe_interfaces() {
            assert_ne!(probe.name, "lo");
            assert!(probe.ip.parse::<Ipv4Addr>().is_ok());
            assert!(!probe.join.is_empty());
        }
    }

    #[test]
    fn kept_log_holds_both_ends_and_marks_the_gap() {
        for i in 0..100 {
            keep_log_line(format!("line {i}"));
        }
        let lines = kept_log();
        assert_eq!(lines.first().map(String::as_str), Some("line 0"));
        assert_eq!(lines[KEPT_HEAD_LINES - 1], "line 23");
        assert!(
            lines[KEPT_HEAD_LINES].starts_with("… "),
            "{:?}",
            lines[KEPT_HEAD_LINES]
        );
        assert_eq!(lines.last().map(String::as_str), Some("line 99"));
        assert_eq!(lines.len(), KEPT_HEAD_LINES + 1 + KEPT_TAIL_LINES);
    }

    #[test]
    fn status_starts_empty_and_serializes() {
        let text = serde_json::to_string(&status()).unwrap();
        assert!(text.contains("\"advertised\""));
        assert!(text.contains("\"interfaces\""));
    }
}
