//! Protocol 4 camera side-channel records.
//!
//! Control requests remain on the package's length-prefixed JSON socket. Once
//! a camera view has been opened there, its separate inherited socket carries
//! only these bounded Annex-B H264 records. It never carries a stream URL,
//! credential or provider-specific descriptor.

use crate::{Error, Result};
use std::{
    fmt,
    io::{Read, Write},
    sync::Arc,
};

/// The first package protocol with camera children and a binary media channel.
pub const CAMERA_PROTOCOL_VERSION: u32 = 4;
/// The largest complete access-unit group a package may hand to the core.
pub const MAX_H264_ACCESS_UNIT: usize = 2 * 1024 * 1024;

/// A provider-owned source of complete Annex-B H264 access-unit groups.
///
/// `next_h264` may block on the device. The [`CameraView`] cancellation
/// callback must interrupt that wait: `couch-plugin` invokes it when the host
/// closes the view, then joins the media worker before acknowledging close.
pub trait CameraStream: Send {
    fn next_h264(&mut self) -> Result<Vec<u8>>;
}

/// One short live view and the operation that interrupts its device I/O.
///
/// The stream itself moves to the package's media worker. Cancellation stays
/// on the control thread, which is why it is a separate thread-safe callback
/// instead of another method on `CameraStream`.
pub struct CameraView {
    stream: Box<dyn CameraStream>,
    cancel: Arc<dyn Fn() + Send + Sync>,
    seconds: u8,
}

impl CameraView {
    pub fn new<S, F>(stream: S, cancel: F, seconds: u8) -> Result<Self>
    where
        S: CameraStream + 'static,
        F: Fn() + Send + Sync + 'static,
    {
        if seconds == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            stream: Box::new(stream),
            cancel: Arc::new(cancel),
            seconds,
        })
    }

    #[doc(hidden)]
    pub fn into_parts(self) -> (Box<dyn CameraStream>, Arc<dyn Fn() + Send + Sync>, u8) {
        (self.stream, self.cancel, self.seconds)
    }
}

impl fmt::Debug for CameraView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CameraView")
            .field("seconds", &self.seconds)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraWireError {
    Io(std::io::ErrorKind),
    Protocol,
}

impl From<std::io::Error> for CameraWireError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

pub type CameraWireResult<T> = std::result::Result<T, CameraWireError>;

fn annex_b(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1])
}

/// Write one complete Annex-B H264 access-unit group.
pub fn write_h264_record(writer: &mut impl Write, payload: &[u8]) -> CameraWireResult<()> {
    if payload.len() > MAX_H264_ACCESS_UNIT || !annex_b(payload) {
        return Err(CameraWireError::Protocol);
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

/// End an open camera stream cleanly.
pub fn write_camera_end(writer: &mut impl Write) -> CameraWireResult<()> {
    writer.write_all(&0_u32.to_be_bytes())?;
    Ok(())
}

/// Read one record. `None` is the terminal zero-length record.
///
/// The declared length is checked before allocation, so a hostile package
/// cannot make the core reserve an unbounded buffer.
pub fn read_h264_record(reader: &mut impl Read) -> CameraWireResult<Option<Vec<u8>>> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 {
        return Ok(None);
    }
    if length > MAX_H264_ACCESS_UNIT {
        return Err(CameraWireError::Protocol);
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    if !annex_b(&payload) {
        return Err(CameraWireError::Protocol);
    }
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_access_unit_and_the_terminal_record_have_fixed_bytes() {
        let payload = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        let mut wire = Vec::new();
        write_h264_record(&mut wire, &payload).unwrap();
        write_camera_end(&mut wire).unwrap();
        assert_eq!(wire, [0, 0, 0, 7, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0, 0, 0, 0]);
        let mut input = wire.as_slice();
        assert_eq!(
            read_h264_record(&mut input).unwrap(),
            Some(payload.to_vec())
        );
        assert_eq!(read_h264_record(&mut input).unwrap(), None);
        assert!(input.is_empty());
    }

    #[test]
    fn invalid_or_oversized_payloads_are_refused_before_handoff() {
        assert_eq!(
            write_h264_record(&mut Vec::new(), b"not h264"),
            Err(CameraWireError::Protocol)
        );
        assert_eq!(
            write_h264_record(&mut Vec::new(), &vec![0; MAX_H264_ACCESS_UNIT + 1]),
            Err(CameraWireError::Protocol)
        );

        let oversized = ((MAX_H264_ACCESS_UNIT + 1) as u32).to_be_bytes();
        assert_eq!(
            read_h264_record(&mut oversized.as_slice()),
            Err(CameraWireError::Protocol)
        );
        let malformed = [0, 0, 0, 4, 1, 2, 3, 4];
        assert_eq!(
            read_h264_record(&mut malformed.as_slice()),
            Err(CameraWireError::Protocol)
        );
    }

    #[test]
    fn a_partial_record_is_transport_not_protocol() {
        let partial = [0, 0, 0, 8, 0, 0, 0, 1];
        assert_eq!(
            read_h264_record(&mut partial.as_slice()),
            Err(CameraWireError::Io(std::io::ErrorKind::UnexpectedEof))
        );
    }
}
