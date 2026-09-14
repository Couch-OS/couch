//! Wake-on-LAN, the only way to reach a TV that took its network interface
//! down with the rest of itself.
//!
//! Copied verbatim into `couch-webos` and `couch-tizen` before this; they keep
//! their own error types and call through here.

use std::net::{Ipv4Addr, UdpSocket};

/// Six 0xff bytes then the MAC sixteen times. `None` if the text is not six
/// hex octets separated by `:` or `-`, or nothing.
pub fn magic_packet(mac: &str) -> Option<[u8; 102]> {
    let compact = mac.replace([':', '-'], "");
    if compact.len() != 12 || !compact.is_ascii() {
        return None;
    }
    let mut address = [0; 6];
    for (i, b) in address.iter_mut().enumerate() {
        *b = u8::from_str_radix(&compact[i * 2..i * 2 + 2], 16).ok()?;
    }
    let mut packet = [0xff; 102];
    for chunk in packet[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&address)
    }
    Some(packet)
}

/// One broadcast datagram to port 9. Sending it proves nothing about the TV:
/// there is no reply, and nothing here is retried.
pub fn wake(packet: &[u8; 102], broadcast: Ipv4Addr) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_broadcast(true)?;
    socket.send_to(packet, (broadcast, 9))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packet_is_a_sync_stream_and_sixteen_copies_of_the_address() {
        let packet = magic_packet("01:23:45:67:89:ab").unwrap();
        assert_eq!(&packet[..6], &[0xff; 6]);
        assert!(packet[6..]
            .chunks_exact(6)
            .all(|c| c == [0x01, 0x23, 0x45, 0x67, 0x89, 0xab]));
        // Dashes are the other spelling users paste in; anything else is not a
        // MAC and must not become a packet full of zeroes.
        assert_eq!(magic_packet("01-23-45-67-89-ab"), Some(packet));
        for bad in ["", "bad address", "01:23:45:67:89", "zz:23:45:67:89:ab"] {
            assert!(magic_packet(bad).is_none(), "{bad}");
        }
    }
}
