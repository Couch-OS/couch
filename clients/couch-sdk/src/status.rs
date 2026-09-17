//! What a client can say about a device it has just spoken to.
//!
//! Shaped after `couch_denon::State` and the fields the GUI actually reads
//! back from Kodi and webOS: every field is optional because "this device does
//! not report its volume" and "this device is at volume zero" are different
//! answers, and the UI has to be able to tell them apart. Nothing is
//! fabricated: a client that cannot observe a field leaves it `None` rather
//! than guessing from the last command it sent.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// Powered on, as *observed*. A device that only accepts discrete power
    /// commands and reports nothing leaves this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    /// 0-100, for devices with a percentage scale. Devices reporting decibels
    /// (an AVR) use `volume_db` and leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// Observed decibels or the receiver's explicit minimum sentinel (v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<couch_model::VolumeDb>,
    /// The selected input's ID, as the device names it - the same string an
    /// `input:<id>` function carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playing: Option<bool>,
    /// What is on screen, if the device says. Shown to the user verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Status {
    pub fn on(on: bool) -> Self {
        Self {
            on: Some(on),
            ..Self::default()
        }
    }
    pub fn with_muted(mut self, muted: bool) -> Self {
        self.muted = Some(muted);
        self
    }
    /// Rejects an out-of-range reading rather than clamping it: a device
    /// claiming volume 300 is a parsing failure, and clamping hides it.
    pub fn with_volume(mut self, volume: u8) -> Result<Self> {
        if volume > 100 {
            return Err(Error::Protocol);
        }
        self.volume = Some(volume);
        Ok(self)
    }
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = Some(input.into());
        self
    }
    pub fn with_playing(mut self, playing: bool) -> Self {
        self.playing = Some(playing);
        self
    }
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// One entry of something a user picks: an input, an app, a source.
///
/// `id` is what an `input:<id>` or `app:<id>` function carries; `name` is what
/// the user named it on the device and is only ever displayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selectable {
    pub id: String,
    pub name: String,
}

impl Selectable {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unobserved_fields_stay_absent_from_the_wire_and_bad_readings_are_errors() {
        let status = Status::on(true).with_input("HDMI1");
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#"{"on":true,"input":"HDMI1"}"#);
        assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), status);
        assert_eq!(
            serde_json::from_str::<Status>("{}").unwrap(),
            Status::default()
        );
        assert_eq!(Status::default().with_volume(101), Err(Error::Protocol));
        assert_eq!(Status::default().with_volume(0).unwrap().volume, Some(0));
    }
}
