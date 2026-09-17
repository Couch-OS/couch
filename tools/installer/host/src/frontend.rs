//! Private, bounded Ratatui event channel. No secrets are passed through argv.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::time::Instant;
use zeroize::{Zeroize, Zeroizing};

const MAX_FRAME: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressUnit {
    Bytes,
    Items,
    Seconds,
}

struct RateSample {
    phase: usize,
    label: String,
    total: u64,
    done: u64,
    at: Instant,
    last_done: u64,
    rate: f64,
}

#[derive(Clone, Serialize)]
pub struct Choice {
    pub label: String,
    pub detail: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    id: u64,
    value: Option<String>,
    cancel: Option<bool>,
}
impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(value) = &mut self.value {
            value.zeroize();
        }
    }
}
#[derive(Serialize)]
struct State {
    event: &'static str,
    step: usize,
    steps: Vec<String>,
    detail: String,
    action: String,
    result: String,
    log_path: String,
    /// Release version of the installer configuration in use, shown on every
    /// screen so a screenshot identifies the build that produced it.
    version: String,
    measurement: Option<(u64, u64, f64)>,
    measurement_unit: ProgressUnit,
}

pub struct Ui {
    input: BufReader<Box<dyn Read>>,
    output: Box<dyn Write>,
    sequence: u64,
    poisoned: bool,
    state: State,
    rate_sample: Option<RateSample>,
}
impl Ui {
    pub fn new(input: Box<dyn Read>, output: Box<dyn Write>) -> Self {
        Self {
            input: BufReader::new(input),
            output,
            sequence: 0,
            poisoned: false,
            rate_sample: None,
            state: State {
                event: "state",
                step: 0,
                steps: vec!["Prepare".into()],
                detail: String::new(),
                action: String::new(),
                result: "active".into(),
                log_path: String::new(),
                version: String::new(),
                measurement: None,
                measurement_unit: ProgressUnit::Bytes,
            },
        }
    }
    /// Takes ownership of the socket explicitly inherited from the TUI.
    #[cfg(unix)]
    pub fn inherited_socket(fd: i32) -> Result<Self> {
        use std::os::{fd::FromRawFd, unix::net::UnixStream};
        ensure!(fd == 3, "expected dedicated installer socket descriptor 3");
        let mut kind: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&kind) as libc::socklen_t;
        unsafe {
            ensure!(
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&mut kind as *mut libc::c_int).cast(),
                    &mut size
                ) == 0
                    && kind == libc::SOCK_STREAM,
                "installer event descriptor is not a stream socket"
            );
            ensure!(
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0,
                "cannot protect installer event descriptor"
            );
            let stream = UnixStream::from_raw_fd(fd);
            stream
                .peer_addr()
                .context("installer event socket is not connected")?;
            Ok(Self::new(Box::new(stream.try_clone()?), Box::new(stream)))
        }
    }
    #[cfg(windows)]
    pub fn stdio() -> Self {
        Self::new(Box::new(std::io::stdin()), Box::new(std::io::stdout()))
    }
    fn send(&mut self, value: &impl Serialize) -> Result<()> {
        ensure!(!self.poisoned, "installer interface stopped");
        self.poisoned = true;
        let mut bytes = Zeroizing::new(serde_json::to_vec(value)?);
        ensure!(
            bytes.len() < MAX_FRAME,
            "installer event exceeds frame bound"
        );
        bytes.push(b'\n');
        self.output
            .write_all(&bytes)
            .context("installer interface disconnected")?;
        self.output.flush()?;
        self.poisoned = false;
        Ok(())
    }
    fn state(&mut self) -> Result<()> {
        // State contains display text only; passwords never enter it.
        self.send(&serde_json::to_value(&self.state)?)
    }
    pub fn set_steps(&mut self, steps: Vec<String>) -> Result<()> {
        ensure!(
            !steps.is_empty() && steps.len() <= 32,
            "invalid installer steps"
        );
        self.clear_progress();
        self.state.steps = steps;
        self.state.step = 0;
        self.state()
    }
    pub fn set_log_path(&mut self, path: &str) -> Result<()> {
        self.state.log_path = path.into();
        self.state()
    }
    /// Name the installer and selected OS releases on screen. Display text only; bounded so a
    /// malformed configuration cannot flood the channel.
    pub fn set_version(&mut self, version: &str) -> Result<()> {
        ensure!(
            !version.is_empty() && version.len() <= 272,
            "invalid installer version"
        );
        self.state.version = version.into();
        self.state()
    }
    pub fn progress(&mut self, phase: usize, label: &str, done: u64, total: u64) -> Result<()> {
        self.progress_with_unit(phase, label, done, total, ProgressUnit::Bytes)
    }
    pub fn progress_with_unit(
        &mut self,
        phase: usize,
        label: &str,
        done: u64,
        total: u64,
        unit: ProgressUnit,
    ) -> Result<()> {
        self.progress_at(phase, label, done, total, unit, Instant::now())
    }
    fn clear_progress(&mut self) {
        self.rate_sample = None;
        self.state.measurement = None;
    }
    fn progress_at(
        &mut self,
        phase: usize,
        label: &str,
        done: u64,
        total: u64,
        unit: ProgressUnit,
        now: Instant,
    ) -> Result<()> {
        ensure!(
            phase < self.state.steps.len() && done <= total,
            "invalid installer progress"
        );
        let mut rate = 0.0;
        if unit == ProgressUnit::Bytes && total > 0 {
            let reset = self.rate_sample.as_ref().is_none_or(|sample| {
                sample.phase != phase
                    || sample.label != label
                    || sample.total != total
                    || done < sample.last_done
                    || done == 0
            });
            if reset {
                self.rate_sample = Some(RateSample {
                    phase,
                    label: label.into(),
                    total,
                    done,
                    last_done: done,
                    at: now,
                    rate: 0.0,
                });
            } else if let Some(sample) = &mut self.rate_sample {
                let elapsed = now.saturating_duration_since(sample.at).as_secs_f64();
                // Avoid unstable sub-millisecond samples while still reporting the final chunk.
                if elapsed >= 0.25 || (done == total && elapsed > 0.0) {
                    sample.rate = (done - sample.done) as f64 / elapsed;
                    sample.done = done;
                    sample.at = now;
                }
                sample.last_done = done;
                rate = sample.rate;
            }
        } else {
            self.rate_sample = None;
        }
        self.state.step = phase;
        self.state.detail = label.into();
        self.state.measurement = (total > 0).then_some((done, total, rate));
        self.state.measurement_unit = unit;
        self.state()
    }
    fn request(
        &mut self,
        title: &str,
        body: &str,
        kind: &str,
        options: serde_json::Value,
    ) -> Result<Zeroizing<String>> {
        self.clear_progress();
        self.state.detail = body.into();
        self.state.action.clear();
        self.state()?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("prompt sequence exhausted")?;
        self.send(&serde_json::json!({"event":"prompt", "id":self.sequence,
            "title":title, "kind":kind, "options":options}))?;
        self.poisoned = true;
        let mut bytes = Zeroizing::new(Vec::new());
        let count = self
            .input
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        ensure!(
            count > 0 && count <= MAX_FRAME && bytes.last() == Some(&b'\n'),
            "installer reply interrupted or oversized"
        );
        let mut reply: Reply = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid installer reply"))?;
        ensure!(
            reply.id == self.sequence,
            "installer prompt sequence differs"
        );
        ensure!(
            reply.cancel.is_none() && reply.value.is_some(),
            "installer cancelled"
        );
        let answer = Zeroizing::new(reply.value.take().unwrap());
        ensure!(
            answer.len() <= 4096 && !answer.chars().any(char::is_control),
            "invalid installer input"
        );
        self.poisoned = false;
        Ok(answer)
    }
    pub fn choose(&mut self, title: &str, body: &str, options: &[Choice]) -> Result<usize> {
        ensure!(
            !options.is_empty() && options.len() <= 128,
            "invalid installer choices"
        );
        let values: Vec<_> = options
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                serde_json::json!({"value":index.to_string(), "label":choice.label,
                              "detail":choice.detail})
            })
            .collect();
        let value = self.request(title, body, "choice", serde_json::json!(values))?;
        let selected = value.parse::<usize>().ok();
        if !selected.is_some_and(|n| n < options.len() && n.to_string() == *value) {
            self.poisoned = true;
            anyhow::bail!("invalid installer selection");
        }
        Ok(selected.unwrap())
    }
    pub fn input(&mut self, title: &str, body: &str, secret: bool) -> Result<Zeroizing<String>> {
        self.request(
            title,
            body,
            if secret { "password" } else { "text" },
            serde_json::json!([]),
        )
    }
    pub fn error(&mut self, message: &str) -> Result<()> {
        self.clear_progress();
        self.state.result = "error".into();
        self.state.detail = message.into();
        self.state.action = "Preserve the saved originals and installation log.".into();
        self.state()
    }
    pub fn finish(&mut self, code: i32) -> Result<()> {
        self.clear_progress();
        self.state.result = if code == 0 { "ok" } else { "error" }.into();
        self.state()?;
        self.send(&serde_json::json!({"event":"finished", "code":code}))?;
        self.poisoned = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, io::Cursor, rc::Rc};
    #[derive(Clone)]
    struct Output(Rc<RefCell<Vec<u8>>>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn fixture(reply: &[u8]) -> (Ui, Output) {
        let output = Output(Rc::new(RefCell::new(Vec::new())));
        (
            Ui::new(
                Box::new(Cursor::new(reply.to_vec())),
                Box::new(output.clone()),
            ),
            output,
        )
    }
    #[test]
    fn version_display_preserves_two_maximum_length_release_identities() {
        let (mut ui, output) = fixture(b"");
        let label = format!(
            "{} · OS {}",
            "v1.0.0-".to_owned() + &"a".repeat(121),
            "v2.0.0-".to_owned() + &"b".repeat(121)
        );
        ui.set_version(&label).unwrap();
        let state: serde_json::Value = serde_json::from_slice(&output.0.borrow()).unwrap();
        assert_eq!(state["version"], label);
        assert!(ui.set_version(&"x".repeat(273)).is_err());
    }
    #[test]
    fn transfer_rate_tracks_elapsed_bytes_and_resets_between_operations() {
        use std::time::Duration;
        let (mut ui, output) = fixture(b"");
        ui.set_steps(vec!["Transfer".into(), "Verify".into()])
            .unwrap();
        let now = Instant::now();
        let mib = 1048576;
        let mut update = |phase, label: &str, done, total, millis, unit| {
            ui.progress_at(
                phase,
                label,
                done,
                total,
                unit,
                now + Duration::from_millis(millis),
            )
            .unwrap();
            ui.state.measurement.map(|(_, _, rate)| rate).unwrap_or(0.0)
        };
        assert_eq!(
            update(0, "Write boot", 0, 10 * mib, 0, ProgressUnit::Bytes),
            0.0
        );
        assert_eq!(
            update(0, "Write boot", mib, 10 * mib, 1000, ProgressUnit::Bytes),
            mib as f64
        );
        assert_eq!(
            update(
                0,
                "Write boot",
                3 * mib,
                10 * mib,
                2000,
                ProgressUnit::Bytes
            ),
            2.0 * mib as f64
        );
        assert_eq!(
            update(
                0,
                "Write boot",
                3 * mib,
                10 * mib,
                3000,
                ProgressUnit::Bytes
            ),
            0.0
        );
        assert_eq!(
            update(
                0,
                "Write boot",
                4 * mib,
                10 * mib,
                4000,
                ProgressUnit::Bytes
            ),
            mib as f64
        );
        // New phase/label, even with the same total, must not borrow transfer speed.
        assert_eq!(
            update(1, "Verify boot", mib, 10 * mib, 5000, ProgressUnit::Bytes),
            0.0
        );
        assert_eq!(
            update(
                1,
                "Verify boot",
                2 * mib,
                10 * mib,
                5500,
                ProgressUnit::Bytes
            ),
            2.0 * mib as f64
        );
        // A rewind within the sampling interval starts a fresh operation.
        update(
            1,
            "Verify boot",
            3 * mib,
            10 * mib,
            5600,
            ProgressUnit::Bytes,
        );
        assert_eq!(
            update(
                1,
                "Verify boot",
                2 * mib,
                10 * mib,
                5650,
                ProgressUnit::Bytes
            ),
            0.0
        );
        assert_eq!(
            update(
                1,
                "Verify boot",
                3 * mib,
                20 * mib,
                6000,
                ProgressUnit::Bytes
            ),
            0.0
        );
        assert_eq!(update(1, "Files", 3, 4, 7000, ProgressUnit::Items), 0.0);
        assert_eq!(update(1, "Wait", 1, 120, 8000, ProgressUnit::Seconds), 0.0);
        assert_eq!(update(1, "Unknown", 0, 0, 9000, ProgressUnit::Bytes), 0.0);
        assert!(ui.state.measurement.is_none());
        let frames = String::from_utf8(output.0.borrow().clone()).unwrap();
        assert!(frames.contains("\"measurement_unit\":\"items\""));
        assert!(frames.contains("\"measurement_unit\":\"seconds\""));
    }
    #[test]
    fn prompts_and_errors_clear_transfer_measurements() {
        let (mut ui, _) = fixture(b"{\"id\":1,\"value\":\"0\"}\n");
        ui.progress(0, "Transfer", 1, 10).unwrap();
        ui.input("Continue", "Wait for user", false).unwrap();
        assert!(ui.state.measurement.is_none() && ui.rate_sample.is_none());
        ui.progress(0, "Transfer", 2, 10).unwrap();
        ui.error("Stopped").unwrap();
        assert!(ui.state.measurement.is_none() && ui.rate_sample.is_none());
    }
    #[test]
    fn secret_reply_is_not_echoed_in_events() {
        let (mut ui, output) = fixture(b"{\"id\":1,\"value\":\"private-password\"}\n");
        assert_eq!(
            &*ui.input("Wi-Fi password", "Enter your password", true)
                .unwrap(),
            "private-password"
        );
        let events = String::from_utf8(output.0.borrow().clone()).unwrap();
        assert!(events.contains("\"kind\":\"password\""));
        assert!(!events.contains("private-password"));
    }
    #[test]
    fn malformed_cancelled_or_stale_replies_stop_channel() {
        for reply in [
            b"{\"id\":2,\"value\":\"x\"}\n".as_slice(),
            b"{\"id\":1,\"cancel\":true}\n",
            b"{\"id\":1,\"value\":\"x\"}",
            b"{\"id\":1,\"value\":\"x\",\"extra\":1}\n",
            b"not json\n",
        ] {
            let (mut ui, _) = fixture(reply);
            assert!(ui.input("Prompt", "", false).is_err());
            assert!(ui.finish(0).is_err());
        }
    }
    #[test]
    fn choice_must_be_an_offered_canonical_index() {
        let choices = [Choice {
            label: "Continue".into(),
            detail: String::new(),
        }];
        for value in ["1", "00", "-1"] {
            let (mut ui, _) = fixture(format!("{{\"id\":1,\"value\":\"{value}\"}}\n").as_bytes());
            assert!(ui.choose("Choose", "", &choices).is_err());
            assert!(ui.finish(0).is_err());
        }
        let (mut ui, _) = fixture(b"{\"id\":1,\"value\":\"0\"}\n");
        assert_eq!(ui.choose("Choose", "", &choices).unwrap(), 0);
    }
    #[test]
    fn oversized_reply_and_progress_are_rejected() {
        let (mut ui, _) = fixture(&vec![b'x'; MAX_FRAME + 1]);
        assert!(ui.input("Prompt", "", true).is_err());
        let (mut ui, _) = fixture(b"");
        assert!(ui.progress(1, "wrong phase", 0, 1).is_err());
        assert!(ui.progress(0, "wrong count", 2, 1).is_err());
    }
}
