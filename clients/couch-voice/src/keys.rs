//! Waiting for a key on the remote's own keypad.
//!
//! This exists so a recording can be bracketed by the thing a person is
//! actually holding. Asking someone to speak into a microphone during a window
//! that started when a shell command ran, seconds before they could read that
//! it had, is how you collect recordings of somebody about to talk.
//!
//! Push-to-talk is the shape the finished product wants too - hold a key, say
//! the thing, let go - so this is the primitive rather than a test fixture.
//!
//! Both keypad nodes are read: on this device a given key arrives on one of
//! `mt_gpio_kpd` or `mtk-kpd` and which one is not obvious. `couch-gui` reads
//! them at the same time, which is fine - every reader of an evdev node gets
//! its own copy of the stream.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

const NODES: [&str; 2] = ["/dev/input/event1", "/dev/input/event2"];
const EVENT_SIZE: usize = 16;
const EV_KEY: u16 = 0x01;

pub struct Keypad {
    files: Vec<File>,
}

impl Keypad {
    /// Opens whatever keypad nodes exist. Missing ones are skipped rather than
    /// fatal: a device with one is still usable.
    pub fn open() -> Keypad {
        let files = NODES
            .iter()
            .filter_map(|p| {
                let f = File::options().read(true).open(p).ok()?;
                unsafe {
                    let fd = f.as_raw_fd();
                    let fl = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                }
                Some(f)
            })
            .collect::<Vec<_>>();
        Keypad { files }
    }

    pub fn present(&self) -> bool {
        !self.files.is_empty()
    }

    /// Any key going down, or None if nothing has yet.
    ///
    /// Key-down rather than key-up, because the press is when a person thinks
    /// the thing happened; and auto-repeat (value 2) is ignored, so leaning on
    /// a key does not read as a second press.
    pub fn pressed(&mut self) -> Option<u16> {
        let mut buf = [0u8; EVENT_SIZE * 16];
        for f in &mut self.files {
            while let Ok(n) = f.read(&mut buf) {
                if n < EVENT_SIZE {
                    break;
                }
                for chunk in buf[..n - n % EVENT_SIZE].chunks_exact(EVENT_SIZE) {
                    let kind = u16::from_le_bytes([chunk[8], chunk[9]]);
                    let code = u16::from_le_bytes([chunk[10], chunk[11]]);
                    let value = i32::from_le_bytes([chunk[12], chunk[13], chunk[14], chunk[15]]);
                    if kind == EV_KEY && value == 1 {
                        return Some(code);
                    }
                }
            }
        }
        None
    }

    /// Throws away anything already queued, so a key pressed before we started
    /// looking does not count as the one that starts the recording.
    pub fn drain(&mut self) {
        while self.pressed().is_some() {}
    }

    /// Blocks until a key goes down, or the deadline passes.
    ///
    /// Polled rather than blocking on the fd because two nodes have to be
    /// watched and a timeout has to be honoured; at 20ms nobody can tell, and
    /// it costs nothing next to what the recording is about to do.
    pub fn wait(&mut self, timeout: Duration) -> Option<u16> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(code) = self.pressed() {
                return Some(code);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
