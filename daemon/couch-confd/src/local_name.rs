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
use std::net::SocketAddr;

pub const HOST: &str = "couch.local.";
pub const SERVICE: &str = "_http._tcp.local.";
pub const INSTANCE: &str = "Couch";
/// The port a browser assumes when the URL names none.
pub const PLAIN_PORT: u16 = 80;

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

/// Answer for `couch.local` on every interface, advertising `port`. The
/// daemon is leaked on purpose: it must live as long as the process.
pub fn advertise(port: u16) -> Result<(), String> {
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
    Ok(())
}

/// For the log line: the host name without its trailing dot.
pub fn display_host() -> &'static str {
    HOST.trim_end_matches('.')
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
}
