use crate::Manifest;
use couch_sdk::{Credential, KeyPhase, PairInput, PairStep, Reason, Selectable, Status};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};

/// The current package contract. Protocol 4 adds camera children and their
/// bounded H264 side channel while preserving protocols 1-3 byte for byte.
pub const PROTOCOL_VERSION: u32 = 4;
/// The protocol generation that introduced children, typed actions, pairing,
/// and host-owned credentials. Keep this name for source compatibility with
/// packages developed while protocol 3 was in preview.
pub const NEXT_PROTOCOL_VERSION: u32 = 3;
/// The newest manifest protocol this build admits.
pub const fn accepted_protocol_version() -> u32 {
    PROTOCOL_VERSION
}
pub const MAX_FRAME: usize = 64 * 1024;
/// Protocol 4: largest JPEG the core will assemble from snapshot chunks.
pub const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
/// Protocol 4: largest base64 field in one ordinary control frame. This leaves
/// room for the JSON envelope under [`MAX_FRAME`].
pub const MAX_SNAPSHOT_CHUNK_BASE64: usize = 48 * 1024;
/// The decoded bytes represented by [`MAX_SNAPSHOT_CHUNK_BASE64`].
pub const MAX_SNAPSHOT_CHUNK_BYTES: usize = MAX_SNAPSHOT_CHUNK_BASE64 / 4 * 3;
/// Protocol 4: an explicitly opened live view is always short-lived.
pub const MAX_CAMERA_SECONDS: u8 = 60;

/// The only media codec protocol 4 admits on its binary side channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraCodec {
    H264AnnexB,
}

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello {
        protocol_version: u32,
    },
    Configure {
        settings: serde_json::Value,
        /// Protocol 3. The key Couch is holding for this connection. Absent
        /// whenever there is none, and the gate strips it for any package
        /// below protocol 3 or without `pairing` in its manifest, so a
        /// published package is configured with the bytes it always read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<Credential>,
    },
    Command {
        function: String,
        /// Protocol 3. A tap is never written, so a protocol 1 or 2 package,
        /// which refuses unknown fields, receives the bytes it always did; and
        /// a frame without it reads as a tap.
        #[serde(default, skip_serializing_if = "KeyPhase::is_tap")]
        phase: KeyPhase,
        /// Protocol 3. Which child of the connection this is for. Absent for
        /// the connection itself, which is every request a protocol 1 or 2
        /// package can be sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
    },
    Action {
        action: couch_sdk::TypedAction,
        /// Protocol 3. See [`Request::Command`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
    },
    /// Protocol 3 turned this from a unit variant into a struct variant with
    /// one skipped field. Serde writes an internally tagged struct variant
    /// whose every field is skipped exactly as it writes a unit variant, so
    /// `{"method":"status"}` is still the byte for byte frame a published
    /// package reads, and that package's unit variant still reads it.
    Status {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
    },
    Inputs,
    /// Protocol 3. One page of the children behind this connection, starting
    /// after nothing (`None`) or at a cursor the package gave out.
    Children {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// Protocol 4. Read one bounded chunk of a camera's JPEG snapshot.
    CameraSnapshot {
        resource: String,
        offset: u32,
    },
    /// Protocol 4. Begin one short live view on the inherited media socket.
    CameraOpen {
        resource: String,
    },
    /// Protocol 4. Cancel a live view. Dropping either socket is equivalent.
    CameraClose {
        resource: String,
    },
    /// Protocol 3. Begin a pairing conversation for these settings. It needs
    /// no prior `configure`: pairing is how a connection becomes usable, and
    /// the daemon runs it in a child of its own so the connection that is
    /// already paired keeps working on its old key.
    PairStart {
        settings: serde_json::Value,
        /// The key Couch already holds, for a device that issues a second one
        /// against the first. Stripped by the gate exactly as
        /// [`Request::Configure`]'s is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<Credential>,
    },
    /// Protocol 3. The next step of the conversation the package named,
    /// carrying whatever Couch collected for the previous prompt.
    PairContinue {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<PairInput>,
    },
    /// Protocol 3. The person closed the dialog, or the deadline passed.
    /// Answered `ok`, and nothing is stored.
    PairCancel {
        session: String,
    },
}
impl Request {
    /// Settings, and no key. The builder exists so a later field never breaks
    /// a struct literal again: every caller that has no key writes this, and
    /// its bytes are the ones a published package has always read.
    pub fn configure(settings: serde_json::Value) -> Self {
        Self::Configure {
            settings,
            credential: None,
        }
    }
    /// Settings, and the key Couch is holding. The gate strips the key for
    /// any package that may not be told one.
    pub fn configure_with(settings: serde_json::Value, credential: Option<&Credential>) -> Self {
        Self::Configure {
            settings,
            credential: credential.cloned(),
        }
    }
    /// Protocol 3: begin a pairing conversation.
    pub fn pair_start(settings: serde_json::Value, credential: Option<&Credential>) -> Self {
        Self::PairStart {
            settings,
            credential: credential.cloned(),
        }
    }
    /// Protocol 3: the next step of a conversation already under way.
    pub fn pair_continue(session: impl Into<String>, input: Option<PairInput>) -> Self {
        Self::PairContinue {
            session: session.into(),
            input,
        }
    }
    /// Protocol 3: end one, storing nothing.
    pub fn pair_cancel(session: impl Into<String>) -> Self {
        Self::PairCancel {
            session: session.into(),
        }
    }
    /// The pairing session this request belongs to, if it belongs to one.
    pub fn session(&self) -> Option<&str> {
        match self {
            Self::PairContinue { session, .. } | Self::PairCancel { session } => Some(session),
            _ => None,
        }
    }
    /// Whether this is part of a pairing conversation at all.
    pub fn is_pairing(&self) -> bool {
        matches!(
            self,
            Self::PairStart { .. } | Self::PairContinue { .. } | Self::PairCancel { .. }
        )
    }
    /// A command as every protocol sends it: a tap, to the connection itself.
    pub fn command(function: impl Into<String>) -> Self {
        Self::key(function, KeyPhase::Tap)
    }
    /// A command that says how the key was pressed. The host sends the phase
    /// only to a protocol 3 package and downgrades it to a tap for the rest.
    pub fn key(function: impl Into<String>, phase: KeyPhase) -> Self {
        Self::Command {
            function: function.into(),
            phase,
            resource: None,
        }
    }
    /// A status read of the connection itself.
    pub fn status() -> Self {
        Self::Status { resource: None }
    }
    /// A typed action on the connection itself.
    pub fn action(action: couch_sdk::TypedAction) -> Self {
        Self::Action {
            action,
            resource: None,
        }
    }
    /// Protocol 3: one page of the connection's children.
    pub fn children(cursor: Option<String>) -> Self {
        Self::Children { cursor }
    }
    /// Protocol 4: read a JPEG snapshot from this byte offset.
    pub fn camera_snapshot(resource: impl Into<String>, offset: u32) -> Self {
        Self::CameraSnapshot {
            resource: resource.into(),
            offset,
        }
    }
    /// Protocol 4: begin a short H264 view.
    pub fn camera_open(resource: impl Into<String>) -> Self {
        Self::CameraOpen {
            resource: resource.into(),
        }
    }
    /// Protocol 4: close a view.
    pub fn camera_close(resource: impl Into<String>) -> Self {
        Self::CameraClose {
            resource: resource.into(),
        }
    }
    /// Protocol 3: the same request, aimed at one child of the connection.
    ///
    /// Only a command, a typed action and a status read can name a child;
    /// anything else is returned as it was, and the gate refuses what it must.
    pub fn at(mut self, resource: impl Into<String>) -> Self {
        match &mut self {
            Self::Command { resource: at, .. }
            | Self::Action { resource: at, .. }
            | Self::Status { resource: at } => *at = Some(resource.into()),
            Self::Hello { .. }
            | Self::Configure { .. }
            | Self::Inputs
            | Self::Children { .. }
            | Self::CameraSnapshot { .. }
            | Self::CameraOpen { .. }
            | Self::CameraClose { .. }
            | Self::PairStart { .. }
            | Self::PairContinue { .. }
            | Self::PairCancel { .. } => (),
        }
        self
    }
    /// Which child this request names, if any.
    pub fn resource(&self) -> Option<&str> {
        match self {
            Self::Command { resource, .. }
            | Self::Action { resource, .. }
            | Self::Status { resource } => resource.as_deref(),
            Self::CameraSnapshot { resource, .. }
            | Self::CameraOpen { resource }
            | Self::CameraClose { resource } => Some(resource),
            Self::Hello { .. }
            | Self::Configure { .. }
            | Self::Inputs
            | Self::Children { .. }
            | Self::PairStart { .. }
            | Self::PairContinue { .. }
            | Self::PairCancel { .. } => None,
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
    /// Protocol 3. One page of children, and the cursor for the next page if
    /// there is one. A page with a cursor is never empty.
    Children {
        children: Vec<couch_sdk::Child>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// Protocol 4. One base64-encoded JPEG chunk at `offset` of `total` bytes.
    CameraSnapshot {
        data: String,
        offset: u32,
        total: u32,
    },
    /// Protocol 4. The media socket will now carry bounded H264 records.
    CameraOpen {
        codec: CameraCodec,
        seconds: u8,
    },
    /// Protocol 3. One step of a pairing conversation, and the session it
    /// belongs to. The package names the session on the first step and
    /// repeats it on every one after; the host refuses any other.
    Pairing {
        session: String,
        step: PairStep,
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

/// A reply, and the one thing a package may say beside it.
///
/// `Envelope<Response>` is what this was and, with no `store_credential`,
/// still is byte for byte: a published package's reply parses here and this
/// writes the same bytes back. A key rotated by the device rides along with
/// the answer that discovered it, so the daemon can store it under the
/// connection's lock before the answer is returned.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplyEnvelope {
    pub id: u64,
    pub body: Response,
    /// Protocol 3. Never written by a package whose manifest is below 3 or
    /// does not declare `pairing`, and a protocol error from one that is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_credential: Option<Credential>,
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
