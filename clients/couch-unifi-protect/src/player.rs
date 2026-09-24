//! One bounded, silent software decoder. Network secrets never reach argv.
use crate::{
    media::{Cancellation, Session},
    settings::Settings,
    Quality,
};
use couch_camera::FRAME_BYTES;
pub use couch_camera::{DecoderFailure, HEIGHT, WIDTH};
#[cfg(test)]
use std::process::{Command, Stdio};
use std::{
    io::{Read, Write},
    process::Child,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Playing,
    Ended,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Setup(crate::Error),
    Descriptor(crate::Error),
    Media(crate::Error),
    Decoder(DecoderFailure),
    Stream(crate::Error),
}
pub struct Player {
    stop: Arc<Cancellation>,
    child: Arc<Mutex<Option<Child>>>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    status: Arc<Mutex<Status>>,
    failure: Arc<Mutex<Option<Failure>>>,
}
impl Player {
    /// Starts an explicit 60-second view. Closing or dropping cancels transport
    /// and kills the decoder; the UI never waits for a stalled socket.
    pub fn start(settings: Settings, camera_id: String) -> Self {
        let stop = Arc::new(Cancellation::default());
        let child = Arc::new(Mutex::new(None));
        let latest = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(Status::Connecting));
        let failure = Arc::new(Mutex::new(None));
        let result = Self {
            stop: stop.clone(),
            child: child.clone(),
            latest: latest.clone(),
            status: status.clone(),
            failure: failure.clone(),
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
            let run = (|| -> Result<(), Failure> {
                let client = settings.client().map_err(Failure::Setup)?;
                let view = client
                    .live_view(&camera_id, Quality::Low, Duration::from_secs(60))
                    .map_err(Failure::Descriptor)?;
                if stop.is_cancelled() {
                    return Ok(());
                }
                let mut session = Session::connect_cancellable(&view, &settings, stop.clone())
                    .map_err(Failure::Media)?;
                if stop.is_cancelled() {
                    return Ok(());
                }
                let decoder = couch_camera::Decoder::spawn().map_err(Failure::Decoder)?;
                let (process, mut input, mut output) = decoder.into_parts();
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
                let result = (|| -> Result<(), Failure> {
                    while !stop.is_cancelled() {
                        let nal = session.next_h264().map_err(Failure::Stream)?;
                        input.write_all(&nal).map_err(|error| {
                            eprintln!("couch-unifi-protect: decoder input failed: {error}");
                            Failure::Decoder(DecoderFailure::Input(error.kind()))
                        })?;
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
            let succeeded = run.is_ok();
            if let Err(reason) = run {
                *failure.lock().unwrap() = Some(reason);
            }
            *status.lock().unwrap() = if succeeded || cancelled || Instant::now() >= deadline {
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
    pub fn failure(&self) -> Option<Failure> {
        *self.failure.lock().unwrap()
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
                failure: Arc::new(Mutex::new(None)),
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
}
