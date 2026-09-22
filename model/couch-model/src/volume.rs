//! Bounded protocol-v2 decibel measurements and actions. Percentage volume is
//! intentionally a separate quantity; minimum is not a fabricated reading.
//!
//! Protocol 3 (unreleased) adds three typed actions for the children of a
//! connection: a light, a cover and a thermostat ([`crate::domain`]).
use crate::domain::{
    ClimateMode, MAX_CLIMATE_TENTHS, MAX_MIREK, MAX_PERCENT, MAX_XY, MIN_CLIMATE_TENTHS, MIN_MIREK,
};
use serde::{Deserialize, Serialize};

pub const MIN_VOLUME_TENTHS: i16 = -1000;
pub const MAX_VOLUME_TENTHS: i16 = 300;

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
        }
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
}
impl PluginActionSchema {
    pub fn kind(self) -> ActionKind {
        match self {
            Self::SetVolumeDb { .. } => ActionKind::SetVolumeDb,
            Self::SetLight {} => ActionKind::SetLight,
            Self::SetCover {} => ActionKind::SetCover,
            Self::SetClimate {} => ActionKind::SetClimate,
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
            Self::SetLight {} | Self::SetCover {} | Self::SetClimate {} => true,
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
            | (Self::SetClimate {}, TypedAction::SetClimate { .. }) => action.is_valid(),
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
