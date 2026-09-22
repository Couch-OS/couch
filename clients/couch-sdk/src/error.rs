//! One error vocabulary, chosen to match what the broker already reports.
//!
//! `couch_control::Error` has five variants and the daemon and GUI both
//! render them to the user. A client that invents its own set has to be
//! translated twice, so this enum is those five plus the two mistakes a new
//! client makes most often - asking for a function it never declared, and
//! being handed settings that cannot address a device.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Why a request failed, in words Couch can show or act on.
///
/// Protocol 3, which is unreleased and switched off: a reason leaves a package
/// only when its manifest says protocol 3, and no shipped Couch accepts such a
/// manifest. A protocol 1 or 2 package that returns one from its client sends
/// exactly the bytes it always sent: the code alone.
///
/// The text is for the person holding the remote. Never put a credential, a
/// path or a stack trace in it. Keep it to [`Reason::MAX_TEXT`] bytes with no
/// control characters; Couch refuses a longer one and retires the package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reason {
    /// One setting is wrong. `field` is the id of a setting the manifest
    /// declares, so the form can mark it.
    InvalidSetting { field: String, text: String },
    /// Anything else worth saying.
    Message { text: String },
}

impl Reason {
    /// The longest text Couch accepts, in bytes.
    pub const MAX_TEXT: usize = 160;

    pub fn text(&self) -> &str {
        match self {
            Self::InvalidSetting { text, .. } | Self::Message { text } => text,
        }
    }

    /// The setting this reason blames, if it blames one.
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::InvalidSetting { field, .. } => Some(field),
            Self::Message { .. } => None,
        }
    }

    /// Short enough and printable. Whether `field` names a declared setting is
    /// the manifest's question, asked by `couch-plugin`.
    pub fn is_well_formed(&self) -> bool {
        let text = self.text();
        text.len() <= Self::MAX_TEXT
            && !text.chars().any(char::is_control)
            && self
                .field()
                .is_none_or(|field| !field.is_empty() && field.len() <= 64)
    }
}

/// Why a request did not produce a result.
///
/// The mapping into the broker's type is total and lossless enough to be worth
/// writing down, because the wiring step in `couch-control` needs it:
///
/// | `couch_sdk::Error` | `couch_control::Error` |
/// |---|---|
/// | [`Error::Protocol`] | `Protocol` |
/// | [`Error::Transport`] | `Transport` |
/// | [`Error::Timeout`] | `Timeout` |
/// | [`Error::Rejected`] | `Rejected` |
/// | [`Error::Unsupported`] | `Remote(_)` |
/// | [`Error::Invalid`] | `Remote(_)` |
/// | [`Error::Remote`] | `Remote(_)` |
///
/// The broker's type lives in `clients/couch-control/src/lib.rs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device answered, and the answer did not parse or did not confirm
    /// the request. Never retried: a device that replies with nonsense to one
    /// command replies with nonsense to the next one too.
    Protocol,
    /// The device could not be reached, or the connection dropped mid-request.
    Transport,
    /// No reply before the deadline. This does **not** prove the command was
    /// not executed, which is why nothing in this repository retries it.
    Timeout,
    /// The device understood the request and refused it - a revoked pairing, a
    /// locked input, an unsupported app.
    Rejected,
    /// The client does not implement this function. Returned by
    /// [`DeviceClient::command`](crate::DeviceClient::command) before any I/O
    /// when the requested function is absent from
    /// [`DeviceClient::capabilities`](crate::DeviceClient::capabilities).
    Unsupported,
    /// The settings cannot address a device: empty host, port 0, a URL with a
    /// control character in it. Raised before connecting.
    Invalid,
    /// Anything with a message worth showing. The daemon puts this string in a
    /// 502 body and the GUI puts it on screen, so write it for the person
    /// holding the remote, not for a log reader.
    Remote(String),
    /// The device no longer accepts this remote's pairing, and only pairing
    /// again will fix it. Protocol 3: a protocol 1 or 2 package reports it to
    /// Couch as [`Error::Rejected`], which is what it was before it had a name.
    Unpaired,
    /// Another error, with a [`Reason`]. Build it with [`Error::because`].
    Explained { error: Box<Error>, reason: Reason },
}

impl Error {
    /// Attach a reason. Explaining an error twice keeps the newer reason.
    pub fn because(self, reason: Reason) -> Self {
        Self::Explained {
            error: Box::new(self.into_code()),
            reason,
        }
    }

    /// The error without its reason.
    pub fn code(&self) -> &Error {
        match self {
            Self::Explained { error, .. } => error.code(),
            other => other,
        }
    }

    fn into_code(self) -> Error {
        match self {
            Self::Explained { error, .. } => error.into_code(),
            other => other,
        }
    }

    /// The reason, if one was attached.
    pub fn reason(&self) -> Option<&Reason> {
        match self {
            Self::Explained { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// A short message safe to show a user. Never include a credential here:
    /// these strings reach the browser UI and the device screen.
    pub fn message(&self) -> &str {
        match self {
            Self::Protocol => "The device sent a response this client could not use",
            Self::Transport => "The device could not be reached",
            Self::Timeout => "The device did not reply before the deadline",
            Self::Rejected => "The device refused the request",
            Self::Unsupported => "This device does not support that function",
            Self::Invalid => "These connection settings are incomplete",
            Self::Remote(message) => message,
            Self::Unpaired => "This device needs to be paired again",
            Self::Explained { reason, .. } => reason.text(),
        }
    }

    /// Whether a caller may reasonably try the *same* request again later.
    ///
    /// False for [`Error::Timeout`] on purpose. A lost reply does not prove a
    /// lost command, and repeating a power or input command that did in fact
    /// arrive is worse than reporting the failure.
    pub fn retryable(&self) -> bool {
        matches!(self.code(), Self::Transport)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match e.kind() {
            TimedOut | WouldBlock => Self::Timeout,
            PermissionDenied => Self::Remote("Permission denied while accessing the device".into()),
            _ => Self::Transport,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Protocol
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_kinds_map_to_the_variant_the_user_sees_and_timeouts_are_not_retryable() {
        let timeout: Error = std::io::Error::from(std::io::ErrorKind::TimedOut).into();
        assert_eq!(timeout, Error::Timeout);
        assert!(!timeout.retryable());
        let refused: Error = std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into();
        assert_eq!(refused, Error::Transport);
        assert!(refused.retryable());
        let denied: Error = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/home/couch/connections/living-room/secret.json",
        )
        .into();
        assert_eq!(
            denied,
            Error::Remote("Permission denied while accessing the device".into()),
            "a user-facing error must not reveal a credential path"
        );
        let bad: Error = serde_json::from_str::<u32>("{").unwrap_err().into();
        assert_eq!(bad, Error::Protocol);
        assert_eq!(
            Error::Remote("Pair this TV first".into()).to_string(),
            "Pair this TV first"
        );
    }

    #[test]
    fn a_reason_rides_on_an_error_without_changing_what_the_error_is() {
        let reason = Reason::InvalidSetting {
            field: "port".into(),
            text: "The port must not be 0".into(),
        };
        let explained = Error::Invalid.because(reason.clone());
        assert_eq!(explained.code(), &Error::Invalid);
        assert_eq!(explained.reason(), Some(&reason));
        assert_eq!(explained.to_string(), "The port must not be 0");
        assert_eq!(Error::Invalid.reason(), None);
        assert!(Error::Transport.because(reason.clone()).retryable());
        assert!(!Error::Unpaired.retryable());
        let newer = Reason::Message {
            text: "Pair again".into(),
        };
        let twice = explained.because(newer.clone());
        assert_eq!(twice.code(), &Error::Invalid);
        assert_eq!(twice.reason(), Some(&newer));
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            r#"{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}"#
        );
        assert_eq!(
            serde_json::to_string(&newer).unwrap(),
            r#"{"kind":"message","text":"Pair again"}"#
        );
        for unknown in [
            r#"{"kind":"message","text":"a","extra":1}"#,
            r#"{"kind":"link","text":"a"}"#,
            r#"{"text":"a"}"#,
        ] {
            assert!(
                serde_json::from_str::<Reason>(unknown).is_err(),
                "{unknown}"
            );
        }
        assert!(Reason::Message {
            text: "a".repeat(Reason::MAX_TEXT)
        }
        .is_well_formed());
        for text in ["a".repeat(Reason::MAX_TEXT + 1), "line\nbreak".into()] {
            assert!(!Reason::Message { text }.is_well_formed());
        }
        assert!(!Reason::InvalidSetting {
            field: String::new(),
            text: "a".into()
        }
        .is_well_formed());
    }
}
