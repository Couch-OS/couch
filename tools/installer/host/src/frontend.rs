//! Private, bounded Ratatui event channel. No secrets are passed through argv.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use zeroize::{Zeroize, Zeroizing};

const MAX_FRAME: usize = 65536;

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
    measurement: Option<(u64, u64, f64)>,
}

pub struct Ui {
    input: BufReader<Box<dyn Read>>,
    output: Box<dyn Write>,
    sequence: u64,
    poisoned: bool,
    state: State,
}
impl Ui {
    pub fn new(input: Box<dyn Read>, output: Box<dyn Write>) -> Self {
        Self {
            input: BufReader::new(input),
            output,
            sequence: 0,
            poisoned: false,
            state: State {
                event: "state",
                step: 0,
                steps: vec!["Prepare".into()],
                detail: String::new(),
                action: String::new(),
                result: "active".into(),
                log_path: String::new(),
                measurement: None,
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
        self.state.steps = steps;
        self.state.step = 0;
        self.state()
    }
    pub fn set_log_path(&mut self, path: &str) -> Result<()> {
        self.state.log_path = path.into();
        self.state()
    }
    pub fn progress(&mut self, phase: usize, label: &str, done: u64, total: u64) -> Result<()> {
        ensure!(
            phase < self.state.steps.len() && done <= total,
            "invalid installer progress"
        );
        self.state.step = phase;
        self.state.detail = label.into();
        self.state.measurement = (total > 0).then_some((done, total, 0.0));
        self.state()
    }
    fn request(
        &mut self,
        title: &str,
        body: &str,
        kind: &str,
        options: serde_json::Value,
    ) -> Result<Zeroizing<String>> {
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
        self.state.result = "error".into();
        self.state.detail = message.into();
        self.state.action = "Preserve the saved originals and installation log.".into();
        self.state()
    }
    pub fn finish(&mut self, code: i32) -> Result<()> {
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
