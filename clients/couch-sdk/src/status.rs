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
    /// Protocol 5. The device's current sound-output token, matching one of
    /// the package's fixed sound-output commands when it can observe it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_output: Option<String>,
    /// Protocol 5. The current picture-mode token when the TV reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_mode: Option<String>,
    /// Protocol 3 (unreleased). What one child of a connection reports, when
    /// the request named one. Absent from every protocol 1 and 2 status, in
    /// both directions: `serve` clears these for an older manifest as it
    /// clears `volume_db` for a protocol 1 one, and the host retires a package
    /// that sends one anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<couch_model::LightState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<couch_model::CoverState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climate: Option<couch_model::ClimateState>,
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
    /// Protocol 3 (unreleased): what one lamp reports.
    pub fn with_light(mut self, light: couch_model::LightState) -> Self {
        self.light = Some(light);
        self
    }
    pub fn with_cover(mut self, cover: couch_model::CoverState) -> Self {
        self.cover = Some(cover);
        self
    }
    pub fn with_climate(mut self, climate: couch_model::ClimateState) -> Self {
        self.climate = Some(climate);
        self
    }
    /// Whether every reading is one a device could really have made. The host
    /// checks it on the way in; a client that builds a status by hand can use
    /// the same answer.
    pub fn is_valid(&self) -> bool {
        self.volume.is_none_or(|v| v <= 100)
            && self.volume_db.is_none_or(|v| v.is_valid())
            && [
                &self.input,
                &self.title,
                &self.sound_output,
                &self.picture_mode,
            ]
            .iter()
            .all(|text| {
                text.as_ref()
                    .is_none_or(|v| v.len() <= 4096 && !v.chars().any(char::is_control))
            })
            && self.light.is_none_or(|state| state.is_valid())
            && self.cover.is_none_or(|state| state.is_valid())
            && self.climate.is_none_or(|state| state.is_valid())
    }
    /// Protocol 3 (unreleased): whether this status says anything only a child
    /// of a connection can say.
    pub fn is_child_state(&self) -> bool {
        self.light.is_some() || self.cover.is_some() || self.climate.is_some()
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
