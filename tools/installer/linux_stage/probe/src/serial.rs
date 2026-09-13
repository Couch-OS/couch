//! The framed protocol on the stage's CDC ACM function.
//!
//! Windows has no driver for the FunctionFS vendor interface and libusb cannot
//! open one without it, but Windows binds its own `usbser` driver to the ACM
//! function the stage carries beside it. This channel serves the same
//! requests over that port, so the host needs no driver work. Linux and macOS
//! hosts do not use it; on those, a modem manager may still probe the port with
//! AT commands, so anything before a valid header is skipped rather than
//! treated as a protocol violation, and a malformed frame ends only this
//! channel, never the FunctionFS one. The port is reopened after a hang-up
//! (the host re-enumerating the device) so the channel survives that too.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::unix::io::AsRawFd,
    thread,
    time::Duration,
};

/// Serve forever; each channel failure closes and reopens the port.
pub fn run(path: &str) {
    loop {
        if let Err(error) = serve(path) {
            eprintln!("Serial protocol channel ended: {error}");
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn serve(path: &str) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    raw(&file)?;
    // `&File` reads and writes, which lets one open port serve as both sides.
    let (mut input, mut output) = (&file, &file);
    loop {
        let header = frame(&mut input)?;
        let (op, length) = super::request(&header)?;
        super::handle(op, length, &mut input, &mut output)?;
    }
}

/// Read a 16-byte header, skipping bytes until it starts with the magic.
fn frame(input: &mut impl Read) -> io::Result<[u8; 16]> {
    let mut header = [0u8; 16];
    input.read_exact(&mut header)?;
    let mut skipped = 0usize;
    while &header[..4] != b"CBP1" {
        header.copy_within(1.., 0);
        input.read_exact(&mut header[15..])?;
        skipped += 1;
        if skipped > 65536 {
            return Err(super::invalid("no protocol frame on the serial port"));
        }
    }
    Ok(header)
}

/// Eight-bit clean, no echo, no line editing: the port carries binary frames.
fn raw(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::cfmakeraw(&mut attributes) };
    attributes.c_cc[libc::VMIN] = 1;
    attributes.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_resynchronise_past_junk() {
        let mut stream: &[u8] =
            b"AT\r\nATI\r\nCBP1\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00tail";
        let header = frame(&mut stream).unwrap();
        assert_eq!(&header[..4], b"CBP1");
        assert_eq!(stream, b"tail");
        let mut junk: &[u8] = &[b'x'; 70000];
        assert!(frame(&mut junk).is_err());
        let mut short: &[u8] = b"CBP";
        assert!(frame(&mut short).is_err());
    }
}
