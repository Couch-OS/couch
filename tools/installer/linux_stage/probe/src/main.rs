//! Linux 3.18 FunctionFS probe; private-install and wifi-debug are separate RAM stages.
//!
//! The installer stage answers the same framed protocol on its CDC ACM function
//! as well (`serial`), for hosts whose operating system binds a serial driver
//! to that function but nothing to the vendor interface: Windows. Linux and
//! macOS hosts keep using FunctionFS through libusb.
#[cfg(all(feature = "private-install", feature = "wifi-debug"))]
compile_error!("private-install and wifi-debug must be built as separate stage binaries");
#[cfg(feature = "private-install")]
mod install;
#[cfg(feature = "private-install")]
mod serial;
#[cfg(not(feature = "wifi-debug"))]
mod scan;
mod wifi;
#[cfg(not(feature = "wifi-debug"))]
use std::time::Instant;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
#[cfg(any(test, not(feature = "wifi-debug")))]
const MAX: u64 = 64 * 1024 * 1024;
#[cfg(not(feature = "wifi-debug"))]
const CHUNK: usize = 65536;
#[cfg(feature = "private-install")]
const CAPABILITIES: &str = "COUCH_PRIVATE_WIFI_INSTALLER_V1";
#[cfg(feature = "wifi-debug")]
const CAPABILITIES: &str = "COUCH_PRIVATE_WIFI_DEBUG_STAGE_V1";
#[cfg(not(any(feature = "private-install", feature = "wifi-debug")))]
const CAPABILITIES: &str = "COUCH_READONLY_RAM_PROBE_V1";
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn descriptors() -> Vec<u8> {
    // Exact packed usb_functionfs_descs_head v1 from our Linux 3.18 header.
    let mut out = Vec::new();
    for n in [1u32, 62, 3, 3] {
        out.extend(n.to_le_bytes());
    }
    for packet in [64u16, 512] {
        out.extend([9, 4, 0, 0, 2, 0xff, 0, 0, 1]);
        for endpoint in [0x01, 0x82] {
            out.extend([7, 5, endpoint, 2]);
            out.extend(packet.to_le_bytes());
            out.push(0);
        }
    }
    out
}
fn strings() -> Vec<u8> {
    #[cfg(feature = "wifi-debug")]
    let text = b"Couch Wi-Fi debug stage\0";
    #[cfg(all(not(feature = "wifi-debug"), feature = "private-install"))]
    let text = b"Couch private Wi-Fi installer\0";
    #[cfg(not(any(feature = "wifi-debug", feature = "private-install")))]
    let text = b"Couch read-only benchmark\0";
    let mut out = Vec::new();
    for n in [2u32, (18 + text.len()) as u32, 1, 1] {
        out.extend(n.to_le_bytes());
    }
    out.extend(0x0409u16.to_le_bytes());
    out.extend(text);
    out
}
fn request(header: &[u8; 16]) -> io::Result<(u32, u64)> {
    if &header[..4] != b"CBP1" {
        return Err(invalid("bad protocol magic"));
    }
    let op = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let length = u64::from_le_bytes(header[8..].try_into().unwrap());
    #[cfg(not(feature = "wifi-debug"))]
    let known = matches!(
        (op, length),
        (0, 0) | (1..=2, 1..=MAX) | (3, 1) | (5, 0) | (7, 0)
    );
    #[cfg(not(feature = "wifi-debug"))]
    let known = known || matches!((op, length), (4, 1..=16384));
    #[cfg(feature = "private-install")]
    let known = known || matches!((op, length), (6, 1..=512) | (10, 0));
    #[cfg(feature = "wifi-debug")]
    let known = matches!((op, length), (0, 0) | (5, 0) | (8, 0) | (9, 0));
    if !known {
        return Err(invalid("unsupported command or length"));
    }
    Ok((op, length))
}
fn response(out: &mut impl Write, length: u64) -> io::Result<()> {
    out.write_all(b"CBR1")?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&length.to_le_bytes())
}
#[cfg(not(feature = "wifi-debug"))]
fn transfer(
    input: &mut impl Read,
    output: &mut impl Write,
    op: u32,
    length: u64,
) -> io::Result<()> {
    let mut buffer = [0xa5; CHUNK];
    let mut left = length;
    if op == 1 {
        response(output, length)?;
    }
    while left > 0 {
        let n = left.min(CHUNK as u64) as usize;
        if op == 1 {
            output.write_all(&buffer[..n])?;
        } else {
            input.read_exact(&mut buffer[..n])?;
            if buffer[..n].iter().any(|v| *v != 0xa5) {
                return Err(invalid("RAM test pattern mismatch"));
            }
        }
        left -= n as u64;
    }
    if op == 2 {
        response(output, 0)?;
    }
    Ok(())
}
#[cfg(not(feature = "wifi-debug"))]
fn recovery_hash() -> io::Result<Vec<u8>> {
    // No host paths, partition selection, offsets, or shell syntax are accepted.
    if std::fs::read_to_string("/sys/class/block/mmcblk0p9/size")?.trim() != "32768" {
        return Err(invalid("recovery size differs from reviewed HA100 layout"));
    }
    let start = Instant::now();
    let result = std::process::Command::new("/bin/busybox")
        .args(["sha256sum", "/dev/mmcblk0p9"])
        .output()?;
    if !result.status.success()
        || result.stdout.len() < 64
        || !result.stdout[..64].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(invalid("recovery SHA failed"));
    }
    let mut answer = result.stdout[..64].to_vec();
    answer.extend((start.elapsed().as_nanos() as u64).to_le_bytes());
    Ok(answer)
}
fn run(root: &Path) -> io::Result<()> {
    let ready = Path::new("/tmp/couch-probe.ready");
    let _ = std::fs::remove_file(ready);
    let mut control = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("ep0"))?;
    // FunctionFS requires each registration block in one write, not fragmented writes.
    let desc = descriptors();
    if control.write(&desc)? != desc.len() {
        return Err(invalid("short descriptors"));
    }
    let text = strings();
    if control.write(&text)? != text.len() {
        return Err(invalid("short strings"));
    }
    let mut input = File::open(root.join("ep1"))?;
    let mut output = OpenOptions::new().write(true).open(root.join("ep2"))?;
    let enabled = Arc::new(AtomicBool::new(false));
    let flag = enabled.clone();
    thread::spawn(move || {
        let mut event = [0u8; 12];
        loop {
            if control.read_exact(&mut event).is_err() {
                if wifi::active() {
                    return;
                }
                std::process::exit(2);
            }
            match event[8] {
                2 => {
                    flag.store(true, Ordering::Release);
                }
                1 | 3 => {
                    if wifi::active() {
                        return;
                    }
                    std::process::exit(2);
                }
                4 => {
                    // Stall unsupported control setup with the opposite data direction.
                    if event[0] & 0x80 != 0 {
                        let _ = control.read(&mut []);
                    } else {
                        let _ = control.write(&[]);
                    }
                }
                _ => {}
            }
        }
    });
    std::fs::write(ready, b"ready\n")?;
    while !enabled.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(20));
    }
    loop {
        let mut header = [0u8; 16];
        input.read_exact(&mut header)?;
        let (op, length) = request(&header)?;
        handle(op, length, &mut input, &mut output)?;
    }
}
/// One validated request on either channel. Errors end the caller's channel.
fn handle(
    op: u32,
    length: u64,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    #[cfg(feature = "wifi-debug")]
    let _ = length;
    match op {
        0 => {
            response(output, 4)?;
            output.write_all(b"CBP1")?;
        }
        #[cfg(not(feature = "wifi-debug"))]
        1 | 2 => transfer(input, output, op, length)?,
        #[cfg(not(feature = "wifi-debug"))]
        3 => {
            let data = recovery_hash()?;
            response(output, data.len() as u64)?;
            output.write_all(&data)?;
        }
        #[cfg(not(feature = "wifi-debug"))]
        4 => {
            let mut payload = vec![0; length as usize];
            input.read_exact(&mut payload)?;
            wifi::provision(&payload)?;
            response(output, 0)?;
        }
        5 => {
            let data = wifi::status();
            response(output, data.len() as u64)?;
            output.write_all(&data)?;
        }
        #[cfg(not(feature = "wifi-debug"))]
        7 => {
            let data = scan::response();
            response(output, data.len() as u64)?;
            output.write_all(&data)?;
        }
        #[cfg(feature = "wifi-debug")]
        8 => {
            let data = wifi::debug_status();
            response(output, data.len() as u64)?;
            output.write_all(&data)?;
        }
        #[cfg(feature = "wifi-debug")]
        9 => {
            wifi::request_debug_retry()?;
            response(output, 0)?;
        }
        #[cfg(feature = "private-install")]
        6 => {
            let mut payload = vec![0; length as usize];
            input.read_exact(&mut payload)?;
            install::bind(&payload)?;
            response(output, 0)?;
        }
        #[cfg(feature = "private-install")]
        10 => return Err(invalid("installation is TLS-only")),
        _ => unreachable!(),
    }
    Ok(())
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--capabilities") {
        println!("{CAPABILITIES}");
        return;
    }
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/ffs-couch".into());
    #[cfg(feature = "private-install")]
    if let Some(port) = std::env::args().nth(2) {
        thread::spawn(move || serial::run(&port));
    }
    if let Err(error) = run(Path::new(&root)) {
        if wifi::active() {
            eprintln!("USB control ended; provisioned TLS benchmark remains available.");
            loop {
                thread::park();
            }
        }
        eprintln!("Probe stopped: {error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_abi_lengths_and_endpoints() {
        let d = descriptors();
        assert_eq!(d.len(), 62);
        assert_eq!(&d[..4], &1u32.to_le_bytes());
        assert_eq!(&d[25..32], &[7, 5, 1, 2, 64, 0, 0]);
        assert_eq!(&d[55..62], &[7, 5, 0x82, 2, 0, 2, 0]);
        let s = strings();
        assert_eq!(
            u32::from_le_bytes(s[4..8].try_into().unwrap()) as usize,
            s.len()
        );
    }
    #[test]
    fn commands_fail_closed() {
        for (op, n, valid) in [
            (0, 0, true),
            (1, MAX, !cfg!(feature = "wifi-debug")),
            (1, MAX + 1, false),
            (2, 0, false),
            (3, 1, !cfg!(feature = "wifi-debug")),
            (3, 2, false),
            (4, 0, false),
            (4, 1, !cfg!(feature = "wifi-debug")),
            (7, 0, !cfg!(feature = "wifi-debug")),
            (7, 1, false),
            (6, 128, cfg!(feature = "private-install")),
            (6, 513, false),
            (10, 0, cfg!(feature = "private-install")),
            (8, 0, cfg!(feature = "wifi-debug")),
            (9, 0, cfg!(feature = "wifi-debug")),
        ] {
            let mut h = [0u8; 16];
            h[..4].copy_from_slice(b"CBP1");
            h[4..8].copy_from_slice(&(op as u32).to_le_bytes());
            h[8..].copy_from_slice(&n.to_le_bytes());
            assert_eq!(request(&h).is_ok(), valid);
        }
    }
    #[cfg(not(feature = "wifi-debug"))]
    #[test]
    fn transfer_chunk_boundary_and_corruption() {
        let n = CHUNK as u64 + 17;
        let mut out = Vec::new();
        transfer(&mut io::empty(), &mut out, 1, n).unwrap();
        assert_eq!(out.len(), n as usize + 16);
        assert!(out[16..].iter().all(|v| *v == 0xa5));
        let mut ack = Vec::new();
        transfer(&mut &out[16..], &mut ack, 2, n).unwrap();
        assert_eq!(ack.len(), 16);
        assert!(transfer(&mut &[0u8][..], &mut Vec::new(), 2, 1).is_err());
    }
}
