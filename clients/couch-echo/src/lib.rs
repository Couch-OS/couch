//! A complete client for a television that does not exist.
//!
//! Copy this crate to `clients/couch-<yours>`, rename the types, and replace
//! the four protocol helpers at the bottom with your device's wire format.
//! Everything above them - the settings file, the capability declaration, the
//! refusal of anything undeclared - is the part `couch-sdk` fixes for you.
//!
//! The device speaks a newline protocol chosen to be boring:
//!
//! ```text
//! -> CMD volume-up          <- OK
//! -> CMD power-off          <- ERR the TV is locked
//! -> GET STATUS             <- STATUS power=on;mute=off;volume=30;input=hdmi1
//! -> LIST INPUTS            <- INPUT hdmi1 Blu-ray
//!                              INPUT hdmi2 Console
//!                              END
//! ```
//!
//! Run `cargo run -p couch-echo --example demo` to watch a session, including
//! the failures.

#[cfg(feature = "protocol-3-preview")]
pub mod v3;

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use couch_sdk::{
    couch_model::commands::Function, Capability, ClientSettings, DeviceClient, Discover,
    Discovered, Error, Result, Selectable, Status,
};
use serde::{Deserialize, Serialize};

/// How long the device has to answer one request. Deliberately short: the
/// broker above drops anything that has waited 750 ms in its queue, so a client
/// that blocks for ten seconds is only making the user wait for a stale answer.
pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const DEFAULT_PORT: u16 = 9299;

/// What this client needs to reach one television.
///
/// Keep it small and keep it JSON: this file is written to the remote's flash
/// at mode 0600 and is never part of an exported house configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// A pairing token stands in here for whatever secret your device issues.
    #[serde(default)]
    pub token: String,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl ClientSettings for Settings {
    const FILE_PREFIX: &'static str = "echo";

    fn validate(&self) -> Result<()> {
        // Reject what cannot address a device, and reject anything that could
        // smuggle a newline into the protocol. Do this once, here, so no
        // transport ever has to wonder.
        if self.host.is_empty()
            || self.host.len() > 253
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '\\'))
            || self.port == 0
            || self.token.chars().any(char::is_control)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// One television, one socket, one owner.
pub struct EchoTv {
    socket: TcpStream,
    buffer: Vec<u8>,
}

impl DeviceClient for EchoTv {
    type Settings = Settings;

    const KIND: &'static str = "echo";
    const LABEL: &'static str = "Echo TV (example)";

    /// Only what the protocol above can actually do. Every ID here is a
    /// `couch_model::commands::Function`; the labels are what the button
    /// picker shows, so write them for a person choosing a key.
    fn capabilities() -> &'static [Capability] {
        &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("power-on", "Power on"),
            ("power-off", "Power off"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("play-pause", "Play / pause"),
        ]
    }

    fn connect(settings: &Settings) -> Result<Self> {
        settings.validate()?;
        let deadline = Instant::now() + TIMEOUT;
        let mut socket = None;
        for address in (settings.host.as_str(), settings.port).to_socket_addrs()? {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            if let Ok(s) = TcpStream::connect_timeout(&address, left) {
                socket = Some(s);
                break;
            }
        }
        let socket = socket.ok_or(Error::Transport)?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        socket.set_nodelay(true)?;
        Ok(Self {
            socket,
            buffer: Vec::new(),
        })
    }

    /// One function, one round trip, never retried.
    ///
    /// `execute` is reached only through `command`, which has already refused
    /// anything absent from `capabilities`. The catch-all arm is therefore not
    /// dead code but a promise: if the declaration and this match ever drift,
    /// the device is told nothing.
    fn execute(&mut self, function: &Function) -> Result<()> {
        match function {
            Function::Up
            | Function::Down
            | Function::Left
            | Function::Right
            | Function::Ok
            | Function::Back
            | Function::Home
            | Function::PowerOn
            | Function::PowerOff
            | Function::VolumeUp
            | Function::VolumeDown
            | Function::Mute
            | Function::PlayPause
            | Function::Input(_) => {
                let reply = self.request(&format!("CMD {}", function.id()))?;
                match reply.as_str() {
                    "OK" => Ok(()),
                    // The device understood and said no. Surface its reason:
                    // "the TV is locked" is useful, "Protocol error" is not.
                    rejected if rejected.starts_with("ERR ") => {
                        Err(Error::Remote(rejected[4..].to_string()))
                    }
                    _ => Err(Error::Protocol),
                }
            }
            _ => Err(Error::Unsupported),
        }
    }

    fn status(&mut self) -> Result<Status> {
        let reply = self.request("GET STATUS")?;
        let body = reply.strip_prefix("STATUS ").ok_or(Error::Protocol)?;
        let mut status = Status::default();
        for field in body.split(';').filter(|f| !f.is_empty()) {
            let (key, value) = field.split_once('=').ok_or(Error::Protocol)?;
            match key {
                "power" => status.on = Some(boolean(value)?),
                "mute" => status.muted = Some(boolean(value)?),
                // A reading outside the scale is a parsing failure, not
                // something to clamp: it means we are misreading the device.
                "volume" => {
                    status = status.with_volume(value.parse().map_err(|_| Error::Protocol)?)?
                }
                "input" => status.input = Some(value.to_string()),
                "title" => status.title = Some(value.to_string()),
                // An unknown field is a newer firmware, not a broken one.
                _ => {}
            }
        }
        Ok(status)
    }

    fn inputs(&mut self) -> Result<Vec<Selectable>> {
        self.send("LIST INPUTS")?;
        let deadline = Instant::now() + TIMEOUT;
        let mut inputs = Vec::new();
        loop {
            let line = self.line(deadline)?;
            if line == "END" {
                return Ok(inputs);
            }
            let entry = line.strip_prefix("INPUT ").ok_or(Error::Protocol)?;
            let (id, name) = entry.split_once(' ').ok_or(Error::Protocol)?;
            if !Self::supports_input(id) || name.is_empty() {
                return Err(Error::Protocol);
            }
            inputs.push(Selectable::new(id, name));
            // A device that never says END must not grow this forever.
            if inputs.len() > 64 {
                return Err(Error::Protocol);
            }
        }
    }

    /// Input IDs are constrained here rather than trusted from the device,
    /// because this ID is persisted into `config.json` as `input:<id>` and read
    /// back by other processes.
    fn supports_input(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 32
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
}

/// Optional, and only because this device advertises itself. The daemon owns
/// the mDNS browser; a client contributes the service name to browse for and
/// the translation from what was found into its own settings.
///
/// `couch-androidtv` and `couch-appletv` already publish exactly this constant
/// (`MDNS_SERVICE`) for the daemon to use; this is the same fact, typed.
impl Discover for EchoTv {
    const MDNS_SERVICE: &'static str = "_echotv._tcp.local.";

    fn settings_for(found: &Discovered) -> Result<Settings> {
        // A discovered device is untrusted input. The pairing token is not
        // discoverable, so a found TV still needs setting up by hand: an empty
        // token here is deliberate, not an oversight.
        let settings = Settings {
            host: found.address.clone(),
            port: found.port,
            token: String::new(),
        };
        settings.validate()?;
        Ok(settings)
    }
}

// The four protocol helpers. In a real client this is the only part that knows
// anything about the device.
impl EchoTv {
    fn request(&mut self, line: &str) -> Result<String> {
        self.send(line)?;
        self.line(Instant::now() + TIMEOUT)
    }

    fn send(&mut self, line: &str) -> Result<()> {
        if line.contains('\n') {
            return Err(Error::Invalid);
        }
        self.socket.write_all(line.as_bytes())?;
        self.socket.write_all(b"\n")?;
        Ok(())
    }

    /// Read one line, honouring a deadline rather than a per-read timeout: a
    /// device that dribbles a byte at a time must not extend the wait forever.
    fn line(&mut self, deadline: Instant) -> Result<String> {
        loop {
            if let Some(at) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=at).collect();
                return String::from_utf8(line[..at].to_vec()).map_err(|_| Error::Protocol);
            }
            if self.buffer.len() > 8 * 1024 {
                return Err(Error::Protocol);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout);
            }
            self.socket.set_read_timeout(Some(left))?;
            let mut bytes = [0; 512];
            match self.socket.read(&mut bytes) {
                // The device hung up. Not a timeout, and not retried here.
                Ok(0) => return Err(Error::Transport),
                Ok(n) => self.buffer.extend_from_slice(&bytes[..n]),
                Err(e) => return Err(Error::from(e)),
            }
        }
    }
}

fn boolean(value: &str) -> Result<bool> {
    match value {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(Error::Protocol),
    }
}
