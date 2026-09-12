use crate::Result;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::{IpAddr, UdpSocket},
    time::{Duration, Instant},
};
/// An unverified UPnP media-server advertisement, not proof of CoreELEC identity.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub address: IpAddr,
    pub server: String,
}
/// Three-second IPv4 SSDP search. Requires Kodi UPnP sharing to be enabled.
/// Does not fetch LOCATION URLs, authenticate, or send playback/OS commands.
pub fn discover() -> Result<Vec<Candidate>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_multicast_ttl_v4(2)?;
    socket.send_to(b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n", "239.255.255.250:1900")?;
    let end = Instant::now() + Duration::from_secs(3);
    let mut found = BTreeMap::new();
    let mut buf = [0; 8192];
    while Instant::now() < end && found.len() < 64 {
        socket.set_read_timeout(Some(
            end.saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        match socket.recv_from(&mut buf) {
            Ok((n, peer)) => {
                if let Some(candidate) = parse(&buf[..n], peer.ip()) {
                    found.insert(peer.ip(), candidate);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(found.into_values().collect())
}
fn parse(bytes: &[u8], address: IpAddr) -> Option<Candidate> {
    let s = std::str::from_utf8(bytes).ok()?;
    let mut lines = s.lines();
    if lines.next()?.trim_end() != "HTTP/1.1 200 OK" {
        return None;
    }
    let mut server = None;
    let mut st = None;
    for line in lines.take_while(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':')?;
        if key.eq_ignore_ascii_case("server") {
            if server.is_some() {
                return None;
            }
            server = Some(value.trim());
        }
        if key.eq_ignore_ascii_case("st") {
            if st.is_some() {
                return None;
            }
            st = Some(value.trim());
        }
    }
    let server = server?;
    if st? != "urn:schemas-upnp-org:device:MediaServer:1"
        || server.len() > 512
        || server.chars().any(char::is_control)
    {
        return None;
    }
    Some(Candidate {
        address,
        server: server.into(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_server_candidates_are_not_os_identity() {
        let packet=b"HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\nSERVER: Linux UPnP/1.0 Kodi/21.2\r\n\r\n";
        assert!(parse(packet, "192.0.2.1".parse().unwrap()).is_some());
        assert!(parse(
            &String::from_utf8_lossy(packet)
                .replace("Kodi", "Other")
                .into_bytes(),
            "192.0.2.1".parse().unwrap()
        )
        .is_some());
        assert!(parse(
            b"HTTP/1.1 200 OK\r\nSERVER: Kodi\r\n\r\n",
            "192.0.2.1".parse().unwrap()
        )
        .is_none());
    }
}
