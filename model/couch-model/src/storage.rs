//! Disk-only compatibility envelope. HTTP exports keep the ordinary `Config`
//! shape. Old runtimes ignore the extension and see usable built-in devices;
//! current readers recover the complete document from the same atomic write.
//!
//! The file is up to four documents, each one the projection of the next:
//!
//! | key | read by | is |
//! | --- | --- | --- |
//! | top level | every Couch | `projection`: no packages at all |
//! | `integration_config` | protocol 1 cores | `v1_projection` of the next |
//! | `integration_config_v2` | protocol 2 cores | `v2_projection` of the next |
//! | `integration_config_v3` | this core (protocol 3, unreleased) | everything |
//!
//! A layer is written only when it differs from the one before it, so a file
//! that needs no newer layer has exactly the bytes an older Couch wrote. Every
//! reader recomputes each projection from the richer layer and refuses the file
//! if it does not match, and an older Couch rewrites the file without the keys
//! it does not know, so its save is the truth after the next update.
//!
//! The layers are computed by composition (`v1 = v1_projection(v2)`, never
//! `v1_projection(everything)`), so the checks a released core runs hold by
//! construction whatever a later protocol adds. `tools/tests/config-crossload.sh`
//! runs the released model's source against files this one writes.
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
    /// Protocol-v2 cores read integration_config_v2 and ignore this extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integration_config_v3: Option<Config>,
}

impl StoredConfig {
    pub fn new(config: &Config) -> Self {
        let v2 = v2_projection(config);
        let v1 = v1_projection(&v2);
        let rollback = projection(&v2);
        Self {
            integration_config: (rollback != v1).then(|| v1.clone()),
            integration_config_v3: (v2 != *config).then(|| config.clone()),
            integration_config_v2: (v1 != v2).then_some(v2),
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
        let v2 = match self.integration_config_v2 {
            Some(config) if v1_projection(&config) == v1 => config,
            Some(_) => {
                return Err("Protocol-v2 configuration does not match its protocol-v1 projection")
            }
            None => v1,
        };
        match self.integration_config_v3 {
            Some(config) if v2_projection(&config) == v2 => Ok(config),
            Some(_) => Err("Protocol-v3 configuration does not match its protocol-v2 projection"),
            None => Ok(v2),
        }
    }

    pub fn has_integrations(&self) -> bool {
        self.integration_config.is_some()
            || self.integration_config_v2.is_some()
            || self.integration_config_v3.is_some()
    }
}

/// What a protocol 2 core can read: `config` without anything protocol 3
/// (unreleased) added. It strips by vocabulary, not by a package's version, so
/// a configuration with nothing new in it is returned unchanged and its file
/// keeps the exact bytes a protocol 2 core writes.
///
/// A released protocol 2 core refuses a file whose package snapshot has a
/// capability it cannot parse or more than one typed action, fails to parse a
/// component or action `kind` it does not know, and refuses any saved command
/// it cannot parse, on any device. Each of those is one predicate below.
///
/// Adding to protocol 3: a new field on a `Plugin` variant must be skipped when
/// empty and cleared here; a new enum variant must be removed here; a new
/// command grammar must be refused by [`v2_command`]. Then add it to the states
/// in `tools/tests/config-crossload.rs`, which is what proves it.
///
/// Children (one connection, many devices) are the second such addition. That
/// core ignores a field it does not know on a device, a connection or a scene,
/// so none of these would stop it parsing; each is removed because of what it
/// would do with the rest:
///
/// 1. the kinds a connection declares are cleared;
/// 2. a device's child snapshot is cleared, on both saved forms, and its
///    connection and resource are kept;
/// 3. everything aimed at such a device goes: a key binding is kept as a
///    disabled binding, steps, on and off commands and page buttons are
///    removed, the activity forgets the device, and a key that toggles it is
///    dropped while a key that opens it stays;
/// 4. a package scene is removed together with the areas' references to it;
/// 5. the typed actions are cut to the one that core knows, as before;
/// 6. a light, cover or climate component is removed;
/// 7. no command grammar is new: `dim:`, `position:` and `mode:` already parse.
///
/// A packaged media player is the third. The player component and the
/// percentage volume control are removed with everything else that core cannot
/// parse, and the five media schemas go with `v2_action_schemas`; the one thing
/// that is genuinely new is that `volume:30` *does* parse there and is refused
/// by its `supports`, which is the rule below.
///
/// As with the v1 projection, a save by a protocol 2 core is authoritative on
/// re-upgrade: what it never saw must not be resurrected.
fn v2_projection(config: &Config) -> Config {
    let mut result = config.clone();
    fn snapshot(
        capabilities: &mut Vec<crate::PluginCapability>,
        presentation: &mut Vec<crate::PluginComponent>,
        actions: &mut Vec<crate::PluginActionSchema>,
    ) {
        capabilities.retain(|capability| v2_command(&capability.id));
        presentation.retain_mut(v2_component);
        v2_action_schemas(actions);
    }
    for connection in &mut result.connections {
        if let Provider::Plugin {
            capabilities,
            presentation,
            actions,
            children,
            ..
        } = &mut connection.provider
        {
            snapshot(capabilities, presentation, actions);
            children.clear();
        }
    }
    // The devices that are children of a connection. Each keeps its
    // connection and its resource, which a protocol 2 core reads as a device
    // of the whole connection and cannot drive (it refuses the package), so
    // the next update finds the device where it was and only has to list the
    // connection's children again to know what it is.
    let mut children: Vec<crate::DeviceId> = Vec::new();
    for room in &mut result.rooms {
        for device in &mut room.devices {
            let child = match &mut device.integration {
                Integration::Plugin {
                    capabilities,
                    presentation,
                    actions,
                    child,
                    ..
                } => {
                    snapshot(capabilities, presentation, actions);
                    child
                }
                Integration::Connection { child, .. } => child,
                _ => continue,
            };
            if child.take().is_some() {
                children.push(device.id.clone());
            }
        }
    }
    // Whatever is aimed at a child goes, whether or not an older core could
    // parse it and whether or not it would accept it. That core validates a
    // binding against the connection's commands, not the kind's (`dim:30` and
    // `toggle` are refused, and the daemon then does not start), and what it
    // did accept it would send with no resource: to the whole bridge.
    let child = |device: &crate::DeviceId| children.contains(device);
    for activity in &mut result.activities {
        for id in &children {
            activity.setup.forget_device(id);
        }
        for binding in &mut activity.buttons {
            if binding.action.as_ref().is_some_and(|a| child(&a.device)) {
                binding.action = None;
            }
        }
        activity.steps.retain(|step| !child(&step.device));
    }
    for scene in &mut result.scenes {
        scene.steps.retain(|step| !child(&step.device));
    }
    // A key that switches a child is refused by an older core. A key that
    // opens one is a key that opens a device, which it has always accepted.
    for area in &mut result.areas {
        area.shortcuts.retain(
            |s| !matches!(&s.action, crate::ShortcutAction::Toggle { device } if child(device)),
        );
    }
    // A package scene is an empty scene to an older core: a button that does
    // nothing. It goes, with every area's reference to it.
    let package_scenes: Vec<crate::SceneId> = result
        .scenes
        .iter()
        .filter(|scene| scene.resource.is_some())
        .map(|scene| scene.id.clone())
        .collect();
    for id in &package_scenes {
        result.remove_scene(id);
    }
    // The one rule protocol 3 adds that is about what a file *says* rather
    // than about a word an older reader cannot parse. A percentage aimed at a
    // packaged connection is the typed action the one host gate makes from it,
    // and a protocol 2 core has no such action: its `supports` is a literal
    // capability match, so `volume:30` there makes the whole file invalid and
    // the daemon does not start.
    //
    // It goes from exactly the three sites that core validates with
    // `supports_device` - a key binding, a setup on/off command and a page
    // widget. A step and a scene step it only parses, and it can already hold
    // them, so leaving those alone is what keeps this the identity on anything
    // that core can write. A `volume:` id a package literally declares as one
    // of its own buttons is an ordinary command that core accepts, and stays.
    let packaged: Vec<(crate::DeviceId, Vec<alloc::string::String>)> = config
        .devices()
        .filter_map(
            |(_, device)| match config.resolve_integration(&device.integration) {
                Some(Integration::Plugin { capabilities, .. }) => Some((
                    device.id.clone(),
                    capabilities.into_iter().map(|c| c.id).collect(),
                )),
                _ => None,
            },
        )
        .collect();
    let keeps_percent = |action: &crate::Action| {
        !action.command.starts_with("volume:")
            || packaged
                .iter()
                .all(|(id, declared)| id != &action.device || declared.contains(&action.command))
    };
    for activity in &mut result.activities {
        for binding in &mut activity.buttons {
            if binding
                .action
                .as_ref()
                .is_some_and(|action| !keeps_percent(action))
            {
                // A disabled binding, as everywhere else here: removing it
                // would give the key its activity default back.
                binding.action = None;
            }
        }
        for steps in [&mut activity.setup.on, &mut activity.setup.off] {
            steps.retain(|step| !matches!(step, crate::SequenceStep::Command { action } if !keeps_percent(action)));
        }
        for page in &mut activity.setup.pages {
            page.widgets.retain(|widget| keeps_percent(&widget.action));
        }
    }
    // Every device, not only packaged ones: an old core refuses a command it
    // cannot parse wherever it is aimed.
    let compatible = |action: &crate::Action| v2_command(&action.command);
    for activity in &mut result.activities {
        for binding in &mut activity.buttons {
            if binding
                .action
                .as_ref()
                .is_some_and(|action| !compatible(action))
            {
                // Kept as an explicit disabled binding; removing it would
                // restore the key's default action.
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

/// Whether a protocol 2 core can parse this command or capability id. By
/// prefix rather than by [`crate::commands::Function::parse`]: a malformed
/// `x:` id must not reach an old reader either.
fn v2_command(id: &str) -> bool {
    !id.starts_with("x:")
}

/// Keeps a component a protocol 2 core can parse and validate. A group keeps
/// the commands that core knows and goes only if none are left; a switch needs
/// both of its commands.
fn v2_component(component: &mut crate::PluginComponent) -> bool {
    match component {
        crate::PluginComponent::CommandGroup { commands, .. } => {
            let before = commands.len();
            commands.retain(|command| v2_command(command));
            !(commands.is_empty() && before > 0)
        }
        crate::PluginComponent::Toggle { on, off, .. } => v2_command(on) && v2_command(off),
        crate::PluginComponent::StatusText { .. }
        | crate::PluginComponent::VolumeDbControl { .. }
        | crate::PluginComponent::InputSelector { .. } => true,
        // A protocol 2 core cannot parse these tags at all. The typed actions
        // they are drawn over go in `v2_action_schemas`.
        crate::PluginComponent::Light { .. }
        | crate::PluginComponent::Cover { .. }
        | crate::PluginComponent::Climate { .. }
        | crate::PluginComponent::MediaPlayer { .. }
        | crate::PluginComponent::VolumePercentControl { .. } => false,
    }
}

/// A protocol 2 core knows one typed action and accepts at most one schema.
fn v2_action_schemas(actions: &mut Vec<crate::PluginActionSchema>) {
    actions.retain(|schema| matches!(schema, crate::PluginActionSchema::SetVolumeDb { .. }));
    actions.truncate(1);
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
    use alloc::vec::Vec;

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
                children: vec![],
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: Id::new("external"),
            resource_id: "zone1".into(),
            child: None,
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
            child: None,
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
        // Bytes, not only values: key order is part of what a release wrote.
        assert_eq!(
            serde_json::to_vec_pretty(&StoredConfig::new(&config)).unwrap(),
            serde_json::to_vec_pretty(&config).unwrap()
        );
        let mut stored = StoredConfig::new(&plugin_config());
        stored.rollback.revision += 1;
        assert!(stored.into_config().is_err());
    }

    /// `StoredConfig` exactly as `v0.1.0-alpha.20260919.188` declares and
    /// builds it. `projection` and `v1_projection` are that release's, unedited,
    /// so this is the file that release writes for a configuration it can hold.
    #[derive(Serialize)]
    struct Stored188 {
        #[serde(flatten)]
        rollback: Config,
        #[serde(skip_serializing_if = "Option::is_none")]
        integration_config: Option<Config>,
        #[serde(skip_serializing_if = "Option::is_none")]
        integration_config_v2: Option<Config>,
    }
    impl Stored188 {
        fn new(config: &Config) -> Self {
            let rollback = projection(config);
            let v1 = v1_projection(config);
            Self {
                integration_config: (rollback != v1).then_some(v1.clone()),
                integration_config_v2: (v1 != *config).then(|| config.clone()),
                rollback,
            }
        }
    }

    /// What `.188` can parse of a package snapshot and of saved commands,
    /// mirrored rather than reused: today's richer enums would conceal exactly
    /// the regression this looks for. The real release source is run against
    /// real files by `tools/tests/config-crossload.sh`.
    mod release_188 {
        use alloc::{string::String, vec::Vec};
        use serde::Deserialize;

        #[allow(dead_code)]
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Field {
            On,
            Playing,
            Muted,
            Volume,
            VolumeDb,
            Input,
            Title,
        }
        #[allow(dead_code)]
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        pub enum Component {
            CommandGroup {
                title: String,
                commands: Vec<String>,
            },
            StatusText {
                label: String,
                field: Field,
            },
            Toggle {
                label: String,
                state: Field,
                on: String,
                off: String,
            },
            VolumeDbControl {
                label: String,
            },
            InputSelector {
                label: String,
            },
        }
        #[allow(dead_code)]
        #[derive(Deserialize)]
        #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
        pub enum ActionSchema {
            SetVolumeDb {
                min_tenths: i16,
                max_tenths: i16,
                step_tenths: u16,
            },
        }
        #[derive(Deserialize)]
        pub struct Capability {
            pub id: String,
        }
        #[derive(Deserialize, Default)]
        pub struct Snapshot {
            #[serde(default)]
            pub capabilities: Vec<Capability>,
            #[serde(default)]
            pub supports_inputs: bool,
            #[serde(default)]
            pub presentation: Vec<Component>,
            #[serde(default)]
            pub actions: Vec<ActionSchema>,
        }
        impl Snapshot {
            /// That release's `Function::supports` for a packaged device: an
            /// input if the package has inputs, otherwise an id the snapshot
            /// lists. It knows nothing of kinds of child, so `dim:30` and
            /// `toggle` on a lamp of a bridge are refused.
            fn supports(&self, command: &str) -> bool {
                if command.starts_with("input:") {
                    return self.supports_inputs;
                }
                self.capabilities.iter().any(|c| c.id == command)
            }
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "kebab-case")]
        pub enum Provider {
            Plugin(Snapshot),
            #[serde(other)]
            BuiltIn,
        }
        #[derive(Deserialize)]
        #[serde(tag = "via", rename_all = "kebab-case")]
        pub enum Integration {
            Plugin(Snapshot),
            /// `child` is not here because that release has no such field: it
            /// reads a child as a device of the whole connection.
            Connection {
                connection_id: String,
            },
            #[serde(other)]
            Other,
        }
        #[derive(Deserialize)]
        pub struct Connection {
            pub id: String,
            pub provider: Provider,
        }
        #[derive(Deserialize)]
        pub struct Device {
            pub id: String,
            pub integration: Integration,
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "kebab-case")]
        pub enum ShortcutAction {
            Toggle {
                device: String,
            },
            #[serde(other)]
            Other,
        }
        #[derive(Deserialize)]
        pub struct Shortcut {
            pub action: ShortcutAction,
        }
        #[derive(Deserialize)]
        pub struct Area {
            #[serde(default)]
            pub shortcuts: Vec<Shortcut>,
        }
        #[derive(Deserialize)]
        pub struct Room {
            pub devices: Vec<Device>,
        }
        #[derive(Deserialize)]
        pub struct Action {
            pub device: String,
            pub command: String,
        }
        #[derive(Deserialize)]
        pub struct Binding {
            pub action: Option<Action>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "kebab-case")]
        pub enum Step {
            Command { action: Action },
            Delay {},
        }
        #[derive(Deserialize)]
        pub struct Widget {
            pub action: Action,
        }
        #[derive(Deserialize)]
        pub struct Page {
            #[serde(default)]
            pub widgets: Vec<Widget>,
        }
        #[derive(Deserialize, Default)]
        pub struct Setup {
            #[serde(default)]
            pub pages: Vec<Page>,
            #[serde(default)]
            pub on: Vec<Step>,
            #[serde(default)]
            pub off: Vec<Step>,
        }
        #[derive(Deserialize)]
        pub struct Activity {
            #[serde(default)]
            pub setup: Setup,
            #[serde(default)]
            pub buttons: Vec<Binding>,
            #[serde(default)]
            pub steps: Vec<Action>,
        }
        #[derive(Deserialize)]
        pub struct Scene {
            #[serde(default)]
            pub steps: Vec<Action>,
        }
        #[derive(Deserialize)]
        pub struct Config {
            #[serde(default)]
            pub connections: Vec<Connection>,
            #[serde(default)]
            pub areas: Vec<Area>,
            pub rooms: Vec<Room>,
            pub scenes: Vec<Scene>,
            pub activities: Vec<Activity>,
        }
        #[derive(Deserialize)]
        pub struct Envelope {
            #[serde(flatten)]
            pub rollback: Config,
            #[serde(default)]
            pub integration_config: Option<Config>,
            #[serde(default)]
            pub integration_config_v2: Option<Config>,
        }
        /// That release's vocabulary is today's without the package namespace.
        /// A later protocol 3 step that adds a grammar has to refuse it here.
        fn parses(command: &str) -> bool {
            crate::commands::Function::parse(command)
                .is_some_and(|f| !matches!(f, crate::commands::Function::Custom(_)))
        }
        impl Config {
            /// The package snapshot that release resolves a device to, if it
            /// is a packaged one.
            fn packaged(&self, device: &str) -> Option<&Snapshot> {
                let device = self
                    .rooms
                    .iter()
                    .flat_map(|room| &room.devices)
                    .find(|d| d.id == device)?;
                match &device.integration {
                    Integration::Plugin(snapshot) => Some(snapshot),
                    Integration::Connection { connection_id } => self
                        .connections
                        .iter()
                        .find(|c| &c.id == connection_id)
                        .and_then(|c| match &c.provider {
                            Provider::Plugin(snapshot) => Some(snapshot),
                            Provider::BuiltIn => None,
                        }),
                    Integration::Other => None,
                }
            }
            /// The release's own refusals that the types above cannot express.
            pub fn refusal(&self) -> Option<String> {
                // A key, a page button and an on or off command have to be
                // something their device supports; a step only has to parse.
                // Only packaged devices are judged here: the built-in catalogs
                // are the same in that release and are not what changes.
                for activity in &self.activities {
                    let bound = activity
                        .buttons
                        .iter()
                        .filter_map(|b| b.action.as_ref())
                        .chain(
                            activity
                                .setup
                                .pages
                                .iter()
                                .flat_map(|page| page.widgets.iter().map(|w| &w.action)),
                        )
                        .chain(
                            activity
                                .setup
                                .on
                                .iter()
                                .chain(&activity.setup.off)
                                .filter_map(|step| match step {
                                    Step::Command { action } => Some(action),
                                    Step::Delay {} => None,
                                }),
                        );
                    for action in bound {
                        if self
                            .packaged(&action.device)
                            .is_some_and(|snapshot| !snapshot.supports(&action.command))
                        {
                            return Some(alloc::format!(
                                "{:?} is bound to a packaged device that does not list it",
                                action.command
                            ));
                        }
                    }
                }
                // Its `can_toggle` is false for every packaged device.
                for shortcut in self.areas.iter().flat_map(|area| &area.shortcuts) {
                    if let ShortcutAction::Toggle { device } = &shortcut.action {
                        if self.packaged(device).is_some() {
                            return Some("a key toggles a packaged device".into());
                        }
                    }
                }
                let mut commands: Vec<&str> = Vec::new();
                let mut snapshots: Vec<&Snapshot> = Vec::new();
                for connection in &self.connections {
                    if let Provider::Plugin(snapshot) = &connection.provider {
                        snapshots.push(snapshot);
                    }
                }
                for device in self.rooms.iter().flat_map(|room| &room.devices) {
                    if let Integration::Plugin(snapshot) = &device.integration {
                        snapshots.push(snapshot);
                    }
                }
                for snapshot in snapshots {
                    if snapshot.actions.len() > 1 {
                        return Some("more than one typed action".into());
                    }
                    commands.extend(snapshot.capabilities.iter().map(|c| c.id.as_str()));
                    for component in &snapshot.presentation {
                        match component {
                            Component::CommandGroup { commands: ids, .. } => {
                                if ids.is_empty() {
                                    return Some("an empty command group".into());
                                }
                                commands.extend(ids.iter().map(String::as_str));
                            }
                            Component::Toggle { on, off, .. } => commands.extend([&**on, &**off]),
                            _ => {}
                        }
                    }
                }
                for activity in &self.activities {
                    commands.extend(
                        activity
                            .buttons
                            .iter()
                            .filter_map(|b| b.action.as_ref())
                            .chain(&activity.steps)
                            .chain(
                                activity.setup.pages.iter().flat_map(|page| {
                                    page.widgets.iter().map(|widget| &widget.action)
                                }),
                            )
                            .chain(
                                activity
                                    .setup
                                    .on
                                    .iter()
                                    .chain(&activity.setup.off)
                                    .filter_map(|step| match step {
                                        Step::Command { action } => Some(action),
                                        Step::Delay {} => None,
                                    }),
                            )
                            .map(|action| action.command.as_str()),
                    );
                }
                commands.extend(
                    self.scenes
                        .iter()
                        .flat_map(|scene| &scene.steps)
                        .map(|action| action.command.as_str()),
                );
                commands
                    .into_iter()
                    .find(|command| !parses(command))
                    .map(|command| alloc::format!("cannot parse {command:?}"))
            }
        }
        impl Envelope {
            /// The configuration that release ends up holding. Its consistency
            /// checks are the unedited functions and are asserted separately.
            pub fn holds(&self) -> &Config {
                self.integration_config_v2
                    .as_ref()
                    .or(self.integration_config.as_ref())
                    .unwrap_or(&self.rollback)
            }
        }
    }

    /// The `.130`-era shape `build/webui-review-empty.json` still has: a
    /// scene step naming `"bright"`, which no release has ever run. `.188`
    /// refuses it exactly as this tree does before `Config::migrate_commands`
    /// runs; both accept `dim:100`, which is what that rewrite leaves behind.
    /// A full crossload state was not added for this
    /// (`tools/tests/config-crossload.sh`) because every existing state
    /// starts from a config only this tree can build; this reuses the
    /// frozen `.188` mirror in `release_188` instead, the way the tests
    /// above already do.
    #[test]
    fn a_legacy_bright_step_is_refused_by_release_188_and_migrating_it_fixes_that() {
        let mut config = Config::default();
        config.rooms.push(crate::Room {
            id: "kitchen".into(),
            name: "Kitchen".into(),
            icon: None,
            devices: vec![crate::Device::new(
                "kitchen-hue".into(),
                "Kitchen light",
                crate::DeviceKind::Light,
            )
            .with_integration(Integration::Hue {
                light_id: "1".into(),
            })],
        });
        config.scenes.push(crate::Scene {
            id: "dinner".into(),
            name: "Dinner".into(),
            icon: None,
            steps: vec![crate::Action::new(Id::new("kitchen-hue"), "bright")],
            hue: None,
            resource: None,
            rooms: vec![],
        });
        let before = serde_json::to_vec(&Stored188::new(&config)).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&before).unwrap();
        assert_eq!(
            old.holds().refusal(),
            Some("cannot parse \"bright\"".into())
        );

        assert!(config.migrate_commands());
        assert_eq!(config.scenes[0].steps[0].command, "dim:100");
        assert!(config.validate().is_ok());
        let after = serde_json::to_vec(&Stored188::new(&config)).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&after).unwrap();
        assert_eq!(old.holds().refusal(), None);
    }

    fn named(ids: &[&str]) -> Vec<crate::PluginCapability> {
        ids.iter()
            .map(|id| crate::PluginCapability {
                id: (*id).into(),
                label: "Child".into(),
            })
            .collect()
    }

    /// The four kinds of child a bridge-like package declares.
    fn child_kinds() -> Vec<crate::PluginChildKind> {
        use crate::{ChildComponent, DeviceKind, PluginActionSchema, PluginChildKind};
        let kind = |kind: &str, device_kind, component, ids: &[&str], actions| PluginChildKind {
            kind: kind.into(),
            label: "Kind".into(),
            device_kind,
            component,
            capabilities: named(ids),
            actions,
        };
        vec![
            kind(
                "light",
                DeviceKind::Light,
                ChildComponent::Light,
                &["on", "off", "toggle", "x:blink"],
                vec![PluginActionSchema::SetLight {}],
            ),
            kind(
                "scene",
                DeviceKind::Other,
                ChildComponent::Scene,
                &["on"],
                vec![],
            ),
            kind(
                "blind",
                DeviceKind::Blind,
                ChildComponent::Cover,
                &["open", "close", "stop", "toggle"],
                vec![PluginActionSchema::SetCover {}],
            ),
            kind(
                "thermostat",
                DeviceKind::Thermostat,
                ChildComponent::Climate,
                &["temperature-up", "temperature-down"],
                vec![PluginActionSchema::SetClimate {}],
            ),
        ]
    }

    fn snapshot(kind: &str) -> crate::ChildSnapshot {
        crate::ChildSnapshot {
            kind: kind.into(),
            light: None,
            cover: None,
            climate: None,
        }
    }

    /// A valid configuration with one packaged connection and a random mix of
    /// protocol 1, 2 and (with `custom`) 3 content in every place a command can
    /// be saved, aimed at packaged and built-in devices alike. Protocol 3 is
    /// package-named buttons, children of the connection saved as room devices
    /// in both forms, package scenes, and a connection that is itself a lamp.
    fn random_config(random: &mut crate::commands::tests::Lcg, custom: bool) -> Config {
        use crate::commands::Function;
        let mut config = Config::seed();
        let mut capabilities: Vec<crate::PluginCapability> = Vec::new();
        for id in [
            "power-on",
            "power-off",
            "volume-up",
            "volume-down",
            "mute",
            "menu",
        ] {
            if random.below(3) > 0 {
                capabilities.push(crate::PluginCapability {
                    id: id.into(),
                    label: "Known".into(),
                });
            }
        }
        if custom {
            for _ in 0..random.below(5) {
                let id = alloc::format!("x:{}", random.custom_id());
                if capabilities.iter().all(|c| c.id != id) {
                    capabilities.push(crate::PluginCapability {
                        id,
                        label: "Own".into(),
                    });
                }
            }
        }
        let supports_inputs = random.below(2) == 0;
        let mut actions = vec![];
        let mut presentation = vec![];
        if random.below(2) == 0 {
            actions.push(crate::PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5,
            });
            presentation.push(crate::PluginComponent::VolumeDbControl {
                label: "Volume".into(),
            });
            presentation.push(crate::PluginComponent::StatusText {
                label: "Volume".into(),
                field: crate::PluginStatusField::VolumeDb,
            });
        }
        if supports_inputs {
            presentation.push(crate::PluginComponent::InputSelector {
                label: "Source".into(),
            });
        }
        if custom && random.below(3) == 0 {
            actions.push(crate::PluginActionSchema::SetLight {});
            presentation.push(crate::PluginComponent::Light {
                label: "Lamp".into(),
            });
            if random.below(2) == 0 {
                actions.push(crate::PluginActionSchema::SetCover {});
                actions.push(crate::PluginActionSchema::SetClimate {});
                presentation.push(crate::PluginComponent::Cover {
                    label: "Blind".into(),
                });
                presentation.push(crate::PluginComponent::Climate {
                    label: "Heating".into(),
                });
            }
        }
        // A packaged media player (protocol 3, unreleased). Its row opens the
        // player screen, and `volume:30` aimed at it is the typed action the
        // one host gate makes from it, which no protocol 2 core has.
        if custom && random.below(2) == 0 && actions.len() + 2 <= crate::volume::MAX_ACTIONS {
            for id in ["play-pause", "next"] {
                if capabilities.iter().all(|c| c.id != id) {
                    capabilities.push(crate::PluginCapability {
                        id: id.into(),
                        label: "Player".into(),
                    });
                }
            }
            actions.push(crate::PluginActionSchema::SetVolumePercent { max_percent: 100 });
            actions.push(crate::PluginActionSchema::StepVolumePercent { max_delta: 5 });
            // Sources are a sheet of their own, so a modes sheet only fits
            // while the package has no inputs: three sheets, no more.
            if !supports_inputs && actions.len() < crate::volume::MAX_ACTIONS {
                actions.push(crate::PluginActionSchema::SetMode {
                    modes: crate::PlayModeSet::new()
                        .with(crate::PlayMode::Shuffle)
                        .with(crate::PlayMode::Repeat),
                });
            }
            if actions.len() < crate::volume::MAX_ACTIONS && random.below(2) == 0 {
                actions.push(crate::PluginActionSchema::Seek {});
            }
            presentation.push(crate::PluginComponent::MediaPlayer {
                layout: crate::MediaLayout::Music,
                artwork: vec![crate::ArtRole::Cover],
                lists: vec![],
                up_next: false,
                navigation: false,
                refresh_ms: crate::DEFAULT_REFRESH_MS,
                keys: vec![],
            });
            presentation.push(crate::PluginComponent::VolumePercentControl {
                label: "Volume".into(),
            });
        }
        let children = if custom && random.below(4) > 0 {
            child_kinds()
        } else {
            vec![]
        };
        for _ in 0..random.below(4) {
            if capabilities.is_empty() {
                break;
            }
            let mut commands: Vec<alloc::string::String> = Vec::new();
            for _ in 0..1 + random.below(4) {
                let id = &capabilities[random.below(capabilities.len())].id;
                if !commands.contains(id) {
                    commands.push(id.clone());
                }
            }
            presentation.push(crate::PluginComponent::CommandGroup {
                title: "Group".into(),
                commands,
            });
        }
        if capabilities.len() >= 2 {
            let on = random.below(capabilities.len());
            let off = (on + 1 + random.below(capabilities.len() - 1)) % capabilities.len();
            presentation.push(crate::PluginComponent::Toggle {
                label: "Switch".into(),
                state: crate::PluginStatusField::On,
                on: capabilities[on].id.clone(),
                off: capabilities[off].id.clone(),
            });
        }
        let connection = Id::new("external");
        config.connections.push(Connection {
            id: connection.clone(),
            name: "External".into(),
            provider: Provider::Plugin {
                id: "echo".into(),
                label: "Echo".into(),
                capabilities: capabilities.clone(),
                supports_inputs,
                presentation: presentation.clone(),
                actions: actions.clone(),
                children: children.clone(),
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: connection.clone(),
            resource_id: "zone1".into(),
            child: None,
        };
        if random.below(2) == 0 {
            config.rooms[0].devices[1].integration = Integration::Plugin {
                id: "echo".into(),
                connection_id: connection.clone(),
                resource_id: "zone2".into(),
                capabilities,
                supports_inputs,
                presentation,
                actions,
                child: None,
            };
        }
        // Children of the connection as room devices: a lamp in the saved
        // form, a lamp in the resolved form, a blind and a thermostat, each
        // with traits that decide whether a level can be bound to it.
        let mut members: Vec<Id> = config.rooms[0].devices[..3]
            .iter()
            .map(|device| device.id.clone())
            .collect();
        if !children.is_empty() {
            let mut lamp = snapshot("light");
            lamp.light = Some(crate::LightTraits {
                dimmable: random.below(3) > 0,
                mirek: (random.below(2) == 0).then_some((153, 500)),
                color: random.below(2) == 0,
            });
            let mut blind = snapshot("blind");
            blind.cover = Some(crate::CoverTraits {
                position: random.below(3) > 0,
                stop: true,
            });
            let mut thermostat = snapshot("thermostat");
            thermostat.climate = Some(crate::ClimateTraits {
                min_tenths: 70,
                max_tenths: 300,
                step_tenths: 5,
                unit: crate::TempUnit::Celsius,
                modes: crate::domain::ALL_CLIMATE_MODES
                    .iter()
                    .copied()
                    .filter(|_| random.below(2) == 0)
                    .collect(),
                range: random.below(2) == 0,
            });
            let light_kind = &children[0];
            // (room, device, the kind of device it has to be, how it is saved)
            let saved = [
                (
                    0,
                    3,
                    crate::DeviceKind::Light,
                    Integration::Connection {
                        connection_id: connection.clone(),
                        resource_id: "5f0c9a52-7d1e-4a63-9b0e-2f6d1c3a8e41".into(),
                        child: Some(lamp.clone()),
                    },
                ),
                (
                    0,
                    4,
                    crate::DeviceKind::Light,
                    Integration::Plugin {
                        id: "echo".into(),
                        connection_id: connection.clone(),
                        resource_id: "room/9d2b7c10-35aa-4c0e-8a57-6e1f0b94d2c3".into(),
                        capabilities: light_kind.capabilities.clone(),
                        supports_inputs: false,
                        presentation: vec![],
                        actions: light_kind.actions.clone(),
                        child: Some(lamp),
                    },
                ),
                (
                    2,
                    2,
                    crate::DeviceKind::Blind,
                    Integration::Connection {
                        connection_id: connection.clone(),
                        resource_id: "cover/blind-1".into(),
                        child: Some(blind),
                    },
                ),
                (
                    1,
                    1,
                    crate::DeviceKind::Thermostat,
                    Integration::Connection {
                        connection_id: connection.clone(),
                        resource_id: "climate.hall_1".into(),
                        child: Some(thermostat),
                    },
                ),
            ];
            for (room, device, kind, integration) in saved {
                if random.below(4) == 0 {
                    continue;
                }
                let device = &mut config.rooms[room].devices[device];
                device.kind = kind;
                device.integration = integration;
                members.push(device.id.clone());
            }
            // Package scenes, listed by an area or not. Never the first scene:
            // the tests below edit that one.
            for n in 0..random.below(3) {
                let id = Id::new(alloc::format!("package-scene-{n}"));
                config.scenes.push(crate::Scene {
                    id: id.clone(),
                    name: "Package scene".into(),
                    icon: None,
                    steps: vec![],
                    hue: None,
                    resource: Some(crate::SceneResource {
                        connection_id: connection.clone(),
                        resource_id: alloc::format!("scene/{n}"),
                        kind: "scene".into(),
                    }),
                    rooms: vec![config.rooms[0].id.clone()],
                });
                if random.below(2) == 0 {
                    config.areas[random.below(2)].scenes.push(id);
                }
            }
        }
        // Everything each of those devices accepts, packaged or not.
        let mut offered: Vec<crate::Action> = Vec::new();
        for device in config
            .devices()
            .map(|(_, device)| device)
            .filter(|device| members.contains(&device.id))
        {
            let integration = config.resolve_integration(&device.integration).unwrap();
            let mut ids: Vec<alloc::string::String> =
                crate::buttons::function_choices(&integration)
                    .into_iter()
                    .map(|choice| choice.0)
                    .collect();
            ids.extend(["input:HDMI1".into(), "input:HD RADIO".into()]);
            // Levels are not catalog rows: a picker collects the number.
            ids.extend(
                [
                    "dim:30",
                    "volume:30",
                    "position:40",
                    "mode:heat",
                    "mode:fan_only",
                ]
                .map(alloc::string::String::from),
            );
            for id in ids {
                if Function::parse(&id).is_some_and(|f| f.supports_device(device, &config)) {
                    offered.push(crate::Action::new(device.id.clone(), id));
                }
            }
        }
        // Quick-access keys that switch or open whichever of those devices
        // this configuration lets them.
        let mut keys = crate::SHORTCUT_BUTTONS.iter().copied();
        let mut shortcuts = Vec::new();
        for device in config
            .devices()
            .map(|(_, device)| device)
            .filter(|device| members.contains(&device.id))
        {
            if config.can_toggle(device) && random.below(2) == 0 {
                shortcuts.push(crate::Shortcut {
                    button: keys.next().unwrap(),
                    action: crate::ShortcutAction::Toggle {
                        device: device.id.clone(),
                    },
                });
            }
            if random.below(4) == 0 {
                shortcuts.push(crate::Shortcut {
                    button: keys.next().unwrap(),
                    action: crate::ShortcutAction::Device {
                        device: device.id.clone(),
                    },
                });
            }
        }
        config.areas[0].shortcuts = shortcuts;
        let pick = |random: &mut crate::commands::tests::Lcg, most: usize| -> Vec<crate::Action> {
            if offered.is_empty() {
                return Vec::new();
            }
            (0..random.below(most + 1))
                .map(|_| offered[random.below(offered.len())].clone())
                .collect()
        };
        let buttons = [
            crate::buttons::Button::Red,
            crate::buttons::Button::Green,
            crate::buttons::Button::Blue,
            crate::buttons::Button::Yellow,
            crate::buttons::Button::Menu,
        ];
        let bound = pick(random, buttons.len());
        let steps = pick(random, 6);
        let on = pick(random, 6);
        let off = pick(random, 6);
        let widgets = pick(random, 6);
        let scene = pick(random, 6);
        let activity = &mut config.activities[0];
        activity.buttons = bound
            .into_iter()
            .zip(buttons)
            .map(|(action, button)| crate::buttons::Binding {
                button,
                gesture: Default::default(),
                action: Some(action),
            })
            .collect();
        activity.steps = steps;
        activity.setup.devices = members;
        let sequence = |actions: Vec<crate::Action>| {
            actions
                .into_iter()
                .map(|action| crate::SequenceStep::Command { action })
                .collect()
        };
        activity.setup.on = sequence(on);
        activity.setup.off = sequence(off);
        activity.setup.pages = vec![crate::ActivityPage {
            title: "Page".into(),
            widgets: widgets
                .into_iter()
                .map(|action| crate::ActivityWidget {
                    label: "Button".into(),
                    icon: None,
                    action,
                })
                .collect(),
        }];
        config.scenes[0].steps = scene;
        config.revision = random.next();
        config
            .validate()
            .unwrap_or_else(|e| panic!("the generator made an invalid configuration: {e}"));
        config
    }

    /// The words only protocol 3 writes, as `config-crossload.rs` looks for
    /// them in a file.
    fn mentions_custom(value: &impl Serialize) -> bool {
        let text = serde_json::to_string(value).unwrap();
        [
            "\"x:",
            "\"child\"",
            "\"children\"",
            "\"resource\":{",
            "set_light",
            "set_cover",
            "set_climate",
            // Quoted on both sides: `media_player` on its own also matches the
            // seed's Home Assistant entity `media_player.kitchen`, which every
            // release has always been able to hold.
            "\"media_player\"",
            "\"volume_percent_control\"",
            "\"set_volume_percent\"",
            "\"step_volume_percent\"",
            "\"seek\"",
            "\"seek_by\"",
            "\"set_mode\"",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    }

    #[test]
    fn anything_release_188_can_hold_is_its_own_v2_projection_and_keeps_its_bytes() {
        let mut random = crate::commands::tests::Lcg(188);
        let mut layers = [0usize; 3];
        for round in 0..300 {
            let config = random_config(&mut random, false);
            assert_eq!(v2_projection(&config), config, "round {round}");
            let stored = StoredConfig::new(&config);
            assert!(stored.integration_config_v3.is_none(), "round {round}");
            layers[usize::from(stored.integration_config.is_some())
                + usize::from(stored.integration_config_v2.is_some())] += 1;
            let ours = [
                serde_json::to_vec(&stored).unwrap(),
                serde_json::to_vec_pretty(&stored).unwrap(),
            ];
            let theirs = [
                serde_json::to_vec(&Stored188::new(&config)).unwrap(),
                serde_json::to_vec_pretty(&Stored188::new(&config)).unwrap(),
            ];
            assert!(
                ours == theirs,
                "round {round}: the file a release wrote changed"
            );
            // Loading that release's file and saving it moves no byte.
            let loaded = serde_json::from_slice::<StoredConfig>(&theirs[1])
                .unwrap()
                .into_config()
                .unwrap();
            assert_eq!(loaded, config, "round {round}");
            assert!(
                serde_json::to_vec_pretty(&StoredConfig::new(&loaded)).unwrap() == theirs[1],
                "round {round}"
            );
        }
        assert!(
            layers[1] > 0 && layers[2] > 0,
            "the generator reached v1-only and v1+v2 files: {layers:?}"
        );
    }

    #[test]
    fn v2_projection_is_idempotent_valid_and_what_release_188_loads() {
        let mut random = crate::commands::tests::Lcg(3);
        let (mut with_v3, mut without_v2) = (0, 0);
        // [child devices, keys bound to one, a key that toggles one, package
        // scenes an area lists, a connection that is itself a lamp, a
        // percentage aimed at a packaged player where the projection removes
        // it, and one where it stays]
        let mut reached = [0usize; 7];
        for round in 0..300 {
            let config = random_config(&mut random, true);
            let v2 = v2_projection(&config);
            let children: Vec<&crate::Device> = config
                .devices()
                .map(|(_, d)| d)
                .filter(|d| config.device_child_kind(&d.integration).is_some())
                .collect();
            let is_child = |id: &Id| children.iter().any(|d| &d.id == id);
            reached[0] += children.len();
            reached[1] += config.activities[0]
                .buttons
                .iter()
                .filter(|b| b.action.as_ref().is_some_and(|a| is_child(&a.device)))
                .count();
            reached[2] += config.areas[0]
                .shortcuts
                .iter()
                .filter(|s| matches!(&s.action, crate::ShortcutAction::Toggle { device } if is_child(device)))
                .count();
            reached[3] += config
                .areas
                .iter()
                .flat_map(|area| &area.scenes)
                .filter(|id| config.scene(id).is_some_and(|s| s.resource.is_some()))
                .count();
            reached[4] += usize::from(
                serde_json::to_string(&config.connections)
                    .unwrap()
                    .contains("\"kind\":\"light\",\"label\":\"Lamp\""),
            );
            // The one new on-disk rule: a `volume:` command aimed at a
            // packaged device goes from the three sites `.188` validates, and
            // stays in the two it only parses.
            let packaged = |id: &Id| {
                config.devices().any(|(_, d)| {
                    &d.id == id
                        && matches!(
                            config.resolve_integration(&d.integration),
                            Some(Integration::Plugin { .. })
                        )
                })
            };
            let percent = |action: &crate::Action| {
                action.command.starts_with("volume:") && packaged(&action.device)
            };
            let validated = |config: &Config| {
                config.activities[0]
                    .buttons
                    .iter()
                    .filter_map(|b| b.action.as_ref())
                    .filter(|a| percent(a))
                    .count()
                    + config.activities[0]
                        .setup
                        .on
                        .iter()
                        .chain(&config.activities[0].setup.off)
                        .filter(
                            |step| matches!(step, crate::SequenceStep::Command { action } if percent(action)),
                        )
                        .count()
                    + config.activities[0]
                        .setup
                        .pages
                        .iter()
                        .flat_map(|page| &page.widgets)
                        .filter(|w| percent(&w.action))
                        .count()
            };
            let parsed_only = |config: &Config| {
                config.activities[0]
                    .steps
                    .iter()
                    .chain(config.scenes.iter().flat_map(|scene| &scene.steps))
                    .filter(|a| percent(a))
                    .count()
            };
            reached[5] += validated(&config);
            reached[6] += parsed_only(&config);
            assert_eq!(validated(&v2), 0, "round {round}");
            assert_eq!(parsed_only(&v2), parsed_only(&config), "round {round}");
            // A child keeps its place and its address, so the next update
            // finds it again; everything else about it is gone.
            for child in &children {
                let kept = v2.devices().find(|(_, d)| d.id == child.id).unwrap().1;
                let address = |integration: &Integration| match integration {
                    Integration::Connection {
                        connection_id,
                        resource_id,
                        child,
                    }
                    | Integration::Plugin {
                        connection_id,
                        resource_id,
                        child,
                        ..
                    } => (connection_id.clone(), resource_id.clone(), child.is_some()),
                    _ => unreachable!(),
                };
                let (connection, resource, _) = address(&child.integration);
                assert_eq!(
                    address(&kept.integration),
                    (connection, resource, false),
                    "round {round}"
                );
                assert_eq!(kept.kind, child.kind, "round {round}");
                assert_eq!(kept.name, child.name, "round {round}");
            }
            assert_eq!(v2_projection(&v2), v2, "round {round}: idempotent");
            assert!(!mentions_custom(&v2), "round {round}");
            v2.validate()
                .unwrap_or_else(|e| panic!("round {round}: the v2 layer is invalid: {e}"));
            v1_projection(&v2).validate().unwrap();
            projection(&v2).validate().unwrap();

            let stored = StoredConfig::new(&config);
            assert_eq!(
                stored.integration_config_v3.is_some(),
                mentions_custom(&config),
                "round {round}: the v3 layer exists exactly when something needs it"
            );
            with_v3 += usize::from(stored.integration_config_v3.is_some());
            without_v2 += usize::from(
                stored.integration_config_v3.is_some() && stored.integration_config_v2.is_none(),
            );
            let bytes = serde_json::to_vec(&stored).unwrap();
            assert_eq!(
                serde_json::from_slice::<StoredConfig>(&bytes)
                    .unwrap()
                    .into_config()
                    .unwrap(),
                config,
                "round {round}: this core reads back everything it wrote"
            );

            // The release parses the file, holds the v2 projection, runs its own
            // (unedited) consistency checks on it, and finds nothing to refuse.
            let old: release_188::Envelope = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("round {round}: release 188 cannot parse: {e}"));
            assert_eq!(old.rollback.refusal(), None, "round {round}");
            assert_eq!(old.holds().refusal(), None, "round {round}");
            let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            file.as_object_mut()
                .unwrap()
                .remove("integration_config_v3");
            assert!(!mentions_custom(&file), "round {round}");
            let held = serde_json::from_value::<StoredConfig>(file)
                .unwrap()
                .into_config()
                .unwrap();
            assert_eq!(held, v2, "round {round}");

            // It saves, with an edit of its own. That save is the truth.
            let mut edited = held;
            edited.revision = edited.revision.wrapping_add(1);
            edited.rooms[0].name = "Renamed by the old core".into();
            let old_save = serde_json::to_vec(&Stored188::new(&edited)).unwrap();
            let after = serde_json::from_slice::<StoredConfig>(&old_save)
                .unwrap()
                .into_config()
                .unwrap();
            assert_eq!(after, edited, "round {round}");
            assert!(
                !mentions_custom(&after),
                "round {round}: a release 188 save cannot resurrect x: bindings"
            );
            assert_eq!(
                serde_json::to_vec(&StoredConfig::new(&after)).unwrap(),
                old_save,
                "round {round}: and saving it again adds no layer"
            );
        }
        assert!(with_v3 > 100, "the generator reached v3 files: {with_v3}");
        assert!(
            reached.iter().all(|n| *n > 20),
            "the generator reached children, keys bound to them, keys that toggle them, \
             listed package scenes, a connection that is a lamp, and a percentage aimed at \
             a packaged player in both kinds of place: {reached:?}"
        );
        assert!(
            without_v2 > 0,
            "and v3 files with no v2 layer: {without_v2}"
        );
    }

    #[test]
    fn a_tampered_v3_layer_fails_closed() {
        let mut random = crate::commands::tests::Lcg(7);
        let mut checked = 0;
        for _ in 0..200 {
            let config = random_config(&mut random, true);
            let file = serde_json::to_value(StoredConfig::new(&config)).unwrap();
            if file.get("integration_config_v3").is_none() {
                continue;
            }
            checked += 1;
            let load = |file: &serde_json::Value| {
                serde_json::from_value::<StoredConfig>(file.clone())
                    .unwrap()
                    .into_config()
            };
            assert_eq!(load(&file).unwrap(), config);
            // An edit to any one layer that the others do not share.
            for layer in [
                Some("integration_config_v3"),
                Some("integration_config_v2"),
                Some("integration_config"),
                None,
            ] {
                for edit in 0..3 {
                    let mut tampered = file.clone();
                    let document = match layer {
                        Some(key) if tampered.get(key).is_none() => continue,
                        Some(key) => &mut tampered[key],
                        None => &mut tampered,
                    };
                    match edit {
                        0 => document["revision"] = serde_json::json!(1),
                        1 => document["rooms"][0]["name"] = serde_json::json!("Tampered"),
                        _ => document["scenes"][0]["steps"] = serde_json::json!([]),
                    }
                    if tampered == file {
                        continue;
                    }
                    // The newest layer is the only copy of what only it can
                    // say, exactly as the v2 layer is for a spaced input: a
                    // change no older layer can see is a change to the
                    // configuration, not an inconsistency. Everything an older
                    // layer does see has to agree with it.
                    let only_v3_content = layer == Some("integration_config_v3")
                        && edit == 2
                        && v2_projection(&config).scenes[0].steps.is_empty();
                    if only_v3_content {
                        assert!(load(&tampered).unwrap().scenes[0].steps.is_empty());
                        continue;
                    }
                    assert!(load(&tampered).is_err(), "{layer:?} edit {edit}");
                }
            }
            // A v3 layer beside layers it was not made from.
            let mut other = serde_json::to_value(StoredConfig::new(&Config::seed())).unwrap();
            other["integration_config_v3"] = file["integration_config_v3"].clone();
            assert_eq!(
                load(&other).unwrap_err(),
                "Protocol-v3 configuration does not match its protocol-v2 projection"
            );
        }
        assert!(checked > 50, "{checked}");
    }

    #[test]
    fn package_named_buttons_live_only_in_the_v3_layer() {
        let mut config = plugin_config();
        if let Provider::Plugin {
            capabilities,
            presentation,
            ..
        } = &mut config.connections.last_mut().unwrap().provider
        {
            *capabilities = ["menu", "x:info", "x:osd", "x:subs-on", "x:subs-off"]
                .into_iter()
                .map(|id| crate::PluginCapability {
                    id: id.into(),
                    label: "Label".into(),
                })
                .collect();
            *presentation = vec![
                crate::PluginComponent::CommandGroup {
                    title: "Mixed".into(),
                    commands: vec!["x:info".into(), "menu".into()],
                },
                crate::PluginComponent::CommandGroup {
                    title: "Own".into(),
                    commands: vec!["x:info".into(), "x:osd".into()],
                },
                crate::PluginComponent::Toggle {
                    label: "Subtitles".into(),
                    state: crate::PluginStatusField::Playing,
                    on: "x:subs-on".into(),
                    off: "x:subs-off".into(),
                },
            ];
        }
        let device = config.rooms[0].devices[0].id.clone();
        let own = crate::Action::new(device.clone(), "x:info");
        let known = crate::Action::new(device.clone(), "menu");
        config.activities[0].steps = vec![own.clone(), known.clone()];
        config.activities[0].buttons = vec![crate::buttons::Binding {
            button: crate::buttons::Button::Power,
            gesture: Default::default(),
            action: Some(own.clone()),
        }];
        config.activities[0].setup.devices = vec![device];
        config.activities[0].setup.on = vec![crate::SequenceStep::Command {
            action: own.clone(),
        }];
        config.activities[0].setup.off = vec![crate::SequenceStep::Command {
            action: known.clone(),
        }];
        config.activities[0].setup.pages = vec![crate::ActivityPage {
            title: "Player".into(),
            widgets: vec![crate::ActivityWidget {
                label: "Info".into(),
                icon: None,
                action: own.clone(),
            }],
        }];
        config.scenes[0].steps = vec![own];
        config.validate().unwrap();

        let stored = StoredConfig::new(&config);
        assert!(stored.has_integrations());
        assert!(stored.integration_config.is_some());
        assert!(
            stored.integration_config_v2.is_none(),
            "nothing here is protocol 2, so that layer is the v1 layer"
        );
        assert_eq!(stored.integration_config_v3.as_ref(), Some(&config));
        let v2 = v2_projection(&config);
        let Provider::Plugin {
            capabilities,
            presentation,
            ..
        } = &v2.connections.last().unwrap().provider
        else {
            unreachable!()
        };
        assert_eq!(capabilities.len(), 1);
        assert_eq!(
            presentation,
            &vec![crate::PluginComponent::CommandGroup {
                title: "Mixed".into(),
                commands: vec!["menu".into()],
            }],
            "a group keeps what an older Couch knows; a group or switch with nothing left goes"
        );
        assert_eq!(v2.activities[0].steps, vec![known.clone()]);
        assert_eq!(v2.activities[0].buttons.len(), 1);
        assert_eq!(v2.activities[0].buttons[0].action, None);
        assert_eq!(
            serde_json::to_value(&v2.activities[0].buttons[0]).unwrap()["action"],
            serde_json::Value::Null,
            "retain the explicit disabled binding; removing it would restore the default Power action"
        );
        assert!(v2.activities[0].setup.on.is_empty());
        assert_eq!(
            v2.activities[0].setup.off,
            vec![crate::SequenceStep::Command { action: known }]
        );
        assert!(v2.activities[0].setup.pages[0].widgets.is_empty());
        assert!(v2.scenes[0].steps.is_empty());

        // A file with only a v3 layer on top of a plain one: a built-in device
        // cannot hold an x: command, so this shape needs a package, but the
        // loader must not depend on the middle layers being present.
        let bytes = serde_json::to_vec(&stored).unwrap();
        assert!(serde_json::from_slice::<LegacyConfig>(&bytes).is_ok());
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            config
        );
    }

    /// A house with a packaged media player in it, and the same house with an
    /// ordinary package. The one new on-disk rule is that a percentage aimed
    /// at the player goes from the three places a protocol 2 core validates
    /// and stays in the two it only parses, so the two houses save the same
    /// bytes everywhere that core can see.
    #[test]
    fn a_player_and_a_house_without_one_write_the_same_bytes_for_an_older_core() {
        use crate::buttons::{Binding, Button, Gesture};
        let house = |player: bool| {
            let mut config = plugin_config();
            if let Provider::Plugin {
                capabilities,
                presentation,
                actions,
                ..
            } = &mut config.connections.last_mut().unwrap().provider
            {
                *capabilities = named(&["play-pause", "next", "volume-up"]);
                if player {
                    *presentation = vec![
                        crate::PluginComponent::MediaPlayer {
                            layout: crate::MediaLayout::Music,
                            artwork: vec![crate::ArtRole::Cover],
                            lists: vec![],
                            up_next: true,
                            navigation: false,
                            refresh_ms: 2_000,
                            keys: vec![crate::MediaKey {
                                key: crate::ColourKey::Red,
                                command: "next".into(),
                            }],
                        },
                        crate::PluginComponent::VolumePercentControl {
                            label: "Volume".into(),
                        },
                    ];
                    *actions = vec![
                        crate::PluginActionSchema::SetVolumePercent { max_percent: 100 },
                        crate::PluginActionSchema::StepVolumePercent { max_delta: 5 },
                        crate::PluginActionSchema::Seek {},
                        crate::PluginActionSchema::SeekBy {
                            max_delta_ms: 30_000,
                        },
                        crate::PluginActionSchema::SetMode {
                            modes: crate::PlayModeSet::new().with(crate::PlayMode::Shuffle),
                        },
                    ];
                }
            }
            let device = config.rooms[0].devices[0].id.clone();
            let percent = crate::Action::new(device.clone(), "volume:30");
            let known = crate::Action::new(device.clone(), "next");
            // The three sites that core validates hold the percentage only in
            // the house that has a player; the two it merely parses hold it in
            // both, which is what keeps this the identity on its own files.
            let at_a_validated_site = |action: &crate::Action| player.then(|| action.clone());
            let activity = &mut config.activities[0];
            activity.setup.devices = vec![device];
            activity.setup.custom_screen = true;
            activity.buttons = vec![
                Binding {
                    button: Button::Red,
                    gesture: Gesture::Short,
                    action: at_a_validated_site(&percent),
                },
                Binding {
                    button: Button::Green,
                    gesture: Gesture::Short,
                    action: Some(known.clone()),
                },
            ];
            activity.setup.on = at_a_validated_site(&percent)
                .map(|action| crate::SequenceStep::Command { action })
                .into_iter()
                .chain([crate::SequenceStep::Command {
                    action: known.clone(),
                }])
                .collect();
            activity.setup.off = vec![crate::SequenceStep::Command {
                action: known.clone(),
            }];
            activity.setup.pages = vec![crate::ActivityPage {
                title: "Player".into(),
                widgets: at_a_validated_site(&percent)
                    .into_iter()
                    .map(|action| crate::ActivityWidget {
                        label: "Volume".into(),
                        icon: None,
                        action,
                    })
                    .collect(),
            }];
            activity.steps = vec![percent.clone(), known];
            config.scenes[0].steps = vec![percent];
            config.validate().unwrap();
            config
        };
        let with_player = house(true);
        let without = house(false);
        let v2 = v2_projection(&with_player);
        assert_eq!(v2, without, "the two houses have the same v2 projection");
        assert_eq!(v2_projection(&v2), v2, "idempotent");
        v2.validate().unwrap();
        v1_projection(&v2).validate().unwrap();
        projection(&v2).validate().unwrap();
        assert!(!mentions_custom(&v2));

        // The binding stays, disabled: removing it would give the key its
        // activity default back.
        assert_eq!(v2.activities[0].buttons.len(), 2);
        assert_eq!(v2.activities[0].buttons[0].action, None);
        assert_eq!(v2.activities[0].setup.on.len(), 1);
        assert!(v2.activities[0].setup.pages[0].widgets.is_empty());
        // A step and a scene step are only parsed by that core, which can
        // already hold both.
        assert_eq!(v2.activities[0].steps[0].command, "volume:30");
        assert_eq!(v2.scenes[0].steps[0].command, "volume:30");

        // The file: the v3 layer is the only place any of it is written, and
        // the bytes an older core sees are the other house's, exactly.
        let stored = StoredConfig::new(&with_player);
        assert_eq!(stored.integration_config_v3.as_ref(), Some(&with_player));
        let mut ours = serde_json::to_value(&stored).unwrap();
        ours.as_object_mut()
            .unwrap()
            .remove("integration_config_v3");
        assert_eq!(
            ours,
            serde_json::to_value(StoredConfig::new(&without)).unwrap()
        );
        assert!(!mentions_custom(&ours));

        // That core parses the file, holds the projection, and finds nothing
        // to refuse in it.
        let bytes = serde_json::to_vec(&stored).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.holds().refusal(), None);
        assert_eq!(old.rollback.refusal(), None);
        // And it would have refused the file if the percentage had stayed.
        // The component and the schemas it is drawn over are already gone by
        // then - that core cannot even parse those tags - so what is left to
        // prove is the binding itself.
        let mut leaked = v2.clone();
        leaked.activities[0].buttons[0].action = Some(crate::Action::new(
            with_player.rooms[0].devices[0].id.clone(),
            "volume:30",
        ));
        let kept = serde_json::to_vec(&Stored188::new(&leaked)).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&kept).unwrap();
        assert_eq!(
            old.holds().refusal(),
            Some("\"volume:30\" is bound to a packaged device that does not list it".into())
        );

        // A save by that core is the truth: what it never saw is not restored.
        let after = serde_json::from_slice::<StoredConfig>(
            &serde_json::to_vec(&Stored188::new(&without)).unwrap(),
        )
        .unwrap()
        .into_config()
        .unwrap();
        assert_eq!(after, without);
        assert!(!mentions_custom(&after));
    }

    /// The seven rules, one assertion each, on a bridge that also has controls
    /// of its own (so what is aimed at the connection has to survive).
    #[test]
    fn children_live_only_in_the_v3_layer_and_what_is_left_is_inert() {
        use crate::buttons::{Binding, Button};
        let mut config = plugin_config();
        let connection = Id::new("external");
        let kinds = child_kinds();
        if let Provider::Plugin {
            capabilities,
            presentation,
            actions,
            children,
            ..
        } = &mut config.connections.last_mut().unwrap().provider
        {
            *capabilities = named(&["power-on", "toggle"]);
            *presentation = vec![
                crate::PluginComponent::CommandGroup {
                    title: "Power".into(),
                    commands: vec!["power-on".into()],
                },
                crate::PluginComponent::Light {
                    label: "Lamp".into(),
                },
                crate::PluginComponent::Cover {
                    label: "Blind".into(),
                },
                crate::PluginComponent::Climate {
                    label: "Heating".into(),
                },
            ];
            *actions = vec![
                crate::PluginActionSchema::SetLight {},
                crate::PluginActionSchema::SetCover {},
                crate::PluginActionSchema::SetClimate {},
            ];
            *children = kinds.clone();
        }
        let mut lamp = snapshot("light");
        lamp.light = Some(crate::LightTraits {
            dimmable: true,
            mirek: Some((153, 500)),
            color: true,
        });
        let whole = config.rooms[0].devices[0].id.clone();
        let saved = config.rooms[0].devices[4].id.clone();
        let resolved = config.rooms[0].devices[3].id.clone();
        config.rooms[0].devices[4].integration = Integration::Connection {
            connection_id: connection.clone(),
            resource_id: "room/9d2b7c10".into(),
            child: Some(lamp.clone()),
        };
        config.rooms[0].devices[3].kind = crate::DeviceKind::Light;
        config.rooms[0].devices[3].integration = Integration::Plugin {
            id: "echo".into(),
            connection_id: connection.clone(),
            resource_id: "5f0c9a52".into(),
            capabilities: kinds[0].capabilities.clone(),
            supports_inputs: false,
            presentation: vec![],
            actions: kinds[0].actions.clone(),
            child: Some(lamp),
        };
        let actions: Vec<crate::Action> = [
            (&saved, "dim:30"),
            (&resolved, "toggle"),
            (&saved, "x:blink"),
            // `toggle` to the connection is a protocol 1 word aimed at no
            // child: it is that core's to keep.
            (&whole, "toggle"),
        ]
        .into_iter()
        .map(|(device, command)| crate::Action::new(device.clone(), command))
        .collect();
        let kept = actions[3].clone();
        let activity = &mut config.activities[0];
        activity.source = Some(saved.clone());
        activity.buttons = actions
            .iter()
            .zip([Button::Red, Button::Green, Button::Blue, Button::Yellow])
            .map(|(action, button)| Binding {
                button,
                gesture: Default::default(),
                action: Some(action.clone()),
            })
            .collect();
        activity.steps = actions.clone();
        activity.setup.devices = vec![saved.clone(), resolved.clone(), whole.clone()];
        let sequence: Vec<crate::SequenceStep> = actions
            .iter()
            .map(|action| crate::SequenceStep::Command {
                action: action.clone(),
            })
            .chain([crate::SequenceStep::Delay { ms: 250 }])
            .collect();
        activity.setup.on = sequence.clone();
        activity.setup.off = sequence;
        activity.setup.custom_screen = true;
        activity.setup.pages = vec![crate::ActivityPage {
            title: "Lights".into(),
            widgets: actions
                .iter()
                .map(|action| crate::ActivityWidget {
                    label: "Button".into(),
                    icon: None,
                    action: action.clone(),
                })
                .collect(),
        }];
        config.scenes[0].steps = actions.clone();
        config.areas[0].shortcuts = vec![
            crate::Shortcut {
                button: Button::Lights,
                action: crate::ShortcutAction::Toggle {
                    device: saved.clone(),
                },
            },
            crate::Shortcut {
                button: Button::Red,
                action: crate::ShortcutAction::Device {
                    device: resolved.clone(),
                },
            },
        ];
        let listed = Id::new("package-scene");
        let before = config.scenes.len();
        config.scenes.push(crate::Scene {
            id: listed.clone(),
            name: "Relax".into(),
            icon: None,
            steps: vec![],
            hue: None,
            resource: Some(crate::SceneResource {
                connection_id: connection.clone(),
                resource_id: "scene/3a1f6c8e".into(),
                kind: "scene".into(),
            }),
            rooms: vec![config.rooms[0].id.clone()],
        });
        config.areas[0].scenes.push(listed.clone());
        config.areas[1].scenes.insert(0, listed.clone());
        config.validate().unwrap();

        let v2 = v2_projection(&config);
        v2.validate().unwrap();
        let Provider::Plugin {
            capabilities,
            presentation,
            actions: schemas,
            children,
            ..
        } = &v2.connections.last().unwrap().provider
        else {
            unreachable!()
        };
        assert!(children.is_empty(), "1: the kinds a connection declares");
        assert_eq!(capabilities, &named(&["power-on", "toggle"]));
        let device = |id: &Id| v2.devices().find(|(_, d)| &d.id == id).unwrap().1.clone();
        assert_eq!(
            device(&saved).integration,
            Integration::Connection {
                connection_id: connection.clone(),
                resource_id: "room/9d2b7c10".into(),
                child: None,
            },
            "2: the snapshot goes, the connection and the resource stay"
        );
        assert!(matches!(
            device(&resolved).integration,
            Integration::Plugin { ref resource_id, child: None, ref actions, .. }
                if resource_id == "5f0c9a52" && actions.is_empty()
        ));
        assert_eq!(device(&resolved).kind, crate::DeviceKind::Light);
        let activity = &v2.activities[0];
        assert_eq!(
            activity
                .buttons
                .iter()
                .map(|b| (b.button, b.action.clone()))
                .collect::<Vec<_>>(),
            vec![
                (Button::Red, None),
                (Button::Green, None),
                (Button::Blue, None),
                (Button::Yellow, Some(kept.clone())),
            ],
            "3: a key aimed at a child stays as a disabled key, whatever it sent"
        );
        assert_eq!(activity.steps, vec![kept.clone()]);
        assert_eq!(activity.setup.devices, vec![whole.clone()]);
        let left = vec![
            crate::SequenceStep::Command {
                action: kept.clone(),
            },
            crate::SequenceStep::Delay { ms: 250 },
        ];
        assert_eq!(activity.setup.on, left);
        assert_eq!(activity.setup.off, left);
        assert_eq!(activity.setup.pages[0].widgets.len(), 1);
        assert_eq!(activity.setup.pages[0].widgets[0].action, kept);
        assert_eq!(v2.scenes[0].steps, vec![kept]);
        assert_eq!(
            v2.areas[0].shortcuts,
            vec![crate::Shortcut {
                button: Button::Red,
                action: crate::ShortcutAction::Device { device: resolved },
            }],
            "3: a key that toggles a child goes, a key that opens one stays"
        );
        assert_eq!(v2.scenes.len(), before, "4: the package scene");
        assert!(v2.scene(&listed).is_none());
        assert!(v2.areas.iter().all(|area| !area.scenes.contains(&listed)));
        assert_eq!(
            v2.areas[1].scenes,
            config.areas[1].scenes[1..],
            "4: and nothing else an area lists"
        );
        assert!(schemas.is_empty(), "5: the typed actions that core knows");
        assert_eq!(
            presentation,
            &vec![crate::PluginComponent::CommandGroup {
                title: "Power".into(),
                commands: vec!["power-on".into()],
            }],
            "6: a light, cover or climate component"
        );
        assert_eq!(
            v2.activities[0].source,
            Some(saved),
            "the device still exists, so what points at it as a device is left alone"
        );
        assert!(!mentions_custom(&v2));
        assert_eq!(v2_projection(&v2), v2);

        // The file: every older layer validates, the release finds nothing to
        // refuse, this core reads it all back, and a save by the release is
        // the truth afterwards.
        let stored = StoredConfig::new(&config);
        assert_eq!(stored.integration_config_v3.as_ref(), Some(&config));
        stored.rollback.validate().unwrap();
        stored
            .integration_config
            .as_ref()
            .unwrap()
            .validate()
            .unwrap();
        let bytes = serde_json::to_vec(&stored).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.holds().refusal(), None);
        assert_eq!(old.rollback.refusal(), None);
        assert_eq!(
            serde_json::from_slice::<StoredConfig>(&bytes)
                .unwrap()
                .into_config()
                .unwrap(),
            config
        );
        // Without the projection that release refuses the same configuration.
        let unprojected = serde_json::to_vec(&Stored188::new(&config));
        assert!(
            serde_json::from_slice::<release_188::Envelope>(&unprojected.unwrap()).is_err(),
            "the release cannot parse a light component"
        );
        let mut parses = config.clone();
        if let Provider::Plugin {
            presentation,
            actions,
            ..
        } = &mut parses.connections.last_mut().unwrap().provider
        {
            presentation.truncate(1);
            actions.clear();
        }
        if let Integration::Plugin { actions, .. } = &mut parses.rooms[0].devices[3].integration {
            actions.clear();
        }
        let unprojected = serde_json::to_vec(&Stored188::new(&parses)).unwrap();
        let old: release_188::Envelope = serde_json::from_slice(&unprojected).unwrap();
        assert!(
            old.holds().refusal().is_some(),
            "and refuses what is bound to a child"
        );
    }

    #[test]
    fn a_child_on_a_connection_with_a_denon_pilot_receipt_is_the_one_known_gap() {
        // The plain layer turns a connection that still has a pilot receipt
        // back into the built-in receiver, which has exactly one device and no
        // resources, and the v2 view keeps a child's resource. So the plain
        // layer of such a file does not validate on a core from before
        // packages existed. It cannot be reached through a running daemon:
        // `Config::migrate` drops every receipt when a file is opened, so no
        // child is ever added beside one; only a hand-made file (state L of
        // `config-crossload.rs`) has both. The release a remote rolls back to
        // reads the v2 layer and never validates the plain one, and
        // `projection` is that release's function and is not edited. Recorded
        // here so that a change to either side is noticed.
        let mut config = plugin_config();
        let connection = Id::new("external");
        if let Provider::Plugin { id, children, .. } =
            &mut config.connections.last_mut().unwrap().provider
        {
            *id = "denon".into();
            *children = child_kinds();
        }
        config.denon_migrations.insert(
            connection.clone(),
            crate::DenonMigration {
                host: "avr.invalid".into(),
                port: 23,
            },
        );
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: connection.clone(),
            resource_id: alloc::string::String::new(),
            child: None,
        };
        config.rooms[0].devices[4].integration = Integration::Connection {
            connection_id: connection,
            resource_id: "zone/2".into(),
            child: Some(snapshot("light")),
        };
        config.validate().unwrap();
        let stored = StoredConfig::new(&config);
        stored
            .integration_config
            .as_ref()
            .unwrap()
            .validate()
            .unwrap();
        assert!(stored.rollback.validate().is_err());
        let mut opened = config;
        assert!(opened.migrate());
        StoredConfig::new(&opened).rollback.validate().unwrap();
    }

    #[test]
    fn more_than_one_action_schema_never_reaches_an_older_core() {
        // Unreachable through validation in this step (there is one kind, and a
        // kind is declared once), but the layer an old core reads is cut to what
        // it accepts regardless of how the document came to be.
        let mut config = plugin_config();
        let schema = crate::PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        if let Provider::Plugin { actions, .. } =
            &mut config.connections.last_mut().unwrap().provider
        {
            *actions = vec![schema, schema];
        }
        assert!(config.validate().is_err());
        let v2 = v2_projection(&config);
        let Provider::Plugin { actions, .. } = &v2.connections.last().unwrap().provider else {
            unreachable!()
        };
        assert_eq!(actions, &vec![schema]);
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
