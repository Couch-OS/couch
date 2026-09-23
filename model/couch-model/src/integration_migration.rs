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
//! Each departed built-in has one row and a `Legacy*` provider variant.
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
    /// The saved connection's own fields, as the package's settings.
    pub settings: fn(&Provider) -> Option<LegacySettings>,
    /// The file the built-in client kept its private settings in,
    /// inside `connections/<id>/` - `"hue-connection.json"`, say. `None` for a
    /// built-in that stored nothing.
    ///
    /// The daemon reads it, maps it with [`LegacyBuiltin::credential`] and
    /// hands the result to the package as its credential. The old file is
    /// **left where it is**: a Couch rolled back to one that still has the
    /// built-in client has to find its pairing.
    pub credential_file: Option<&'static str>,
    /// Settings derived from that private file, such as Hue's bridge address.
    pub stored_settings: Option<fn(&serde_json::Value) -> Option<LegacySettings>>,
    /// What that file's contents become as a package credential, or `None` if
    /// this one cannot be handed over (a half-written file, a shape the
    /// built-in never wrote). Pure JSON to JSON: nothing here performs I/O,
    /// and the result must be a JSON object.
    pub credential: Option<fn(&serde_json::Value) -> Option<serde_json::Value>>,
    /// Rewrite saved resources after the provider has become the package.
    pub convert: fn(&mut Config, &Id) -> Result<(), &'static str>,
}

/// Every built-in that has left, in the order they left.
pub const LEGACY_BUILTINS: &[LegacyBuiltin] = &[
    LegacyBuiltin {
        kind: "denon",
        package: "denon",
        name: "Denon",
        settings: denon_settings,
        credential_file: None,
        stored_settings: None,
        credential: None,
        convert: keep_resources,
    },
    LegacyBuiltin {
        kind: "hue",
        package: "hue",
        name: "Philips Hue",
        settings: hue_settings,
        credential_file: Some("hue-connection.json"),
        stored_settings: Some(hue_stored_settings),
        credential: Some(hue_credential),
        convert: convert_hue_resources,
    },
];

fn denon_settings(provider: &Provider) -> Option<LegacySettings> {
    match provider {
        Provider::LegacyDenon { host, port } => Some(alloc::vec![
            ("host", LegacySetting::Text(host.clone())),
            ("port", LegacySetting::Integer(i64::from(*port))),
        ]),
        _ => None,
    }
}

fn hue_settings(provider: &Provider) -> Option<LegacySettings> {
    matches!(provider, Provider::LegacyHue).then(Vec::new)
}

fn hue_stored_settings(stored: &serde_json::Value) -> Option<LegacySettings> {
    let host = stored.get("url")?.as_str()?.trim();
    (!host.is_empty()).then(|| alloc::vec![("host", LegacySetting::Text(host.into()))])
}

fn hue_credential(stored: &serde_json::Value) -> Option<serde_json::Value> {
    let application_key = stored.get("token")?.as_str()?;
    if application_key.is_empty() || application_key.len() > 128 {
        return None;
    }
    let certificate = stored.get("certificate")?.as_array()?;
    // Keep the encoded credential comfortably inside couch-plugin's bounded
    // credential frame. Hue bridge certificates are ordinarily about 1 KiB.
    if certificate.is_empty() || certificate.len() > 8 * 1024 {
        return None;
    }
    let bytes: Option<Vec<u8>> = certificate
        .iter()
        .map(|byte| u8::try_from(byte.as_u64()?).ok())
        .collect();
    Some(serde_json::json!({
        "application_key": application_key,
        "certificate": base64(&bytes?),
    }))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn keep_resources(_: &mut Config, _: &Id) -> Result<(), &'static str> {
    Ok(())
}

fn convert_hue_resources(config: &mut Config, connection: &Id) -> Result<(), &'static str> {
    let children = match &config
        .connection(connection)
        .ok_or("That connection no longer exists")?
        .provider
    {
        Provider::Plugin { children, .. } => children.clone(),
        _ => return Err("The Hue connection did not become a package"),
    };
    let light = children
        .iter()
        .find(|child| child.kind == "light")
        .ok_or("The Hue package does not offer lights")?;
    let group = children
        .iter()
        .find(|child| child.kind == "group")
        .ok_or("The Hue package does not offer rooms")?;
    let scene = children
        .iter()
        .find(|child| child.kind == "scene")
        .ok_or("The Hue package does not offer scenes")?;

    for device in config.rooms.iter_mut().flat_map(|room| &mut room.devices) {
        let Integration::Connection {
            connection_id,
            resource_id,
            child,
        } = &mut device.integration
        else {
            continue;
        };
        if connection_id != connection {
            continue;
        }
        let kind = if let Some(id) = resource_id.strip_prefix("room:") {
            *resource_id = alloc::format!("room/{id}");
            group
        } else {
            light
        };
        device.kind = kind.device_kind;
        *child = Some(crate::ChildSnapshot {
            kind: kind.kind.clone(),
            light: Some(crate::LightTraits {
                dimmable: true,
                mirek: None,
                color: false,
            }),
            cover: None,
            climate: None,
        });
    }
    for saved in &mut config.scenes {
        let Some(hue) = saved.hue.as_ref() else {
            continue;
        };
        if &hue.connection_id != connection {
            continue;
        }
        saved.resource = Some(crate::SceneResource {
            connection_id: connection.clone(),
            resource_id: alloc::format!("scene/{}", hue.scene_id),
            kind: scene.kind.clone(),
        });
        saved.hue = None;
    }
    Ok(())
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

    pub fn map_stored_settings(&self, stored: &serde_json::Value) -> Option<LegacySettings> {
        (self.stored_settings?)(stored)
    }

    /// What the built-in client's stored file becomes
    /// as the package's credential. `None` when this row hands nothing over,
    /// when the mapping refuses what it was given, or when the result is not a
    /// JSON object. Nothing here reads a file: the daemon does that and passes
    /// the contents in.
    pub fn map_credential(&self, stored: &serde_json::Value) -> Option<serde_json::Value> {
        let mapped = (self.credential?)(stored)?;
        mapped.is_object().then_some(mapped)
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
            Integration::LegacyDenon { .. } | Integration::Hue { .. } => {
                LegacyBuiltin::for_kind(self.via())
            }
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
        (row.convert)(&mut next, id)?;
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
    /// converts the same way. One without a usable address is left alone.
    /// Pilot receipts are dropped: see [`DenonMigration`].
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
                // An inline receiver was never checked for an address, so a
                // file can hold one with none. A connection is, and one made
                // from that would stop the whole file loading. It never worked
                // and there is nothing to hand to a package: it stays as it
                // is, inert and readable.
                if host.trim().is_empty() || port == 0 {
                    continue;
                }
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
                    child: None,
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
            children: alloc::vec![],
        }
    }

    fn built_in_era() -> Config {
        let stored: StoredConfig = serde_json::from_str(BUILT_IN_ERA).unwrap();
        stored.into_config().unwrap()
    }

    fn hue_package() -> Provider {
        let light = |kind: &str, label: &str| crate::PluginChildKind {
            kind: kind.into(),
            label: label.into(),
            device_kind: crate::DeviceKind::Light,
            component: crate::ChildComponent::Light,
            capabilities: ["on", "off", "toggle"]
                .into_iter()
                .map(|id| crate::PluginCapability {
                    id: id.into(),
                    label: id.into(),
                })
                .collect(),
            actions: alloc::vec![crate::PluginActionSchema::SetLight {}],
        };
        Provider::Plugin {
            id: "hue".into(),
            label: "Philips Hue".into(),
            capabilities: alloc::vec![],
            supports_inputs: false,
            presentation: alloc::vec![],
            actions: alloc::vec![],
            children: alloc::vec![
                light("light", "Hue light"),
                light("group", "Hue room"),
                crate::PluginChildKind {
                    kind: "scene".into(),
                    label: "Hue scene".into(),
                    device_kind: crate::DeviceKind::Other,
                    component: crate::ChildComponent::Scene,
                    capabilities: alloc::vec![crate::PluginCapability {
                        id: "on".into(),
                        label: "Recall".into(),
                    }],
                    actions: alloc::vec![],
                },
            ],
        }
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
        assert_eq!(Provider::LegacyHue.legacy_builtin().unwrap().package, "hue");
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
    fn hue_conversion_carries_private_pairing_and_rewrites_lights_rooms_and_scenes() {
        const LIGHT: &str = "00000000-0000-0000-0000-000000000001";
        const ROOM: &str = "00000000-0000-0000-0000-000000000002";
        const SCENE: &str = "00000000-0000-0000-0000-000000000003";
        let mut config = Config::default();
        config.connections.push(Connection {
            id: "bridge".into(),
            name: "Hue bridge".into(),
            provider: Provider::LegacyHue,
        });
        config.rooms.push(crate::Room {
            id: "living".into(),
            name: "Living room".into(),
            icon: None,
            devices: alloc::vec![
                crate::Device::new("lamp".into(), "Lamp", crate::DeviceKind::Light)
                    .with_integration(Integration::Connection {
                        connection_id: "bridge".into(),
                        resource_id: LIGHT.into(),
                        child: None,
                    }),
                crate::Device::new(
                    "room-lights".into(),
                    "Room lights",
                    crate::DeviceKind::Light
                )
                .with_integration(Integration::Connection {
                    connection_id: "bridge".into(),
                    resource_id: alloc::format!("room:{ROOM}"),
                    child: None,
                }),
            ],
        });
        config.scenes.push(crate::Scene {
            id: "relax".into(),
            name: "Relax".into(),
            icon: None,
            steps: alloc::vec![],
            hue: Some(crate::HueScene {
                connection_id: "bridge".into(),
                scene_id: SCENE.into(),
            }),
            resource: None,
            rooms: alloc::vec!["living".into()],
        });
        config.validate().unwrap();

        let row = Provider::LegacyHue.legacy_builtin().unwrap();
        let stored = serde_json::json!({
            "url": "https://192.0.2.20/",
            "token": "abc-123",
            "certificate": [1, 2, 3, 4]
        });
        assert_eq!(
            row.map_stored_settings(&stored),
            Some(alloc::vec![(
                "host",
                LegacySetting::Text("https://192.0.2.20/".into())
            )])
        );
        assert_eq!(
            row.map_credential(&stored),
            Some(serde_json::json!({
                "application_key": "abc-123",
                "certificate": "AQIDBA=="
            }))
        );

        assert!(config
            .convert_legacy(&Id::new("bridge"), hue_package())
            .unwrap());
        config.validate().unwrap();
        let devices = &config.rooms[0].devices;
        assert_eq!(
            devices[0].integration,
            Integration::Connection {
                connection_id: "bridge".into(),
                resource_id: LIGHT.into(),
                child: Some(crate::ChildSnapshot {
                    kind: "light".into(),
                    light: Some(crate::LightTraits {
                        dimmable: true,
                        mirek: None,
                        color: false
                    }),
                    cover: None,
                    climate: None
                })
            }
        );
        assert!(
            matches!(&devices[1].integration, Integration::Connection { resource_id, child: Some(child), .. }
            if resource_id == &alloc::format!("room/{ROOM}") && child.kind == "group")
        );
        assert!(config.scenes[0].hue.is_none());
        assert_eq!(
            config.scenes[0].resource.as_ref().unwrap().resource_id,
            alloc::format!("scene/{SCENE}")
        );
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
            .convert_legacy(&Id::new("receiver"), Provider::LegacyHue)
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
                    child: None,
                }
            );
        }
        assert_eq!(config.legacy_connections().count(), 2);
        assert!(!config.migrate());
    }

    #[test]
    fn an_inline_receiver_without_an_address_stays_inert_and_the_file_keeps_loading() {
        // All three were accepted by the release with the built-in client,
        // which never validated an inline receiver's address.
        for (host, port) in [("", 23), ("  \t", 23), ("avr.invalid", 0)] {
            let mut config = built_in_era();
            config.connections.clear();
            config.rooms[0].devices.remove(0);
            config.activities[0].source = None;
            config.activities[0].steps.remove(0);
            config.activities[0].buttons.clear();
            config.rooms[0].devices[0].integration = Integration::LegacyDenon {
                host: host.into(),
                port,
            };
            config.validate().unwrap();
            let before = config.clone();
            assert!(!config.migrate(), "{host:?}:{port}");
            assert_eq!(config, before);
            config.validate().unwrap();
            assert_eq!(config.legacy_connections().count(), 0);
            // It still says what it is to whoever draws it.
            assert!(config.rooms[0].devices[0]
                .integration
                .legacy_builtin()
                .is_some());
            // And what is saved next loads again, here and in an older release.
            let bytes = serde_json::to_vec(&StoredConfig::new(&config)).unwrap();
            let mut again = serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap();
            again.migrate();
            again.validate().unwrap();
            assert_eq!(again, config);
            let older: Config = serde_json::from_slice(&bytes).unwrap();
            older.validate().unwrap();
        }
    }

    /// The generic credential hook remains independent of Hue's concrete
    /// mapping, and Denon proves the no-credential path.
    #[test]
    fn a_row_that_names_a_stored_key_maps_it_and_one_that_does_not_hands_nothing_over() {
        fn mapped(stored: &serde_json::Value) -> Option<serde_json::Value> {
            let key = stored.get("application_key")?.as_str()?;
            (!key.is_empty()).then(|| serde_json::json!({"key": key}))
        }
        let row = LegacyBuiltin {
            kind: "hue",
            package: "hue",
            name: "Hue",
            settings: denon_settings,
            credential_file: Some("hue-connection.json"),
            stored_settings: None,
            credential: Some(mapped),
            convert: keep_resources,
        };
        assert_eq!(row.credential_file, Some("hue-connection.json"));
        assert_eq!(
            row.map_credential(&serde_json::json!({"url": "https://b/", "application_key": "abc"})),
            Some(serde_json::json!({"key": "abc"}))
        );
        // A file the built-in never wrote, an empty key, and a mapping that
        // would not give back an object: all of them hand nothing over.
        for stored in [
            serde_json::json!({}),
            serde_json::json!({"application_key": ""}),
            serde_json::json!({"application_key": 7}),
            serde_json::json!("nonsense"),
        ] {
            assert_eq!(row.map_credential(&stored), None, "{stored}");
        }
        fn not_an_object(_: &serde_json::Value) -> Option<serde_json::Value> {
            Some(serde_json::json!(["k"]))
        }
        let listy = LegacyBuiltin {
            credential: Some(not_an_object),
            ..row
        };
        assert_eq!(listy.map_credential(&serde_json::json!({})), None);

        // The shipped row stores nothing, and says so.
        let denon = LegacyBuiltin::for_kind("denon").unwrap();
        assert_eq!(denon.credential_file, None);
        assert!(denon.stored_settings.is_none());
        assert!(denon.credential.is_none());
        assert_eq!(denon.map_credential(&serde_json::json!({"a": 1})), None);
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
