//! Bluetooth bring-up for the HA100: the bridge that gives BlueZ an `hci0`.
//!
//! The MediaTek CONSYS chip's Bluetooth transport reaches userland as
//! `/dev/stpbt`, a character device that speaks raw HCI in H4 framing (one
//! packet-type byte, then the packet). BlueZ wants an `hci_dev` registered
//! with the kernel's Bluetooth core, which the vendor tree never provides.
//! Linux's virtual HCI driver (`/dev/vhci`, `CONFIG_BT_HCIVHCI`) is the
//! cheapest way across: whatever userland writes to it is what the kernel
//! sees as the controller, and whatever the kernel sends the controller comes
//! out of it. So this crate shuttles packets both ways:
//!
//! ```text
//!   BlueZ ─ hci0 ─ kernel BT core ─ /dev/vhci ═══ couch-bt-bridge ═══ /dev/stpbt ─ STP ─ radio
//! ```
//!
//! Two facts about the two ends shape the code, both read from the kernel
//! source (`drivers/bluetooth/hci_vhci.c`, `stp_chrdev_bt.c`):
//!
//! * **`/dev/vhci` is packet oriented.** Each `write` must be exactly one H4
//!   packet, and before the first event or data packet arrives the device has
//!   to exist: either a vendor packet `[0xff, type]` creates it at once, or a
//!   one-second timer creates a default one. Writing an event before that
//!   returns `ENODEV`. The bridge therefore creates the device explicitly as
//!   its first act. Reads return one whole packet each.
//! * **`/dev/stpbt` is a byte stream on read.** `read` returns whatever the STP
//!   receive queue holds, up to the size asked, with no respect for packet
//!   boundaries. So bytes from the radio are reassembled with [`h4::Framer`]
//!   before each is written to `/dev/vhci` as one packet. Writes are taken
//!   whole. During a whole-chip reset, reads and writes fail with errno 88
//!   (reset started) and 99 (reset ended); the bridge waits through the first
//!   and reopens the device on the second.
//!
//! Nothing here is Bluetooth policy. Pairing, bonding, advertising and the HID
//! service are BlueZ's and a later daemon's; this only makes the radio
//! visible. See `docs/bluetooth.md`.

pub mod h4 {
    //! H4 framing: the packet-type byte and the length fields that follow it.

    /// Packet types, as the first byte of an H4 frame.
    pub const COMMAND: u8 = 0x01;
    pub const ACL: u8 = 0x02;
    pub const SCO: u8 = 0x03;
    pub const EVENT: u8 = 0x04;
    /// The virtual HCI driver's own control packet; never on the wire.
    pub const VENDOR: u8 = 0xff;

    /// Largest frame either end accepts (`HCI_MAX_FRAME_SIZE` is 1028 with
    /// the type byte; STP's buffer is 2 KiB). A header claiming more than this
    /// is corruption, not a big packet.
    pub const MAX_FRAME: usize = 1028;

    /// The total frame length once enough header bytes are present, or
    /// `None` while more are needed. `Err` names an unknown packet type,
    /// which means the stream has lost sync.
    pub fn frame_len(head: &[u8]) -> Result<Option<usize>, u8> {
        let Some(&kind) = head.first() else {
            return Ok(None);
        };
        let (header, len_at, len_bytes) = match kind {
            COMMAND => (4, 3, 1), // opcode(2) len(1)
            ACL => (5, 3, 2),     // handle(2) len(2)
            SCO => (4, 3, 1),     // handle(2) len(1)
            EVENT => (3, 2, 1),   // code(1) len(1)
            other => return Err(other),
        };
        if head.len() < header {
            return Ok(None);
        }
        let payload = if len_bytes == 1 {
            head[len_at] as usize
        } else {
            u16::from_le_bytes([head[len_at], head[len_at + 1]]) as usize
        };
        Ok(Some(header + payload))
    }

    /// Reassembles H4 frames from a byte stream.
    #[derive(Default)]
    pub struct Framer {
        pending: Vec<u8>,
    }

    impl Framer {
        /// Feed bytes; get back every complete frame they finish, in order.
        /// An unknown type byte drops the buffered bytes and reports it, so
        /// one corrupt byte costs one resync rather than a stuck bridge.
        pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, u8> {
            self.pending.extend_from_slice(bytes);
            let mut frames = Vec::new();
            loop {
                match frame_len(&self.pending) {
                    Ok(Some(len)) if len > MAX_FRAME => {
                        let kind = self.pending[0];
                        self.pending.clear();
                        return Err(kind);
                    }
                    Ok(Some(len)) if self.pending.len() >= len => {
                        let rest = self.pending.split_off(len);
                        frames.push(std::mem::replace(&mut self.pending, rest));
                    }
                    Ok(_) => return Ok(frames),
                    Err(kind) => {
                        self.pending.clear();
                        return Err(kind);
                    }
                }
            }
        }

        /// Bytes waiting for the rest of their frame.
        pub fn buffered(&self) -> usize {
            self.pending.len()
        }
    }

    /// The vendor packet that makes `/dev/vhci` register a primary (BR/EDR
    /// plus LE) controller now rather than after its one-second timer.
    pub fn vhci_create_primary() -> [u8; 2] {
        [VENDOR, 0x00]
    }

    /// An HCI command frame for `opcode` with `params`.
    pub fn command(opcode: u16, params: &[u8]) -> Vec<u8> {
        assert!(params.len() <= 255);
        let mut frame = Vec::with_capacity(4 + params.len());
        frame.push(COMMAND);
        frame.extend_from_slice(&opcode.to_le_bytes());
        frame.push(params.len() as u8);
        frame.extend_from_slice(params);
        frame
    }

    /// A Bluetooth device address as the wire wants it: little-endian, so
    /// "AA:BB:CC:DD:EE:FF" is sent as FF EE DD CC BB AA.
    pub fn bdaddr_bytes(text: &str) -> Result<[u8; 6], String> {
        let parts: Vec<&str> = text.trim().split(':').collect();
        if parts.len() != 6 {
            return Err(format!("{text:?} is not a Bluetooth address"));
        }
        let mut out = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            out[5 - i] = u8::from_str_radix(part, 16)
                .map_err(|_| format!("{text:?} is not a Bluetooth address"))?;
        }
        Ok(out)
    }

    /// The vendor command that programs the controller's address. MediaTek's
    /// Bluedroid vendor library uses opcode 0xFC1A ("Set BD_ADDR") with the
    /// six address bytes on this generation of CONSYS chips; the HA100 has not
    /// confirmed it, which is why the opcode is a parameter and the result of
    /// the first try belongs in `docs/bluetooth.md`.
    pub fn set_bdaddr(opcode: u16, addr: &[u8; 6]) -> Vec<u8> {
        command(opcode, addr)
    }
}

pub mod bridge {
    //! The pump between the two devices.

    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::time::Duration;

    use crate::h4::{Framer, EVENT, MAX_FRAME};

    /// errno values `/dev/stpbt` returns around a whole-chip reset.
    pub const STP_RESET_START: i32 = 88;
    pub const STP_RESET_END: i32 = 99;

    /// What happened on one pass of the pump, for the log and the tests.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct Counters {
        pub to_controller: u64,
        pub to_host: u64,
        pub resyncs: u64,
        /// Hardware Error events swallowed rather than forwarded.
        pub hardware_errors: u32,
    }

    /// One direction's file, opened non-blocking. The pump polls before every
    /// read; a *blocking* read that arrives mid whole-chip-reset parks the
    /// process in an uninterruptible wait (D state) still holding the radio,
    /// which is how a stuck bridge took Wi-Fi down with it on the shared combo
    /// chip. Non-blocking, such a read returns EWOULDBLOCK and the pump backs
    /// off or exits cleanly, releasing the transport.
    pub fn open_device(path: &str) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    }

    /// Write one whole frame, backing off rather than tearing the bridge down
    /// when the transport is momentarily full. `/dev/stpbt` returns ENOSPC when
    /// its STP transmit queue has no room ("native program should not call
    /// BT_write with no delay") and a non-blocking fd can report EWOULDBLOCK;
    /// both are transient. The whole-chip-reset errnos are passed back so the
    /// caller can reopen. Dying on ENOSPC, as the bridge used to, is what let
    /// the STP link wedge and escalate to a chip reset that also reset Wi-Fi.
    fn write_frame(f: &mut File, buf: &[u8], log: &mut dyn FnMut(&str)) -> io::Result<()> {
        let step = Duration::from_millis(5);
        let limit = Duration::from_millis(500);
        let mut waited = Duration::ZERO;
        loop {
            match f.write(buf) {
                Ok(n) if n == buf.len() => return Ok(()),
                Ok(n) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("wrote {n} of {} bytes", buf.len()),
                    ))
                }
                Err(e)
                    if e.raw_os_error() == Some(STP_RESET_START)
                        || e.raw_os_error() == Some(STP_RESET_END) =>
                {
                    return Err(e)
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(libc::ENOSPC) =>
                {
                    if waited >= limit {
                        log("transport stayed full; dropping this frame");
                        return Err(e);
                    }
                    std::thread::sleep(step);
                    waited += step;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Wait until either file is readable. `timeout` bounds the wait so a
    /// caller can stop the bridge; `Ok(false)` means nothing was ready.
    fn wait_readable(a: &File, b: &File, timeout: Duration) -> io::Result<(bool, bool)> {
        let mut fds = [
            libc::pollfd {
                fd: a.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: b.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok((false, false));
            }
            return Err(err);
        }
        let ready =
            |f: &libc::pollfd| f.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
        Ok((ready(&fds[0]), ready(&fds[1])))
    }

    /// Run the pump until `keep_going` says stop or a side fails for good.
    ///
    /// `host` is `/dev/vhci` (packet per read and write); `controller` is
    /// `/dev/stpbt` (byte stream on read). Both may be any pair of files in
    /// tests. The controller side's reset errnos are handled here: 88 waits,
    /// 99 returns `Err` with that errno so the caller reopens the device.
    pub fn pump(
        host: &mut File,
        controller: &mut File,
        keep_going: &mut dyn FnMut(&Counters) -> bool,
        log: &mut dyn FnMut(&str),
    ) -> io::Result<Counters> {
        let mut counters = Counters::default();
        let mut framer = Framer::default();
        let mut host_buf = vec![0u8; MAX_FRAME];
        let mut ctl_buf = vec![0u8; 2048];
        while keep_going(&counters) {
            let (host_ready, ctl_ready) =
                wait_readable(host, controller, Duration::from_millis(250))?;
            if host_ready {
                // One whole frame per read from the kernel side.
                match host.read(&mut host_buf) {
                    Ok(0) => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "vhci closed"))
                    }
                    Ok(n) => {
                        write_frame(controller, &host_buf[..n], &mut *log)?;
                        counters.to_controller += 1;
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::Interrupted
                            || e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            if ctl_ready {
                match controller.read(&mut ctl_buf) {
                    Ok(0) => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stpbt closed"))
                    }
                    Ok(n) => match framer.push(&ctl_buf[..n]) {
                        Ok(frames) => {
                            for frame in frames {
                                // The MediaTek firmware raises HCI Hardware
                                // Error (code 2) during its own bring-up and
                                // after some setup commands. The in-tree
                                // 3.18 core only logged the event; a 4.x core
                                // resets the device on it, and a burst of
                                // those resets starves the STP transport into
                                // a whole-chip reset. Keep the 3.18 behaviour.
                                if frame.len() >= 4 && frame[0] == EVENT && frame[1] == 0x10 {
                                    counters.hardware_errors += 1;
                                    if counters.hardware_errors <= 3 {
                                        log(&format!(
                                            "dropped HCI hardware error 0x{:02x} from the radio",
                                            frame[3]
                                        ));
                                    }
                                    continue;
                                }
                                write_frame(host, &frame, &mut *log)?;
                                counters.to_host += 1;
                            }
                        }
                        Err(kind) => {
                            counters.resyncs += 1;
                            log(&format!("controller stream lost sync on type 0x{kind:02x}; dropped buffered bytes"));
                        }
                    },
                    Err(e) if e.raw_os_error() == Some(STP_RESET_START) => {
                        log("controller reports whole-chip reset started; waiting");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::Interrupted
                            || e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(counters)
    }
}

#[cfg(test)]
mod tests {
    use super::h4::*;

    #[test]
    fn frame_lengths_follow_the_header_of_each_type() {
        assert_eq!(frame_len(&[COMMAND, 0x03, 0x0c, 0x00]), Ok(Some(4)));
        assert_eq!(frame_len(&[COMMAND, 0x03, 0x0c]), Ok(None));
        assert_eq!(frame_len(&[EVENT, 0x0e, 0x04]), Ok(Some(7)));
        assert_eq!(frame_len(&[ACL, 0x40, 0x00, 0x05, 0x00]), Ok(Some(10)));
        assert_eq!(frame_len(&[SCO, 0x40, 0x00, 0x02]), Ok(Some(6)));
        assert_eq!(frame_len(&[0x09]), Err(0x09));
        assert_eq!(frame_len(&[]), Ok(None));
    }

    #[test]
    fn the_framer_reassembles_split_and_merged_packets_and_resyncs_on_junk() {
        let reset_done = [EVENT, 0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00];
        let acl = [ACL, 0x40, 0x00, 0x02, 0x00, 0xaa, 0xbb];
        let mut stream = Vec::new();
        stream.extend_from_slice(&reset_done);
        stream.extend_from_slice(&acl);
        // Byte at a time: every frame still comes out whole and once.
        let mut framer = Framer::default();
        let mut out = Vec::new();
        for byte in &stream {
            out.extend(framer.push(&[*byte]).unwrap());
        }
        assert_eq!(out, vec![reset_done.to_vec(), acl.to_vec()]);
        assert_eq!(framer.buffered(), 0);
        // Both at once, plus the start of a third.
        let mut framer = Framer::default();
        let mut more = stream.clone();
        more.extend_from_slice(&[EVENT, 0x13]);
        let out = framer.push(&more).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(framer.buffered(), 2);
        // Junk type byte at a frame boundary: reported, buffer dropped, the
        // next frame comes through clean.
        let mut framer = Framer::default();
        assert_eq!(framer.push(&[0x77, 1, 2, 3]), Err(0x77));
        assert_eq!(framer.buffered(), 0);
        assert_eq!(framer.push(&reset_done).unwrap(), vec![reset_done.to_vec()]);
        // A header claiming an impossible length is corruption, not a packet.
        let mut framer = Framer::default();
        assert_eq!(framer.push(&[ACL, 0, 0, 0xff, 0x7f]), Err(ACL));
    }

    #[test]
    fn commands_and_addresses_are_encoded_little_endian() {
        assert_eq!(command(0x0c03, &[]), vec![COMMAND, 0x03, 0x0c, 0x00]);
        assert_eq!(
            bdaddr_bytes("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa]
        );
        assert!(bdaddr_bytes("AA:BB").is_err());
        let frame = set_bdaddr(0xfc1a, &bdaddr_bytes("00:11:22:33:44:55").unwrap());
        assert_eq!(&frame[..4], &[COMMAND, 0x1a, 0xfc, 6]);
        assert_eq!(&frame[4..], &[0x55, 0x44, 0x33, 0x22, 0x11, 0x00]);
        assert_eq!(vhci_create_primary(), [VENDOR, 0]);
    }

    #[test]
    fn the_pump_moves_whole_frames_each_way_over_ordinary_files() {
        use super::bridge::{pump, Counters};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        // Sockets stand in for the two character devices: the "kernel" end
        // of vhci and the "radio" end of stpbt.
        let (mut kernel, vhci) = UnixStream::pair().unwrap();
        let (mut radio, stpbt) = UnixStream::pair().unwrap();
        let mut vhci_file = std::fs::File::from(std::os::fd::OwnedFd::from(vhci));
        let mut stpbt_file = std::fs::File::from(std::os::fd::OwnedFd::from(stpbt));
        let reset = command(0x0c03, &[]);
        kernel.write_all(&reset).unwrap();
        // The radio answers in two ragged pieces, which must arrive at the
        // kernel end as one frame.
        let done = [EVENT, 0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00];
        radio.write_all(&done[..2]).unwrap();
        radio.write_all(&done[2..]).unwrap();
        let mut log = Vec::new();
        let counters = pump(
            &mut vhci_file,
            &mut stpbt_file,
            &mut |c: &Counters| c.to_controller < 1 || c.to_host < 1,
            &mut |line| log.push(line.to_owned()),
        )
        .unwrap();
        assert_eq!(
            counters,
            Counters {
                to_controller: 1,
                to_host: 1,
                resyncs: 0,
                hardware_errors: 0
            }
        );
        let mut got = vec![0u8; reset.len()];
        radio.read_exact(&mut got).unwrap();
        assert_eq!(got, reset);
        let mut got = vec![0u8; done.len()];
        kernel.read_exact(&mut got).unwrap();
        assert_eq!(got, done);
        assert!(log.is_empty());
    }
}
