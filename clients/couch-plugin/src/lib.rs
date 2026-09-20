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
/// Protocol 3 (unreleased): the harness for a package with children. It moves
/// into `testing` when the protocol ships; until then it is behind the same
/// switch as everything else protocol 3.
#[cfg(all(feature = "testing", feature = "protocol-3-preview"))]
pub mod testing_v3;
#[cfg(test)]
mod wire_golden;
#[cfg(test)]
mod wire_mirror;

pub use couch_sdk::couch_model::{valid_resource, ChildComponent};
pub use couch_sdk::couch_model::{PluginComponent as Component, PluginStatusField as StatusField};
pub use couch_sdk::{
    ActionKind, Child, ChildPage, ChildSnapshot, ClimateMode, ClimateState, ClimateTraits,
    CoverState, CoverTraits, KeyPhase, LightState, LightTraits, PluginActionSchema,
    PluginChildKind, Reason, Selectable, Status, TypedAction, VolumeDb, MAX_PAGE,
};
pub use host::{
    list_children, local_request, local_request_detailed, read_frame_timeout, requires,
    write_frame_timeout, Endpoint, Host, HostPolicy, LocalRequest, CHILD_LISTING_DEADLINE,
    MAX_CHILDREN, MAX_CHILD_PAGES, QUEUE_CAPACITY, QUEUE_TTL, REQUEST_TIMEOUT, STARTUP_TIMEOUT,
};
pub use manifest::{Capability, FieldKind, Manifest, SettingField};
pub use protocol::{
    accepted_protocol_version, read_frame, write_frame, Error, Failure, Request, Response, Result,
    MAX_FRAME, NEXT_PROTOCOL_VERSION, PROTOCOL_VERSION,
};
pub use server::serve;
