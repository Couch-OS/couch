//! Independently installable integrations, with a versioned JSON protocol.
//!
//! A package executable reads and writes u32 big-endian length-prefixed JSON
//! on stdin/stdout. The host supplies a Unix socket for both descriptors, so
//! complete requests have deadlines even when a child dribbles partial frames.
//! stdout is exclusively protocol traffic. Settings travel over the socket,
//! never argv or environment. Handshake and configuration do not contact devices.

mod host;
mod manifest;
mod protocol;
mod server;
#[cfg(feature = "testing")]
pub mod testing;

pub use couch_sdk::couch_model::{PluginComponent as Component, PluginStatusField as StatusField};
pub use couch_sdk::{Selectable, Status};
pub use host::{
    local_request, read_frame_timeout, write_frame_timeout, Endpoint, Host, HostPolicy,
    LocalRequest, QUEUE_CAPACITY, QUEUE_TTL, REQUEST_TIMEOUT, STARTUP_TIMEOUT,
};
pub use manifest::{Capability, FieldKind, Manifest, SettingField};
pub use protocol::{
    read_frame, write_frame, Error, Request, Response, Result, MAX_FRAME, PROTOCOL_VERSION,
};
pub use server::serve;
