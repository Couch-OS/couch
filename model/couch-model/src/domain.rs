//! Protocol 3 (unreleased): what a light, a blind and a thermostat are to
//! Couch when a package drives them, and how one connection names its many
//! children.
//!
//! Traits say what one particular lamp or blind can do. They arrive with the
//! child when it is listed and are saved with the room device, so a binding
//! validates and the panel draws the right row while the package is not
//! running. State is what a status read reports; `None` is "unknown", never an
//! inferred "off". Everything is an integer so the types stay `Eq`: brightness
//! and position are percentages, colour temperature is in mirek, colour is CIE
//! xy in ten-thousandths, temperatures are tenths of a degree.
//!
//! Nothing here reaches a protocol 2 core: see `storage::v2_projection`.
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Brightness and cover position are percentages.
pub const MAX_PERCENT: u8 = 100;
/// Colour temperature in mirek: 10 000 K down to 1000 K. Hue lamps span
/// 153..=500.
pub const MIN_MIREK: u16 = 100;
pub const MAX_MIREK: u16 = 1000;
/// CIE xy in ten-thousandths: 0.0 to 1.0.
pub const MAX_XY: u16 = 10_000;
/// Temperatures and set points in tenths of a degree, in either unit.
pub const MIN_CLIMATE_TENTHS: i16 = -500;
pub const MAX_CLIMATE_TENTHS: i16 = 1500;
/// How many kinds of child one package may declare.
pub const MAX_CHILD_KINDS: usize = 8;
/// How many commands one kind of child may declare.
pub const MAX_CHILD_CAPABILITIES: usize = 32;

/// The name of one child of a connection, as the package spells it: 1 to 128
/// bytes of `[A-Za-z0-9._/+-]`, read as segments between `/`, none of them
/// empty, `.` or `..`. Hue uses a bare light UUID, `room/<uuid>` and
/// `scene/<uuid>`. The id travels in URLs and cache keys, so it can never climb
/// out of its connection.
///
/// A device that is *not* a child keeps the older, looser rule (the same
/// alphabet, empty allowed), so files saved with `zone1` or nothing keep
/// loading.
pub fn valid_resource(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/+-".contains(&b))
        && id
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn percent(value: Option<u8>) -> bool {
    value.is_none_or(|v| v <= MAX_PERCENT)
}
fn mirek(value: Option<u16>) -> bool {
    value.is_none_or(|v| (MIN_MIREK..=MAX_MIREK).contains(&v))
}
fn xy(value: Option<(u16, u16)>) -> bool {
    value.is_none_or(|(x, y)| x <= MAX_XY && y <= MAX_XY)
}
fn tenths(value: Option<i16>) -> bool {
    value.is_none_or(|v| (MIN_CLIMATE_TENTHS..=MAX_CLIMATE_TENTHS).contains(&v))
}

/// What this particular lamp can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightTraits {
    pub dimmable: bool,
    /// The colour temperature range in mirek, coolest first, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirek: Option<(u16, u16)>,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub color: bool,
}
impl LightTraits {
    pub fn is_valid(&self) -> bool {
        self.mirek
            .is_none_or(|(low, high)| low <= high && mirek(Some(low)) && mirek(Some(high)))
    }
}

/// What a lamp reports. Brightness is kept while it is off: it is the level
/// the lamp returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirek: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xy: Option<(u16, u16)>,
}
impl LightState {
    pub fn is_valid(&self) -> bool {
        percent(self.brightness) && mirek(self.mirek) && xy(self.xy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverTraits {
    /// It can be sent to a position, not only opened and closed.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub position: bool,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub stop: bool,
}

/// 0 is closed and 100 is open, as `couch-ha` has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u8>,
}
impl CoverState {
    pub fn is_valid(&self) -> bool {
        percent(self.position)
    }
}

/// A thermostat's operating mode: the words of [`crate::commands::HVAC_MODES`],
/// so `mode:heat_cool` and `ClimateMode::HeatCool` are the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClimateMode {
    Off,
    Heat,
    Cool,
    HeatCool,
    Auto,
    Dry,
    FanOnly,
}
pub const ALL_CLIMATE_MODES: &[ClimateMode] = &[
    ClimateMode::Off,
    ClimateMode::Heat,
    ClimateMode::Cool,
    ClimateMode::HeatCool,
    ClimateMode::Auto,
    ClimateMode::Dry,
    ClimateMode::FanOnly,
];
impl ClimateMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Heat => "heat",
            Self::Cool => "cool",
            Self::HeatCool => "heat_cool",
            Self::Auto => "auto",
            Self::Dry => "dry",
            Self::FanOnly => "fan_only",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        ALL_CLIMATE_MODES
            .iter()
            .copied()
            .find(|mode| mode.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

/// What this particular thermostat accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClimateTraits {
    pub min_tenths: i16,
    pub max_tenths: i16,
    pub step_tenths: u16,
    #[serde(default)]
    pub unit: TempUnit,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ClimateMode>,
    /// It holds a low and a high set point at once (heat/cool).
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub range: bool,
}
impl ClimateTraits {
    pub fn is_valid(&self) -> bool {
        tenths(Some(self.min_tenths))
            && tenths(Some(self.max_tenths))
            && self.min_tenths < self.max_tenths
            && self.step_tenths > 0
            && i32::from(self.step_tenths)
                <= i32::from(self.max_tenths) - i32::from(self.min_tenths)
            && self.modes.len() <= ALL_CLIMATE_MODES.len()
            && self
                .modes
                .iter()
                .enumerate()
                .all(|(index, mode)| !self.modes[..index].contains(mode))
    }
    fn holds(&self, value: Option<i16>) -> bool {
        value.is_none_or(|v| (self.min_tenths..=self.max_tenths).contains(&v))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClimateState {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ClimateMode>,
    /// Whether it is calling for heat right now, where the device says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_tenths: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_tenths: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_tenths: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_tenths: Option<i16>,
}
impl ClimateState {
    pub fn is_valid(&self) -> bool {
        tenths(self.current_tenths)
            && tenths(self.target_tenths)
            && tenths(self.low_tenths)
            && tenths(self.high_tenths)
    }
}

/// Which built-in control a kind of child is drawn and driven with. A scene is
/// not a device: it attaches to a room's Scenes button ([`crate::Scene`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildComponent {
    Light,
    Scene,
    Cover,
    Climate,
}
impl ChildComponent {
    /// The one typed action a child of this component may declare.
    pub fn action(self) -> Option<crate::ActionKind> {
        match self {
            Self::Light => Some(crate::ActionKind::SetLight),
            Self::Cover => Some(crate::ActionKind::SetCover),
            Self::Climate => Some(crate::ActionKind::SetClimate),
            Self::Scene => None,
        }
    }
    /// The kinds of room device a child of this component may be saved as.
    pub fn device_kinds(self) -> &'static [crate::DeviceKind] {
        match self {
            Self::Light => &[crate::DeviceKind::Light, crate::DeviceKind::Switch],
            Self::Cover => &[crate::DeviceKind::Blind],
            Self::Climate => &[crate::DeviceKind::Thermostat],
            Self::Scene => &[crate::DeviceKind::Other],
        }
    }
}

/// One kind of child a package declares, copied from its manifest and saved
/// with the connection: what a child of this kind is, and what it can be told,
/// known without the package running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChildKind {
    pub kind: String,
    pub label: String,
    pub device_kind: crate::DeviceKind,
    pub component: ChildComponent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<crate::PluginCapability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<crate::PluginActionSchema>,
}
impl PluginChildKind {
    /// The rules a manifest's `children` entry is held to, one kind at a time.
    /// Uniqueness of `kind` and the package-wide limit on `x:` ids belong to
    /// the list: see [`valid_child_kinds`].
    pub fn is_valid(&self) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        valid_kind_id(&self.kind)
            && valid_label(&self.label)
            && self.component.device_kinds().contains(&self.device_kind)
            && self.capabilities.len() <= MAX_CHILD_CAPABILITIES
            && self.capabilities.iter().all(|capability| {
                let fresh = !seen.contains(&capability.id.as_str());
                seen.push(&capability.id);
                fresh
                    && valid_label(&capability.label)
                    && !capability.id.starts_with("input:")
                    && !capability.id.starts_with("app:")
                    && crate::commands::Function::parse(&capability.id).is_some()
            })
            && crate::PluginActionSchema::valid_set(&self.actions)
            && self
                .actions
                .iter()
                .all(|schema| Some(schema.kind()) == self.component.action())
            && (self.component != ChildComponent::Scene
                || (self.capabilities.len() == 1 && self.capabilities[0].id == "on"))
    }
}

/// At most [`MAX_CHILD_KINDS`], each valid, no kind declared twice.
pub fn valid_child_kinds(children: &[PluginChildKind]) -> bool {
    children.len() <= MAX_CHILD_KINDS
        && children.iter().enumerate().all(|(index, kind)| {
            kind.is_valid()
                && children[..index]
                    .iter()
                    .all(|earlier| earlier.kind != kind.kind)
        })
}

/// What is saved with a room device that is a child: its kind, and what this
/// particular one can do. The kind says which commands exist
/// ([`PluginChildKind`]); the traits say how far they go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildSnapshot {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<LightTraits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverTraits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climate: Option<ClimateTraits>,
}
impl ChildSnapshot {
    /// Traits only of the component the kind is drawn with, and within the
    /// global bounds. A child with no traits at all is one that does nothing
    /// beyond its kind's commands.
    pub fn fits(&self, component: ChildComponent) -> bool {
        (self.light.is_none() || component == ChildComponent::Light)
            && (self.cover.is_none() || component == ChildComponent::Cover)
            && (self.climate.is_none() || component == ChildComponent::Climate)
            && self.light.is_none_or(|traits| traits.is_valid())
            && self.climate.as_ref().is_none_or(|traits| traits.is_valid())
    }

    /// Whether this particular child can carry out a typed action that is
    /// already within the global bounds ([`crate::TypedAction::is_valid`]):
    /// a lamp that cannot be dimmed refuses a brightness, a thermostat refuses
    /// a set point outside its range or a mode it does not have.
    pub fn accepts(&self, action: crate::TypedAction) -> bool {
        use crate::TypedAction;
        if !action.is_valid() {
            return false;
        }
        match action {
            TypedAction::SetVolumeDb { .. } => false,
            TypedAction::SetLight {
                brightness,
                mirek,
                xy,
                ..
            } => self.light.is_some_and(|traits| {
                (brightness.is_none() || traits.dimmable)
                    && mirek.is_none_or(|value| {
                        traits
                            .mirek
                            .is_some_and(|(low, high)| (low..=high).contains(&value))
                    })
                    && (xy.is_none() || traits.color)
            }),
            TypedAction::SetCover { .. } => self.cover.is_some_and(|traits| traits.position),
            TypedAction::SetClimate {
                target_tenths,
                low_tenths,
                high_tenths,
                mode,
            } => self.climate.as_ref().is_some_and(|traits| {
                traits.holds(target_tenths)
                    && traits.holds(low_tenths)
                    && traits.holds(high_tenths)
                    && ((low_tenths.is_none() && high_tenths.is_none()) || traits.range)
                    && mode.is_none_or(|mode| traits.modes.contains(&mode))
            }),
        }
    }
}

/// A scene that belongs to a package: recalled by sending `on` to that child.
/// `kind` is the scene kind the package declared, saved for the same reason a
/// device saves its snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneResource {
    pub connection_id: crate::Id,
    pub resource_id: String,
    pub kind: String,
}

/// A child kind's name: 1 to 64 bytes of lowercase letters, digits, `-`, `_`.
pub fn valid_kind_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginActionSchema, PluginCapability, TypedAction};
    use alloc::vec;
    use serde_json::json;

    #[test]
    fn a_resource_is_segments_that_never_climb() {
        for id in [
            "5f0c9a52-7d1e-4a63-9b0e-2f6d1c3a8e41",
            "room/9d2b7c10-35aa-4c0e-8a57-6e1f0b94d2c3",
            "scene/x",
            "a",
            "a.b/c+d/e-f",
            "...",
            "light.living_room",
            &"a".repeat(128),
        ] {
            assert!(valid_resource(id), "{id}");
        }
        for id in [
            "",
            "/",
            "/a",
            "a/",
            "a//b",
            ".",
            "..",
            "../x",
            "a/../b",
            "a/./b",
            "a/..",
            "room:uuid",
            "with space",
            "a*b",
            "é",
            "a\n",
            &"a".repeat(129),
        ] {
            assert!(!valid_resource(id), "{id:?}");
        }
    }

    #[test]
    fn climate_modes_are_the_words_the_mode_command_already_parses() {
        let names: Vec<&str> = ALL_CLIMATE_MODES.iter().map(|m| m.name()).collect();
        assert_eq!(names, crate::commands::HVAC_MODES);
        for mode in ALL_CLIMATE_MODES {
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(mode.name()));
            assert_eq!(ClimateMode::from_name(mode.name()), Some(*mode));
            assert!(
                crate::commands::Function::parse(&alloc::format!("mode:{}", mode.name())).is_some()
            );
        }
        assert_eq!(ClimateMode::from_name("eco"), None);
    }

    #[test]
    fn traits_and_state_are_small_strict_and_bounded() {
        assert_eq!(
            serde_json::to_value(LightTraits {
                dimmable: true,
                mirek: None,
                color: false
            })
            .unwrap(),
            json!({"dimmable": true})
        );
        assert_eq!(
            serde_json::from_value::<LightTraits>(json!({"dimmable": true, "mirek": [153, 500]}))
                .unwrap(),
            LightTraits {
                dimmable: true,
                mirek: Some((153, 500)),
                color: false
            }
        );
        assert_eq!(
            serde_json::to_value(LightState::default()).unwrap(),
            json!({})
        );
        for value in [
            json!({"dimmable": true, "colour": true}),
            json!({"dimmable": 1}),
            json!({}),
        ] {
            assert!(serde_json::from_value::<LightTraits>(value).is_err());
        }
        assert!(serde_json::from_value::<LightState>(json!({"on": true, "level": 3})).is_err());
        assert!(serde_json::from_value::<LightState>(json!({"brightness": 30.5})).is_err());
        for traits in [(99, 500), (153, 1001), (500, 153)] {
            assert!(!LightTraits {
                dimmable: true,
                mirek: Some(traits),
                color: false
            }
            .is_valid());
        }
        assert!(LightState {
            on: Some(true),
            brightness: Some(100),
            mirek: Some(1000),
            xy: Some((10_000, 0))
        }
        .is_valid());
        for state in [
            LightState {
                brightness: Some(101),
                ..Default::default()
            },
            LightState {
                mirek: Some(99),
                ..Default::default()
            },
            LightState {
                xy: Some((0, 10_001)),
                ..Default::default()
            },
        ] {
            assert!(!state.is_valid());
        }
        assert!(!CoverState {
            open: None,
            position: Some(101)
        }
        .is_valid());
        assert!(!ClimateState {
            available: true,
            target_tenths: Some(1501),
            ..Default::default()
        }
        .is_valid());
        let climate = ClimateTraits {
            min_tenths: 70,
            max_tenths: 300,
            step_tenths: 5,
            unit: TempUnit::Celsius,
            modes: vec![ClimateMode::Off, ClimateMode::Heat],
            range: false,
        };
        assert!(climate.is_valid());
        for broken in [
            ClimateTraits {
                min_tenths: 300,
                max_tenths: 70,
                ..climate.clone()
            },
            ClimateTraits {
                step_tenths: 0,
                ..climate.clone()
            },
            ClimateTraits {
                step_tenths: 231,
                ..climate.clone()
            },
            ClimateTraits {
                min_tenths: -501,
                ..climate.clone()
            },
            ClimateTraits {
                max_tenths: 1501,
                ..climate.clone()
            },
            ClimateTraits {
                modes: vec![ClimateMode::Heat, ClimateMode::Heat],
                ..climate.clone()
            },
        ] {
            assert!(!broken.is_valid(), "{broken:?}");
        }
    }

    fn kind(component: ChildComponent, device_kind: crate::DeviceKind) -> PluginChildKind {
        PluginChildKind {
            kind: "thing".into(),
            label: "Thing".into(),
            device_kind,
            component,
            capabilities: vec![PluginCapability {
                id: "on".into(),
                label: "On".into(),
            }],
            actions: vec![],
        }
    }

    #[test]
    fn a_kind_pairs_its_component_with_a_device_kind_and_one_action() {
        use crate::DeviceKind;
        for (component, accepted, action) in [
            (
                ChildComponent::Light,
                &[DeviceKind::Light, DeviceKind::Switch][..],
                Some(PluginActionSchema::SetLight {}),
            ),
            (
                ChildComponent::Cover,
                &[DeviceKind::Blind][..],
                Some(PluginActionSchema::SetCover {}),
            ),
            (
                ChildComponent::Climate,
                &[DeviceKind::Thermostat][..],
                Some(PluginActionSchema::SetClimate {}),
            ),
            (ChildComponent::Scene, &[DeviceKind::Other][..], None),
        ] {
            for device_kind in crate::ALL_DEVICE_KINDS {
                assert_eq!(
                    kind(component, *device_kind).is_valid(),
                    accepted.contains(device_kind),
                    "{component:?} as {device_kind}"
                );
            }
            for schema in [
                PluginActionSchema::SetLight {},
                PluginActionSchema::SetCover {},
                PluginActionSchema::SetClimate {},
                PluginActionSchema::SetVolumeDb {
                    min_tenths: -800,
                    max_tenths: 180,
                    step_tenths: 5,
                },
            ] {
                let mut declared = kind(component, accepted[0]);
                declared.actions = vec![schema];
                assert_eq!(
                    declared.is_valid(),
                    Some(schema) == action,
                    "{component:?} {schema:?}"
                );
            }
        }
        // A scene is recalled with `on` and nothing else.
        let mut scene = kind(ChildComponent::Scene, DeviceKind::Other);
        scene.capabilities.push(PluginCapability {
            id: "off".into(),
            label: "Off".into(),
        });
        assert!(!scene.is_valid());
        scene.capabilities.clear();
        assert!(!scene.is_valid());

        let light = kind(ChildComponent::Light, DeviceKind::Light);
        for (id, label) in [
            ("input:hdmi1", "Input"),
            ("app:netflix", "App"),
            ("nonsense", "Nonsense"),
            ("on", "Twice"),
            ("off", ""),
        ] {
            let mut broken = light.clone();
            broken.capabilities.push(PluginCapability {
                id: id.into(),
                label: label.into(),
            });
            assert!(!broken.is_valid(), "{id}");
        }
        let mut own = light.clone();
        own.capabilities.push(PluginCapability {
            id: "x:blink".into(),
            label: "Blink".into(),
        });
        assert!(own.is_valid());
        let mut many = light.clone();
        many.capabilities = (0..=MAX_CHILD_CAPABILITIES)
            .map(|n| PluginCapability {
                id: alloc::format!("x:c{n}"),
                label: "Own".into(),
            })
            .collect();
        assert!(!many.is_valid());
        for id in ["", "Light", "a b", "light/2", &"a".repeat(65)] {
            let mut broken = light.clone();
            broken.kind = id.into();
            assert!(!broken.is_valid(), "{id:?}");
        }

        assert!(valid_child_kinds(&[]));
        assert!(!valid_child_kinds(&[light.clone(), light.clone()]));
        let many: Vec<PluginChildKind> = (0..=MAX_CHILD_KINDS)
            .map(|n| PluginChildKind {
                kind: alloc::format!("k{n}"),
                ..light.clone()
            })
            .collect();
        assert!(valid_child_kinds(&many[..MAX_CHILD_KINDS]));
        assert!(!valid_child_kinds(&many));
        assert!(
            serde_json::from_value::<PluginChildKind>(json!({
                "kind": "light", "label": "Light", "device_kind": "light",
                "component": "light", "colour": true
            }))
            .is_err(),
            "an unknown field in a kind is refused, not ignored"
        );
    }

    #[test]
    fn a_snapshot_carries_only_its_own_traits_and_answers_for_this_child() {
        let lamp = ChildSnapshot {
            kind: "light".into(),
            light: Some(LightTraits {
                dimmable: true,
                mirek: Some((153, 500)),
                color: false,
            }),
            cover: None,
            climate: None,
        };
        assert_eq!(
            serde_json::to_value(&lamp).unwrap(),
            json!({"kind": "light", "light": {"dimmable": true, "mirek": [153, 500]}})
        );
        assert!(
            serde_json::from_value::<ChildSnapshot>(json!({"kind": "light", "name": "Desk"}))
                .is_err()
        );
        assert!(lamp.fits(ChildComponent::Light));
        assert!(!lamp.fits(ChildComponent::Cover));
        assert!(!lamp.fits(ChildComponent::Scene));
        let bare = ChildSnapshot {
            kind: "scene".into(),
            light: None,
            cover: None,
            climate: None,
        };
        for component in [
            ChildComponent::Light,
            ChildComponent::Scene,
            ChildComponent::Cover,
            ChildComponent::Climate,
        ] {
            assert!(bare.fits(component));
        }
        let light = |brightness, mirek, xy| TypedAction::SetLight {
            on: None,
            brightness,
            mirek,
            xy,
        };
        assert!(lamp.accepts(light(Some(30), None, None)));
        assert!(lamp.accepts(light(None, Some(153), None)));
        assert!(!lamp.accepts(light(None, Some(152), None)));
        assert!(!lamp.accepts(light(None, None, Some((3000, 3000)))));
        assert!(!lamp.accepts(light(Some(101), None, None)));
        assert!(!lamp.accepts(TypedAction::SetCover { position: 40 }));
        assert!(!bare.accepts(light(Some(30), None, None)));
        let plug = ChildSnapshot {
            light: Some(LightTraits::default()),
            ..lamp.clone()
        };
        assert!(!plug.accepts(light(Some(30), None, None)));
        assert!(plug.accepts(TypedAction::SetLight {
            on: Some(true),
            brightness: None,
            mirek: None,
            xy: None
        }));

        let blind = |position| ChildSnapshot {
            kind: "blind".into(),
            light: None,
            cover: Some(CoverTraits {
                position,
                stop: true,
            }),
            climate: None,
        };
        assert!(blind(true).accepts(TypedAction::SetCover { position: 40 }));
        assert!(!blind(false).accepts(TypedAction::SetCover { position: 40 }));

        let thermostat = |range| ChildSnapshot {
            kind: "thermostat".into(),
            light: None,
            cover: None,
            climate: Some(ClimateTraits {
                min_tenths: 70,
                max_tenths: 300,
                step_tenths: 5,
                unit: TempUnit::Celsius,
                modes: vec![ClimateMode::Off, ClimateMode::Heat],
                range,
            }),
        };
        let climate = |target_tenths, low_tenths, high_tenths, mode| TypedAction::SetClimate {
            target_tenths,
            low_tenths,
            high_tenths,
            mode,
        };
        assert!(thermostat(false).accepts(climate(Some(215), None, None, None)));
        assert!(!thermostat(false).accepts(climate(Some(305), None, None, None)));
        assert!(!thermostat(false).accepts(climate(None, Some(180), Some(240), None)));
        assert!(thermostat(true).accepts(climate(None, Some(180), Some(240), None)));
        assert!(thermostat(false).accepts(climate(None, None, None, Some(ClimateMode::Heat))));
        assert!(!thermostat(false).accepts(climate(None, None, None, Some(ClimateMode::Cool))));
    }
}
