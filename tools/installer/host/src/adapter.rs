//! Bounded worker pipes. The caller owns admission; any failed operation kills
//! the worker. Nested USB deadlines can shorten but never extend its request.
use crate::{session::SessionGuard, stage::CHUNK};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};
#[cfg(windows)]
#[path = "adapter_windows.rs"]
mod windows;

pub fn materialize(session: &SessionGuard) -> Result<std::path::PathBuf> {
    let directory = session.path().join("mtk-adapter");
    std::fs::create_dir(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    for (name, bytes) in [
        (
            "couch_serial.py",
            include_bytes!("../../couch_serial.py").as_slice(),
        ),
        (
            "stage_usb.py",
            include_bytes!("../../stage_usb.py").as_slice(),
        ),
        (
            "mtk_adapter.py",
            include_bytes!("../../mtk_adapter.py").as_slice(),
        ),
        ("mtk_usb.py", include_bytes!("../../mtk_usb.py").as_slice()),
        ("mtk_tty.py", include_bytes!("../../mtk_tty.py").as_slice()),
        (
            "mtk_readonly.py",
            include_bytes!("../../mtk_readonly.py").as_slice(),
        ),
        (
            "mtk_writer.py",
            include_bytes!("../../mtk_writer.py").as_slice(),
        ),
        (
            "mtk_session.py",
            include_bytes!("../../mtk_session.py").as_slice(),
        ),
        (
            "couch_install.py",
            include_bytes!("../../couch_install.py").as_slice(),
        ),
        (
            "mtk_source_inventory.json",
            include_bytes!("../../mtk_source_inventory.json").as_slice(),
        ),
    ] {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(directory.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    Ok(directory.join("mtk_adapter.py"))
}
/// Lease shared by sessions beneath the canonical owner-only installer root.
pub struct UsbLease(std::fs::File);
impl UsbLease {
    pub fn acquire(session: &SessionGuard) -> Result<Self> {
        let parent = session.path().parent().context("missing state root")?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options.open(parent.join("usb.lock"))?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "invalid USB lock");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o077 == 0
                    && metadata.nlink() == 1,
                "USB lock must be private and singly linked"
            );
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(
                metadata.file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                    == 0,
                "USB lock cannot be a reparse point"
            );
        }
        file.try_lock()
            .context("another installer owns USB; close it before starting another session")?;
        Ok(Self(file))
    }
}
impl Drop for UsbLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
enum Request {
    Read(usize),
    Write(zeroize::Zeroizing<Vec<u8>>),
}
struct Pipe {
    requests: SyncSender<Request>,
    responses: Receiver<std::io::Result<Vec<u8>>>,
}
impl Pipe {
    fn exchange(&self, request: Request, deadline: Instant) -> Result<Vec<u8>> {
        let left = deadline
            .checked_duration_since(Instant::now())
            .context("worker deadline expired")?;
        self.requests
            .try_send(request)
            .context("worker pipe busy")?;
        Ok(self
            .responses
            .recv_timeout(left)
            .context("worker pipe deadline or disconnect")??)
    }
}
pub struct Worker {
    child: Child,
    input: Pipe,
    output: Pipe,
    deadline: Instant,
    deadlines: Vec<Instant>,
    poisoned: bool,
    #[cfg(windows)]
    _job: windows::Job,
}
impl Worker {
    /// Executable/runtime/source verification is required before this call.
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn().context("start verified MTK worker")?;
        #[cfg(windows)]
        let job = match windows::Job::assign(&child) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let mut input = child.stdin.take().context("missing worker stdin")?;
        let mut output = child.stdout.take().context("missing worker stdout")?;
        let (wtx, wrx) = mpsc::sync_channel(1);
        let (dtx, drx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(Request::Write(bytes)) = wrx.recv() {
                let result = input
                    .write_all(&bytes)
                    .and_then(|_| input.flush())
                    .map(|_| Vec::new());
                let failed = result.is_err();
                if dtx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let (rtx, rrx) = mpsc::sync_channel(1);
        let (etx, erx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(Request::Read(size)) = rrx.recv() {
                let mut bytes = vec![0; size];
                let result = output.read_exact(&mut bytes).map(|_| bytes);
                let failed = result.is_err();
                if etx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Pipe {
                requests: wtx,
                responses: drx,
            },
            output: Pipe {
                requests: rtx,
                responses: erx,
            },
            deadline: Instant::now(),
            deadlines: Vec::new(),
            poisoned: false,
            #[cfg(windows)]
            _job: job,
        })
    }
    fn stop(&mut self) {
        self.poisoned = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    pub fn operation<T>(
        &mut self,
        budget: Duration,
        run: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        ensure!(!self.poisoned, "MTK worker poisoned");
        ensure!(
            !budget.is_zero() && budget <= Duration::from_secs(1800),
            "invalid worker deadline"
        );
        self.deadline = Instant::now() + budget;
        self.deadlines.clear();
        let result = run(self).and_then(|value| {
            ensure!(
                Instant::now() < self.deadline && self.deadlines.is_empty(),
                "worker deadline expired or left armed"
            );
            Ok(value)
        });
        if result.is_err() {
            self.stop();
        }
        result
    }
    fn read(&mut self, size: usize) -> Result<Vec<u8>> {
        ensure!(!self.poisoned && size <= CHUNK, "invalid worker read");
        self.output.exchange(Request::Read(size), self.deadline)
    }
    pub fn send(&mut self, value: &Value) -> Result<()> {
        ensure!(
            !self.poisoned && value.is_object(),
            "invalid worker command"
        );
        let bytes = serde_json::to_vec(value)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= CHUNK,
            "worker command exceeds bound"
        );
        let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
        frame.extend(bytes);
        self.input.exchange(
            Request::Write(zeroize::Zeroizing::new(frame)),
            self.deadline,
        )?;
        Ok(())
    }
    pub fn event(&mut self) -> Result<Value> {
        for _ in 0..1024 {
            let size = u32::from_le_bytes(self.read(4)?.try_into().unwrap()) as usize;
            ensure!(size > 0 && size <= CHUNK, "invalid worker frame size");
            let value: Value = serde_json::from_slice(&self.read(size)?)?;
            ensure!(value.is_object(), "invalid worker event");
            if value["event"] == "error" {
                let diagnostic = &value["diagnostic"];
                let mut summary = format!("MTK worker stopped: {}", diagnostic_summary(diagnostic));
                // The writer reports every non-InstallError USB fault at one
                // wrapper line, so the cause is what identifies the failure.
                if diagnostic["cause"].is_object() {
                    summary.push_str(&format!(
                        "; cause {}",
                        diagnostic_summary(&diagnostic["cause"])
                    ));
                }
                anyhow::bail!("{summary}; retain originals");
            }
            if value["event"] == "deadline_end" {
                ensure!(
                    value.as_object().unwrap().len() == 1,
                    "invalid deadline end"
                );
                let previous = self.deadlines.pop().context("unmatched deadline end")?;
                ensure!(Instant::now() < self.deadline, "worker operation expired");
                self.deadline = previous;
                self.send(&json!({"ack":"deadline_end"}))?;
                continue;
            }
            if value["event"] != "deadline" {
                return Ok(value);
            }
            let seconds = value["seconds"]
                .as_f64()
                .context("invalid worker deadline")?;
            ensure!(
                seconds.is_finite()
                    && seconds > 0.0
                    && seconds <= 900.0
                    && value.as_object().unwrap().len() == 2
                    && self.deadlines.len() < 8,
                "invalid worker deadline"
            );
            self.deadlines.push(self.deadline);
            self.deadline = self
                .deadline
                .min(Instant::now() + Duration::from_secs_f64(seconds));
            self.send(&json!({"ack":"deadline"}))?;
        }
        anyhow::bail!("too many worker deadline requests")
    }
    pub fn chunk(&mut self, expected: usize) -> Result<Vec<u8>> {
        ensure!(
            expected > 0 && expected <= CHUNK,
            "invalid partition chunk size"
        );
        ensure!(
            self.event()? == json!({"event":"chunk","size":expected}),
            "unexpected chunk transition"
        );
        let header = self.read(12)?;
        ensure!(
            header[..4] == (expected as u32).to_le_bytes()
                && header[4..8] == (expected as u32).to_le_bytes()
                && header[8..] == [0; 4],
            "invalid worker chunk"
        );
        self.read(expected)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}
/// One diagnostic object as a fixed summary. Only an allowlisted category and
/// reviewed source filename appear, with a numeric line and codes; worker text
/// is never rendered.
fn diagnostic_summary(diagnostic: &Value) -> String {
    let category = match diagnostic["category"].as_str().unwrap_or("WorkerError") {
        category @ ("USBError" | "USBTimeoutError" | "InstallError" | "OSError"
        | "PermissionError" | "TimeoutError" | "ValueError" | "TypeError"
        | "AttributeError" | "RuntimeError") => category,
        _ => "WorkerError",
    };
    let source = reviewed_source(diagnostic);
    format!(
        "{category} at {source}:{} (errno {:?}, backend {:?}){}",
        diagnostic["line"].as_u64().unwrap_or(0),
        diagnostic["errno"].as_i64(),
        diagnostic["backend_error_code"].as_i64(),
        diagnostic_frames(diagnostic)
    )
}

/// The reviewed call path behind one fault, outermost first. Each entry is
/// re-filtered exactly like a source name, so only reviewed filenames and
/// numeric lines can appear.
fn diagnostic_frames(diagnostic: &Value) -> String {
    let Some(frames) = diagnostic["frames"].as_array() else {
        return String::new();
    };
    let rendered = frames
        .iter()
        .take(8)
        .map(|frame| {
            format!(
                "{}:{}",
                reviewed_source(frame),
                frame["line"].as_u64().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        String::new()
    } else {
        format!(" via {}", rendered.join(" > "))
    }
}

/// The command that runs the MediaTek worker.
///
/// On macOS it runs as root through sudo. Apple's CDC ACM driver claims the
/// preloader's interfaces, and libusb can only take them back by capturing the
/// device, which needs root. The unprivileged serial-port path proved able to
/// read every partition but could not complete a download-agent write, so the
/// worker uses libusb exactly as the Linux path validated on hardware does.
/// Only this worker is elevated: the host, the terminal and the session
/// directory stay under the user's account. sudo runs non-interactively, so
/// `Privilege::acquire` must have succeeded first. The worker speaks over piped
/// stdio, which sudo preserves.
pub fn mtk_worker(python: impl AsRef<Path>) -> Command {
    if cfg!(target_os = "macos") {
        let mut command = Command::new("sudo");
        command.arg("-n").arg("--").arg(python.as_ref());
        command
    } else {
        Command::new(python.as_ref())
    }
}

fn sudo_succeeds(args: &[&str]) -> bool {
    Command::new("sudo")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Administrator rights held for the MediaTek worker on macOS.
///
/// Acquire it before the long downloads so a missing credential fails early,
/// and keep it until the worker has started: sudo's cached credential lasts a
/// few minutes and a firmware download can outlast it. The refresh thread ends
/// when the guard drops. On other platforms this is inert.
pub struct Privilege {
    _stop: Option<mpsc::Sender<()>>,
}
impl Privilege {
    pub fn acquire() -> Result<Self> {
        if !cfg!(target_os = "macos") {
            return Ok(Self { _stop: None });
        }
        ensure!(
            sudo_succeeds(&["-n", "true"]),
            "The MediaTek USB worker needs administrator rights on macOS. Start the installer through install.sh, which asks once up front, or run `sudo -v` in this terminal before starting it"
        );
        let (stop, stopped) = mpsc::channel::<()>();
        thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(Duration::from_secs(60))
            {
                sudo_succeeds(&["-n", "-v"]);
            }
        });
        Ok(Self { _stop: Some(stop) })
    }
}

fn reviewed_source(value: &Value) -> &str {
    value["source"]
        .as_str()
        .filter(|v| {
            matches!(
                *v,
                "mtk_adapter.py"
                    | "mtk_usb.py"
                    | "mtk_tty.py"
                    | "mtk_readonly.py"
                    | "mtk_writer.py"
            )
        })
        .unwrap_or("worker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::OnceLock};
    fn fixture() -> &'static PathBuf {
        static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let directory = tempfile::tempdir().unwrap().keep();
            let source = directory.join("worker.rs");
            fs::write(&source, r###"
use std::{io::{self,Read,Write},thread,time::Duration};
fn emit(data:&[u8]) { let mut o=io::stdout();o.write_all(&(data.len() as u32).to_le_bytes()).unwrap();o.write_all(data).unwrap();o.flush().unwrap(); }
fn ack() { let mut h=[0;4];io::stdin().read_exact(&mut h).unwrap();let mut b=vec![0;u32::from_le_bytes(h) as usize];io::stdin().read_exact(&mut b).unwrap(); }
fn main() { match std::env::args().nth(1).unwrap().as_str() {
"safe_error" => emit(br#"{"event":"error","diagnostic":{"category":"USBError","source":"mtk_usb.py","line":242,"errno":13,"backend_error_code":-3,"message":"secret"}}"#),
"wrapped_error" => emit(br#"{"event":"error","diagnostic":{"category":"InstallError","source":"mtk_writer.py","line":317,"cause":{"category":"USBTimeoutError","source":"mtk_tty.py","line":161,"errno":60,"backend_error_code":-7,"frames":[{"source":"mtk_writer.py","line":308},{"source":"/private/secret","line":"secret"},{"source":"mtk_tty.py","line":161}],"message":"secret"},"message":"secret"}}"#),
"untrusted_error" => emit(br#"{"event":"error","diagnostic":{"category":"secret","source":"/private/secret","line":"secret","errno":"secret"}}"#),
"hang" => thread::sleep(Duration::from_secs(60)),
"oversize" => { io::stdout().write_all(&u32::MAX.to_le_bytes()).unwrap(); },
"deadline" => { emit(br#"{"event":"deadline","seconds":0.05}"#);ack();thread::sleep(Duration::from_secs(60)); },
"nested" => { emit(br#"{"event":"deadline","seconds":1}"#);ack();emit(br#"{"event":"deadline_end"}"#);ack();emit(br#"{"event":"done"}"#); },
"flood" => loop {emit(br#"{"event":"deadline","seconds":900}"#);ack();emit(br#"{"event":"deadline_end"}"#);ack();},
_ => panic!()
}}
"###).unwrap();
            let executable = directory.join(if cfg!(windows) { "worker.exe" } else { "worker" });
            assert!(Command::new("rustc").arg(&source).arg("-o").arg(&executable).status().unwrap().success());
            executable
        })
    }
    fn worker(mode: &str) -> Worker {
        Worker::spawn(Command::new(fixture()).arg(mode)).unwrap()
    }
    #[test]
    fn mtk_worker_is_elevated_only_on_macos() {
        let command = mtk_worker(Path::new("/opt/runtime/python3"));
        let program = command.get_program().to_string_lossy().into_owned();
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if cfg!(target_os = "macos") {
            assert_eq!(program, "sudo");
            assert_eq!(args, ["-n", "--", "/opt/runtime/python3"]);
        } else {
            assert_eq!(program, "/opt/runtime/python3");
            assert!(args.is_empty());
        }
    }
    #[test]
    fn privilege_is_inert_elsewhere_and_explains_itself_on_macos() {
        match Privilege::acquire() {
            Ok(_) => {}
            Err(error) if cfg!(target_os = "macos") => {
                assert!(error.to_string().contains("sudo -v"), "{error}");
            }
            Err(error) => panic!("privilege must be inert off macOS: {error}"),
        }
    }
    #[test]
    fn wrapped_worker_errors_name_the_originating_fault() {
        let mut worker = worker("wrapped_error");
        let error = worker
            .operation(Duration::from_secs(5), |w| w.event())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("InstallError at mtk_writer.py:317"),
            "{error}"
        );
        assert!(
            error.contains("cause USBTimeoutError at mtk_tty.py:161"),
            "{error}"
        );
        // The call path names the protocol step; an untrusted entry is neutralised.
        assert!(
            error.contains("via mtk_writer.py:308 > worker:0 > mtk_tty.py:161"),
            "{error}"
        );
        assert!(error.contains("Some(60)"), "{error}");
        assert!(!error.contains("secret"), "{error}");
    }
    #[test]
    fn worker_errors_retain_only_allowlisted_diagnostic_fields() {
        let mut good = worker("safe_error");
        let error = good
            .operation(Duration::from_secs(5), |w| w.event())
            .unwrap_err()
            .to_string();
        assert!(error.contains("USBError at mtk_usb.py:242"));
        assert!(error.contains("Some(13)"));
        assert!(!error.contains("secret"));
        let mut bad = worker("untrusted_error");
        let error = bad
            .operation(Duration::from_secs(5), |w| w.event())
            .unwrap_err()
            .to_string();
        assert!(error.contains("WorkerError at worker:0"));
        assert!(!error.contains("secret"));
    }
    #[test]
    fn expired_read_kills_and_poisons_worker() {
        let mut worker = worker("hang");
        let started = Instant::now();
        assert!(worker
            .operation(Duration::from_millis(50), |w| w.event())
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(worker.child.try_wait().unwrap().is_some());
        assert!(worker
            .operation(Duration::from_secs(1), |_| Ok(()))
            .is_err());
    }
    #[test]
    fn blocked_write_is_bounded_too() {
        let mut worker = worker("hang");
        assert!(worker
            .operation(Duration::from_millis(50), |w| {
                w.send(&json!({"large":"x".repeat(CHUNK-100)}))
            })
            .is_err());
        assert!(worker.child.try_wait().unwrap().is_some());
    }
    #[test]
    fn deadline_ack_arms_shorter_process_timeout() {
        let mut worker = worker("deadline");
        let started = Instant::now();
        assert!(worker
            .operation(Duration::from_secs(5), |w| w.event())
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn nested_deadlines_restore_but_never_extend_outer_bound() {
        let mut worker = worker("nested");
        assert_eq!(
            worker
                .operation(Duration::from_secs(5), |w| w.event())
                .unwrap(),
            json!({"event":"done"})
        );
        let mut flood = self::worker("flood");
        let started = Instant::now();
        assert!(flood
            .operation(Duration::from_millis(50), |w| w.event())
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn oversized_frames_and_callback_cancellation_kill_child() {
        let mut worker = worker("oversize");
        assert!(worker
            .operation(Duration::from_secs(5), |w| w.event())
            .is_err());
        let mut cancelled = self::worker("hang");
        assert!(cancelled
            .operation::<()>(Duration::from_secs(5), |_| anyhow::bail!("UI gone"))
            .is_err());
        assert!(cancelled.child.try_wait().unwrap().is_some());
    }
}
