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
            ..
        } = &mut connection.provider
        {
            snapshot(capabilities, presentation, actions);
        }
    }
    for room in &mut result.rooms {
        for device in &mut room.devices {
            if let Integration::Plugin {
                capabilities,
                presentation,
                actions,
                ..
            } = &mut device.integration
            {
                snapshot(capabilities, presentation, actions);
            }
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
            pub presentation: Vec<Component>,
            #[serde(default)]
            pub actions: Vec<ActionSchema>,
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
            #[serde(other)]
            Other,
        }
        #[derive(Deserialize)]
        pub struct Connection {
            pub provider: Provider,
        }
        #[derive(Deserialize)]
        pub struct Device {
            pub integration: Integration,
        }
        #[derive(Deserialize)]
        pub struct Room {
            pub devices: Vec<Device>,
        }
        #[derive(Deserialize)]
        pub struct Action {
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
            /// The release's own refusals that the types above cannot express.
            pub fn refusal(&self) -> Option<String> {
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

    /// A valid configuration with one packaged connection and a random mix of
    /// protocol 1, 2 and (with `custom`) 3 content in every place a command can
    /// be saved, aimed at packaged and built-in devices alike.
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
            },
        });
        config.rooms[0].devices[0].integration = Integration::Connection {
            connection_id: connection.clone(),
            resource_id: "zone1".into(),
        };
        if random.below(2) == 0 {
            config.rooms[0].devices[1].integration = Integration::Plugin {
                id: "echo".into(),
                connection_id: connection,
                resource_id: "zone2".into(),
                capabilities,
                supports_inputs,
                presentation,
                actions,
            };
        }
        // Everything each of the first three devices accepts, packaged or not.
        let mut offered: Vec<crate::Action> = Vec::new();
        for device in &config.rooms[0].devices[..3] {
            let integration = config.resolve_integration(&device.integration).unwrap();
            let mut ids: Vec<alloc::string::String> =
                crate::buttons::function_choices(&integration)
                    .into_iter()
                    .map(|choice| choice.0)
                    .collect();
            ids.extend(["input:HDMI1".into(), "input:HD RADIO".into()]);
            for id in ids {
                if Function::parse(&id).is_some_and(|f| f.supports_device(device, &config)) {
                    offered.push(crate::Action::new(device.id.clone(), id));
                }
            }
        }
        let members: Vec<Id> = config.rooms[0].devices[..3]
            .iter()
            .map(|device| device.id.clone())
            .collect();
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

    fn mentions_custom(value: &impl Serialize) -> bool {
        serde_json::to_string(value).unwrap().contains("\"x:")
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
        for round in 0..300 {
            let config = random_config(&mut random, true);
            let v2 = v2_projection(&config);
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
