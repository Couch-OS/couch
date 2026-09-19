//! Bounded protocol-v2 decibel measurements and actions. Percentage volume is
//! intentionally a separate quantity; minimum is not a fabricated reading.
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
    SetVolumeDb { tenths: i16 },
}

/// Which typed action a request or a declaration is, without its numbers. A
/// package declares at most one schema of each kind, so the kind is how a
/// request finds the bounds it is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    SetVolumeDb,
}
/// How many typed actions one package may declare (protocol 3, unreleased;
/// protocols 1 and 2 stay at none and one).
pub const MAX_ACTIONS: usize = 8;
impl TypedAction {
    pub fn kind(self) -> ActionKind {
        match self {
            Self::SetVolumeDb { .. } => ActionKind::SetVolumeDb,
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
}
impl PluginActionSchema {
    pub fn kind(self) -> ActionKind {
        match self {
            Self::SetVolumeDb { .. } => ActionKind::SetVolumeDb,
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
        }
    }
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
