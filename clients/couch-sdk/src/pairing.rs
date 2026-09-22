//! Protocol 3 (unreleased): pairing, described by a package and drawn by Couch.
//!
//! A package never draws a dialog and never stores a key. It describes one
//! step at a time - press the button, approve on the device, type this many
//! characters - and Couch draws its own headline, collects the input and keeps
//! the key that comes out. The key itself is a [`Credential`]: an opaque JSON
//! object that Couch writes to the connection's private file and hands back on
//! the next [`DeviceClient::connect_with`](crate::DeviceClient::connect_with).
//!
//! Nothing here is reachable with the protocol 3 switch off: only a manifest
//! that declares `pairing` may be sent a pairing request, and only a protocol
//! 3 manifest may declare it.
//!
//! # The shape of a flow
//!
//! ```text
//! host                                   package
//! ---- pair_start {settings}         ->
//!                                    <-  pairing {session, waiting{press_button, 2000}}
//! ---- pair_continue {session}       ->
//!                                    <-  pairing {session, waiting{press_button, 2000}}
//! ---- pair_continue {session}       ->
//!                                    <-  pairing {session, done{credential, summary}}
//! ```
//!
//! Every bound below is checked by the host, and a package that breaks one is
//! answering nonsense: the reply is a protocol error and the child is retired.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Result};

/// The most bytes a credential may serialize to.
pub const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
/// The longest summary or prompt line, in bytes. The same limit a
/// [`Reason`](crate::Reason) has, and for the same reason: it is shown to a
/// person on a small screen.
pub const MAX_PAIR_TEXT: usize = 160;
/// How long a package may make Couch wait between two polls of a prompt that
/// resolves by itself. Zero is legal only while a code is being typed.
pub const MIN_POLL_MS: u32 = 500;
pub const MAX_POLL_MS: u32 = 10_000;
/// The longest code a device may ask a person to type.
pub const MAX_CODE_LENGTH: u8 = 16;
/// The longest a session id may be, and the alphabet it is spelt in.
pub const MAX_SESSION: usize = 64;

/// Whether a string is a session id: 1 to 64 bytes of `[A-Za-z0-9._-]`.
///
/// Deliberately narrower than a resource id: a session never names a path and
/// reaches a URL, so it carries no `/` and no `+`.
pub fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= MAX_SESSION
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Text a package may show a person: short, printable, and not a secret.
fn shown(text: &str) -> bool {
    text.len() <= MAX_PAIR_TEXT && !text.chars().any(char::is_control)
}

/// Truncate to [`MAX_PAIR_TEXT`] bytes on a character boundary and drop
/// control characters, so a constructor can never build a step the host would
/// refuse.
fn clamp_text(text: impl Into<String>) -> String {
    let mut text: String = text.into();
    if text.chars().any(char::is_control) {
        text = text.chars().filter(|c| !c.is_control()).collect();
    }
    if text.len() > MAX_PAIR_TEXT {
        let mut end = MAX_PAIR_TEXT;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// What a device gave Couch to remember: a key, a certificate, a client id.
///
/// An opaque JSON object of at most [`MAX_CREDENTIAL_BYTES`]. Couch never
/// looks inside it, never sends it over HTTP and never exports it; it goes
/// into `connections/<id>/plugin-credential.json` at mode 0600 and comes back
/// to the package on its next configure.
///
/// It prints as `Credential(..)`: the redaction is the whole point, so there
/// is a [`Debug`] and deliberately **no** `Display`, no `AsRef<str>` and no
/// `to_string`. Nothing that formats a value containing one can leak it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Credential(Map<String, Value>);

impl Credential {
    /// The longest a credential may serialize to.
    pub const MAX_BYTES: usize = MAX_CREDENTIAL_BYTES;

    /// A credential from whatever the device handed over.
    ///
    /// [`Error::Invalid`] if it is not a JSON object or does not fit. A
    /// package that needs more than sixteen kilobytes is keeping something
    /// that is not a key.
    pub fn new(value: Value) -> Result<Self> {
        let Value::Object(map) = value else {
            return Err(Error::Invalid);
        };
        let credential = Self(map);
        if !credential.fits() {
            return Err(Error::Invalid);
        }
        Ok(credential)
    }

    /// What is inside, for the package that wrote it. Couch itself never asks.
    pub fn get(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Whether this is within the limit. Checked again on arrival: a
    /// credential can also be deserialized straight off the wire.
    pub fn fits(&self) -> bool {
        self.byte_len().is_some_and(|len| len <= Self::MAX_BYTES)
    }

    /// How many bytes it serializes to, or `None` if it cannot be serialized.
    pub fn byte_len(&self) -> Option<usize> {
        serde_json::to_vec(&self.0).ok().map(|bytes| bytes.len())
    }
}

/// `Credential(..)`, whatever it holds. See the type's documentation.
impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Credential(..)")
    }
}

/// Which characters a code is spelt in, so Couch can draw the right keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeAlphabet {
    Digits,
    Hex,
    Alphanumeric,
}

impl CodeAlphabet {
    /// Whether every character of `code` belongs to this alphabet.
    pub fn accepts(self, code: &str) -> bool {
        code.chars().all(|c| match self {
            Self::Digits => c.is_ascii_digit(),
            Self::Hex => c.is_ascii_hexdigit(),
            Self::Alphanumeric => c.is_ascii_alphanumeric(),
        })
    }
}

/// What the person has to do next. Couch writes its own headline for each of
/// these; `message` is the package's one line under it, and may be left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairPrompt {
    /// "Press the button on the device."
    PressButton {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// "Approve on the device."
    ApproveOnDevice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// "Enter the code shown on the device."
    EnterCode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// How many characters, 1 to [`MAX_CODE_LENGTH`].
        length: u8,
        alphabet: CodeAlphabet,
    },
}

impl PairPrompt {
    pub fn press_button() -> Self {
        Self::PressButton { message: None }
    }
    pub fn approve_on_device() -> Self {
        Self::ApproveOnDevice { message: None }
    }
    /// A code prompt. `length` is clamped into 1..=[`MAX_CODE_LENGTH`], so an
    /// author cannot ask for a code no dialog could collect.
    pub fn enter_code(length: u8, alphabet: CodeAlphabet) -> Self {
        Self::EnterCode {
            message: None,
            length: length.clamp(1, MAX_CODE_LENGTH),
            alphabet,
        }
    }
    /// The package's own line under Couch's headline. Clamped to
    /// [`MAX_PAIR_TEXT`] printable bytes.
    pub fn saying(mut self, message: impl Into<String>) -> Self {
        let text = clamp_text(message);
        match &mut self {
            Self::PressButton { message }
            | Self::ApproveOnDevice { message }
            | Self::EnterCode { message, .. } => *message = Some(text),
        }
        self
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::PressButton { message }
            | Self::ApproveOnDevice { message }
            | Self::EnterCode { message, .. } => message.as_deref(),
        }
    }

    /// Whether this prompt collects something the person types.
    pub fn is_code(&self) -> bool {
        matches!(self, Self::EnterCode { .. })
    }

    /// Whether the input Couch collected is one this prompt asked for. The
    /// host checks this **before** sending, so a mistyped code costs no round
    /// trip and a prompt that asked for nothing is never given anything.
    pub fn accepts(&self, input: &PairInput) -> bool {
        match (self, input) {
            (
                Self::EnterCode {
                    length, alphabet, ..
                },
                PairInput::Code { code },
            ) => code.chars().count() == usize::from(*length) && alphabet.accepts(code),
            _ => false,
        }
    }

    pub fn is_well_formed(&self) -> bool {
        self.message().is_none_or(shown)
            && match self {
                Self::EnterCode { length, .. } => (1..=MAX_CODE_LENGTH).contains(length),
                _ => true,
            }
    }
}

/// What Couch collected from the person and is handing back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairInput {
    Code { code: String },
}

impl PairInput {
    pub fn code(code: impl Into<String>) -> Self {
        Self::Code { code: code.into() }
    }
    /// Short and printable, whatever prompt it answers.
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Code { code } => {
                let chars = code.chars().count();
                chars >= 1
                    && chars <= usize::from(MAX_CODE_LENGTH)
                    && code.chars().all(|c| c.is_ascii_graphic())
            }
        }
    }
}

/// Why a pairing attempt ended without a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairFailure {
    /// The device could not be reached at all.
    Unreachable,
    /// The device said no: the button was not pressed, the request was denied.
    Refused,
    /// The code the person typed was wrong. Couch offers "Try again".
    WrongCode,
    /// The device's own window closed before the person finished.
    TimedOut,
    /// These settings cannot be paired this way.
    Unsupported,
}

/// One step of a pairing conversation, as the package describes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairStep {
    /// Still going. Couch shows `prompt` and comes back in `poll_after_ms`
    /// milliseconds, which is 0 only while a code is being typed (Couch then
    /// comes back when the person submits it) and [`MIN_POLL_MS`] to
    /// [`MAX_POLL_MS`] otherwise.
    Waiting {
        prompt: PairPrompt,
        poll_after_ms: u32,
    },
    /// Paired. `credential` is what Couch stores, `settings` are the settings
    /// as the device would rather have them (they must still pass the
    /// manifest), and `summary` is one line naming what was paired.
    Done {
        credential: Credential,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<Value>,
        summary: String,
    },
    /// Over, with nothing stored.
    Failed {
        reason: PairFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl PairStep {
    /// A step that waits. `poll_after_ms` is clamped, so an author cannot emit
    /// a step outside the bounds the host enforces: 0 stays 0 only for a code
    /// prompt, and anything else lands in [`MIN_POLL_MS`]..=[`MAX_POLL_MS`].
    pub fn waiting(prompt: PairPrompt, poll_after_ms: u32) -> Self {
        let poll_after_ms = if prompt.is_code() && poll_after_ms == 0 {
            0
        } else {
            poll_after_ms.clamp(MIN_POLL_MS, MAX_POLL_MS)
        };
        Self::Waiting {
            prompt,
            poll_after_ms,
        }
    }
    /// The end of a successful flow. `summary` is clamped to
    /// [`MAX_PAIR_TEXT`] printable bytes.
    pub fn done(credential: Credential, summary: impl Into<String>) -> Self {
        Self::Done {
            credential,
            settings: None,
            summary: clamp_text(summary),
        }
    }
    /// Settings the device would rather Couch saved - a corrected port, the
    /// address it answered on. Ignored unless this is a [`PairStep::Done`],
    /// and refused by the host unless the manifest accepts them.
    pub fn with_settings(mut self, value: Value) -> Self {
        if let Self::Done { settings, .. } = &mut self {
            *settings = Some(value);
        }
        self
    }
    pub fn failed(reason: PairFailure) -> Self {
        Self::Failed {
            reason,
            message: None,
        }
    }
    /// The package's own words for a failure, clamped like every other line.
    pub fn because(mut self, text: impl Into<String>) -> Self {
        if let Self::Failed { message, .. } = &mut self {
            *message = Some(clamp_text(text));
        }
        self
    }

    /// Whether the flow is over, either way. The host drops the session then,
    /// and so does `serve`.
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Waiting { .. })
    }

    /// Every bound a step carries with it. `Done.settings` is the one thing
    /// this cannot answer: only the manifest knows, and `couch-plugin` asks it.
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Waiting {
                prompt,
                poll_after_ms,
            } => {
                prompt.is_well_formed()
                    && match *poll_after_ms {
                        0 => prompt.is_code(),
                        ms => (MIN_POLL_MS..=MAX_POLL_MS).contains(&ms),
                    }
            }
            Self::Done {
                credential,
                summary,
                ..
            } => credential.fits() && shown(summary),
            Self::Failed { message, .. } => message.as_deref().is_none_or(shown),
        }
    }
}

/// One pairing conversation, owned by the package.
///
/// `serve` holds at most one of these at a time and drops it as soon as a step
/// is final or the host cancels. A blocking wait on the device belongs inside
/// [`PairFlow::step`], which must return well inside the host's twelve second
/// request timeout: describe a wait with [`PairStep::waiting`] rather than
/// sleeping through it.
pub trait PairFlow: Send {
    /// The next step. `input` is what Couch collected for the previous
    /// prompt, and is `None` for the first step and for every poll of a
    /// prompt that resolves by itself.
    fn step(&mut self, input: Option<PairInput>) -> Result<PairStep>;

    /// The person closed the dialog, or the deadline passed. Tell the device
    /// if it wants telling; nothing is stored either way.
    fn cancel(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_credential_is_a_bounded_object_that_never_prints_itself() {
        let credential = Credential::new(json!({"key": "s3cret", "id": 7})).unwrap();
        assert_eq!(credential.get()["key"], json!("s3cret"));
        assert!(credential.fits());
        // The redaction, which is the whole point of the type.
        assert_eq!(format!("{credential:?}"), "Credential(..)");
        assert!(!format!("{credential:?}").contains("s3cret"));
        // ...and through every type that can hold one.
        let step = PairStep::done(credential.clone(), "Paired with the lamp");
        assert!(!format!("{step:?}").contains("s3cret"));
        assert!(format!("{step:?}").contains("Credential(..)"));

        assert_eq!(Credential::new(json!([1, 2])), Err(Error::Invalid));
        assert_eq!(Credential::new(json!("key")), Err(Error::Invalid));
        assert_eq!(Credential::new(json!(null)), Err(Error::Invalid));
        let big = json!({"key": "a".repeat(Credential::MAX_BYTES)});
        assert_eq!(Credential::new(big.clone()), Err(Error::Invalid));
        // Straight off the wire there is no constructor to refuse it, so the
        // host asks the value itself.
        let smuggled: Credential = serde_json::from_value(big).unwrap();
        assert!(!smuggled.fits());
        assert!(smuggled.byte_len().unwrap() > Credential::MAX_BYTES);
        // The bytes are the object's own: nothing wraps it.
        assert_eq!(
            serde_json::to_string(&Credential::new(json!({"a": 1})).unwrap()).unwrap(),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn a_constructor_cannot_build_a_step_the_host_would_refuse() {
        for (given, expected) in [(0, MIN_POLL_MS), (1, MIN_POLL_MS), (60_000, MAX_POLL_MS)] {
            let step = PairStep::waiting(PairPrompt::press_button(), given);
            assert_eq!(
                step,
                PairStep::Waiting {
                    prompt: PairPrompt::press_button(),
                    poll_after_ms: expected
                }
            );
            assert!(step.is_well_formed());
        }
        // Only a code prompt may say "come back when they have typed it".
        let typed = PairStep::waiting(PairPrompt::enter_code(6, CodeAlphabet::Hex), 0);
        assert_eq!(
            typed,
            PairStep::Waiting {
                prompt: PairPrompt::EnterCode {
                    message: None,
                    length: 6,
                    alphabet: CodeAlphabet::Hex
                },
                poll_after_ms: 0
            }
        );
        assert!(typed.is_well_formed());
        assert!(!PairStep::Waiting {
            prompt: PairPrompt::press_button(),
            poll_after_ms: 0
        }
        .is_well_formed());
        assert!(!PairStep::Waiting {
            prompt: PairPrompt::press_button(),
            poll_after_ms: 499
        }
        .is_well_formed());
        assert!(!PairStep::Waiting {
            prompt: PairPrompt::press_button(),
            poll_after_ms: MAX_POLL_MS + 1
        }
        .is_well_formed());
        // Lengths and text are clamped too.
        assert_eq!(
            PairPrompt::enter_code(0, CodeAlphabet::Digits),
            PairPrompt::EnterCode {
                message: None,
                length: 1,
                alphabet: CodeAlphabet::Digits
            }
        );
        assert_eq!(
            PairPrompt::enter_code(99, CodeAlphabet::Digits),
            PairPrompt::EnterCode {
                message: None,
                length: MAX_CODE_LENGTH,
                alphabet: CodeAlphabet::Digits
            }
        );
        let long = PairPrompt::press_button().saying("a".repeat(400));
        assert_eq!(long.message().unwrap().len(), MAX_PAIR_TEXT);
        assert!(long.is_well_formed());
        let broken = PairStep::failed(PairFailure::Refused).because("two\nlines\u{7}");
        assert_eq!(
            broken,
            PairStep::Failed {
                reason: PairFailure::Refused,
                message: Some("twolines".into())
            }
        );
        assert!(broken.is_well_formed());
        let credential = Credential::new(json!({"k": 1})).unwrap();
        assert_eq!(
            PairStep::done(credential.clone(), "x".repeat(400)),
            PairStep::Done {
                credential,
                settings: None,
                summary: "x".repeat(MAX_PAIR_TEXT)
            }
        );
    }

    #[test]
    fn a_code_is_checked_against_the_prompt_that_asked_for_it() {
        let hex = PairPrompt::enter_code(6, CodeAlphabet::Hex);
        assert!(hex.accepts(&PairInput::code("A1b2C3")));
        assert!(!hex.accepts(&PairInput::code("A1b2C")), "too short");
        assert!(!hex.accepts(&PairInput::code("A1b2C33")), "too long");
        assert!(!hex.accepts(&PairInput::code("A1b2Cg")), "not hex");
        let digits = PairPrompt::enter_code(4, CodeAlphabet::Digits);
        assert!(digits.accepts(&PairInput::code("0417")));
        assert!(!digits.accepts(&PairInput::code("04a7")));
        // A prompt that asked for nothing takes nothing.
        assert!(!PairPrompt::press_button().accepts(&PairInput::code("0417")));
        assert!(!PairPrompt::approve_on_device().accepts(&PairInput::code("0417")));

        assert!(PairInput::code("0417").is_well_formed());
        assert!(!PairInput::code("").is_well_formed());
        assert!(!PairInput::code("a".repeat(17)).is_well_formed());
        assert!(!PairInput::code("04\n17").is_well_formed());
        assert!(!PairInput::code("04 17").is_well_formed());
    }

    #[test]
    fn a_session_id_is_short_and_carries_no_path() {
        assert!(valid_session("p1"));
        assert!(valid_session("a1b2.c-d_e"));
        assert!(valid_session(&"a".repeat(MAX_SESSION)));
        for bad in [
            "",
            "a/b",
            "a b",
            "a+b",
            "a\nb",
            &"a".repeat(MAX_SESSION + 1),
        ] {
            assert!(!valid_session(bad), "{bad}");
        }
    }

    #[test]
    fn the_wire_shape_is_the_one_the_plan_fixed() {
        assert_eq!(
            serde_json::to_string(&PairStep::waiting(
                PairPrompt::press_button().saying("Press pair on the bridge"),
                2000
            ))
            .unwrap(),
            r#"{"step":"waiting","prompt":{"kind":"press_button","message":"Press pair on the bridge"},"poll_after_ms":2000}"#
        );
        assert_eq!(
            serde_json::to_string(&PairStep::waiting(
                PairPrompt::enter_code(4, CodeAlphabet::Digits),
                0
            ))
            .unwrap(),
            r#"{"step":"waiting","prompt":{"kind":"enter_code","length":4,"alphabet":"digits"},"poll_after_ms":0}"#
        );
        assert_eq!(
            serde_json::to_string(&PairStep::done(
                Credential::new(json!({"key": "k"})).unwrap(),
                "Paired with Hall bridge"
            ))
            .unwrap(),
            r#"{"step":"done","credential":{"key":"k"},"summary":"Paired with Hall bridge"}"#
        );
        assert_eq!(
            serde_json::to_string(&PairStep::failed(PairFailure::WrongCode)).unwrap(),
            r#"{"step":"failed","reason":"wrong_code"}"#
        );
        assert_eq!(
            serde_json::to_string(&PairInput::code("0417")).unwrap(),
            r#"{"kind":"code","code":"0417"}"#
        );
        // Unknown fields and unknown variants are refused in both.
        for bad in [
            r#"{"step":"waiting","prompt":{"kind":"press_button"},"poll_after_ms":500,"extra":1}"#,
            r#"{"step":"queued","poll_after_ms":500}"#,
            r#"{"step":"waiting","prompt":{"kind":"scan_qr"},"poll_after_ms":500}"#,
        ] {
            assert!(serde_json::from_str::<PairStep>(bad).is_err(), "{bad}");
        }
    }
}
