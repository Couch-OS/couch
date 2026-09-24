//! Bounded protocol-v2 decibel measurements and actions. Percentage volume is
//! intentionally a separate quantity; minimum is not a fabricated reading.
//!
//! Protocol 3 (unreleased) adds three typed actions for the children of a
//! connection: a light, a cover and a thermostat ([`crate::domain`]), and five
//! more for a packaged media player: percentage volume, seeking, and the way
//! it plays through what it holds.
use crate::domain::{
    ClimateMode, MAX_CLIMATE_TENTHS, MAX_MIREK, MAX_PERCENT, MAX_XY, MIN_CLIMATE_TENTHS, MIN_MIREK,
};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub const MIN_VOLUME_TENTHS: i16 = -1000;
pub const MAX_VOLUME_TENTHS: i16 = 300;

/// The furthest a position or a duration may reach: seven days. Anything
/// beyond it is a package's arithmetic, not a recording.
pub const MAX_MEDIA_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// A relative seek moves at least a second and at most an hour.
pub const MIN_SEEK_DELTA_MS: u32 = 1_000;
pub const MAX_SEEK_DELTA_MS: u32 = 3_600_000;
/// One press of a percentage volume key moves at most this many points.
pub const MAX_VOLUME_STEP: u8 = 50;

/// One way a player may be told to play through what it holds (protocol 3,
/// unreleased). Serialized as its own name, so a set reads as a list of words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    Shuffle,
    Repeat,
    RepeatOne,
    Crossfade,
}
/// The order a [`PlayModeSet`] is always written in, whatever order it was
/// built in: two packages declaring the same modes produce the same bytes.
pub const ALL_PLAY_MODES: [PlayMode; 4] = [
    PlayMode::Shuffle,
    PlayMode::Repeat,
    PlayMode::RepeatOne,
    PlayMode::Crossfade,
];
impl PlayMode {
    const fn bit(self) -> u8 {
        match self {
            Self::Shuffle => 1,
            Self::Repeat => 2,
            Self::RepeatOne => 4,
            Self::Crossfade => 8,
        }
    }
}

/// Which modes a player offers. A bit set, so [`PluginActionSchema`] stays
/// `Copy + Eq` with integers in it; on the wire and on disk it is an ordered
/// list of names, and a name given twice is refused rather than merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "Vec<PlayMode>", try_from = "Vec<PlayMode>")]
pub struct PlayModeSet(u8);
impl PlayModeSet {
    pub const fn new() -> Self {
        Self(0)
    }
    pub const fn with(self, mode: PlayMode) -> Self {
        Self(self.0 | mode.bit())
    }
    pub const fn contains(self, mode: PlayMode) -> bool {
        self.0 & mode.bit() != 0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }
    pub fn iter(self) -> impl Iterator<Item = PlayMode> {
        ALL_PLAY_MODES
            .into_iter()
            .filter(move |mode| self.contains(*mode))
    }
}
impl From<PlayModeSet> for Vec<PlayMode> {
    fn from(set: PlayModeSet) -> Self {
        set.iter().collect()
    }
}
impl TryFrom<Vec<PlayMode>> for PlayModeSet {
    type Error = &'static str;
    fn try_from(modes: Vec<PlayMode>) -> Result<Self, Self::Error> {
        let mut set = Self::new();
        for mode in modes {
            if set.contains(mode) {
                return Err("a play mode is named twice");
            }
            set = set.with(mode);
        }
        Ok(set)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeDb {
    Reading { tenths: i16 },
    Minimum,
}
// Serde's unit variants ignore fields even with deny_unknown_fields. Use an
// empty struct variant on input so minimum+numeric readings cannot be ambiguous.
impl<'de> Deserialize<'de> for VolumeDb {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Reading { tenths: i16 },
            Minimum {},
        }
        let result = match Wire::deserialize(deserializer)? {
            Wire::Reading { tenths } => Self::Reading { tenths },
            Wire::Minimum {} => Self::Minimum,
        };
        if !result.is_valid() {
            return Err(serde::de::Error::custom(
                "decibel reading outside protocol bounds",
            ));
        }
        Ok(result)
    }
}
impl VolumeDb {
    pub fn is_valid(self) -> bool {
        match self {
            Self::Reading { tenths } => (MIN_VOLUME_TENTHS..=MAX_VOLUME_TENTHS).contains(&tenths),
            Self::Minimum => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedAction {
    SetVolumeDb {
        tenths: i16,
    },
    /// Every field is optional and an absent one is left alone; at least one
    /// has to be there. Brightness 0 means off, as it does in built-in Hue.
    SetLight {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brightness: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mirek: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        xy: Option<(u16, u16)>,
    },
    /// 0 is closed, 100 is open.
    SetCover {
        position: u8,
    },
    /// A single set point, or a low and a high one for a thermostat that
    /// holds a range, and the operating mode. At least one has to be there.
    SetClimate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_tenths: Option<i16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        low_tenths: Option<i16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        high_tenths: Option<i16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ClimateMode>,
    },
    /// Protocol 3 (unreleased): a packaged media player. A player that reports
    /// a percentage takes one of the two volume actions, never the decibel
    /// one; a percentage is not a decibel reading and neither is derivable
    /// from the other.
    SetVolumePercent {
        percent: u8,
    },
    /// Never 0: a step that moves nothing is a mistake, not a no-op.
    StepVolumePercent {
        delta: i8,
    },
    Seek {
        position_ms: u64,
    },
    SeekBy {
        delta_ms: i64,
    },
    /// A partial set, as [`Self::SetLight`] is: leaving "repeat one" has to
    /// clear two modes in one write, so an absent field is left alone and at
    /// least one has to be there.
    SetMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shuffle: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat_one: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crossfade: Option<bool>,
    },
}

/// Which typed action a request or a declaration is, without its numbers. A
/// package declares at most one schema of each kind, so the kind is how a
/// request finds the bounds it is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    SetVolumeDb,
    SetLight,
    SetCover,
    SetClimate,
    SetVolumePercent,
    StepVolumePercent,
    Seek,
    SeekBy,
    SetMode,
}
/// How many typed actions one package may declare (protocol 3, unreleased;
/// protocols 1 and 2 stay at none and one).
pub const MAX_ACTIONS: usize = 8;
impl TypedAction {
    pub fn kind(self) -> ActionKind {
        match self {
            Self::SetVolumeDb { .. } => ActionKind::SetVolumeDb,
            Self::SetLight { .. } => ActionKind::SetLight,
            Self::SetCover { .. } => ActionKind::SetCover,
            Self::SetClimate { .. } => ActionKind::SetClimate,
            Self::SetVolumePercent { .. } => ActionKind::SetVolumePercent,
            Self::StepVolumePercent { .. } => ActionKind::StepVolumePercent,
            Self::Seek { .. } => ActionKind::Seek,
            Self::SeekBy { .. } => ActionKind::SeekBy,
            Self::SetMode { .. } => ActionKind::SetMode,
        }
    }
    /// Within the bounds that hold for every device, and saying something.
    /// What one particular device accepts is narrower: a decibel action is
    /// checked against its schema ([`PluginActionSchema::accepts`]), a child's
    /// against its saved traits ([`crate::ChildSnapshot::accepts`]).
    pub fn is_valid(self) -> bool {
        let degrees = |value: Option<i16>| {
            value.is_none_or(|v| (MIN_CLIMATE_TENTHS..=MAX_CLIMATE_TENTHS).contains(&v))
        };
        match self {
            Self::SetVolumeDb { tenths } => {
                (MIN_VOLUME_TENTHS..=MAX_VOLUME_TENTHS).contains(&tenths)
            }
            Self::SetLight {
                on,
                brightness,
                mirek,
                xy,
            } => {
                (on.is_some() || brightness.is_some() || mirek.is_some() || xy.is_some())
                    && brightness.is_none_or(|v| v <= MAX_PERCENT)
                    && mirek.is_none_or(|v| (MIN_MIREK..=MAX_MIREK).contains(&v))
                    && xy.is_none_or(|(x, y)| x <= MAX_XY && y <= MAX_XY)
            }
            Self::SetCover { position } => position <= MAX_PERCENT,
            Self::SetClimate {
                target_tenths,
                low_tenths,
                high_tenths,
                mode,
            } => {
                (target_tenths.is_some()
                    || low_tenths.is_some()
                    || high_tenths.is_some()
                    || mode.is_some())
                    && degrees(target_tenths)
                    && degrees(low_tenths)
                    && degrees(high_tenths)
                    && !matches!((low_tenths, high_tenths), (Some(low), Some(high)) if low >= high)
            }
            Self::SetVolumePercent { percent } => percent <= MAX_PERCENT,
            Self::StepVolumePercent { delta } => {
                delta != 0 && delta.unsigned_abs() <= MAX_VOLUME_STEP
            }
            Self::Seek { position_ms } => position_ms <= MAX_MEDIA_MS,
            Self::SeekBy { delta_ms } => {
                delta_ms != 0 && delta_ms.unsigned_abs() <= u64::from(MAX_SEEK_DELTA_MS)
            }
            Self::SetMode {
                shuffle,
                repeat,
                repeat_one,
                crossfade,
            } => {
                shuffle.is_some() || repeat.is_some() || repeat_one.is_some() || crossfade.is_some()
            }
        }
    }
    /// Which modes a [`Self::SetMode`] names, whatever it sets them to. A
    /// player is only ever told about a mode it declared.
    pub fn modes(self) -> PlayModeSet {
        let Self::SetMode {
            shuffle,
            repeat,
            repeat_one,
            crossfade,
        } = self
        else {
            return PlayModeSet::new();
        };
        let mut set = PlayModeSet::new();
        for (named, mode) in [
            (shuffle.is_some(), PlayMode::Shuffle),
            (repeat.is_some(), PlayMode::Repeat),
            (repeat_one.is_some(), PlayMode::RepeatOne),
            (crossfade.is_some(), PlayMode::Crossfade),
        ] {
            if named {
                set = set.with(mode);
            }
        }
        set
    }
}

pub fn valid_volume_bounds(min: i16, max: i16, step: u16) -> bool {
    min >= MIN_VOLUME_TENTHS
        && max <= MAX_VOLUME_TENTHS
        && min < max
        && step > 0
        && i32::from(step) <= i32::from(max) - i32::from(min)
        && (i32::from(max) - i32::from(min)) % i32::from(step) == 0
}

pub fn volume_in_bounds(tenths: i16, min: i16, max: i16, step: u16) -> bool {
    valid_volume_bounds(min, max, step)
        && (min..=max).contains(&tenths)
        && (i32::from(tenths) - i32::from(min)) % i32::from(step) == 0
}

/// One supported typed action with device-specific validated bounds. Display
/// components only bind this declaration; they never authorize operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginActionSchema {
    SetVolumeDb {
        min_tenths: i16,
        max_tenths: i16,
        step_tenths: u16,
    },
    // Empty struct variants, never unit variants: serde lets a unit variant
    // ignore fields even with deny_unknown_fields (see `VolumeDb` above). What
    // one lamp or thermostat accepts is a trait of that child, not of the
    // package, so these declare only that the action exists.
    SetLight {},
    SetCover {},
    SetClimate {},
    /// Protocol 3 (unreleased): the media actions. What a player accepts is a
    /// trait of the player, so each of these carries the one bound Couch has
    /// to respect before it sends anything.
    SetVolumePercent {
        /// 1 to 100. A speaker whose scale stops short says so here.
        max_percent: u8,
    },
    StepVolumePercent {
        /// 1 to [`MAX_VOLUME_STEP`].
        max_delta: u8,
    },
    Seek {},
    SeekBy {
        /// [`MIN_SEEK_DELTA_MS`] to [`MAX_SEEK_DELTA_MS`].
        max_delta_ms: u32,
    },
    SetMode {
        /// Non-empty: a player that offers no mode declares no schema.
        modes: PlayModeSet,
    },
}
impl PluginActionSchema {
    pub fn kind(self) -> ActionKind {
        match self {
            Self::SetVolumeDb { .. } => ActionKind::SetVolumeDb,
            Self::SetLight {} => ActionKind::SetLight,
            Self::SetCover {} => ActionKind::SetCover,
            Self::SetClimate {} => ActionKind::SetClimate,
            Self::SetVolumePercent { .. } => ActionKind::SetVolumePercent,
            Self::StepVolumePercent { .. } => ActionKind::StepVolumePercent,
            Self::Seek {} => ActionKind::Seek,
            Self::SeekBy { .. } => ActionKind::SeekBy,
            Self::SetMode { .. } => ActionKind::SetMode,
        }
    }
    /// The declaration a request of this kind is checked against.
    pub fn find(actions: &[Self], kind: ActionKind) -> Option<Self> {
        actions.iter().copied().find(|schema| schema.kind() == kind)
    }
    /// At most [`MAX_ACTIONS`], every one valid, no kind declared twice.
    pub fn valid_set(actions: &[Self]) -> bool {
        actions.len() <= MAX_ACTIONS
            && actions.iter().enumerate().all(|(index, schema)| {
                schema.is_valid()
                    && actions[..index]
                        .iter()
                        .all(|earlier| earlier.kind() != schema.kind())
            })
    }
    pub fn is_valid(self) -> bool {
        match self {
            Self::SetVolumeDb {
                min_tenths,
                max_tenths,
                step_tenths,
            } => valid_volume_bounds(min_tenths, max_tenths, step_tenths),
            Self::SetLight {} | Self::SetCover {} | Self::SetClimate {} | Self::Seek {} => true,
            Self::SetVolumePercent { max_percent } => (1..=MAX_PERCENT).contains(&max_percent),
            Self::StepVolumePercent { max_delta } => (1..=MAX_VOLUME_STEP).contains(&max_delta),
            Self::SeekBy { max_delta_ms } => {
                (MIN_SEEK_DELTA_MS..=MAX_SEEK_DELTA_MS).contains(&max_delta_ms)
            }
            Self::SetMode { modes } => !modes.is_empty(),
        }
    }
    /// An action of this schema's kind, within its bounds. For the three child
    /// actions those are the global bounds, a low set point below the high
    /// one, and at least one field set ([`TypedAction::is_valid`]).
    pub fn accepts(self, action: TypedAction) -> bool {
        match (self, action) {
            (
                Self::SetVolumeDb {
                    min_tenths,
                    max_tenths,
                    step_tenths,
                },
                TypedAction::SetVolumeDb { tenths },
            ) => volume_in_bounds(tenths, min_tenths, max_tenths, step_tenths),
            (Self::SetLight {}, TypedAction::SetLight { .. })
            | (Self::SetCover {}, TypedAction::SetCover { .. })
            | (Self::SetClimate {}, TypedAction::SetClimate { .. })
            | (Self::Seek {}, TypedAction::Seek { .. }) => action.is_valid(),
            (Self::SetVolumePercent { max_percent }, TypedAction::SetVolumePercent { percent }) => {
                self.is_valid() && action.is_valid() && percent <= max_percent
            }
            (Self::StepVolumePercent { max_delta }, TypedAction::StepVolumePercent { delta }) => {
                self.is_valid() && action.is_valid() && delta.unsigned_abs() <= max_delta
            }
            (Self::SeekBy { max_delta_ms }, TypedAction::SeekBy { delta_ms }) => {
                self.is_valid()
                    && action.is_valid()
                    && delta_ms.unsigned_abs() <= u64::from(max_delta_ms)
            }
            // Only the modes the player declared, and only ever a mode it has:
            // `set_mode` names one field per mode, so this is the whole check.
            (Self::SetMode { modes }, TypedAction::SetMode { .. }) => {
                self.is_valid()
                    && action.is_valid()
                    && action.modes().iter().all(|mode| modes.contains(mode))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn measurements_keep_minimum_distinct_and_actions_are_typed() {
        assert_eq!(
            serde_json::to_value(VolumeDb::Minimum).unwrap(),
            json!({"kind":"minimum"})
        );
        assert_eq!(
            serde_json::to_value(VolumeDb::Reading { tenths: -345 }).unwrap(),
            json!({"kind":"reading","tenths":-345})
        );
        for value in [
            json!({"kind":"minimum","tenths":0}),
            json!({"kind":"reading","tenths":-34.5}),
            json!({"kind":"reading"}),
        ] {
            assert!(serde_json::from_value::<VolumeDb>(value).is_err());
        }
        for value in [
            json!({"action":"set_volume_db","tenths":-34.5}),
            json!({"action":"set_volume_db","tenths":"-345"}),
            json!({"action":"set_volume_db","tenths":-345,"command":"power-on"}),
            json!({"action":"anything","tenths":0}),
        ] {
            assert!(serde_json::from_value::<TypedAction>(value).is_err());
        }
        assert!(!VolumeDb::Reading { tenths: 301 }.is_valid());
    }

    #[test]
    fn a_set_of_actions_has_at_most_eight_valid_schemas_of_distinct_kinds() {
        let valid = PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        let other_bounds = PluginActionSchema::SetVolumeDb {
            min_tenths: -500,
            max_tenths: 0,
            step_tenths: 10,
        };
        let invalid = PluginActionSchema::SetVolumeDb {
            min_tenths: 0,
            max_tenths: 0,
            step_tenths: 5,
        };
        assert_eq!(valid.kind(), ActionKind::SetVolumeDb);
        assert_eq!(
            TypedAction::SetVolumeDb { tenths: -345 }.kind(),
            valid.kind()
        );
        assert!(PluginActionSchema::valid_set(&[]));
        assert!(PluginActionSchema::valid_set(&[valid]));
        assert!(!PluginActionSchema::valid_set(&[invalid]));
        assert!(
            !PluginActionSchema::valid_set(&[valid, other_bounds]),
            "one kind, one set of bounds: a request must never have two to choose from"
        );
        assert!(!PluginActionSchema::valid_set(&[valid; MAX_ACTIONS + 1]));
        assert_eq!(
            PluginActionSchema::find(&[valid], ActionKind::SetVolumeDb),
            Some(valid)
        );
        assert_eq!(PluginActionSchema::find(&[], ActionKind::SetVolumeDb), None);
    }

    #[test]
    fn child_actions_are_integers_say_something_and_stay_within_global_bounds() {
        fn assert_copy_eq<T: Copy + Eq>() {}
        assert_copy_eq::<TypedAction>();
        assert_copy_eq::<PluginActionSchema>();
        let light = |on, brightness, mirek, xy| TypedAction::SetLight {
            on,
            brightness,
            mirek,
            xy,
        };
        let climate = |target_tenths, low_tenths, high_tenths, mode| TypedAction::SetClimate {
            target_tenths,
            low_tenths,
            high_tenths,
            mode,
        };
        // An absent field is never written, so the frame is as small as the
        // change it asks for.
        assert_eq!(
            serde_json::to_value(light(None, Some(30), None, None)).unwrap(),
            json!({"action":"set_light","brightness":30})
        );
        assert_eq!(
            serde_json::to_value(light(Some(true), None, Some(370), Some((3127, 3290)))).unwrap(),
            json!({"action":"set_light","on":true,"mirek":370,"xy":[3127,3290]})
        );
        assert_eq!(
            serde_json::to_value(TypedAction::SetCover { position: 40 }).unwrap(),
            json!({"action":"set_cover","position":40})
        );
        assert_eq!(
            serde_json::to_value(climate(Some(215), None, None, Some(ClimateMode::HeatCool)))
                .unwrap(),
            json!({"action":"set_climate","target_tenths":215,"mode":"heat_cool"})
        );
        for action in [
            light(None, Some(30), None, None),
            light(Some(false), Some(0), Some(100), Some((0, 10_000))),
            TypedAction::SetCover { position: 100 },
            climate(None, Some(180), Some(240), None),
            climate(None, None, None, Some(ClimateMode::Off)),
        ] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(serde_json::from_str::<TypedAction>(&text).unwrap(), action);
            assert!(action.is_valid(), "{text}");
        }
        for value in [
            json!({"action":"set_light","brightness":30.5}),
            json!({"action":"set_light","brightness":"30"}),
            json!({"action":"set_light","brightness":256}),
            json!({"action":"set_light","brightness":30,"transition":4}),
            json!({"action":"set_light","xy":[0.3, 0.3]}),
            json!({"action":"set_light","xy":[1, 2, 3]}),
            json!({"action":"set_cover"}),
            json!({"action":"set_cover","position":40,"tilt":10}),
            json!({"action":"set_climate","target_tenths":21.5}),
            json!({"action":"set_climate","mode":"eco"}),
            json!({"action":"set_climate","mode":"heat","preset":"away"}),
        ] {
            assert!(
                serde_json::from_value::<TypedAction>(value.clone()).is_err(),
                "{value}"
            );
        }
        for (schema, action, accepted) in [
            (
                PluginActionSchema::SetLight {},
                light(None, None, None, None),
                false,
            ),
            (
                PluginActionSchema::SetLight {},
                light(None, Some(101), None, None),
                false,
            ),
            (
                PluginActionSchema::SetLight {},
                light(None, None, Some(99), None),
                false,
            ),
            (
                PluginActionSchema::SetLight {},
                light(None, None, Some(1001), None),
                false,
            ),
            (
                PluginActionSchema::SetLight {},
                light(None, None, None, Some((10_001, 0))),
                false,
            ),
            (
                PluginActionSchema::SetLight {},
                light(Some(true), None, None, None),
                true,
            ),
            (
                PluginActionSchema::SetLight {},
                light(None, Some(0), None, None),
                true,
            ),
            (
                PluginActionSchema::SetCover {},
                TypedAction::SetCover { position: 101 },
                false,
            ),
            (
                PluginActionSchema::SetCover {},
                TypedAction::SetCover { position: 0 },
                true,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(None, None, None, None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(Some(1501), None, None, None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(Some(-501), None, None, None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(None, Some(240), Some(240), None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(None, Some(250), Some(240), None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(None, Some(180), Some(240), None),
                true,
            ),
            (
                PluginActionSchema::SetClimate {},
                climate(Some(-500), None, None, None),
                true,
            ),
            // A schema answers only for its own kind of action.
            (
                PluginActionSchema::SetLight {},
                TypedAction::SetCover { position: 40 },
                false,
            ),
            (
                PluginActionSchema::SetCover {},
                light(Some(true), None, None, None),
                false,
            ),
            (
                PluginActionSchema::SetClimate {},
                TypedAction::SetVolumeDb { tenths: 0 },
                false,
            ),
            (
                PluginActionSchema::SetVolumeDb {
                    min_tenths: -800,
                    max_tenths: 180,
                    step_tenths: 5,
                },
                light(Some(true), None, None, None),
                false,
            ),
        ] {
            assert_eq!(schema.accepts(action), accepted, "{schema:?} {action:?}");
        }
    }

    #[test]
    fn the_schemas_without_numbers_still_refuse_an_unknown_field() {
        for (name, schema, kind) in [
            (
                "set_light",
                PluginActionSchema::SetLight {},
                ActionKind::SetLight,
            ),
            (
                "set_cover",
                PluginActionSchema::SetCover {},
                ActionKind::SetCover,
            ),
            (
                "set_climate",
                PluginActionSchema::SetClimate {},
                ActionKind::SetClimate,
            ),
        ] {
            assert_eq!(
                serde_json::to_value(schema).unwrap(),
                json!({"action": name})
            );
            assert_eq!(
                serde_json::from_value::<PluginActionSchema>(json!({"action": name})).unwrap(),
                schema
            );
            assert_eq!(schema.kind(), kind);
            assert!(schema.is_valid());
            // A unit variant would have let every one of these through.
            for extra in [
                json!({"action": name, "min_tenths": -800}),
                json!({"action": name, "transition": true}),
                json!({"action": name, "action2": "set_volume_db"}),
            ] {
                assert!(
                    serde_json::from_value::<PluginActionSchema>(extra.clone()).is_err(),
                    "{extra}"
                );
            }
        }
        let all = [
            PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5,
            },
            PluginActionSchema::SetLight {},
            PluginActionSchema::SetCover {},
            PluginActionSchema::SetClimate {},
        ];
        assert!(PluginActionSchema::valid_set(&all));
        assert!(!PluginActionSchema::valid_set(&[
            PluginActionSchema::SetLight {},
            PluginActionSchema::SetLight {}
        ]));
        assert_eq!(
            PluginActionSchema::find(&all, ActionKind::SetCover),
            Some(PluginActionSchema::SetCover {})
        );
    }

    #[test]
    fn a_set_of_play_modes_is_written_in_one_order_and_never_holds_a_name_twice() {
        fn assert_copy_eq<T: Copy + Eq>() {}
        assert_copy_eq::<PlayModeSet>();
        let set = PlayModeSet::new()
            .with(PlayMode::Crossfade)
            .with(PlayMode::Repeat)
            .with(PlayMode::Shuffle);
        assert_eq!(set.len(), 3);
        assert!(!set.is_empty());
        assert!(PlayModeSet::new().is_empty());
        assert!(set.contains(PlayMode::Repeat));
        assert!(!set.contains(PlayMode::RepeatOne));
        // Built in one order, written in the declared one.
        assert_eq!(
            serde_json::to_value(set).unwrap(),
            json!(["shuffle", "repeat", "crossfade"])
        );
        assert_eq!(
            serde_json::from_value::<PlayModeSet>(json!(["crossfade", "shuffle", "repeat"]))
                .unwrap(),
            set
        );
        assert_eq!(serde_json::to_value(PlayModeSet::new()).unwrap(), json!([]));
        for value in [
            json!(["shuffle", "shuffle"]),
            json!(["repeat", "repeat_one", "repeat"]),
            json!(["gapless"]),
            json!("shuffle"),
            json!({"shuffle": true}),
        ] {
            assert!(
                serde_json::from_value::<PlayModeSet>(value.clone()).is_err(),
                "{value}"
            );
        }
        // Every declared mode is reachable, and its own name.
        for mode in ALL_PLAY_MODES {
            let one = PlayModeSet::new().with(mode);
            assert_eq!(one.len(), 1);
            assert_eq!(one.iter().next(), Some(mode));
            assert_eq!(
                serde_json::to_value(one).unwrap(),
                json!([serde_json::to_value(mode).unwrap()])
            );
        }
    }

    #[test]
    fn media_actions_are_integers_say_something_and_stay_within_global_bounds() {
        fn assert_copy_eq<T: Copy + Eq>() {}
        assert_copy_eq::<TypedAction>();
        assert_copy_eq::<PluginActionSchema>();
        let mode = |shuffle, repeat, repeat_one, crossfade| TypedAction::SetMode {
            shuffle,
            repeat,
            repeat_one,
            crossfade,
        };
        // An absent field is never written, so the frame is as small as the
        // change it asks for.
        assert_eq!(
            serde_json::to_value(TypedAction::SetVolumePercent { percent: 30 }).unwrap(),
            json!({"action":"set_volume_percent","percent":30})
        );
        assert_eq!(
            serde_json::to_value(TypedAction::StepVolumePercent { delta: -5 }).unwrap(),
            json!({"action":"step_volume_percent","delta":-5})
        );
        assert_eq!(
            serde_json::to_value(TypedAction::Seek {
                position_ms: 125_000
            })
            .unwrap(),
            json!({"action":"seek","position_ms":125000})
        );
        assert_eq!(
            serde_json::to_value(TypedAction::SeekBy { delta_ms: -30_000 }).unwrap(),
            json!({"action":"seek_by","delta_ms":-30000})
        );
        assert_eq!(
            serde_json::to_value(mode(None, Some(true), Some(false), None)).unwrap(),
            json!({"action":"set_mode","repeat":true,"repeat_one":false})
        );
        for action in [
            TypedAction::SetVolumePercent { percent: 0 },
            TypedAction::SetVolumePercent { percent: 100 },
            TypedAction::StepVolumePercent { delta: 50 },
            TypedAction::StepVolumePercent { delta: -50 },
            TypedAction::Seek { position_ms: 0 },
            TypedAction::Seek {
                position_ms: MAX_MEDIA_MS,
            },
            TypedAction::SeekBy { delta_ms: 1 },
            TypedAction::SeekBy {
                delta_ms: -(MAX_SEEK_DELTA_MS as i64),
            },
            mode(Some(true), None, None, None),
            mode(None, None, None, Some(false)),
        ] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(serde_json::from_str::<TypedAction>(&text).unwrap(), action);
            assert!(action.is_valid(), "{text}");
        }
        for action in [
            TypedAction::SetVolumePercent { percent: 101 },
            // Never 0: a step or a jump that moves nothing is a mistake.
            TypedAction::StepVolumePercent { delta: 0 },
            TypedAction::StepVolumePercent { delta: 51 },
            TypedAction::StepVolumePercent { delta: -51 },
            TypedAction::Seek {
                position_ms: MAX_MEDIA_MS + 1,
            },
            TypedAction::SeekBy { delta_ms: 0 },
            TypedAction::SeekBy {
                delta_ms: MAX_SEEK_DELTA_MS as i64 + 1,
            },
            TypedAction::SeekBy { delta_ms: i64::MIN },
            mode(None, None, None, None),
        ] {
            assert!(!action.is_valid(), "{action:?}");
        }
        for value in [
            json!({"action":"set_volume_percent","percent":30.5}),
            json!({"action":"set_volume_percent","percent":"30"}),
            json!({"action":"set_volume_percent","percent":256}),
            json!({"action":"set_volume_percent","percent":30,"resource":"a"}),
            json!({"action":"set_volume_percent"}),
            json!({"action":"step_volume_percent","delta":128}),
            json!({"action":"seek","position_ms":-1}),
            json!({"action":"seek","position_ms":1000,"item":"a"}),
            json!({"action":"seek_by","delta_ms":1.5}),
            json!({"action":"set_mode","shuffle":"yes"}),
            json!({"action":"set_mode","gapless":true}),
        ] {
            assert!(
                serde_json::from_value::<TypedAction>(value.clone()).is_err(),
                "{value}"
            );
        }
        // `seek` declares nothing but that it exists, so it is an empty struct
        // variant: a unit variant would let a bound through unnoticed.
        assert_eq!(
            serde_json::to_value(PluginActionSchema::Seek {}).unwrap(),
            json!({"action":"seek"})
        );
        for extra in [
            json!({"action":"seek","max_delta_ms":1000}),
            json!({"action":"seek","position_ms":0}),
        ] {
            assert!(
                serde_json::from_value::<PluginActionSchema>(extra.clone()).is_err(),
                "{extra}"
            );
        }
        assert_eq!(
            serde_json::to_value(PluginActionSchema::SetMode {
                modes: PlayModeSet::new().with(PlayMode::Shuffle),
            })
            .unwrap(),
            json!({"action":"set_mode","modes":["shuffle"]})
        );
        for schema in [
            PluginActionSchema::SetVolumePercent { max_percent: 0 },
            PluginActionSchema::SetVolumePercent { max_percent: 101 },
            PluginActionSchema::StepVolumePercent { max_delta: 0 },
            PluginActionSchema::StepVolumePercent { max_delta: 51 },
            PluginActionSchema::SeekBy { max_delta_ms: 999 },
            PluginActionSchema::SeekBy {
                max_delta_ms: 3_600_001,
            },
            PluginActionSchema::SetMode {
                modes: PlayModeSet::new(),
            },
        ] {
            assert!(!schema.is_valid(), "{schema:?}");
            assert!(!PluginActionSchema::valid_set(&[schema]), "{schema:?}");
        }
    }

    #[test]
    fn a_media_schema_answers_only_for_its_own_kind_and_its_own_bounds() {
        let percent = PluginActionSchema::SetVolumePercent { max_percent: 60 };
        let step = PluginActionSchema::StepVolumePercent { max_delta: 5 };
        let seek = PluginActionSchema::Seek {};
        let seek_by = PluginActionSchema::SeekBy {
            max_delta_ms: 30_000,
        };
        let modes = PluginActionSchema::SetMode {
            modes: PlayModeSet::new()
                .with(PlayMode::Shuffle)
                .with(PlayMode::Repeat),
        };
        let mode = |shuffle, repeat, repeat_one| TypedAction::SetMode {
            shuffle,
            repeat,
            repeat_one,
            crossfade: None,
        };
        for (schema, action, accepted) in [
            (percent, TypedAction::SetVolumePercent { percent: 60 }, true),
            (percent, TypedAction::SetVolumePercent { percent: 0 }, true),
            (
                percent,
                TypedAction::SetVolumePercent { percent: 61 },
                false,
            ),
            (
                PluginActionSchema::SetVolumePercent { max_percent: 0 },
                TypedAction::SetVolumePercent { percent: 0 },
                false,
            ),
            (step, TypedAction::StepVolumePercent { delta: 5 }, true),
            (step, TypedAction::StepVolumePercent { delta: -5 }, true),
            (step, TypedAction::StepVolumePercent { delta: 6 }, false),
            (step, TypedAction::StepVolumePercent { delta: 0 }, false),
            (seek, TypedAction::Seek { position_ms: 0 }, true),
            (
                seek,
                TypedAction::Seek {
                    position_ms: MAX_MEDIA_MS + 1,
                },
                false,
            ),
            (seek_by, TypedAction::SeekBy { delta_ms: -30_000 }, true),
            (seek_by, TypedAction::SeekBy { delta_ms: 30_001 }, false),
            (modes, mode(Some(true), None, None), true),
            (modes, mode(Some(false), Some(true), None), true),
            // A mode the player never declared is refused before any I/O.
            (modes, mode(None, None, Some(true)), false),
            (modes, mode(None, None, None), false),
            // Each schema answers for one kind only.
            (percent, TypedAction::StepVolumePercent { delta: 5 }, false),
            (seek, TypedAction::SeekBy { delta_ms: 1_000 }, false),
            (seek_by, TypedAction::Seek { position_ms: 0 }, false),
            (modes, TypedAction::SetVolumePercent { percent: 1 }, false),
            (
                PluginActionSchema::SetVolumeDb {
                    min_tenths: -800,
                    max_tenths: 180,
                    step_tenths: 5,
                },
                TypedAction::SetVolumePercent { percent: 30 },
                false,
            ),
            (percent, TypedAction::SetVolumeDb { tenths: -345 }, false),
        ] {
            assert_eq!(schema.accepts(action), accepted, "{schema:?} {action:?}");
            assert_eq!(schema.kind(), schema.kind());
        }
        // Nine kinds now exist and one package may declare eight of them.
        let all = [
            percent,
            step,
            seek,
            seek_by,
            modes,
            PluginActionSchema::SetLight {},
            PluginActionSchema::SetCover {},
            PluginActionSchema::SetClimate {},
        ];
        assert_eq!(all.len(), MAX_ACTIONS);
        assert!(PluginActionSchema::valid_set(&all));
        for schema in all {
            assert_eq!(
                PluginActionSchema::find(&all, schema.kind()),
                Some(schema),
                "{schema:?}"
            );
        }
        assert!(!PluginActionSchema::valid_set(&[percent, percent]));
    }

    #[test]
    fn action_bounds_reject_out_of_range_steps_and_integer_overflow() {
        let schema = PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        for tenths in [-800, -345, 0, 180] {
            assert!(schema.accepts(TypedAction::SetVolumeDb { tenths }));
        }
        for tenths in [i16::MIN, -805, -344, 181, i16::MAX] {
            assert!(!schema.accepts(TypedAction::SetVolumeDb { tenths }));
        }
        for (min, max, step) in [
            (-800, 180, 0),
            (-800, 180, 3),
            (-800, 180, u16::MAX),
            (0, 0, 5),
            (180, -800, 5),
            (i16::MIN, i16::MAX, 5),
            (-1001, 300, 1),
            (-1000, 301, 1),
        ] {
            assert!(!valid_volume_bounds(min, max, step));
        }
    }
}
