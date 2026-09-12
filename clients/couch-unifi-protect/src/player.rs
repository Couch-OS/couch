//! One bounded, silent software decoder. Network secrets never reach argv.
use crate::{
    media::{Cancellation, Session},
    settings::Settings,
    Quality,
};
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 270;
const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Playing,
    Ended,
    Unavailable,
}
pub struct Player {
    stop: Arc<Cancellation>,
    child: Arc<Mutex<Option<Child>>>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    status: Arc<Mutex<Status>>,
}
impl Player {
    /// Starts an explicit 60-second view. Closing or dropping cancels transport
    /// and kills the decoder; the UI never waits for a stalled socket.
    pub fn start(settings: Settings, camera_id: String) -> Self {
        let stop = Arc::new(Cancellation::default());
        let child = Arc::new(Mutex::new(None));
        let latest = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(Status::Connecting));
        let result = Self {
            stop: stop.clone(),
            child: child.clone(),
            latest: latest.clone(),
            status: status.clone(),
        };
        static ACTIVE: AtomicBool = AtomicBool::new(false);
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            *status.lock().unwrap() = Status::Unavailable;
            return result;
        }
        struct Active;
        impl Drop for Active {
            fn drop(&mut self) {
                ACTIVE.store(false, Ordering::Release);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        watchdog(stop.clone(), child.clone(), deadline);
        thread::spawn(move || {
            let _active = Active;
            let run = (|| -> crate::Result<()> {
                let client = settings.client()?;
                let view = client.live_view(&camera_id, Quality::Low, Duration::from_secs(60))?;
                if stop.is_cancelled() {
                    return Ok(());
                }
                let mut session = Session::connect_cancellable(&view, &settings, stop.clone())?;
                if stop.is_cancelled() {
                    return Ok(());
                }
                let mut process = decoder_command()
                    .spawn()
                    .map_err(|_| crate::Error::Configuration)?;
                let mut input = process.stdin.take().ok_or(crate::Error::Configuration)?;
                let mut output = process.stdout.take().ok_or(crate::Error::Configuration)?;
                {
                    let mut slot = child.lock().unwrap();
                    *slot = Some(process);
                    if stop.is_cancelled() {
                        if let Some(p) = slot.as_mut() {
                            let _ = p.kill();
                        }
                    }
                }
                let frame_stop = stop.clone();
                let frame_latest = latest.clone();
                let frame_status = status.clone();
                let reader = thread::spawn(move || {
                    while !frame_stop.is_cancelled() {
                        let mut pixels = vec![0; FRAME_BYTES];
                        if output.read_exact(&mut pixels).is_err() {
                            break;
                        }
                        // Replace, never append: slow displays cannot grow a queue.
                        *frame_latest.lock().unwrap() = Some(pixels);
                        *frame_status.lock().unwrap() = Status::Playing;
                    }
                });
                let result = (|| -> crate::Result<()> {
                    while !stop.is_cancelled() {
                        let nal = session.next_h264()?;
                        input.write_all(&nal).map_err(|_| crate::Error::Transport)?;
                    }
                    Ok(())
                })();
                drop(input);
                if let Some(p) = child.lock().unwrap().as_mut() {
                    let _ = p.kill();
                }
                let _ = reader.join();
                result
            })();
            let cancelled = stop.is_cancelled();
            stop.cancel();
            if let Some(mut process) = child.lock().unwrap().take() {
                let _ = process.kill();
                let _ = process.wait();
            }
            *status.lock().unwrap() = if run.is_ok() || cancelled || Instant::now() >= deadline {
                Status::Ended
            } else {
                Status::Unavailable
            };
        });
        result
    }
    pub fn take_frame(&self) -> Option<Vec<u8>> {
        self.latest.lock().unwrap().take()
    }
    pub fn status(&self) -> Status {
        *self.status.lock().unwrap()
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(p) = self.child.lock().unwrap().as_mut() {
            let _ = p.kill();
        }
    }
}
fn watchdog(
    stop: Arc<Cancellation>,
    child: Arc<Mutex<Option<Child>>>,
    deadline: Instant,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.is_cancelled() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        stop.cancel();
        if let Some(child) = child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    })
}
fn decoder_command() -> Command {
    let mut command = Command::new("/usr/bin/ffmpeg");
    command.args(["-nostdin","-hide_banner","-loglevel","error","-max_alloc","16777216","-protocol_whitelist","pipe","-threads","1","-probesize","65536","-analyzeduration","500000","-f","h264","-i","pipe:0","-an","-sn","-dn","-filter_threads","1","-vf","fps=8,scale=480:270:force_original_aspect_ratio=decrease,pad=480:270:(ow-iw)/2:(oh-ih)/2","-threads","1","-pix_fmt","rgb24","-f","rawvideo","pipe:1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only async-signal-safe syscalls in the forked child. Cap malformed
        // compressed input before any decoder allocation; no core dumps.
        unsafe {
            command.pre_exec(|| {
                for (resource, limit) in [
                    (libc::RLIMIT_AS, 256 * 1024 * 1024),
                    (libc::RLIMIT_CPU, 90),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let value = libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    };
                    if libc::setrlimit(resource, &value) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    command
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn dropping_view_and_wall_deadline_kill_child_and_interrupt_transport() {
        for close in [true, false] {
            let stop = Arc::new(Cancellation::default());
            let child = Arc::new(Mutex::new(Some(
                Command::new("/bin/sh")
                    .args(["-c", "exec sleep 30"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )));
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let socket = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (_peer, _) = listener.accept().unwrap();
            let mut transport = crate::media_io::Socket::new(
                socket,
                Instant::now() + Duration::from_secs(60),
                stop.clone(),
            )
            .unwrap();
            let reader = thread::spawn(move || transport.read_exact(&mut [0; 4]));
            let view = Player {
                stop: stop.clone(),
                child: child.clone(),
                latest: Arc::new(Mutex::new(None)),
                status: Arc::new(Mutex::new(Status::Connecting)),
            };
            let start = Instant::now();
            let timer = watchdog(
                stop.clone(),
                child.clone(),
                start + Duration::from_millis(60),
            );
            if close {
                drop(view);
            }
            assert!(reader.join().unwrap().is_err());
            timer.join().unwrap();
            let mut process = child.lock().unwrap().take().unwrap();
            assert!(!process.wait().unwrap().success());
            assert!(start.elapsed() < Duration::from_secs(1));
        }
    }
    #[test]
    fn decoder_has_no_network_or_secret_arguments_and_fixed_output_bound() {
        let command = decoder_command();
        let args = command
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|p| p == ["-protocol_whitelist", "pipe"]));
        assert!(args.windows(2).any(|p| p == ["-i", "pipe:0"]));
        assert!(args
            .iter()
            .all(|s| !s.contains("rtsps:") && !s.contains("api_key")));
        assert_eq!(FRAME_BYTES, 388800);
    }
}
