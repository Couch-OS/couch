//! The microphone key: hold it, speak, and the speech goes to Home Assistant's
//! Assist pipeline as it is captured.
//!
//! Two uses, chosen by what is on screen when the key goes down. With the
//! on-screen keyboard open, the run stops at speech-to-text and the words land
//! in the keyboard's field: dictating a search beats spelling it on a D-pad.
//! Anywhere else, the run continues to the intent stage and the assistant's
//! answer is shown beside what it heard. Both go to the first saved Home
//! Assistant connection, over the same WebSocket client the `couch-voice` CLI
//! uses, and nothing is written to disk. With no Home Assistant connection the
//! key still records, to `/tmp/couch-voice.wav`, so the microphone can be
//! proven without a hub; the overlay says why nothing else happened.
//!
//! The capture runs on its own thread because the UI thread must keep drawing:
//! a missed period is an overrun, and an overrun in the middle of a sentence is
//! a hole in a word. The thread owns the `Capture` and the connection; this
//! side keeps a stop handle, two atomics to read every frame, and a mutex the
//! thread writes its progress into.
//!
//! What the screen shows is derived from whether that thread still holds the
//! device, never from a flag someone remembered to set. `live` goes up before
//! anything is opened and is cleared by the worker itself, as its last act,
//! after the stream is stopped and the device closed. There is no code path
//! that starts a capture without the key going down, and no instant in which
//! the device is open and the indicator dark.
//!
//! Stopping is asynchronous, and it has to be. The worker is normally inside
//! ALSA's poll for up to one period, so waiting for it would stall the UI on
//! every release. `stop()` trips the stop handle and returns; the next `read`
//! returns 0, the client sends its end-of-stream byte and collects the rest of
//! the run, the device is closed, `live` comes down. Home Assistant's own voice
//! activity detection can end the stream first, on 0.7 s of silence; the
//! worker then trips the same handle so a tapped (latched) key does not keep
//! the microphone open for a transcript that is already in.
//!
//! A microphone press that arrives while a previous run is still in its tail
//! is ignored, with a line in the log. The device is still open, so opening it
//! again would fail anyway.

use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use couch_voice::alsa::{Pcm, StopHandle, Wanted};
use couch_voice::ha::{Assistant, Event, Options, Stage};
use couch_voice::Source;
use couch_voice::{abi, level::Meter, wav};

/// Where a recording lands when there is no Home Assistant to send it to.
/// One file, overwritten: a scratch buffer for proving the microphone, not an
/// archive. With a hub configured nothing is written.
pub const OUT: &str = "/tmp/couch-voice.wav";

const DEVICE: &str = "hw:0,1";
const RATE: u32 = 16_000;

/// Long enough for anything anyone says to a remote, short enough that a key
/// wedged under a sofa cushion is a nuisance rather than a recording of your
/// evening.
const LIMIT: Duration = Duration::from_secs(30);

/// What the speech is for, decided when the key goes down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Words into the on-screen keyboard: the run stops after speech-to-text.
    Dictation,
    /// A request to the house: the run continues to the intent stage.
    Assistant,
}

/// Where a run is, for the overlay. Ordered as a run proceeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Device open, connecting and authenticating to Home Assistant.
    Connecting,
    /// Audio is going up.
    Listening,
    /// The stream is over; Home Assistant is transcribing or answering.
    Thinking,
    Done,
    Failed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Connecting => "CONNECTING",
            Phase::Listening => "LISTENING",
            Phase::Thinking => "THINKING",
            Phase::Done => "HEARD",
            Phase::Failed => "VOICE FAILED",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Phase::Connecting => "connecting",
            Phase::Listening => "listening",
            Phase::Thinking => "thinking",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }
}

/// How a run ended, handed to the UI thread once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Speech-to-text produced words for the keyboard.
    Dictated(String),
    /// The assistant answered. `heard` may be empty when the pipeline produced
    /// no transcript.
    Answered { heard: String, said: String },
    /// Home Assistant took the audio but produced nothing usable: nothing
    /// heard, or an error event.
    Failed(String),
    /// No Home Assistant connection is saved; the recording went to `OUT`.
    NoAssistant,
}

/// What the worker publishes as it goes. Read by the UI every frame while the
/// overlay is up; written from the capture thread between chunks.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub phase: Option<Phase>,
    pub heard: String,
    pub said: String,
    pub detail: String,
    /// Set once, at the end.
    pub outcome: Option<Outcome>,
}

/// Where the audio goes: host, port and token from a saved Home Assistant
/// connection. The token is used once, to authenticate, and is not kept
/// beyond the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl Endpoint {
    /// From a Home Assistant base URL as the web UI saves it. The Assist
    /// client speaks plain WebSocket, so an `https://` hub cannot be used yet
    /// and says so rather than failing later with a protocol error.
    pub fn from_url(url: &str, token: &str) -> Result<Endpoint, String> {
        let trimmed = url.trim();
        let rest = if let Some(rest) = trimmed.strip_prefix("http://") {
            rest
        } else if trimmed.starts_with("https://") {
            return Err("Voice needs an http:// Home Assistant URL: the remote's Assist client does not speak TLS yet".into());
        } else {
            trimmed
        };
        let authority = rest.split('/').next().unwrap_or("").trim();
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => match p.parse::<u16>() {
                Ok(port) => (h.trim_matches(|c| c == '[' || c == ']').to_owned(), port),
                Err(_) => (authority.to_owned(), couch_voice::ha::DEFAULT_PORT),
            },
            _ => (authority.to_owned(), couch_voice::ha::DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err("The Home Assistant connection has no host".into());
        }
        if token.trim().is_empty() {
            return Err("The Home Assistant connection has no access token".into());
        }
        Ok(Endpoint {
            host,
            port,
            token: token.to_owned(),
        })
    }
}

/// A source that feeds the level meter as it is read, so the overlay's bar
/// keeps moving while the pipeline client owns the read loop.
struct Metered<S: Source> {
    inner: S,
    meter: Arc<AtomicU32>,
    level: Meter,
}

impl<S: Source> Source for Metered<S> {
    fn rate(&self) -> u32 {
        self.inner.rate()
    }
    fn chunk_frames(&self) -> usize {
        self.inner.chunk_frames()
    }
    fn read(&mut self, out: &mut [i16]) -> couch_voice::Result<usize> {
        let n = self.inner.read(out)?;
        if n > 0 {
            self.level.feed(&out[..n]);
            self.meter.store(
                (self.level.level().meter() * 1000.0) as u32,
                Ordering::Relaxed,
            );
        }
        Ok(n)
    }
}

pub struct Mic {
    live: Arc<AtomicBool>,
    /// The meter, 0..1000, as an integer because it is read every frame and
    /// f32 has no atomic.
    meter: Arc<AtomicU32>,
    progress: Arc<Mutex<Progress>>,
    stop: Option<StopHandle>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Default for Mic {
    fn default() -> Self {
        Self::new()
    }
}

impl Mic {
    pub fn new() -> Mic {
        Mic {
            live: Arc::new(AtomicBool::new(false)),
            meter: Arc::new(AtomicU32::new(0)),
            progress: Arc::new(Mutex::new(Progress::default())),
            stop: None,
            worker: None,
        }
    }

    pub fn recording(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }

    /// 0.0 to 1.0, for a level meter.
    pub fn meter(&self) -> f32 {
        self.meter.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// A copy of where the run is, for the overlay.
    pub fn progress(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// The run's result, once. `None` until the worker has finished.
    pub fn take_outcome(&mut self) -> Option<Outcome> {
        let outcome = self.progress.lock().ok().and_then(|mut p| p.outcome.take());
        if outcome.is_some() {
            self.reap();
        }
        outcome
    }

    /// Open the microphone and start a run for `target`, sending to
    /// `endpoint` when there is one.
    pub fn start(&mut self, target: Target, endpoint: Result<Endpoint, String>) {
        self.reap();
        if self.recording() {
            // Either a capture is running - in which case the key is already
            // down and this is not a new press - or the last one is still
            // closing the device. Both mean the device is not ours to open.
            println!("couch-gui: microphone busy; press ignored");
            return;
        }
        // Up before the device is opened, not after: between `configure`
        // returning and the flag being set there would otherwise be a running
        // stream with a dark indicator, which is the one ordering this module
        // exists to rule out.
        self.live.store(true, Ordering::Relaxed);
        self.meter.store(0, Ordering::Relaxed);
        if let Ok(mut p) = self.progress.lock() {
            *p = Progress {
                phase: Some(if endpoint.is_ok() {
                    Phase::Connecting
                } else {
                    Phase::Listening
                }),
                ..Progress::default()
            };
        }
        let wanted = Wanted {
            rate: RATE,
            limit: LIMIT,
            sample_format: abi::FORMAT_S16_LE,
            ..Wanted::default()
        };
        let capture = match Pcm::open(DEVICE).and_then(|p| p.configure(&wanted)) {
            Ok(c) => c,
            Err(e) => {
                // Not fatal, and not silent: a remote whose microphone key does
                // nothing should say why in the log rather than look broken.
                self.live.store(false, Ordering::Relaxed);
                eprintln!("couch-gui: microphone unavailable: {e}");
                if let Ok(mut p) = self.progress.lock() {
                    p.phase = Some(Phase::Failed);
                    p.detail = format!("Microphone unavailable: {e}");
                    p.outcome = Some(Outcome::Failed(p.detail.clone()));
                }
                return;
            }
        };
        let stop = capture.stop_handle();
        self.stop = Some(stop.clone());

        let (live, meter, progress) =
            (self.live.clone(), self.meter.clone(), self.progress.clone());
        self.worker = Some(std::thread::spawn(move || {
            let source = Metered {
                inner: capture,
                meter: meter.clone(),
                level: Meter::default(),
            };
            let outcome = match endpoint {
                Ok(endpoint) => run(source, target, endpoint, stop, &progress),
                Err(reason) => {
                    // No hub: prove the microphone, then say what was missing.
                    record_to_file(source, &progress);
                    if let Ok(mut p) = progress.lock() {
                        p.detail = reason;
                    }
                    Outcome::NoAssistant
                }
            };
            // The stream is over, whether the key came up, the limit was
            // reached, the server heard silence or the device failed. Nothing
            // more will arrive, so the meter says so.
            meter.store(0, Ordering::Relaxed);
            // The device closed when the source dropped inside `run`, before
            // any of the collecting that followed; `live` comes down last.
            if let Ok(mut p) = progress.lock() {
                p.phase = Some(match &outcome {
                    Outcome::Failed(_) | Outcome::NoAssistant => Phase::Failed,
                    _ => Phase::Done,
                });
                p.outcome = Some(outcome);
            }
            live.store(false, Ordering::Relaxed);
        }));
    }

    /// Ends the recording without waiting for it. Returns immediately - the
    /// worker keeps `live` up until the device is closed, so the indicator,
    /// not this call, is what tracks the microphone.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.stop();
        }
    }

    /// Join a worker that has already finished. `live` is the last thing it
    /// clears, so a worker that has cleared it is at the end of its function
    /// and this cannot wait on ALSA or on a socket.
    fn reap(&mut self) {
        if !self.recording() {
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
        }
    }
}

fn set_phase(progress: &Mutex<Progress>, phase: Phase) {
    if let Ok(mut p) = progress.lock() {
        p.phase = Some(phase);
    }
}

/// Connect, stream, collect. The device is closed when `source` is dropped,
/// which happens as soon as the run's audio is over, before the tail of the
/// run is collected.
fn run<S: Source>(
    mut source: Metered<S>,
    target: Target,
    endpoint: Endpoint,
    stop: StopHandle,
    progress: &Arc<Mutex<Progress>>,
) -> Outcome {
    let mut assistant = match Assistant::connect(&endpoint.host, endpoint.port, &endpoint.token) {
        Ok(a) => a,
        Err(e) => {
            drop(source);
            return Outcome::Failed(format!("Could not reach Home Assistant: {e}"));
        }
    };
    let options = Options {
        end_stage: match target {
            Target::Dictation => Stage::Stt,
            Target::Assistant => Stage::Intent,
        },
        ..Options::default()
    };
    set_phase(progress, Phase::Listening);
    let mut heard = String::new();
    let mut said = String::new();
    let mut failure: Option<String> = None;
    let result = assistant.run(&mut source, &options, &mut |event| match event {
        Event::VadEnd { .. } => {
            // The server has the utterance; a latched key need not keep the
            // microphone open for it.
            stop.stop();
            set_phase(progress, Phase::Thinking);
        }
        Event::SttEnd { text } => {
            heard = text.clone();
            stop.stop();
            if let Ok(mut p) = progress.lock() {
                p.heard = text.clone();
                p.phase = Some(Phase::Thinking);
            }
        }
        Event::IntentEnd { speech, .. } => {
            said = speech.clone();
            if let Ok(mut p) = progress.lock() {
                p.said = speech.clone();
            }
        }
        Event::Error { code, message } => {
            failure = Some(if message.is_empty() {
                code.clone()
            } else {
                message.clone()
            });
        }
        _ => {}
    });
    drop(source);
    match result {
        Err(e) => Outcome::Failed(format!("Home Assistant: {e}")),
        Ok(outcome) => {
            if let Some((code, message)) = outcome.error.clone() {
                failure.get_or_insert(if message.is_empty() { code } else { message });
            }
            let heard = outcome.text.clone().unwrap_or(heard);
            let said = outcome.speech.clone().unwrap_or(said);
            match (target, failure) {
                (_, Some(reason)) if heard.trim().is_empty() => Outcome::Failed(readable(&reason)),
                (Target::Dictation, _) if heard.trim().is_empty() => {
                    Outcome::Failed("Nothing heard".into())
                }
                (Target::Dictation, _) => Outcome::Dictated(heard),
                (Target::Assistant, Some(reason)) => Outcome::Answered {
                    heard,
                    said: readable(&reason),
                },
                (Target::Assistant, None) if heard.trim().is_empty() => {
                    Outcome::Failed("Nothing heard".into())
                }
                (Target::Assistant, None) => Outcome::Answered { heard, said },
            }
        }
    }
}

/// Home Assistant's pipeline error codes, in words a person on the sofa can
/// act on; anything else passes through.
fn readable(reason: &str) -> String {
    match reason {
        "stt-no-text-recognized" => "Nothing heard".into(),
        "stt-provider-missing" | "stt-provider-unsupported-metadata" => {
            "The Home Assistant pipeline has no speech-to-text engine".into()
        }
        "intent-not-supported" | "intent-failed" => {
            "Home Assistant could not handle that request".into()
        }
        "stt-stream-failed" => "Home Assistant could not process the audio".into(),
        other => other.to_owned(),
    }
}

/// No hub: read until the key comes up or the limit is hit, then write the
/// WAV so the microphone can be proven with nothing to talk to.
fn record_to_file<S: Source>(mut source: Metered<S>, progress: &Arc<Mutex<Progress>>) {
    let mut buf = vec![0i16; source.chunk_frames().max(1)];
    let mut samples = Vec::new();
    while let Ok(n) = source.read(&mut buf) {
        if n == 0 {
            break;
        }
        samples.extend_from_slice(&buf[..n]);
    }
    drop(source);
    set_phase(progress, Phase::Thinking);
    write_out(OUT, &samples);
}

/// The recording, into a file created for this user alone.
///
/// 0600, like the pairing PIN and the Home Assistant token, and unlinked first
/// so that the mode of a file somebody else left at this path cannot survive.
/// `create_new` rather than truncate for the same reason: after the unlink,
/// anything already at that name is not ours and is not to be written through.
fn write_out(path: &str, samples: &[i16]) {
    let _ = std::fs::remove_file(path);
    let file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path);
    let writer = match file {
        Ok(f) => wav::Writer::from_file(f, path, RATE, 1),
        Err(e) => {
            eprintln!("couch-gui: cannot create {path}: {e}");
            return;
        }
    };
    let done = writer.and_then(|mut w| {
        w.write(samples)?;
        w.finish()
    });
    match done {
        Ok(()) => println!(
            "couch-gui: recorded {:.1}s to {path}",
            samples.len() as f32 / RATE as f32
        ),
        Err(e) => eprintln!("couch-gui: cannot write {path}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recording_is_written_0600_over_whatever_was_there() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("couch-mic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.wav");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_out(path.to_str().unwrap(), &[0, 1, -1, 2]);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(meta.len() > 44);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn endpoints_come_from_the_saved_url_and_refuse_tls_and_blanks() {
        let e = Endpoint::from_url("http://homeassistant.local:8123/", "tok").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("homeassistant.local", 8123));
        let e = Endpoint::from_url("http://192.168.1.5", "tok").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("192.168.1.5", 8123));
        let e = Endpoint::from_url("ha.lan:8124", "tok").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("ha.lan", 8124));
        assert!(Endpoint::from_url("https://ha.lan", "tok")
            .unwrap_err()
            .contains("TLS"));
        assert!(Endpoint::from_url("http://", "tok").is_err());
        assert!(Endpoint::from_url("http://ha.lan", " ").is_err());
    }

    #[test]
    fn pipeline_error_codes_read_as_sentences() {
        assert_eq!(readable("stt-no-text-recognized"), "Nothing heard");
        assert_eq!(readable("something-else"), "something-else");
    }

    #[test]
    fn phases_name_themselves() {
        assert_eq!(Phase::Listening.label(), "LISTENING");
        assert_eq!(Phase::Failed.name(), "failed");
    }
}
