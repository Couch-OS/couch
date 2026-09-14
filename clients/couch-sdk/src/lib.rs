//! The contract a Couch integration client implements.
//!
//! # What this is
//!
//! Couch drives home devices from crates in the `clients/` workspace. Each one
//! owns a transport - Kodi's JSON-RPC, an LG TV's SSAP socket, a Denon AVR's
//! CR-delimited TCP protocol, the remote's own IR blaster - and is linked into
//! the configuration daemon and the device GUI. This crate is the small set of
//! types they have in common, extracted from the ones that already exist:
//!
//! - [`ClientSettings`] - the private per-connection credential file, written
//!   atomically at mode 0600, the way `couch-denon`, `couch-ha`, `couch-hue`,
//!   `couch-kodi` and `couch-webos` each write it today.
//! - [`DeviceClient`] - what the client declares it can do, and how it is asked
//!   to do it, in `couch-model`'s existing [`Function`](couch_model::commands::Function)
//!   vocabulary rather than a new one.
//! - [`Status`] and [`Selectable`] - what it can report back.
//! - `couch_sdk::tls` (feature `tls`) and `couch_sdk::wol` - the pinned
//!   certificate verifier, the plain-or-TLS socket and the magic packet that
//!   the LAN TV clients used to keep one copy of each.
//! - `couch_sdk::testing` - a scripted TCP host and a conformance check, so
//!   all of the above can be developed and proved with no hardware and no
//!   credentials. It is behind the `testing` feature, so pass
//!   `--features testing` to see it in this documentation.
//!
//! # What this is not
//!
//! It is **not a plugin system**. Nothing loads a client at runtime, there is
//! no ABI, and building one does not make it installable. A new integration is
//! a crate in this workspace that the daemon and GUI are edited to link, and
//! those edits are manual and listed in `docs/client-sdk.md`. This crate makes
//! the client itself correct and testable; it does not make it discoverable.
//!
//! It also does not open sockets for you. Transports stay in the client, which
//! is what lets `couch-ir` depend on nothing but `libc` and `couch-kodi` on
//! nothing but serde.
//!
//! # A client in one page
//!
//! ```no_run
//! use couch_sdk::{Capability, ClientSettings, DeviceClient, Error, Result, Status};
//! use couch_model::commands::Function;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Clone, Serialize, Deserialize)]
//! struct LampSettings { host: String, port: u16 }
//!
//! impl ClientSettings for LampSettings {
//!     const FILE_PREFIX: &'static str = "lamp";
//!     fn validate(&self) -> Result<()> {
//!         if self.host.is_empty() || self.port == 0 { return Err(Error::Invalid); }
//!         Ok(())
//!     }
//! }
//!
//! struct Lamp { socket: std::net::TcpStream }
//!
//! impl DeviceClient for Lamp {
//!     type Settings = LampSettings;
//!     const KIND: &'static str = "lamp";
//!     const LABEL: &'static str = "Example lamp";
//!     fn capabilities() -> &'static [Capability] {
//!         &[("on", "On"), ("off", "Off"), ("toggle", "Toggle on / off")]
//!     }
//!     fn connect(settings: &LampSettings) -> Result<Self> {
//!         settings.validate()?;
//!         Ok(Lamp { socket: std::net::TcpStream::connect((settings.host.as_str(), settings.port))? })
//!     }
//!     fn execute(&mut self, function: &Function) -> Result<()> {
//!         match function {
//!             Function::On | Function::Off | Function::Toggle => Ok(()),
//!             _ => Err(Error::Unsupported),
//!         }
//!     }
//!     fn status(&mut self) -> Result<Status> { Ok(Status::on(true)) }
//! }
//! ```
//!
//! A complete, runnable version with a fake device, a scripted failure and a
//! test suite is `clients/couch-echo`.

pub mod client;
pub mod discovery;
pub mod error;
pub mod settings;
pub mod status;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "tls")]
pub mod tls;
pub mod wol;

pub use client::{catalog_differences, Capability, DeviceClient};
pub use discovery::{Discover, Discovered};
pub use error::{Error, Result};
pub use settings::{connection_file, load_private, save_private, ClientSettings};
pub use status::{Selectable, Status};

/// Re-exported so a client crate needs one dependency, not two, to speak the
/// command vocabulary.
pub use couch_model;
