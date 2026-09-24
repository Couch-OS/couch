use crate::{accepted_protocol_version, Error, Reason, Result, NEXT_PROTOCOL_VERSION};
use couch_sdk::{
    couch_model::{
        commands::{valid_input_id, Function, MAX_CUSTOM_FUNCTIONS},
        domain::valid_child_kinds,
        ActionKind, ChildComponent, PluginActionSchema, PluginChildKind, PluginComponent,
        PluginStatusField, TypedAction,
    },
    CAMERA_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Component, Path},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Secret,
    Integer,
    Boolean,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingField {
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub protocol_version: u32,
    #[serde(default = "protocol_one", skip_serializing_if = "is_protocol_one")]
    pub min_core_protocol_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<PluginActionSchema>,
    pub id: String,
    pub label: String,
    pub version: String,
    /// Normal relative path under the immutable package directory.
    pub executable: String,
    pub capabilities: Vec<Capability>,
    pub settings: Vec<SettingField>,
    #[serde(default)]
    pub supports_inputs: bool,
    /// Native controls rendered by Couch; packages never supply UI code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presentation: Vec<PluginComponent>,
    /// Protocol 3 (unreleased): the kinds of child this connection offers, at
    /// most eight. Empty for a package whose connection is one device, which
    /// is every package there is today, and then never written - so the bytes
    /// of a published manifest are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PluginChildKind>,
    /// Protocol 3 (unreleased): this package pairs, and how long one attempt
    /// may take. Absent for every package there is today, and then never
    /// written. A package that does not declare it is never sent a pairing
    /// request and may never be told a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<Pairing>,
    /// Protocol 3 (unreleased): keep this package's child alive between
    /// requests rather than reaping it when the connection goes idle. For a
    /// device that costs seconds to reconnect to; the daemon caps how many
    /// packages may ask.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub keep_alive: bool,
}

/// How a package pairs.
///
/// `required` says a connection is unusable until Couch holds a key for it,
/// so the daemon answers `unpaired` without starting the package at all.
/// `max_seconds` is how long the package wants one attempt to last; Couch
/// gives it that, and never more than [`Pairing::MAX_SECONDS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    #[serde(default)]
    pub required: bool,
    pub max_seconds: u16,
}

impl Pairing {
    pub const MIN_SECONDS: u16 = 10;
    /// Five minutes, the owner's cap. A package may ask for less.
    pub const MAX_SECONDS: u16 = 300;

    pub fn is_valid(&self) -> bool {
        (Self::MIN_SECONDS..=Self::MAX_SECONDS).contains(&self.max_seconds)
    }
}

fn protocol_one() -> u32 {
    1
}
fn is_protocol_one(value: &u32) -> bool {
    *value == 1
}

fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}
fn label(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
}
impl SettingField {
    fn accepts(&self, value: &Value) -> bool {
        match self.kind {
            FieldKind::Text | FieldKind::Secret => value.as_str().is_some_and(|s| {
                s.len() <= 4096
                    && !s.chars().any(char::is_control)
                    && (!self.required || !s.is_empty())
            }),
            FieldKind::Integer => value.as_i64().is_some(),
            FieldKind::Boolean => value.is_boolean(),
        }
    }
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        // A newer host remains compatible with every older package protocol.
        if !(1..=accepted_protocol_version()).contains(&self.protocol_version)
            || self.min_core_protocol_version != self.protocol_version
        {
            return Err(Error::Incompatible);
        }
        if !identifier(&self.id)
            || !label(&self.label)
            || self.version.is_empty()
            || self.version.len() > 96
            || self.version.starts_with('.')
            || !self
                .version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
            || self.executable.len() > 256
            || self.executable.is_empty()
            || !Path::new(&self.executable)
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
            || self.capabilities.len() > 128
            || self.settings.len() > 32
            || self.presentation.len() > 16
            // At most eight valid schemas of distinct kinds; before protocol
            // 3, at most one.
            || !PluginActionSchema::valid_set(&self.actions)
            || (self.protocol_version < NEXT_PROTOCOL_VERSION && self.actions.len() > 1)
            || (self.protocol_version == 1 && !self.actions.is_empty())
            // The one typed action protocol 2 has. couch-model now parses
            // three more (protocol 3, step T2); a protocol 1 or 2 manifest
            // declaring one is invalid, as it was when they did not parse, so
            // no such package can ever be sent one.
            || (self.protocol_version < NEXT_PROTOCOL_VERSION
                && self
                    .actions
                    .iter()
                    .any(|schema| schema.kind() != ActionKind::SetVolumeDb))
            // Children of a connection arrived with protocol 3, like `x:`
            // ids: an older manifest that declares one is invalid, so no
            // package Couch can already run will ever be asked to list them.
            || (self.protocol_version < NEXT_PROTOCOL_VERSION && !self.children.is_empty())
            || !valid_child_kinds(&self.children)
            // Camera is protocol 4 vocabulary. The model and disk envelope
            // may understand it before the host admits protocol 4 packages,
            // but a protocol 3 manifest must never activate the data plane.
            || (self.protocol_version < CAMERA_PROTOCOL_VERSION
                && self
                    .children
                    .iter()
                    .any(|kind| kind.component == ChildComponent::Camera))
            // Pairing and a kept-alive child arrived with protocol 3, like
            // children and `x:` ids: an older manifest that declares either is
            // invalid, so no package Couch can already run is ever sent a
            // pairing request, told a key, or exempted from the idle reaper.
            || (self.protocol_version < NEXT_PROTOCOL_VERSION
                && (self.pairing.is_some() || self.keep_alive))
            || self.pairing.is_some_and(|pairing| !pairing.is_valid())
        {
            return Err(Error::Invalid);
        }
        let mut seen = HashSet::new();
        // A button the package names itself (`x:`) belongs to protocol 3, and
        // the limit is the package's, not one kind's: the same id named by the
        // connection and by two of its kinds is three of the thirty-two.
        let mut named = self
            .children
            .iter()
            .flat_map(|kind| &kind.capabilities)
            .filter(|capability| {
                matches!(Function::parse(&capability.id), Some(Function::Custom(_)))
            })
            .count();
        for cap in &self.capabilities {
            let function = Function::parse(&cap.id);
            if matches!(function, Some(Function::Custom(_))) {
                named += 1;
                if self.protocol_version < NEXT_PROTOCOL_VERSION {
                    return Err(Error::Invalid);
                }
            }
            if !label(&cap.label)
                || !seen.insert(&cap.id)
                || cap.id.len() > 128
                || function.is_none()
                || cap.id.starts_with("input:")
                || cap.id.starts_with("app:")
            {
                return Err(Error::Invalid);
            }
        }
        if named > MAX_CUSTOM_FUNCTIONS {
            return Err(Error::Invalid);
        }
        seen.clear();
        for field in &self.settings {
            if !identifier(&field.id)
                || !label(&field.label)
                || !seen.insert(&field.id)
                || field.default.as_ref().is_some_and(|v| !field.accepts(v))
                || (field.kind == FieldKind::Secret && field.default.is_some())
            {
                return Err(Error::Invalid);
            }
        }
        let declared = |id: &str| self.capabilities.iter().any(|cap| cap.id == id);
        for component in &self.presentation {
            let valid = match component {
                PluginComponent::CommandGroup { title, commands } => {
                    let mut seen = HashSet::new();
                    label(title)
                        && !commands.is_empty()
                        && commands.len() <= 32
                        && commands.iter().all(|id| declared(id) && seen.insert(id))
                }
                PluginComponent::StatusText { label: text, field } => {
                    label(text)
                        && (*field != PluginStatusField::VolumeDb || self.protocol_version >= 2)
                }
                PluginComponent::VolumeDbControl { label: text } => {
                    label(text)
                        && self.protocol_version >= 2
                        && PluginActionSchema::find(&self.actions, ActionKind::SetVolumeDb)
                            .is_some()
                }
                PluginComponent::Toggle {
                    label: text,
                    state,
                    on,
                    off,
                } => {
                    label(text) && state.is_boolean() && on != off && declared(on) && declared(off)
                }
                PluginComponent::InputSelector { label: text } => {
                    label(text) && self.supports_inputs
                }
                // A connection that is itself one lamp, blind or thermostat.
                // Protocol 3 only, and only over the action that drives it, as
                // the decibel control is.
                PluginComponent::Light { label: text } => self.composes(text, ActionKind::SetLight),
                PluginComponent::Cover { label: text } => self.composes(text, ActionKind::SetCover),
                PluginComponent::Climate { label: text } => {
                    self.composes(text, ActionKind::SetClimate)
                }
            };
            if !valid {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }

    /// A protocol 3 control the connection itself composes: a shown label, and
    /// the typed action that drives it declared on this manifest.
    fn composes(&self, text: &str, kind: ActionKind) -> bool {
        label(text)
            && self.protocol_version >= NEXT_PROTOCOL_VERSION
            && PluginActionSchema::find(&self.actions, kind).is_some()
    }

    /// Whether this package may be sent a pairing request and told a key.
    ///
    /// Both at once, deliberately: `pairing` is only valid on a protocol 3
    /// manifest, so this is the single question the gate asks before it lets
    /// a credential or a `pair_*` request out.
    pub fn pairs(&self) -> bool {
        self.protocol_version >= NEXT_PROTOCOL_VERSION && self.pairing.is_some()
    }

    /// The kind of child this resource is, as this manifest declares it.
    pub fn child_kind(&self, kind: &str) -> Option<&PluginChildKind> {
        self.children.iter().find(|child| child.kind == kind)
    }

    /// Validate without rendering values into errors. Unknown keys are refused.
    pub fn validate_settings(&self, settings: &Value) -> Result<()> {
        self.validate()?;
        let map = settings.as_object().ok_or(Error::Invalid)?;
        if map
            .keys()
            .any(|key| !self.settings.iter().any(|field| &field.id == key))
        {
            return Err(Error::Invalid);
        }
        for field in &self.settings {
            match map.get(&field.id).or(field.default.as_ref()) {
                Some(value) if !field.accepts(value) => return Err(Error::Invalid),
                None if field.required => return Err(Error::Invalid),
                _ => (),
            }
        }
        Ok(())
    }

    pub fn with_defaults(&self, mut settings: Value) -> Result<Value> {
        self.validate_settings(&settings)?;
        let map = settings.as_object_mut().ok_or(Error::Invalid)?;
        for field in &self.settings {
            if let Some(value) = &field.default {
                map.entry(field.id.clone()).or_insert_with(|| value.clone());
            }
        }
        Ok(settings)
    }

    pub fn validate_action(&self, action: TypedAction) -> Result<()> {
        if self.protocol_version < 2 {
            return Err(Error::Unsupported);
        }
        let schema =
            PluginActionSchema::find(&self.actions, action.kind()).ok_or(Error::Unsupported)?;
        if !schema.accepts(action) {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    /// Whether a reason may name this setting, and whether its text is fit to
    /// show. Only a protocol 3 package may send a reason at all; the host
    /// checks that first.
    pub fn accepts_reason(&self, reason: &Reason) -> bool {
        reason.is_well_formed()
            && reason
                .field()
                .is_none_or(|id| self.settings.iter().any(|field| field.id == id))
    }

    pub fn supports(&self, function: &str) -> bool {
        match Function::parse(function) {
            None => return false,
            // Never sent to a protocol 1 or 2 package, whatever it declares.
            Some(Function::Custom(_)) if self.protocol_version < NEXT_PROTOCOL_VERSION => {
                return false
            }
            Some(_) => (),
        }
        self.capabilities.iter().any(|c| c.id == function)
            || (self.supports_inputs
                && function
                    .strip_prefix("input:")
                    .is_some_and(|id| valid_input_id(id)))
    }
}
