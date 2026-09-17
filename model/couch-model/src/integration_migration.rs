//! Explicit, reversible Denon pilot. Package admission and settings persistence
//! belong to the daemon; these transformations never perform device I/O.
use crate::{Config, Id, Integration, Provider};
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenonMigration {
    pub host: String,
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Connection, StoredConfig};

    fn fixture() -> (Config, Id, Provider) {
        let mut config = Config::seed();
        let id = Id::new("receiver");
        config.connections.push(Connection {
            id: id.clone(),
            name: "Receiver".into(),
            provider: Provider::Denon {
                host: "avr.invalid".into(),
                port: 23,
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: id.clone(),
            resource_id: "".into(),
        };
        let device = config.rooms[0].devices[0].id.clone();
        config.activities[0].steps.clear();
        config.activities[0]
            .steps
            .push(Action::new(device.as_str(), "input:HDMI1"));
        config.activities[0].buttons.clear();
        config.activities[0].buttons.push(crate::buttons::Binding {
            button: crate::buttons::Button::Red,
            gesture: Default::default(),
            action: Some(Action::new(device.as_str(), "volume-up")),
        });
        let plugin = Provider::Plugin {
            id: "denon".into(),
            label: "Denon".into(),
            capabilities: [
                "power-on",
                "power-off",
                "volume-up",
                "volume-down",
                "mute",
                "mute-on",
                "mute-off",
            ]
            .into_iter()
            .map(|id| crate::PluginCapability {
                id: id.into(),
                label: id.into(),
            })
            .collect(),
            supports_inputs: true,
            presentation: alloc::vec![],
            actions: alloc::vec![],
        };
        config.validate().unwrap();
        (config, id, plugin)
    }

    #[test]
    fn migration_is_explicit_idempotent_and_restores_exact_native_config() {
        let (mut config, id, plugin) = fixture();
        let original = config.clone();
        assert!(config.migrate_denon(&id, plugin.clone()).unwrap());
        assert_eq!(config.rooms, original.rooms);
        assert_eq!(config.activities, original.activities);
        assert_eq!(config.scenes, original.scenes);
        let migrated = config.clone();
        assert!(!config.migrate_denon(&id, plugin).unwrap());
        assert_eq!(config, migrated);
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        let legacy: Config = serde_json::from_slice(&bytes).unwrap();
        legacy.validate().unwrap();
        assert_eq!(legacy, original);
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            migrated
        );
        assert!(config.restore_native_denon(&id).unwrap());
        assert!(!config.restore_native_denon(&id).unwrap());
        assert_eq!(config, original);
    }

    #[test]
    fn missing_capabilities_and_duplicate_native_owners_leave_config_untouched() {
        let (mut config, id, mut plugin) = fixture();
        let original = config.clone();
        if let Provider::Plugin { capabilities, .. } = &mut plugin {
            capabilities.clear();
        }
        assert!(config.migrate_denon(&id, plugin).is_err());
        assert_eq!(config, original);
        let (_, _, plugin) = fixture();
        let mut duplicate = config.connections.last().unwrap().clone();
        duplicate.id = Id::new("duplicate");
        config.connections.push(duplicate);
        let original = config.clone();
        assert!(config.migrate_denon(&id, plugin).is_err());
        assert_eq!(config, original);
    }

    #[test]
    fn receipts_cannot_point_to_missing_or_non_denon_connections() {
        let (mut config, id, plugin) = fixture();
        config.migrate_denon(&id, plugin).unwrap();
        config.connections.last_mut().unwrap().provider = Provider::Hue;
        assert!(config.validate().is_err());
    }
}

impl DenonMigration {
    pub fn provider(&self) -> Provider {
        Provider::Denon {
            host: self.host.clone(),
            port: self.port,
        }
    }
}

impl Config {
    pub(crate) fn validate_denon_migrations(&self, problems: &mut alloc::vec::Vec<crate::Problem>) {
        if self.denon_migrations.is_empty() {
            return;
        }
        let mut native = self.clone();
        native.denon_migrations.clear();
        for (id, original) in &self.denon_migrations {
            if self.migrated_denon(id).is_none()
                || original.host.trim().is_empty()
                || original.port == 0
            {
                problems.push(crate::Problem { at: alloc::format!("denon_migrations.{id}"),
                    message: "A migration receipt requires its Denon package connection and original address".into() });
                continue;
            }
            if self.connections.iter().any(|c| c.provider == original.provider())
                || self.rooms.iter().flat_map(|r| &r.devices).any(|d| matches!(&d.integration,
                    Integration::Denon { host, port } if *host == original.host && *port == original.port))
            {
                problems.push(crate::Problem { at: alloc::format!("denon_migrations.{id}"),
                    message: "A migrated Denon target cannot also have a built-in owner".into() });
            }
            native
                .connections
                .iter_mut()
                .find(|c| c.id == *id)
                .unwrap()
                .provider = original.provider();
        }
        // Pilot connections remain reversible after ordinary activity edits and
        // metadata refreshes, so every saved command must work in the old core.
        if native.validate().is_err() {
            problems.push(crate::Problem {
                at: "denon_migrations".into(),
                message: "Migrated Denon commands must remain compatible with built-in rollback"
                    .into(),
            });
        }
    }

    pub fn migrated_denon(&self, id: &Id) -> Option<&DenonMigration> {
        if matches!(&self.connection(id)?.provider, Provider::Plugin { id, .. } if id == "denon") {
            self.denon_migrations.get(id)
        } else {
            None
        }
    }

    /// The caller must verify the installed manifest and prepare its private
    /// settings before committing this new configuration.
    pub fn migrate_denon(&mut self, id: &Id, plugin: Provider) -> Result<bool, &'static str> {
        if self.migrated_denon(id).is_some() {
            return Ok(false);
        }
        if !matches!(&plugin, Provider::Plugin { id, .. } if id == "denon") {
            return Err("The Denon migration requires the Denon package");
        }
        let Some(Provider::Denon { host, port }) = self.connection(id).map(|c| &c.provider) else {
            return Err("Choose a built-in Denon connection");
        };
        let original = DenonMigration {
            host: host.clone(),
            port: *port,
        };
        // A second native owner for the same target would remain active after
        // conversion. Require the operator to consolidate it first.
        if self.connections.iter().any(|c| c.id != *id && c.provider == original.provider())
            || self.rooms.iter().flat_map(|r| &r.devices).any(|d| matches!(&d.integration,
                Integration::Denon { host, port } if *host == original.host && *port == original.port))
        {
            return Err("Consolidate duplicate built-in Denon targets before migrating");
        }
        let mut next = self.clone();
        next.connections
            .iter_mut()
            .find(|c| c.id == *id)
            .unwrap()
            .provider = plugin;
        next.denon_migrations.insert(id.clone(), original);
        next.validate()
            .map_err(|_| "The package does not support all saved Denon commands")?;
        *self = next;
        Ok(true)
    }

    pub fn restore_native_denon(&mut self, id: &Id) -> Result<bool, &'static str> {
        let Some(original) = self.migrated_denon(id).cloned() else {
            return if matches!(
                self.connection(id).map(|c| &c.provider),
                Some(Provider::Denon { .. })
            ) {
                Ok(false)
            } else {
                Err("This connection has no native Denon migration to restore")
            };
        };
        let mut next = self.clone();
        next.connections
            .iter_mut()
            .find(|c| c.id == *id)
            .unwrap()
            .provider = original.provider();
        next.denon_migrations.remove(id);
        next.validate()
            .map_err(|_| "Saved commands need features unavailable in built-in Denon")?;
        *self = next;
        Ok(true)
    }
}
