use crate::Manifest;
use couch_sdk::{KeyPhase, Reason, Selectable, Status};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u32 = 2;
/// Protocol 3 is unreleased. Its wire types are compiled in so that one host
/// can be tested against both, but nothing accepts a protocol 3 manifest
/// unless the `protocol-3-preview` feature is on, and no shipped crate turns
/// it on.
pub const NEXT_PROTOCOL_VERSION: u32 = 3;
/// The newest manifest protocol this build admits. The only thing the
/// `protocol-3-preview` feature changes. [`PROTOCOL_VERSION`] is what a release
/// supports and is deliberately not feature-dependent.
pub const fn accepted_protocol_version() -> u32 {
    if cfg!(feature = "protocol-3-preview") {
        NEXT_PROTOCOL_VERSION
    } else {
        PROTOCOL_VERSION
    }
}
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    Invalid,
    Unsupported,
    Incompatible,
    Protocol,
    Transport,
    Timeout,
    Busy,
    Expired,
    Rejected,
    /// Protocol 3. The device wants pairing again. Only a package whose
    /// manifest says protocol 3 may send it; from any other it is a protocol
    /// error.
    Unpaired,
}
pub type Result<T> = std::result::Result<T, Error>;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Invalid integration settings or package",
            Self::Unsupported => "The integration does not support this request",
            Self::Incompatible => "The integration requires a different protocol version",
            Self::Protocol => "The integration sent an invalid response",
            Self::Transport => "The integration could not be reached",
            Self::Timeout => "The integration did not reply before the deadline",
            Self::Busy => "The integration request queue is full",
            Self::Expired => "The integration request expired before execution",
            Self::Rejected => "The device refused the request",
            Self::Unpaired => "The device needs to be paired again",
        })
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            Self::Timeout
        } else {
            Self::Transport
        }
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Protocol
    }
}
impl From<couch_sdk::Error> for Error {
    fn from(e: couch_sdk::Error) -> Self {
        match e {
            couch_sdk::Error::Invalid => Self::Invalid,
            couch_sdk::Error::Unsupported => Self::Unsupported,
            couch_sdk::Error::Protocol => Self::Protocol,
            couch_sdk::Error::Timeout => Self::Timeout,
            couch_sdk::Error::Transport => Self::Transport,
            couch_sdk::Error::Rejected | couch_sdk::Error::Remote(_) => Self::Rejected,
            couch_sdk::Error::Unpaired => Self::Unpaired,
            couch_sdk::Error::Explained { error, .. } => (*error).into(),
        }
    }
}

/// An [`Error`] and, from a protocol 3 package, why. [`Error`] stays a plain
/// `Copy` code so every existing signature keeps its meaning; the reason
/// travels only through the `_detailed` calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: Error,
    pub reason: Option<Reason>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            Some(reason) if !reason.text().is_empty() => f.write_str(reason.text()),
            _ => self.code.fmt(f),
        }
    }
}
impl std::error::Error for Failure {}
impl From<Error> for Failure {
    fn from(code: Error) -> Self {
        Self { code, reason: None }
    }
}
impl From<couch_sdk::Error> for Failure {
    fn from(error: couch_sdk::Error) -> Self {
        let reason = error.reason().cloned();
        Self {
            code: error.into(),
            reason,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello {
        protocol_version: u32,
    },
    Configure {
        settings: serde_json::Value,
    },
    Command {
        function: String,
        /// Protocol 3. A tap is never written, so a protocol 1 or 2 package,
        /// which refuses unknown fields, receives the bytes it always did; and
        /// a frame without it reads as a tap.
        #[serde(default, skip_serializing_if = "KeyPhase::is_tap")]
        phase: KeyPhase,
    },
    Action {
        action: couch_sdk::TypedAction,
    },
    Status,
    Inputs,
}
impl Request {
    /// A command as every protocol sends it: a tap.
    pub fn command(function: impl Into<String>) -> Self {
        Self::key(function, KeyPhase::Tap)
    }
    /// A command that says how the key was pressed. The host sends the phase
    /// only to a protocol 3 package and downgrades it to a tap for the rest.
    pub fn key(function: impl Into<String>, phase: KeyPhase) -> Self {
        Self::Command {
            function: function.into(),
            phase,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Hello {
        manifest: Manifest,
    },
    Ok,
    Status {
        status: Status,
    },
    Inputs {
        inputs: Vec<Selectable>,
    },
    Error {
        code: Error,
        /// Protocol 3. Absent from every protocol 1 and 2 error, in both
        /// directions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<Reason>,
    },
}
impl Response {
    pub fn error(failure: impl Into<Failure>) -> Self {
        let Failure { code, reason } = failure.into();
        Self::Error { code, reason }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope<T> {
    pub id: u64,
    pub body: T,
}

pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let size = u32::from_be_bytes(length) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, frame: &T) -> Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Error::Protocol);
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}
