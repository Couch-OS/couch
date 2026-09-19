//! Built-in integrations that left the OS for a package.
//!
//! An integration now lives in its own repository and reaches the remote
//! through the package feed; the OS carries only the host that runs it. A
//! connection saved while its client was still built in keeps loading (its
//! [`Provider`] variant stays, named `Legacy*`), and [`LEGACY_BUILTINS`] says
//! which package takes it over and which of its fields become that package's
//! settings. The daemon installs the package and then calls
//! [`Config::convert_legacy`]; nothing here performs I/O.
//!
//! The conversion is one way and keeps the connection's id, so rooms,
//! activities, shortcuts and button maps that point at it are untouched.
//! Denon is the first row. The next built-in to leave adds a row, a `Legacy*`
//! variant and nothing else.
use crate::{Config, Connection, Id, Integration, Provider};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// A value a legacy connection carried that its package takes as a setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacySetting {
    Text(String),
    Integer(i64),
}

/// The package settings a legacy connection's own fields become.
pub type LegacySettings = Vec<(&'static str, LegacySetting)>;

/// One built-in integration that became a package.
pub struct LegacyBuiltin {
    /// [`Provider::kind`] of the saved connection, and [`Integration::via`] of
    /// what it resolves to.
    pub kind: &'static str,
    /// The package id in the feed (`couch-integration-<package>`), which is
    /// also the `id` of the [`Provider::Plugin`] the connection becomes.
    pub package: &'static str,
    /// The package's name in a sentence: "Needs the Denon package".
    pub name: &'static str,
    settings: fn(&Provider) -> Option<LegacySettings>,
}

/// Every built-in that has left, in the order they left.
pub const LEGACY_BUILTINS: &[LegacyBuiltin] = &[LegacyBuiltin {
    kind: "denon",
    package: "denon",
    name: "Denon",
    settings: denon_settings,
}];

fn denon_settings(provider: &Provider) -> Option<LegacySettings> {
    match provider {
        Provider::LegacyDenon { host, port } => Some(alloc::vec![
            ("host", LegacySetting::Text(host.clone())),
            ("port", LegacySetting::Integer(i64::from(*port))),
        ]),
        _ => None,
    }
}

impl LegacyBuiltin {
    pub fn for_kind(kind: &str) -> Option<&'static LegacyBuiltin> {
        LEGACY_BUILTINS.iter().find(|row| row.kind == kind)
    }

    /// The package settings this connection's saved fields become, or `None`
    /// for a provider that is not this row's.
    pub fn settings(&self, provider: &Provider) -> Option<LegacySettings> {
        (self.settings)(provider)
    }

    /// The one sentence every surface shows for an unconverted connection: the
    /// web UI beside its name, the remote on its row and as the toast when it
    /// or one of its keys is pressed. Short enough for that toast's one line.
    pub fn needs_package(&self) -> String {
        alloc::format!("Needs the {} package", self.name)
    }
}

impl Provider {
    /// The package that replaced this provider's built-in client, if it is one
    /// of the `Legacy*` variants.
    pub fn legacy_builtin(&self) -> Option<&'static LegacyBuiltin> {
        LegacyBuiltin::for_kind(self.kind()).filter(|row| row.settings(self).is_some())
    }
}

impl Integration {
    /// As [`Provider::legacy_builtin`], for what a device resolves to.
    pub fn legacy_builtin(&self) -> Option<&'static LegacyBuiltin> {
        match self {
            Integration::LegacyDenon { .. } => LegacyBuiltin::for_kind(self.via()),
            _ => None,
        }
    }
}

/// A receipt the reversible Denon pilot (September 2026) wrote beside a
/// connection it had switched to the package, so the switch could be undone.
/// There is no built-in client to go back to: receipts are still read, because
/// a file written by that pilot only matches its own rollback projection with
/// them, and are dropped by [`Config::migrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenonMigration {
    pub host: String,
    pub port: u16,
}

impl DenonMigration {
    pub(crate) fn provider(&self) -> Provider {
        Provider::LegacyDenon {
            host: self.host.clone(),
            port: self.port,
        }
    }
}

impl Config {
    /// The pilot receipt for a connection the pilot switched to the package.
    /// Only the disk projection of a file that still has receipts reads this.
    pub(crate) fn migrated_denon(&self, id: &Id) -> Option<&DenonMigration> {
        if matches!(&self.connection(id)?.provider, Provider::Plugin { id, .. } if id == "denon") {
            self.denon_migrations.get(id)
        } else {
            None
        }
    }

    /// Saved connections whose built-in client is gone, with their package.
    pub fn legacy_connections(
        &self,
    ) -> impl Iterator<Item = (&Connection, &'static LegacyBuiltin)> + '_ {
        self.connections
            .iter()
            .filter_map(|c| c.provider.legacy_builtin().map(|row| (c, row)))
    }

    /// Hand a legacy connection to its package, in place: same id, same name,
    /// every device, activity and shortcut still pointing at it.
    ///
    /// `plugin` is the installed package's public snapshot. The caller has
    /// verified that package and saved its settings first; a refusal here
    /// leaves the configuration exactly as it was. `Ok(false)` means the
    /// connection already belongs to the package.
    pub fn convert_legacy(&mut self, id: &Id, plugin: Provider) -> Result<bool, &'static str> {
        let Provider::Plugin { id: package, .. } = &plugin else {
            return Err("A legacy connection can only become a package connection");
        };
        let Some(connection) = self.connection(id) else {
            return Err("That connection no longer exists");
        };
        if matches!(&connection.provider, Provider::Plugin { id, .. } if id == package) {
            return Ok(false);
        }
        let Some(row) = connection.provider.legacy_builtin() else {
            return Err("That connection does not wait for a package");
        };
        if row.package != package {
            return Err("That is not the package this connection needs");
        }
        let mut next = self.clone();
        next.connections
            .iter_mut()
            .find(|c| c.id == *id)
            .expect("looked up above")
            .provider = plugin;
        next.denon_migrations.remove(id);
        next.validate()
            .map_err(|_| "The package does not support every command saved for this connection")?;
        *self = next;
        Ok(true)
    }

    /// The part of [`Config::migrate`] that belongs to departed built-ins.
    ///
    /// A device that named its receiver inline, from before named connections,
    /// gets a legacy connection of its own (or shares one with the same
    /// address), so that everything waiting for a package is a connection and
    /// converts the same way. Pilot receipts are dropped: see
    /// [`DenonMigration`].
    pub(crate) fn migrate_legacy_builtins(&mut self) -> bool {
        let mut changed = !self.denon_migrations.is_empty();
        self.denon_migrations.clear();
        for room in 0..self.rooms.len() {
            for device in 0..self.rooms[room].devices.len() {
                let Integration::LegacyDenon { host, port } =
                    self.rooms[room].devices[device].integration.clone()
                else {
                    continue;
                };
                let provider = Provider::LegacyDenon { host, port };
                let connection_id = match self.connections.iter().find(|c| c.provider == provider) {
                    Some(existing) => existing.id.clone(),
                    None => {
                        let name = self.rooms[room].devices[device].name.clone();
                        let id = Id::unique(&name, self.connections.iter().map(|c| &c.id));
                        self.connections.push(Connection {
                            id: id.clone(),
                            name,
                            provider,
                        });
                        id
                    }
                };
                self.rooms[room].devices[device].integration = Integration::Connection {
                    connection_id,
                    resource_id: String::new(),
                };
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, StoredConfig};

    /// What the last release with a built-in Denon client wrote: a named
    /// connection, a device on it, a device from before named connections, an
    /// activity that switches an input and a button that turns it up.
    const BUILT_IN_ERA: &str = r#"{
        "schema_version": 1, "revision": 7,
        "connections": [{"id":"receiver","name":"Receiver","provider":{"kind":"denon","host":"avr.invalid","port":23}}],
        "areas": [{"id":"home","name":"Home","rooms":["den"],"activities":["movie"]}],
        "rooms": [{"id":"den","name":"Den","devices":[
            {"id":"avr","name":"AVR","kind":"speaker","integration":{"via":"connection","connection_id":"receiver"}},
            {"id":"old-avr","name":"Old AVR","kind":"speaker","integration":{"via":"denon","host":"192.0.2.7","port":23}}]}],
        "activities": [{"id":"movie","name":"Movie","room":"den","source":"avr",
            "steps":[{"device":"avr","command":"input:SAT/CBL"},{"device":"old-avr","command":"power-on"}],
            "buttons":[{"button":"red","action":{"device":"avr","command":"volume-up"}}]}]
    }"#;

    fn package() -> Provider {
        Provider::Plugin {
            id: "denon".into(),
            label: "Denon AVR".into(),
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
        }
    }

    fn built_in_era() -> Config {
        let stored: StoredConfig = serde_json::from_str(BUILT_IN_ERA).unwrap();
        stored.into_config().unwrap()
    }

    #[test]
    fn a_file_from_the_built_in_era_loads_validates_and_says_what_it_needs() {
        let config = built_in_era();
        config.validate().unwrap();
        let waiting: Vec<_> = config.legacy_connections().collect();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].0.id, Id::new("receiver"));
        assert_eq!(waiting[0].1.package, "denon");
        assert_eq!(waiting[0].1.needs_package(), "Needs the Denon package");
        assert_eq!(
            waiting[0].1.settings(&waiting[0].0.provider).unwrap(),
            alloc::vec![
                ("host", LegacySetting::Text("avr.invalid".into())),
                ("port", LegacySetting::Integer(23)),
            ]
        );
        assert!(Provider::Hue.legacy_builtin().is_none());
        // The device resolves to something every surface can name.
        let resolved = config
            .resolve_integration(&config.rooms[0].devices[0].integration)
            .unwrap();
        assert_eq!(resolved.legacy_builtin().unwrap().package, "denon");
        // Written back, it is still the shape an older release reads.
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        let again: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(again["connections"][0]["provider"]["kind"], "denon");
        assert_eq!(
            again["rooms"][0]["devices"][1]["integration"]["via"],
            "denon"
        );
    }

    #[test]
    fn conversion_keeps_the_id_so_rooms_and_activities_still_resolve() {
        let mut config = built_in_era();
        let before = config.clone();
        assert!(config
            .convert_legacy(&Id::new("receiver"), package())
            .unwrap());
        config.validate().unwrap();
        assert_eq!(config.rooms, before.rooms);
        assert_eq!(config.activities, before.activities);
        assert_eq!(config.areas, before.areas);
        assert_eq!(config.connections[0].id, before.connections[0].id);
        assert_eq!(config.connections[0].name, before.connections[0].name);
        let device = &config.rooms[0].devices[0];
        assert!(matches!(
            config.resolve_integration(&device.integration),
            Some(Integration::Plugin { id, connection_id, .. })
                if id == "denon" && connection_id == Id::new("receiver")
        ));
        for action in [
            Action::new("avr", "input:SAT/CBL"),
            Action::new("avr", "volume-up"),
        ] {
            let function = crate::commands::Function::parse(&action.command).unwrap();
            assert!(function.supports_device(device, &config));
        }
        assert_eq!(config.legacy_connections().count(), 0);
        // Once converted there is nothing left to do, and nothing to undo.
        let converted = config.clone();
        assert!(!config
            .convert_legacy(&Id::new("receiver"), package())
            .unwrap());
        assert_eq!(config, converted);
    }

    #[test]
    fn a_refused_conversion_changes_nothing() {
        let mut config = built_in_era();
        let before = config.clone();
        let mut bare = package();
        if let Provider::Plugin { capabilities, .. } = &mut bare {
            capabilities.clear();
        }
        assert!(config.convert_legacy(&Id::new("receiver"), bare).is_err());
        let mut other = package();
        if let Provider::Plugin { id, .. } = &mut other {
            *id = "echo".into();
        }
        assert!(config.convert_legacy(&Id::new("receiver"), other).is_err());
        assert!(config
            .convert_legacy(&Id::new("missing"), package())
            .is_err());
        assert!(config
            .convert_legacy(&Id::new("receiver"), Provider::Hue)
            .is_err());
        assert_eq!(config, before);
    }

    #[test]
    fn migrate_gives_an_inline_receiver_a_connection_and_is_then_quiet() {
        let mut config = built_in_era();
        let mut twin = config.rooms[0].devices[1].clone();
        twin.id = Id::new("zone-2");
        config.rooms[0].devices.push(twin);
        assert!(config.migrate());
        config.validate().unwrap();
        assert_eq!(config.connections.len(), 2, "one address, one connection");
        let made = &config.connections[1];
        assert_eq!(
            (made.id.as_str(), made.name.as_str()),
            ("old-avr", "Old AVR")
        );
        assert_eq!(
            made.provider,
            Provider::LegacyDenon {
                host: "192.0.2.7".into(),
                port: 23
            }
        );
        for device in &config.rooms[0].devices[1..] {
            assert_eq!(
                device.integration,
                Integration::Connection {
                    connection_id: Id::new("old-avr"),
                    resource_id: String::new(),
                }
            );
        }
        assert_eq!(config.legacy_connections().count(), 2);
        assert!(!config.migrate());
    }

    #[test]
    fn a_file_the_reversible_pilot_wrote_loads_and_loses_its_receipt() {
        // The pilot saved the package connection in the envelope and the
        // built-in one, from its receipt, in the outer document.
        let mut piloted = built_in_era();
        piloted.rooms[0].devices.truncate(1);
        piloted.activities[0].steps.truncate(1);
        piloted.connections[0].provider = package();
        piloted.denon_migrations.insert(
            Id::new("receiver"),
            DenonMigration {
                host: "avr.invalid".into(),
                port: 23,
            },
        );
        let bytes = serde_json::to_vec(&StoredConfig::new(&piloted)).unwrap();
        let outer: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(outer["connections"][0]["provider"]["kind"], "denon");
        assert_eq!(
            outer["integration_config"]["denon_migrations"]["receiver"]["host"],
            "avr.invalid"
        );

        let mut config = serde_json::from_slice::<StoredConfig>(&bytes)
            .unwrap()
            .into_config()
            .unwrap();
        config.validate().unwrap();
        assert!(config.migrate());
        assert!(config.denon_migrations.is_empty());
        assert_eq!(config.connections[0].provider, package());
        config.validate().unwrap();
        // What is written next is an ordinary package connection.
        let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            config
        );
        assert!(!config.migrate());
    }
}
