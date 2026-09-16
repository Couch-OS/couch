//! `couch-bt-bridge`: give the kernel an `hci0` backed by the MediaTek radio.
//!
//! ```text
//! couch-bt-bridge [--vhci /dev/vhci] [--stpbt /dev/stpbt]
//!                 [--bdaddr AA:BB:CC:DD:EE:FF [--bdaddr-opcode 0xFC1A]]
//!                 [--once]
//! ```
//!
//! Opens both devices, creates the virtual controller, optionally queues the
//! vendor command that programs the address (sent to the radio before BlueZ
//! gets to talk, so `hci0`'s address is the owner's recorded one), then pumps
//! packets until a side goes away. A whole-chip reset on the radio side ends
//! the pump with errno 99; the bridge then reopens `/dev/stpbt` and starts
//! over, unless `--once` was given. Runs as root, like everything on the
//! remote; no network, no files beyond the two devices.
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use couch_bt::bridge::{open_device, pump, Counters, STP_RESET_END, STP_RESET_START};
use couch_bt::h4;

/// Only one bridge may ever hold the radio. The vendor `/dev/stpbt` has no
/// open guard: a second open re-powers Bluetooth and creates a second virtual
/// controller, and the two desync the STP link until it resets the whole combo
/// chip, taking Wi-Fi with it. A whole-file lock makes a second instance a
/// no-op.
fn singleton() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o644)
        .open("/tmp/couch-bt-bridge.lock")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(lock)
}

struct Args {
    vhci: String,
    stpbt: String,
    bdaddr: Option<[u8; 6]>,
    bdaddr_opcode: u16,
    once: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        vhci: "/dev/vhci".into(),
        stpbt: "/dev/stpbt".into(),
        bdaddr: None,
        bdaddr_opcode: 0xfc1a,
        once: false,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--vhci" => a.vhci = value()?.clone(),
            "--stpbt" => a.stpbt = value()?.clone(),
            "--bdaddr" => a.bdaddr = Some(h4::bdaddr_bytes(value()?)?),
            "--bdaddr-opcode" => {
                let text = value()?;
                a.bdaddr_opcode = u16::from_str_radix(text.trim_start_matches("0x"), 16)
                    .map_err(|_| format!("{text:?} is not an opcode"))?;
            }
            "--once" => a.once = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(a)
}

fn log(line: &str) {
    println!("couch-bt-bridge: {line}");
    let _ = std::io::stdout().flush();
}


/// Wait until the radio answers HCI. On the first open after boot the
/// firmware takes a moment past WMT's "BT on" before it acknowledges STP
/// frames, and anything sent before then is lost at the transport, which then
/// times out and retries. The in-tree 3.18 core never sent a command until
/// bluetoothd powered the adapter seconds later; the backported 4.4 core runs
/// its setup pass the instant the controller exists. So probe with HCI Reset
/// and only create the controller once a Command Complete comes back.
fn radio_ready(stpbt: &mut std::fs::File, log: &mut dyn FnMut(&str)) -> bool {
    // Nothing may be written for a moment after BT_open: the STP layer
    // reports "ready" before the firmware acknowledges frames, and a frame
    // sent in that window times out at STP level, which the driver escalates
    // into a whole-chip reset (taking Wi-Fi down with it). Half a second is
    // well past the longest gap seen on the HA100.
    // The first function-on after boot is slower still (the firmware patch
    // goes down with it) and a probe at 600 ms still provoked one reset; the
    // marker lives in /tmp, which is emptied by every boot.
    let first_open = "/tmp/couch-bt-opened-once";
    let settle = if std::path::Path::new(first_open).exists() { 600 } else { 2000 };
    let _ = std::fs::write(first_open, b"");
    std::thread::sleep(Duration::from_millis(settle));
    // WMT keeps talking to the firmware for a while after the first open
    // (vendor command completes, hardware-error events); anything we send
    // into that collides with it. Discard what arrives until the transport
    // has been quiet for a second, bounded so a chatty radio cannot stall us.
    let quiet_for = Duration::from_millis(1000);
    let drain_until = Instant::now() + Duration::from_millis(8000);
    let mut last = Instant::now();
    let mut drained = 0usize;
    let mut buf = [0u8; h4::MAX_FRAME];
    while Instant::now() < drain_until && last.elapsed() < quiet_for {
        match stpbt.read(&mut buf) {
            Ok(n) if n > 0 => {
                drained += n;
                last = Instant::now();
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    if drained > 0 {
        log(&format!("discarded {drained} bytes of bring-up chatter from the radio"));
    }
    // Read Local Version, not HCI Reset: the firmware takes a while to come
    // back from a Reset and does not acknowledge STP frames meanwhile, so a
    // Reset here followed at once by the core's own Reset (the first thing a
    // 4.x core sends a new controller) is exactly the back-to-back pair that
    // timed out at the transport. A read leaves the core's Reset as the only one.
    let probe = h4::command(0x1001, &[]);
    let mut framer = h4::Framer::default();
    for attempt in 1..=5u32 {
        if let Err(e) = stpbt.write_all(&probe) {
            log(&format!("readiness probe not sent: {e}"));
        }
        let deadline = Instant::now() + Duration::from_millis(1000);
        while Instant::now() < deadline {
            match stpbt.read(&mut buf) {
                Ok(n) if n > 0 => {
                    if let Ok(frames) = framer.push(&buf[..n]) {
                        if frames.iter().any(|f| {
                            f.len() >= 7 && f[0] == h4::EVENT && f[1] == 0x0e && f[4] == 0x01 && f[5] == 0x10
                        }) {
                            if attempt > 1 {
                                log(&format!("radio answered the readiness probe on try {attempt}"));
                            }
                            return true;
                        }
                    }
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => {
                    log(&format!("readiness probe read failed: {e}"));
                    return false;
                }
            }
        }
    }
    log("radio never answered the readiness probe; creating the controller anyway");
    false
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&raw) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("couch-bt-bridge: {e}");
            }
            eprintln!("usage: couch-bt-bridge [--vhci PATH] [--stpbt PATH] [--bdaddr AA:BB:CC:DD:EE:FF [--bdaddr-opcode 0xFC1A]] [--once]");
            return ExitCode::from(2);
        }
    };
    let _lock = match singleton() {
        Ok(lock) => lock,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            log("another couch-bt-bridge already holds the radio; nothing to do");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("couch-bt-bridge: cannot take the singleton lock: {e}");
            return ExitCode::from(1);
        }
    };
    // A run of resets with no traffic between them means the chip is unhappy;
    // stop reopening so the bridge cannot hold the shared radio down.
    let mut resets: u32 = 0;
    loop {
        // Opening /dev/stpbt powers the Bluetooth function on through WMT;
        // a failure here is the radio, not us.
        let mut stpbt = match open_device(&args.stpbt) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "couch-bt-bridge: cannot open {}: {e} (WMT refused to power Bluetooth on?)",
                    args.stpbt
                );
                return ExitCode::from(1);
            }
        };
        radio_ready(&mut stpbt, &mut log);
        // Open /dev/vhci only now: the driver creates a controller by itself
        // one second after the open if no vendor packet has named one, and a
        // create packet after that fails with EBADFD. The probe above can take
        // longer than that second, so the open and the create stay together.
        let mut vhci = match open_device(&args.vhci) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "couch-bt-bridge: cannot open {}: {e} (is CONFIG_BT_HCIVHCI in this kernel?)",
                    args.vhci
                );
                return ExitCode::from(1);
            }
        };
        // Create the controller before any event can arrive for it.
        if let Err(e) = vhci.write_all(&h4::vhci_create_primary()) {
            eprintln!("couch-bt-bridge: cannot create the virtual controller: {e}");
            return ExitCode::from(1);
        }
        log(&format!("bridging {} <-> {}", args.vhci, args.stpbt));
        if let Some(addr) = &args.bdaddr {
            // Straight to the radio: the kernel has not started its own
            // initialisation yet, and the reply event is passed back to it
            // like any other, where it is ignored as an unsolicited complete.
            let frame = h4::set_bdaddr(args.bdaddr_opcode, addr);
            match stpbt.write_all(&frame) {
                Ok(()) => log(&format!(
                    "sent set-address vendor command 0x{:04x}",
                    args.bdaddr_opcode
                )),
                Err(e) => log(&format!("set-address command not sent: {e}")),
            }
        }
        let outcome = pump(&mut vhci, &mut stpbt, &mut |_: &Counters| true, &mut log);
        match outcome {
            Ok(counters) => {
                log(&format!("stopped: {counters:?}"));
                return ExitCode::SUCCESS;
            }
            Err(e)
                if matches!(e.raw_os_error(), Some(STP_RESET_START | STP_RESET_END))
                    && !args.once =>
            {
                resets += 1;
                if resets > 5 {
                    eprintln!(
                        "couch-bt-bridge: chip reset {resets} times without recovering; giving up"
                    );
                    return ExitCode::from(1);
                }
                log("controller reports whole-chip reset; letting WMT settle before reopening");
                drop(stpbt);
                drop(vhci);
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("couch-bt-bridge: stopped: {e}");
                return ExitCode::from(1);
            }
        }
    }
}
