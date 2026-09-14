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
use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use couch_bt::bridge::{open_device, pump, Counters, STP_RESET_END};
use couch_bt::h4;

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
    loop {
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
            Err(e) if e.raw_os_error() == Some(STP_RESET_END) && !args.once => {
                log("controller reports whole-chip reset ended; reopening");
                drop(stpbt);
                drop(vhci);
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                eprintln!("couch-bt-bridge: stopped: {e}");
                return ExitCode::from(1);
            }
        }
    }
}
