//! A protocol 3 fixture. Not a package, and not the template: copy `EchoTv`.
//!
//! Protocol 3 is unreleased and no shipped Couch accepts its manifest, so this
//! exists only behind the `protocol-3-preview` feature, for the tests that run
//! the SDK's `serve` loop end to end with the switch on. It is the same
//! television with three things an older package cannot say:
//!
//! - a button of its own, `x:info`;
//! - the key phase, which it passes to the device so a test can read it back
//!   from the fake: a tap is `CMD volume-up` as ever, anything else is
//!   `CMD volume-up repeat` or `CMD x:info long_press`;
//! - reasons: a refusal carries the device's words, `ERR unpaired` becomes
//!   [`Error::Unpaired`], and a bad port names the `port` setting.

use crate::{EchoTv, Settings};
use couch_sdk::{
    couch_model::commands::{Function, KeyPhase},
    Capability, ClientSettings, DeviceClient, Error, Reason, Result, Selectable, Status,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettingsV3(pub Settings);

impl ClientSettings for SettingsV3 {
    const FILE_PREFIX: &'static str = "echo-v3";

    fn validate(&self) -> Result<()> {
        if self.0.port == 0 {
            return Err(Error::Invalid.because(Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }));
        }
        self.0.validate()
    }
}

pub struct EchoTvV3(EchoTv);

impl DeviceClient for EchoTvV3 {
    type Settings = SettingsV3;

    const KIND: &'static str = "echo-v3";
    const LABEL: &'static str = "Echo TV (protocol 3 fixture)";

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
            ("x:info", "Info"),
        ]
    }

    fn connect(settings: &SettingsV3) -> Result<Self> {
        settings.validate()?;
        EchoTv::connect(&settings.0).map(Self)
    }

    fn execute(&mut self, function: &Function) -> Result<()> {
        self.execute_phased(function, KeyPhase::Tap)
    }

    fn execute_phased(&mut self, function: &Function, phase: KeyPhase) -> Result<()> {
        let line = match phase {
            KeyPhase::Tap => format!("CMD {}", function.id()),
            KeyPhase::Repeat => format!("CMD {} repeat", function.id()),
            KeyPhase::LongPress => format!("CMD {} long_press", function.id()),
        };
        let reply = self.0.request(&line)?;
        match reply.as_str() {
            "OK" => Ok(()),
            "ERR unpaired" => Err(Error::Unpaired.because(Reason::Message {
                text: "Pair this TV again".into(),
            })),
            rejected if rejected.starts_with("ERR ") => {
                Err(Error::Rejected.because(Reason::Message {
                    text: rejected[4..].to_string(),
                }))
            }
            _ => Err(Error::Protocol),
        }
    }

    fn status(&mut self) -> Result<Status> {
        self.0.status()
    }

    fn inputs(&mut self) -> Result<Vec<Selectable>> {
        self.0.inputs()
    }

    fn supports_input(id: &str) -> bool {
        EchoTv::supports_input(id)
    }
}
