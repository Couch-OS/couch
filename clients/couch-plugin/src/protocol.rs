use crate::Manifest;
use couch_sdk::{Selectable, Status};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u32 = 1;
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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello { protocol_version: u32 },
    Configure { settings: serde_json::Value },
    Command { function: String },
    Status,
    Inputs,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Hello { manifest: Manifest },
    Ok,
    Status { status: Status },
    Inputs { inputs: Vec<Selectable> },
    Error { code: Error },
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
