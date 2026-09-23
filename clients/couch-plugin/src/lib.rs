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
/// Admission harness for a protocol 3 package with children or pairing.
#[cfg(feature = "testing")]
pub mod testing_v3;
#[cfg(test)]
mod wire_golden;
#[cfg(test)]
mod wire_mirror;

pub use couch_sdk::couch_model::{valid_resource, ChildComponent};
pub use couch_sdk::couch_model::{PluginComponent as Component, PluginStatusField as StatusField};
pub use couch_sdk::{
    valid_session, ActionKind, Child, ChildPage, ChildSnapshot, ClimateMode, ClimateState,
    ClimateTraits, CodeAlphabet, CoverState, CoverTraits, Credential, KeyPhase, LightState,
    LightTraits, PairFailure, PairFlow, PairInput, PairPrompt, PairStep, PluginActionSchema,
    PluginChildKind, Reason, Selectable, Status, TypedAction, VolumeDb, MAX_CODE_LENGTH,
    MAX_CREDENTIAL_BYTES, MAX_PAGE, MAX_PAIR_TEXT, MAX_POLL_MS, MAX_SESSION, MIN_POLL_MS,
};
pub use host::{
    is_non_dumpable, list_children, local_request, local_request_detailed, read_frame_timeout,
    requires, write_frame_timeout, Endpoint, Host, HostPolicy, LocalRequest,
    CHILD_LISTING_DEADLINE, MAX_CHILDREN, MAX_CHILD_PAGES, QUEUE_CAPACITY, QUEUE_TTL,
    REQUEST_TIMEOUT, STARTUP_TIMEOUT,
};
pub use manifest::{Capability, FieldKind, Manifest, Pairing, SettingField};
pub use protocol::{
    accepted_protocol_version, read_frame, write_frame, Error, Failure, Request, Response, Result,
    MAX_FRAME, NEXT_PROTOCOL_VERSION, PROTOCOL_VERSION,
};
pub use server::serve;
