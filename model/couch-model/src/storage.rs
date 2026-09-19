//! Disk-only compatibility envelope. HTTP exports keep the ordinary `Config`
//! shape. Old runtimes ignore the extension and see usable built-in devices;
//! current readers recover the complete document from the same atomic write.
use crate::{Config, Integration, Provider};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredConfig {
    #[serde(flatten)]
    rollback: Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integration_config: Option<Config>,
    /// Protocol-v1 cores read integration_config and ignore this extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integration_config_v2: Option<Config>,
}

impl StoredConfig {
    pub fn new(config: &Config) -> Self {
        let rollback = projection(config);
        let v1 = v1_projection(config);
        Self {
            integration_config: (rollback != v1).then_some(v1.clone()),
            integration_config_v2: (v1 != *config).then(|| config.clone()),
            rollback,
        }
    }

    /// Reject inconsistent envelopes rather than silently ignoring edits to the
    /// outer document. Ordinary legacy files and ordinary API exports also load.
    pub fn into_config(self) -> Result<Config, &'static str> {
        let v1 = match self.integration_config {
            Some(config) if projection(&config) == self.rollback => config,
            Some(_) => {
                return Err("Integration configuration does not match its rollback projection")
            }
            None => self.rollback,
        };
        match self.integration_config_v2 {
            Some(config) if v1_projection(&config) == v1 => Ok(config),
            Some(_) => Err("Protocol-v2 configuration does not match its protocol-v1 projection"),
            None => Ok(v1),
        }
    }

    pub fn has_integrations(&self) -> bool {
        self.integration_config.is_some() || self.integration_config_v2.is_some()
    }
}

/// Strip only v2 display/action metadata and newly valid spaced input bindings.
/// A v1 core keeps package identity/settings/migration receipts, and its live
/// manifest gate rejects a v2-only package. A save by that core is authoritative
/// on re-upgrade: omitted v2 bindings must not be resurrected.
fn v1_projection(config: &Config) -> Config {
    let mut result = config.clone();
    fn components(
        presentation: &mut Vec<crate::PluginComponent>,
        actions: &mut Vec<crate::PluginActionSchema>,
    ) {
        actions.clear();
        presentation.retain(|component| {
            !matches!(
                component,
                crate::PluginComponent::VolumeDbControl { .. }
                    | crate::PluginComponent::StatusText {
                        field: crate::PluginStatusField::VolumeDb,
                        ..
                    }
            )
        });
    }
    for connection in &mut result.connections {
        if let Provider::Plugin {
            presentation,
            actions,
            ..
        } = &mut connection.provider
        {
            components(presentation, actions);
        }
    }
    for room in &mut result.rooms {
        for device in &mut room.devices {
            if let Integration::Plugin {
                presentation,
                actions,
                ..
            } = &mut device.integration
            {
                components(presentation, actions);
            }
        }
    }
    let compatible = |action: &crate::Action| {
        !action
            .command
            .strip_prefix("input:")
            .is_some_and(|id| id.contains(' '))
    };
    for activity in &mut result.activities {
        for binding in &mut activity.buttons {
            if binding
                .action
                .as_ref()
                .is_some_and(|action| !compatible(action))
            {
                binding.action = None;
            }
        }
        activity.steps.retain(compatible);
        for steps in [&mut activity.setup.on, &mut activity.setup.off] {
            steps.retain(|step| !matches!(step, crate::SequenceStep::Command { action } if !compatible(action)));
        }
        for page in &mut activity.setup.pages {
            page.widgets.retain(|widget| compatible(&widget.action));
        }
    }
    for scene in &mut result.scenes {
        scene.steps.retain(compatible);
    }
    result
}

fn projection(config: &Config) -> Config {
    let mut result = v1_projection(config);
    let mut disabled = Vec::new();
    for connection in &mut result.connections {
        if let Some(original) = config.migrated_denon(&connection.id) {
            connection.provider = original.provider();
        }
    }
    result.denon_migrations.clear();
    result
        .connections
        .retain(|connection| !matches!(connection.provider, Provider::Plugin { .. }));
    for room in &mut result.rooms {
        for device in &mut room.devices {
            let external = match &device.integration {
                Integration::Plugin { .. } => true,
                Integration::Connection { connection_id, .. } => {
                    config.migrated_denon(connection_id).is_none()
                        && config
                            .connection(connection_id)
                            .is_some_and(|c| matches!(c.provider, Provider::Plugin { .. }))
                }
                _ => false,
            };
            if external {
                disabled.push(device.id.clone());
                // Keep device IDs and all scene/activity references intact.
                // An old executor sees an unconfigured device, never a plugin
                // interpreted as a different provider or sent to a wrong host.
                device.integration = Integration::None;
            }
        }
    }
    // Supported-command validation also runs in old daemons. Commands which
    // depend on external capabilities must not make their projection invalid.
    for activity in &mut result.activities {
        for id in &disabled {
            activity.setup.forget_device(id);
        }
        for binding in &mut activity.buttons {
            if binding
                .action
                .as_ref()
                .is_some_and(|a| disabled.contains(&a.device))
            {
                binding.action = None;
            }
        }
        activity.steps.retain(|a| !disabled.contains(&a.device));
    }
    for scene in &mut result.scenes {
        scene.steps.retain(|a| !disabled.contains(&a.device));
    }
    result.app_shortcuts.retain(|id, _| !disabled.contains(id));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Connection, Id};
    use alloc::vec;

    fn plugin_config() -> Config {
        let mut config = Config::seed();
        config.connections.push(Connection {
            id: Id::new("external"),
            name: "External".into(),
            provider: Provider::Plugin {
                id: "echo".into(),
                label: "Echo".into(),
                capabilities: vec![],
                supports_inputs: false,
                presentation: vec![],
                actions: vec![],
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: Id::new("external"),
            resource_id: "zone1".into(),
        };
        config
    }

    #[test]
    fn v2_cache_and_spaced_inputs_are_preserved_while_both_old_readers_stay_safe() {
        let mut config = plugin_config();
        if let Provider::Plugin {
            supports_inputs,
            presentation,
            actions,
            ..
        } = &mut config.connections.last_mut().unwrap().provider
        {
            *supports_inputs = true;
            *presentation = vec![
                crate::PluginComponent::VolumeDbControl {
                    label: "Volume".into(),
                },
                crate::PluginComponent::StatusText {
                    label: "Volume".into(),
                    field: crate::PluginStatusField::VolumeDb,
                },
            ];
            *actions = vec![crate::PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5,
            }];
        }
        let device = config.rooms[0].devices[0].id.clone();
        let spaced = crate::Action::new(device.clone(), "input:HD RADIO");
        let old = crate::Action::new(device.clone(), "input:SAT/CBL");
        config.activities[0].steps = vec![spaced.clone(), old.clone()];
        config.activities[0].buttons = vec![crate::buttons::Binding {
            button: crate::buttons::Button::Power,
            gesture: Default::default(),
            action: Some(spaced.clone()),
        }];
        config.activities[0].setup.devices = vec![device];
        config.activities[0].setup.on = vec![crate::SequenceStep::Command {
            action: spaced.clone(),
        }];
        config.activities[0].setup.off = vec![crate::SequenceStep::Command {
            action: old.clone(),
        }];
        config.activities[0].setup.pages = vec![crate::ActivityPage {
            title: "Receiver".into(),
            widgets: vec![crate::ActivityWidget {
                label: "Source".into(),
                icon: None,
                action: spaced.clone(),
            }],
        }];
        config.scenes[0].steps = vec![spaced];
        config.validate().unwrap();
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        assert!(serde_json::from_slice::<LegacyConfig>(&bytes).is_ok());
        // Mirrors the v1 component enum, rather than reusing today's richer
        // enum (which would conceal precisely the compatibility regression).
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum OldComponent {
            CommandGroup,
            StatusText,
            Toggle,
            InputSelector,
        }
        #[derive(Deserialize)]
        struct OldProvider {
            #[serde(default)]
            presentation: Vec<OldComponent>,
        }
        #[derive(Deserialize)]
        struct OldConnection {
            provider: OldProvider,
        }
        #[derive(Deserialize)]
        struct OldConfig {
            connections: Vec<OldConnection>,
        }
        #[derive(Deserialize)]
        struct OldEnvelope {
            integration_config: OldConfig,
        }
        let old_reader: OldEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert!(old_reader
            .integration_config
            .connections
            .last()
            .unwrap()
            .provider
            .presentation
            .is_empty());
        assert!(
            serde_json::from_slice::<OldConfig>(&serde_json::to_vec(&config).unwrap()).is_err()
        );
        let current: StoredConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(current.into_config().unwrap(), config);
        let v1 = v1_projection(&config);
        v1.validate().unwrap();
        assert_eq!(v1.activities[0].steps, vec![old.clone()]);
        assert_eq!(v1.activities[0].buttons.len(), 1);
        assert_eq!(
            v1.activities[0].buttons[0].button,
            crate::buttons::Button::Power
        );
        assert_eq!(v1.activities[0].buttons[0].action, None);
        assert_eq!(serde_json::to_value(&v1.activities[0].buttons[0]).unwrap()["action"],serde_json::Value::Null,
            "retain the explicit disabled binding; removing it would restore the default Power action");
        assert!(v1.activities[0].setup.on.is_empty());
        assert_eq!(
            v1.activities[0].setup.off,
            vec![crate::SequenceStep::Command { action: old }]
        );
        assert!(v1.activities[0].setup.pages[0].widgets.is_empty());
        assert!(v1.scenes[0].steps.is_empty());
        let mut old_save: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old_save
            .as_object_mut()
            .unwrap()
            .remove("integration_config_v2");
        assert_eq!(
            serde_json::from_value::<StoredConfig>(old_save)
                .unwrap()
                .into_config()
                .unwrap(),
            v1,
            "a v1 save cannot resurrect dropped v2 bindings"
        );
        let mut mismatched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        mismatched["integration_config_v2"]["revision"] = serde_json::json!(42);
        assert!(serde_json::from_value::<StoredConfig>(mismatched)
            .unwrap()
            .into_config()
            .is_err());
    }

    #[test]
    fn native_spaced_inputs_also_need_the_v2_envelope() {
        let mut config = Config::seed();
        config.rooms[0].devices[0].integration = Integration::LegacyDenon {
            host: "avr.invalid".into(),
            port: 23,
        };
        config.activities[0].steps = vec![crate::Action::new(
            config.rooms[0].devices[0].id.clone(),
            "input:HD RADIO",
        )];
        config.activities[0].buttons.clear();
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        let legacy: Config = serde_json::from_slice(&bytes).unwrap();
        assert!(legacy.activities[0].steps.is_empty());
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            config
        );
    }

    #[test]
    fn envelope_roundtrips_and_legacy_projection_keeps_valid_references() {
        let config = plugin_config();
        config.validate().unwrap();
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        // Tag vocabulary from v0.1.0-alpha.20260916.170. Current Config alone
        // cannot prove this: its enums also understand the plugin variants.
        assert!(serde_json::from_slice::<LegacyConfig>(&bytes).is_ok());
        assert!(
            serde_json::from_slice::<LegacyConfig>(&serde_json::to_vec(&config).unwrap()).is_err()
        );
        let old: Config = serde_json::from_slice(&bytes).unwrap();
        old.validate().unwrap();
        assert_eq!(old, projection(&config));
        assert_eq!(old.rooms[0].devices[0].integration, Integration::None);
        assert!(old.connection(&Id::new("external")).is_none());
        let current: StoredConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(current.into_config().unwrap(), config);
    }

    #[test]
    fn direct_plugin_variants_are_also_hidden_from_old_readers() {
        let mut config = plugin_config();
        config.rooms[0].devices[0].integration = Integration::Plugin {
            id: "echo".into(),
            connection_id: Id::new("external"),
            resource_id: "zone1".into(),
            capabilities: vec![],
            supports_inputs: false,
            presentation: vec![],
            actions: vec![],
        };
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        assert!(serde_json::from_slice::<LegacyConfig>(&bytes).is_ok());
        let old: Config = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.rooms[0].devices[0].integration, Integration::None);
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            config
        );
    }

    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct LegacyConfig {
        connections: Vec<LegacyConnection>,
        rooms: Vec<LegacyRoom>,
    }
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct LegacyConnection {
        provider: LegacyProvider,
    }
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct LegacyRoom {
        devices: Vec<LegacyDevice>,
    }
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct LegacyDevice {
        integration: LegacyIntegration,
    }
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "kebab-case")]
    enum LegacyProvider {
        CoreElec,
        Sonos,
        Kodi,
        Denon,
        HomeAssistant,
        Hue,
        WebOs,
        AndroidTv,
        AppleTv,
        Tizen,
        BluetoothTv,
        UnifiProtect,
        Matter,
        Ir,
    }
    #[derive(Deserialize)]
    #[serde(tag = "via", rename_all = "kebab-case")]
    enum LegacyIntegration {
        Sonos,
        Denon,
        None,
        Connection,
        Kodi,
        Hue,
        WebOs,
        AndroidTv,
        AppleTv,
        Tizen,
        BluetoothTv,
        UnifiProtect,
        Matter,
        HomeAssistant,
        Ir,
    }

    #[test]
    fn plain_files_keep_their_shape_and_inconsistent_envelopes_fail_closed() {
        let config = Config::seed();
        assert_eq!(
            serde_json::to_value(StoredConfig::new(&config)).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
        let mut stored = StoredConfig::new(&plugin_config());
        stored.rollback.revision += 1;
        assert!(stored.into_config().is_err());
    }

    #[test]
    fn plugin_commands_are_inactive_and_valid_in_rollback_activities() {
        let mut config = plugin_config();
        let id = config.rooms[0].devices[0].id.clone();
        if let Provider::Plugin { capabilities, .. } =
            &mut config.connections.last_mut().unwrap().provider
        {
            capabilities.push(crate::PluginCapability {
                id: "power-on".into(),
                label: "On".into(),
            });
        }
        let action = crate::Action::new(id.as_str(), "power-on");
        let activity = &mut config.activities[0];
        activity.buttons.push(crate::buttons::Binding {
            button: crate::buttons::Button::Red,
            gesture: Default::default(),
            action: Some(action.clone()),
        });
        activity.setup.devices.push(id.clone());
        activity.setup.on.push(crate::SequenceStep::Command {
            action: action.clone(),
        });
        activity.setup.custom_screen = true;
        activity.setup.pages.push(crate::ActivityPage {
            title: "Control".into(),
            widgets: vec![crate::ActivityWidget {
                label: "On".into(),
                icon: None,
                action,
            }],
        });
        config.validate().unwrap();
        let stored = StoredConfig::new(&config);
        stored.rollback.validate().unwrap();
        assert!(stored.rollback.activities[0]
            .buttons
            .last()
            .unwrap()
            .action
            .is_none());
        assert!(stored.rollback.activities[0].setup.on.is_empty());
        assert!(stored.rollback.activities[0].setup.pages[0]
            .widgets
            .is_empty());
        assert_eq!(stored.into_config().unwrap(), config);
    }
}
