//! A one-shot CLI over the encoders and the blaster, for exercising IR from a
//! laptop (encode and print) or from the device (encode and transmit).
//!
//! Arguments are parsed by hand, the same call couch-voice makes: a handful of
//! flags does not repay a dependency on a target we cross-compile. Every send
//! prints what it is about to transmit first, because IR is invisible - there
//! is no way to see that the right code went out except to have said so and to
//! watch the television.

use std::process::ExitCode;

use couch_ir::codeset::{self, Codeset};
use couch_ir::proto::{self, Message, Protocol, Repeat};
use couch_ir::pwm::{self, Solution};
use couch_ir::tx::{self, Irtx};

const USAGE: &str = "\
usage: couch-ir <command> [options]

Encodes a consumer-IR command and sends it out the HA100's blaster, or prints
the timing table it would send.

commands:
  send <protocol> <address> <command>   encode one command and send it
  raw <carrier_hz> <us,us,us,...>        send a literal mark/space table
  codeset <file> <button>                look a button up in a codeset and send
  list                                   the protocols and their carriers

options:
  --dry-run              print the carrier and timing table; touch no hardware
  --repeats N            after the first frame, send N held-key repeats (default 0)
  --toggle               set the RC5/RC6 toggle bit (a distinct, not held, press)
  --carrier HZ           override the protocol's carrier (send/codeset)
  --device PATH          blaster device node                 (default /dev/irtx)
  --solution 0|1         dry-run: which waveform to size for (default 1, this
                         device's PWM-only encoding; a real send asks the driver)
  -h, --help             this text

addresses and commands take decimal, 0x-hex or 0b-binary.

examples:
  couch-ir send nec 0x04 0x08 --dry-run     # print the NEC table, send nothing
  couch-ir send nec 0x04 0x08               # send it (needs the blaster)
  couch-ir raw 38000 9000,4500,560,560      # a literal table
  couch-ir list";

/// The blaster node, matching the driver's `device_create(... \"irtx\")`.
const DEFAULT_DEVICE: &str = "/dev/irtx";

macro_rules! say {
    ($($a:tt)*) => {{
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        if writeln!(out, $($a)*).is_err() {
            std::process::exit(0);
        }
    }};
}

enum Fail {
    Usage(String),
    Silent,
    Ir(couch_ir::Error),
}

impl From<couch_ir::Error> for Fail {
    fn from(e: couch_ir::Error) -> Self {
        Fail::Ir(e)
    }
}

struct Args {
    words: Vec<String>,
    dry_run: bool,
    repeats: u32,
    toggle: bool,
    carrier: Option<u32>,
    device: String,
    solution: Solution,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            eprintln!("couch-ir: {m}\ntry --help");
            ExitCode::from(2)
        }
        Err(Fail::Silent) => ExitCode::from(2),
        Err(Fail::Ir(e)) => {
            eprintln!("couch-ir: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Fail> {
    let args = parse()?;
    let Some(command) = args.words.first().cloned() else {
        eprintln!("{USAGE}");
        return Err(Fail::Silent);
    };
    match command.as_str() {
        "send" => send(&args),
        "raw" => raw(&args),
        "codeset" => codeset_cmd(&args),
        "list" => list(),
        other => Err(Fail::Usage(format!("unknown command {other}"))),
    }
}

fn parse() -> Result<Args, Fail> {
    let mut a = Args {
        words: Vec::new(),
        dry_run: false,
        repeats: 0,
        toggle: false,
        carrier: None,
        device: DEFAULT_DEVICE.to_string(),
        // Default to this device's real encoding, so a dry-run's word/byte
        // count matches what a real send would produce here.
        solution: Solution::PwmOnly,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| -> Result<String, Fail> {
            it.next()
                .ok_or_else(|| Fail::Usage(format!("{flag} needs a value")))
        };
        match arg.as_str() {
            "--dry-run" => a.dry_run = true,
            "--repeats" => a.repeats = num(&value("--repeats")?, "--repeats")?,
            "--toggle" => a.toggle = true,
            "--carrier" => {
                a.carrier = Some(codeset::parse_u32(&value("--carrier")?).map_err(Fail::Usage)?)
            }
            "--device" => a.device = value("--device")?,
            "--solution" => {
                a.solution = match value("--solution")?.as_str() {
                    "0" => Solution::IrtxPwm,
                    "1" => Solution::PwmOnly,
                    other => {
                        return Err(Fail::Usage(format!(
                            "--solution takes 0 or 1, not {other:?}"
                        )))
                    }
                }
            }
            "-h" | "--help" => {
                say!("{USAGE}");
                std::process::exit(0);
            }
            _ if arg.starts_with("--") => return Err(Fail::Usage(format!("unknown option {arg}"))),
            _ => a.words.push(arg),
        }
    }
    Ok(a)
}

fn num(s: &str, flag: &str) -> Result<u32, Fail> {
    codeset::parse_u32(s).map_err(|_| Fail::Usage(format!("{flag} does not take {s:?}")))
}

// --- commands ---------------------------------------------------------------

fn send(args: &Args) -> Result<(), Fail> {
    let proto_name = args
        .words
        .get(1)
        .ok_or_else(|| Fail::Usage("send needs a protocol".into()))?;
    let protocol = Protocol::from_name(proto_name)
        .ok_or_else(|| Fail::Usage(format!("unknown protocol {proto_name:?}")))?;
    if protocol == Protocol::Raw {
        return Err(Fail::Usage("use `couch-ir raw` for a literal table".into()));
    }
    let address = num(
        args.words
            .get(2)
            .ok_or_else(|| Fail::Usage("send needs an address".into()))?,
        "address",
    )?;
    let command = num(
        args.words
            .get(3)
            .ok_or_else(|| Fail::Usage("send needs a command".into()))?,
        "command",
    )?;
    let mut message = proto::encode(protocol, address, command, args.toggle)?;
    if let Some(hz) = args.carrier {
        message.frame.carrier_hz = hz;
    }
    let label = format!(
        "{} address {address:#x} command {command:#x}",
        protocol.name()
    );
    deliver(args, &message, &label)
}

fn raw(args: &Args) -> Result<(), Fail> {
    let carrier = num(
        args.words
            .get(1)
            .ok_or_else(|| Fail::Usage("raw needs a carrier in Hz".into()))?,
        "carrier",
    )?;
    let table = args
        .words
        .get(2)
        .ok_or_else(|| Fail::Usage("raw needs a comma-separated us table".into()))?;
    let mut pattern = Vec::new();
    for field in table.split(',') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        pattern.push(num(field, "table")?);
    }
    let message = proto::raw(carrier, pattern)?;
    deliver(args, &message, &format!("raw at {carrier} Hz"))
}

fn codeset_cmd(args: &Args) -> Result<(), Fail> {
    let path = args
        .words
        .get(1)
        .ok_or_else(|| Fail::Usage("codeset needs a file".into()))?;
    let button = args
        .words
        .get(2)
        .ok_or_else(|| Fail::Usage("codeset needs a button".into()))?;
    let text = std::fs::read_to_string(path)
        .map_err(|e| Fail::Usage(format!("cannot read {path}: {e}")))?;
    let cs = Codeset::parse(path, &text)?;
    let entry = cs.get(button).ok_or_else(|| {
        Fail::Usage(format!(
            "no button {button:?} in {path}; have: {}",
            cs.buttons().join(", ")
        ))
    })?;
    let mut message = entry.encode(args.toggle)?;
    if let Some(hz) = args.carrier {
        message.frame.carrier_hz = hz;
    }
    deliver(
        args,
        &message,
        &format!(
            "{button} ({} {:#x}/{:#x})",
            entry.protocol.name(),
            entry.address,
            entry.command
        ),
    )
}

fn list() -> Result<(), Fail> {
    say!("protocol   carrier   codes");
    for p in Protocol::all() {
        say!(
            "{:<10} {:>5} Hz  {}",
            p.name(),
            p.carrier_hz(),
            p.describe()
        );
    }
    say!("");
    say!("carriers are nominal; the dry-run and the encoder use these exact values.");
    Ok(())
}

// --- shared -----------------------------------------------------------------

/// Print what will go out, then either stop (dry-run) or send it.
fn deliver(args: &Args, message: &Message, label: &str) -> Result<(), Fail> {
    let frame = &message.frame;
    say!("{label}");
    say!(
        "carrier {} Hz, {} marks/spaces, {} us airtime",
        frame.carrier_hz,
        frame.pattern_us.len(),
        frame.duration_us()
    );
    say!("us: {}", join_us(&frame.pattern_us));
    match &message.repeat {
        Repeat::Resend { period_ms } => {
            if args.repeats > 0 {
                say!(
                    "repeat: resend whole frame every {period_ms} ms x{}",
                    args.repeats
                );
            }
        }
        Repeat::Ditto { frame, period_ms } => {
            if args.repeats > 0 {
                say!(
                    "repeat: ditto every {period_ms} ms x{}: {}",
                    args.repeats,
                    join_us(&frame.pattern_us)
                );
            }
        }
    }

    if args.dry_run {
        // Show the PWM buffer size for the chosen solution, so a bench receiver
        // or logic analyser has the same numbers a real send would DMA.
        let wave = pwm::to_wave(frame, args.solution);
        say!(
            "pwm ({}): {} words, {} bytes to /dev/irtx",
            solution_name(args.solution),
            wave.len(),
            wave.len() * 4
        );
        return Ok(());
    }

    // Real send. Open the blaster, transmit, report. The driver is asked which
    // waveform it wants; --solution is a dry-run knob only.
    let mut irtx = Irtx::open(&args.device)?;
    say!("sending via {} ...", args.device);
    let sent = tx::transmit(&mut irtx, message, args.repeats)?;
    say!(
        "sent {} frame(s); driver wanted {} encoding",
        sent.frames,
        solution_name(sent.solution)
    );
    Ok(())
}

fn solution_name(s: Solution) -> &'static str {
    match s {
        Solution::IrtxPwm => "IRTX+PWM (type 0)",
        Solution::PwmOnly => "PWM-only (type 1)",
    }
}

/// The timing table as space-separated microseconds - the form a logic
/// analyser capture is easiest to diff against.
fn join_us(pattern: &[u32]) -> String {
    let mut s = String::new();
    for (i, d) in pattern.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&d.to_string());
    }
    s
}
