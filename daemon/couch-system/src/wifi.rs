//! Wi-Fi status across the initramfs/Alpine boundary. Both share /tmp/wpa,
//! but the GUI cannot execute Alpine's /sbin/wpa_cli from its own root.
use std::io;
use std::os::unix::{fs::PermissionsExt, net::UnixDatagram};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock, RwLock,
};
use std::time::Duration;

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Status {
    pub state: String,
    pub ssid: String,
}

impl Status {
    pub(crate) fn parse(reply: &str) -> Self {
        let mut status = Self::default();
        for line in reply.lines() {
            if let Some(value) = line.strip_prefix("wpa_state=") {
                status.state = value.to_owned();
            } else if let Some(value) = line.strip_prefix("ssid=") {
                status.ssid = decode_ssid(value);
            }
        }
        // Supplicant can retain the previous SSID during a failed reconnect.
        if status.state != "COMPLETED" {
            status.ssid.clear();
        }
        status
    }
}

fn decode_ssid(value: &str) -> String {
    let mut result = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            result.push(byte);
            continue;
        }
        match bytes.next() {
            Some(b'x') => {
                let digits: Vec<_> = bytes.by_ref().take(2).collect();
                if let Ok(hex) = std::str::from_utf8(&digits) {
                    if let Ok(byte) = u8::from_str_radix(hex, 16) {
                        result.push(byte);
                        continue;
                    }
                }
                result.extend_from_slice(b"\\x");
                result.extend(digits);
            }
            Some(b'n') => result.push(b'\n'),
            Some(b'r') => result.push(b'\r'),
            Some(b't') => result.push(b'\t'),
            Some(b'e') => result.push(27),
            Some(byte) => result.push(byte),
            None => result.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

pub fn signal_dbm(wireless: &str) -> Option<i32> {
    let value = wireless
        .lines()
        .find(|line| line.trim_start().starts_with("wlan0:"))?
        .split_whitespace()
        .nth(3)?
        .trim_end_matches('.')
        .parse::<f32>()
        .ok()?;
    // Reject missing/sentinel/quality values instead of displaying fictitious dBm.
    (value.is_finite() && (-127.0..=0.0).contains(&value)).then(|| value.round() as i32)
}

struct LocalSocket(PathBuf);
impl Drop for LocalSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One bounded request; no ATTACH means unsolicited events cannot interleave.
fn request_at(server: &Path, command: &str, timeout: Duration) -> io::Result<String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    if command.contains(['\0', '\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid control command",
        ));
    }
    let path = std::env::temp_dir().join(format!(
        "couch-wpa-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = UnixDatagram::bind(&path)?;
    let _cleanup = LocalSocket(path.clone());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    socket.connect(server)?;
    socket.send(command.as_bytes())?;
    let mut reply = [0u8; 4096];
    let count = socket.recv(&mut reply)?;
    if count == reply.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversize control reply",
        ));
    }
    Ok(String::from_utf8_lossy(&reply[..count]).into_owned())
}

pub fn command(command: &str) -> Result<String, String> {
    request_at(Path::new("/tmp/wpa/wlan0"), command, Duration::from_secs(1))
        .map(|reply| reply.trim_end().to_owned())
        .map_err(|_| "Wi-Fi service is not responding".to_owned())
}

pub fn status() -> Status {
    static CACHE: OnceLock<RwLock<Status>> = OnceLock::new();
    static WORKER: OnceLock<()> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(Status::default()));
    WORKER.get_or_init(|| {
        std::thread::spawn(move || loop {
            let current = request_at(
                Path::new("/tmp/wpa/wlan0"),
                "STATUS",
                Duration::from_millis(200),
            )
            .map(|reply| Status::parse(&reply))
            .unwrap_or_default();
            if let Ok(mut value) = cache.write() {
                *value = current;
            }
            std::thread::sleep(Duration::from_secs(1));
        });
    });
    cache.read().map(|value| value.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_uses_rssi_not_quality_and_rejects_invalid_values() {
        assert_eq!(signal_dbm(" wlan0: 0000 0 -53 -256 0 0"), Some(-53));
        assert_eq!(signal_dbm(" wlan0: 0000 70. -63. -256 0"), Some(-63));
        assert_eq!(signal_dbm(" wlan0: 0000 70 NaN 0"), None);
        assert_eq!(signal_dbm(" wlan0: 0000 70 203 0"), None);
        assert_eq!(signal_dbm(" wlan1: 0000 70 -50 0"), None);
    }

    #[test]
    fn status_preserves_network_names_and_clears_disconnected_names() {
        assert_eq!(
            Status::parse("wpa_state=COMPLETED\nssid= Living=Room\\x20Wi-Fi \n"),
            Status {
                state: "COMPLETED".into(),
                ssid: " Living=Room Wi-Fi ".into()
            }
        );
        assert_eq!(Status::parse("ssid=old\nwpa_state=SCANNING\n").ssid, "");
        assert_eq!(Status::parse("FAIL\n"), Status::default());
    }

    #[test]
    fn control_socket_works_without_cli_and_cleans_up() {
        let path = std::env::temp_dir().join(format!("couch-wpa-test-{}", std::process::id()));
        let server = UnixDatagram::bind(&path).unwrap();
        let _cleanup = LocalSocket(path.clone());
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut buf = [0; 64];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"STATUS");
            let from = from.as_pathname().unwrap();
            assert_eq!(
                std::fs::metadata(from).unwrap().permissions().mode() & 0o777,
                0o600
            );
            server
                .send_to(b"wpa_state=COMPLETED\nssid=Test\n", from)
                .unwrap();
            from.to_owned()
        });
        let reply = request_at(&path, "STATUS", Duration::from_secs(1)).unwrap();
        let local = worker.join().unwrap();
        assert_eq!(Status::parse(&reply).ssid, "Test");
        assert!(!local.exists());
    }

    #[test]
    fn missing_control_socket_is_an_error() {
        assert!(request_at(
            Path::new("/no-such-couch-wpa/socket"),
            "STATUS",
            Duration::from_millis(10)
        )
        .is_err());
    }
}
