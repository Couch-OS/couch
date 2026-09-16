use crate::{Error, Result, PROTOCOL_VERSION};
use couch_sdk::couch_model::{commands::Function, PluginComponent};
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
        if self.protocol_version != PROTOCOL_VERSION {
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
        {
            return Err(Error::Invalid);
        }
        let mut seen = HashSet::new();
        for cap in &self.capabilities {
            if !label(&cap.label)
                || !seen.insert(&cap.id)
                || cap.id.len() > 128
                || Function::parse(&cap.id).is_none()
                || cap.id.starts_with("input:")
                || cap.id.starts_with("app:")
            {
                return Err(Error::Invalid);
            }
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
                PluginComponent::StatusText { label: text, .. } => label(text),
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
            };
            if !valid {
                return Err(Error::Invalid);
            }
        }
        Ok(())
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

    pub fn supports(&self, function: &str) -> bool {
        self.capabilities.iter().any(|c| c.id == function)
            || (self.supports_inputs
                && function.strip_prefix("input:").is_some_and(|id| {
                    !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
                }))
    }
}
