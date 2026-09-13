//! What the Settings menu's Network section shows: address, gateway, DNS,
//! the Wi-Fi MAC and the web UI's name.
//!
//! Read from the kernel's own tables rather than by running `ip` or `route`:
//! `/proc/net/route` carries the default route and the subnet mask, the
//! resolver file carries DNS, and sysfs the MAC. The address is what the
//! kernel would source a packet to the gateway from, found by connecting a
//! UDP socket, which sends nothing.
use std::net::{Ipv4Addr, UdpSocket};

/// The Wi-Fi interface every route of interest is on.
const INTERFACE: &str = "wlan0";
/// The name the config daemon advertises over mDNS.
pub const WEB_HOST: &str = "couch.local";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// "192.168.1.127 / 24", or "" when there is no address.
    pub address: String,
    pub gateway: String,
    /// Comma separated, at most two.
    pub dns: String,
    pub mac: String,
}

impl Info {
    /// The URL a browser reaches the web UI on: the mDNS name, with the plain
    /// address as the fallback shown beside it when one is known.
    pub fn web(&self) -> String {
        match self.address.split(" /").next().filter(|a| !a.is_empty()) {
            Some(ip) => format!("http://{WEB_HOST}  ·  {ip}:8090"),
            None => format!("http://{WEB_HOST}"),
        }
    }
}

/// One route table row: destination, gateway and mask, as the kernel prints
/// them (little-endian hex), already turned into addresses.
struct Route {
    destination: Ipv4Addr,
    gateway: Ipv4Addr,
    mask: Ipv4Addr,
}

fn hex_address(field: &str) -> Option<Ipv4Addr> {
    let raw = u32::from_str_radix(field, 16).ok()?;
    Some(Ipv4Addr::from(raw.swap_bytes()))
}

/// The interface's routes from a `/proc/net/route` listing.
fn routes(table: &str, interface: &str) -> Vec<Route> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 || fields[0] != interface {
                return None;
            }
            Some(Route {
                destination: hex_address(fields[1])?,
                gateway: hex_address(fields[2])?,
                mask: hex_address(fields[7])?,
            })
        })
        .collect()
}

/// The default route's gateway, if the interface has one.
pub fn gateway_of(table: &str, interface: &str) -> Option<Ipv4Addr> {
    routes(table, interface)
        .into_iter()
        .find(|route| route.destination.is_unspecified() && !route.gateway.is_unspecified())
        .map(|route| route.gateway)
}

/// The prefix length of the interface's on-link route, for "/24".
pub fn prefix_of(table: &str, interface: &str) -> Option<u32> {
    routes(table, interface)
        .into_iter()
        .filter(|route| !route.destination.is_unspecified() && route.gateway.is_unspecified())
        .map(|route| u32::from(route.mask).count_ones())
        .max()
}

/// Nameservers from a resolver file, first two, in order.
pub fn nameservers(resolv: &str) -> Vec<String> {
    resolv
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "nameserver")
                .then(|| words.next())
                .flatten()
        })
        .filter(|address| address.parse::<std::net::IpAddr>().is_ok())
        .take(2)
        .map(str::to_owned)
        .collect()
}

/// The address the kernel sources traffic to `peer` from, without sending any.
fn source_address(peer: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect((peer, 9)).ok()?;
    match socket.local_addr().ok()? {
        std::net::SocketAddr::V4(address) => Some(*address.ip()),
        _ => None,
    }
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The live picture, read fresh each time; every piece degrades to "" alone.
pub fn current() -> Info {
    let table = read("/proc/net/route");
    let gateway = gateway_of(&table, INTERFACE);
    let prefix = prefix_of(&table, INTERFACE);
    let address = gateway
        .or(Some(Ipv4Addr::new(192, 0, 2, 1)))
        .and_then(source_address)
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    Info {
        address: match (address, prefix) {
            (Some(ip), Some(prefix)) => format!("{ip} /{prefix}"),
            (Some(ip), None) => ip.to_string(),
            (None, _) => String::new(),
        },
        gateway: gateway.map(|g| g.to_string()).unwrap_or_default(),
        dns: nameservers(&read("/etc/resolv.conf")).join(", "),
        mac: read(&format!("/sys/class/net/{INTERFACE}/address"))
            .trim()
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
ap0\t000A0A0A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn route_table_yields_the_gateway_and_prefix_of_the_wifi_interface() {
        assert_eq!(
            gateway_of(TABLE, "wlan0"),
            Some(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(prefix_of(TABLE, "wlan0"), Some(24));
        assert_eq!(gateway_of(TABLE, "ap0"), None);
        assert_eq!(prefix_of(TABLE, "eth0"), None);
        assert_eq!(gateway_of("Iface\tDestination\n", "wlan0"), None);
        assert_eq!(gateway_of("wlan0\tjunk\n", "wlan0"), None);
    }

    #[test]
    fn nameservers_keep_order_skip_junk_and_stop_at_two() {
        let resolv = "# generated\nsearch lan\nnameserver 192.168.1.1\nnameserver not-an-address\nnameserver 1.1.1.1\nnameserver 8.8.8.8\n";
        assert_eq!(nameservers(resolv), ["192.168.1.1", "1.1.1.1"]);
        assert!(nameservers("").is_empty());
    }

    #[test]
    fn web_row_names_the_mdns_host_and_the_address_when_known() {
        let mut info = Info::default();
        assert_eq!(info.web(), "http://couch.local");
        info.address = "192.168.1.127 /24".into();
        assert_eq!(info.web(), "http://couch.local  ·  192.168.1.127:8090");
    }
}
