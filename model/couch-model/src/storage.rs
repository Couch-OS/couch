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
}

impl StoredConfig {
    pub fn new(config: &Config) -> Self {
        let rollback = projection(config);
        Self {
            integration_config: (rollback != *config).then(|| config.clone()),
            rollback,
        }
    }

    /// Reject inconsistent envelopes rather than silently ignoring edits to the
    /// outer document. Ordinary legacy files and ordinary API exports also load.
    pub fn into_config(self) -> Result<Config, &'static str> {
        match self.integration_config {
            Some(config) if projection(&config) == self.rollback => Ok(config),
            Some(_) => Err("Integration configuration does not match its rollback projection"),
            None => Ok(self.rollback),
        }
    }

    pub fn has_integrations(&self) -> bool {
        self.integration_config.is_some()
    }
}

fn projection(config: &Config) -> Config {
    let mut result = config.clone();
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
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: Id::new("external"),
            resource_id: "zone1".into(),
        };
        config
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
