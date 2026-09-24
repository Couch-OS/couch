//! What has to be true of a config before anything is allowed to store it.
//!
//! The daemon runs this on every write and refuses the whole document if it
//! fails, which is the only reason readers elsewhere in this crate can be
//! relaxed about dangling ids. Every check here exists because breaking it
//! would produce a house that renders wrong rather than an error: two rooms
//! with the same id means one of them is unreachable, and an area pointing at a
//! room that was deleted means a gap in the list with no explanation.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::{Config, Id, SCHEMA_VERSION};

/// One thing wrong, named well enough to put in front of a user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// A dotted path into the document: `areas[1].rooms[0]`.
    pub at: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationError {
    pub problems: Vec<Problem>,
}

impl core::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, p) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}: {}", p.at, p.message)?;
        }
        Ok(())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ValidationError {}

impl Config {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut problems = Vec::new();
        self.validate_app_shortcuts(&mut problems);
        self.validate_shortcuts(&mut problems);
        if self.appearance.rgb().is_none() {
            problems.push(Problem {
                at: "appearance.accent".into(),
                message: "Use a color in #RRGGBB format".into(),
            });
        }

        if self.schema_version > SCHEMA_VERSION {
            problems.push(Problem {
                at: "schema_version".to_string(),
                message: alloc::format!(
                    "written by a newer build (schema {}, this understands {})",
                    self.schema_version,
                    SCHEMA_VERSION
                ),
            });
        }

        // Ids and names, per collection. A blank name is worth rejecting: the
        // hub renders it as an empty row that cannot be told apart from a
        // rendering bug, and there is no way to select it to fix it.
        let mut seen: Vec<&Id> = Vec::new();
        for (i, a) in self.areas.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "areas", i, &a.id, &a.name);
        }
        seen.clear();
        for (i, r) in self.rooms.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "rooms", i, &r.id, &r.name);
        }
        seen.clear();
        for (i, s) in self.scenes.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "scenes", i, &s.id, &s.name);
        }
        seen.clear();
        for (i, a) in self.activities.iter().enumerate() {
            check_entity(&mut problems, &mut seen, "activities", i, &a.id, &a.name);
        }

        // Device ids are unique across the home, not per room: an Action names
        // a device and nothing else, so a duplicate id makes a scene ambiguous.
        let mut device_ids: Vec<&Id> = Vec::new();
        for (ri, room) in self.rooms.iter().enumerate() {
            for (di, d) in room.devices.iter().enumerate() {
                let at = alloc::format!("rooms[{ri}].devices[{di}]");
                if d.ir
                    .as_ref()
                    .is_some_and(|ir| !crate::DeviceIr::valid_codeset(&ir.codeset))
                {
                    problems.push(Problem { at: alloc::format!("{at}.ir"), message: "Choose a lowercase IR codeset ID with letters, numbers, hyphens or underscores (maximum 64 characters)".into() });
                }
                if d.id.is_empty() {
                    problems.push(Problem {
                        at: at.clone(),
                        message: "blank id".to_string(),
                    });
                }
                if d.name.trim().is_empty() {
                    problems.push(Problem {
                        at: at.clone(),
                        message: "blank name".to_string(),
                    });
                }
                if device_ids.contains(&&d.id) {
                    problems.push(Problem {
                        at,
                        message: alloc::format!("duplicate device id \"{}\"", d.id),
                    });
                } else {
                    device_ids.push(&d.id);
                }
            }
        }

        let mut connection_ids = Vec::new();
        for (i, c) in self.connections.iter().enumerate() {
            check_entity(
                &mut problems,
                &mut connection_ids,
                "connections",
                i,
                &c.id,
                &c.name,
            );
            if let crate::Provider::Kodi { host, port }
            | crate::Provider::CoreElec { host, port }
            | crate::Provider::LegacyDenon { host, port } = &c.provider
            {
                if host.trim().is_empty() || *port == 0 {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}]"),
                        message: "Connection needs an address and a TCP port from 1 to 65535"
                            .into(),
                    });
                }
            }
            if let crate::Provider::Sonos { host } = &c.provider {
                if host.parse::<core::net::Ipv4Addr>().is_err() {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}]"),
                        message: "Sonos needs an IPv4 address".into(),
                    });
                }
            }
            if let crate::Provider::Plugin {
                id,
                label,
                capabilities,
                supports_inputs,
                presentation,
                actions,
                children,
            } = &c.provider
            {
                if !valid_plugin_id(id) || !valid_plugin_label(label) || capabilities.len() > 128 {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}].provider"),
                        message: "External integration metadata is invalid".into(),
                    });
                }
                let mut capability_ids = Vec::new();
                for capability in capabilities {
                    if crate::commands::Function::parse(&capability.id).is_none()
                        || !valid_plugin_label(&capability.label)
                        || capability.id.starts_with("input:")
                        || capability.id.starts_with("app:")
                        || capability_ids.contains(&&capability.id)
                    {
                        problems.push(Problem {
                            at: alloc::format!("connections[{i}].provider.capabilities"),
                            message: "External integration advertises an unsupported command"
                                .into(),
                        });
                    }
                    capability_ids.push(&capability.id);
                }
                if !crate::domain::valid_child_kinds(children) {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}].provider.children"),
                        message: "External integration declares an invalid kind of device".into(),
                    });
                }
                // The limit on a package's own buttons is for the package,
                // whichever of its kinds of child names them.
                for capability in children.iter().flat_map(|kind| &kind.capabilities) {
                    if !capability_ids.contains(&&capability.id) {
                        capability_ids.push(&capability.id);
                    }
                }
                if capability_ids
                    .iter()
                    .filter(|id| id.starts_with("x:"))
                    .count()
                    > crate::commands::MAX_CUSTOM_FUNCTIONS
                {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}].provider.capabilities"),
                        message: "External integration names too many buttons of its own".into(),
                    });
                }
                if !crate::PluginActionSchema::valid_set(actions)
                    || presentation.len() > 16
                    // One screen, so at most one player (protocol 3,
                    // unreleased).
                    || presentation
                        .iter()
                        .filter(|component| {
                            matches!(component, crate::PluginComponent::MediaPlayer { .. })
                        })
                        .count()
                        > 1
                    || presentation.iter().any(|component| {
                        !valid_plugin_component(component, capabilities, *supports_inputs, actions)
                    })
                {
                    problems.push(Problem {
                        at: alloc::format!("connections[{i}].provider.presentation"),
                        message: "External integration presentation is invalid".into(),
                    });
                }
            }
            if c.provider == crate::Provider::Ir
                && self.connections[..i]
                    .iter()
                    .any(|old| old.provider == crate::Provider::Ir)
            {
                problems.push(Problem{at:alloc::format!("connections[{i}]"),message:"Use the built-in IR connection and configure a separate codeset on each device".into()});
            }
            if c.id.as_str().len() > 128
                || !c
                    .id
                    .as_str()
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                problems.push(Problem {
                    at: alloc::format!("connections[{i}]"),
                    message: "Connection IDs must be safe alphanumeric identifiers".into(),
                });
            }
        }
        for (room, device) in self.devices() {
            if let Some(bond) = &device.bluetooth {
                if !bond.address.is_empty() && !crate::DeviceBluetooth::valid_address(&bond.address)
                {
                    problems.push(Problem {
                        at: alloc::format!("rooms.{}.devices.{}.bluetooth", room.id, device.id),
                        message:
                            "A Bluetooth address is six uppercase hex pairs separated by colons"
                                .into(),
                    });
                }
                if bond.name.len() > 128 {
                    problems.push(Problem {
                        at: alloc::format!("rooms.{}.devices.{}.bluetooth", room.id, device.id),
                        message: "The Bluetooth name must be at most 128 bytes".into(),
                    });
                }
            }
            if let crate::Integration::Sonos { host } = &device.integration {
                if host.parse::<core::net::Ipv4Addr>().is_err() {
                    problems.push(Problem {
                        at: alloc::format!("rooms.{}.devices.{}", room.id, device.id),
                        message: "Sonos needs an IPv4 address".into(),
                    });
                }
            }
            if let crate::Integration::Connection {
                connection_id,
                child: Some(child),
                resource_id,
            }
            | crate::Integration::Plugin {
                connection_id,
                child: Some(child),
                resource_id,
                ..
            } = &device.integration
            {
                // The resolved form of a child is what `resolve_integration`
                // makes of one: no inputs and no screen of its own.
                let resolved_as_a_whole_connection = matches!(
                    &device.integration,
                    crate::Integration::Plugin { supports_inputs, presentation, .. }
                        if *supports_inputs || !presentation.is_empty()
                );
                if let Some(message) = self
                    .child_problem(connection_id, resource_id, child, device.kind)
                    .or(resolved_as_a_whole_connection
                        .then_some("A device of an integration has no inputs or screen of its own"))
                {
                    problems.push(Problem {
                        at: alloc::format!("rooms.{}.devices.{}.child", room.id, device.id),
                        message: message.into(),
                    });
                }
            }
            if let crate::Integration::Connection {
                connection_id,
                resource_id,
                ..
            } = &device.integration
            {
                let at = alloc::format!("rooms.{}.devices.{}", room.id, device.id);
                match self.connection(connection_id) {
                    None=>problems.push(Problem{at,message:"This device refers to a missing connection; remove its devices before deleting the connection".into()}),
                    Some(c)=>{
                        let valid=match c.provider {
                            crate::Provider::Kodi{..}|crate::Provider::CoreElec{..}|crate::Provider::Sonos{..}|crate::Provider::LegacyDenon{..}|crate::Provider::LegacyWebOs|crate::Provider::AndroidTv|crate::Provider::AppleTv|crate::Provider::Tizen|crate::Provider::BluetoothTv=>resource_id.is_empty(),
                            crate::Provider::UnifiProtect=>device.kind==crate::DeviceKind::Camera && !resource_id.is_empty() && resource_id.len()<=128 && resource_id.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-' || b==b'_'),
                            crate::Provider::HomeAssistant=>valid_ha_resource(resource_id, device.kind),
                            crate::Provider::Matter=>valid_matter_resource(resource_id),
                            crate::Provider::LegacyHue=>{ let id=resource_id.strip_prefix("room:").unwrap_or(resource_id); id.len()==36 && id.bytes().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i){b==b'-'}else{b.is_ascii_hexdigit()}) },
                            crate::Provider::Plugin{..}=>resource_id.len()<=128 && resource_id.bytes().all(|b|b.is_ascii_alphanumeric()||b"._/-+".contains(&b)),
                            crate::Provider::Ir=>!resource_id.is_empty() && resource_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'),
                        };
                        if !valid {problems.push(Problem{at,message:"Choose a valid device from this connection".into()});}
                    }
                }
            }
        }

        for (i, scene) in self.scenes.iter().enumerate() {
            for room in &scene.rooms {
                if self.room(room).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("scenes[{i}].rooms"),
                        message: "Choose an existing room".into(),
                    });
                }
            }
            if let Some(hue) = &scene.hue {
                let id = &hue.scene_id;
                let valid = self
                    .connection(&hue.connection_id)
                    .is_some_and(|c| c.provider == crate::Provider::LegacyHue)
                    && id.len() == 36
                    && id.bytes().enumerate().all(|(i, b)| {
                        if [8, 13, 18, 23].contains(&i) {
                            b == b'-'
                        } else {
                            b.is_ascii_hexdigit()
                        }
                    })
                    && scene.steps.is_empty();
                if !valid {
                    problems.push(Problem{at:alloc::format!("scenes[{i}].hue"),message:"Choose a Hue connection and scene; bridge scenes cannot include device steps".into()});
                }
            }
            if let Some(resource) = &scene.resource {
                let valid = scene.hue.is_none()
                    && scene.steps.is_empty()
                    && crate::valid_resource(&resource.resource_id)
                    && self
                        .child_kind(&resource.connection_id, &resource.kind)
                        .is_some_and(|kind| kind.component == crate::ChildComponent::Scene);
                if !valid {
                    problems.push(Problem {
                        at: alloc::format!("scenes[{i}].resource"),
                        message: "Choose a scene this integration offers; a package scene cannot include device steps or a Hue scene".into(),
                    });
                }
            }
        }
        // References.
        for (i, area) in self.areas.iter().enumerate() {
            for (j, id) in area.rooms.iter().enumerate() {
                if self.room(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].rooms[{j}]"),
                        message: alloc::format!("no room \"{id}\""),
                    });
                }
                if area.rooms[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].rooms[{j}]"),
                        message: alloc::format!("room \"{id}\" listed twice"),
                    });
                }
            }
            for (j, id) in area.scenes.iter().enumerate() {
                if self.scene(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].scenes[{j}]"),
                        message: alloc::format!("no scene \"{id}\""),
                    });
                }
                if area.scenes[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].scenes[{j}]"),
                        message: alloc::format!("scene \"{id}\" listed twice"),
                    });
                }
            }
            for (j, id) in area.activities.iter().enumerate() {
                if self.activity(id).is_none() {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].activities[{j}]"),
                        message: alloc::format!("no activity \"{id}\""),
                    });
                }
                if area.activities[..j].contains(id) {
                    problems.push(Problem {
                        at: alloc::format!("areas[{i}].activities[{j}]"),
                        message: alloc::format!("activity \"{id}\" listed twice"),
                    });
                }
            }
        }

        // A step is run by the same executor as a button binding, which parses
        // the command and gives up on anything it does not know, so a step that
        // does not parse saves and then fails on the first press. Only the parse
        // is checked here, not `supports_device`: a binding picks from a
        // device's button catalog, while a step says "on" or "off" to a device
        // whose catalog has neither. A package-named button (`x:info`) is the
        // exception: Couch has no meaning of its own for it, so it is a step
        // only for a device whose package declares that exact id.
        let step_problem = |step: &crate::Action| -> Option<String> {
            if !device_ids.contains(&&step.device) {
                return Some(alloc::format!("no device \"{}\"", step.device));
            }
            let declared = match crate::commands::Function::parse(&step.command) {
                None => false,
                Some(function @ crate::commands::Function::Custom(_)) => self
                    .devices()
                    .find(|(_, d)| d.id == step.device)
                    .is_some_and(|(_, d)| function.supports_device(d, self)),
                Some(_) => true,
            };
            (!declared).then(|| alloc::format!("unsupported command \"{}\"", step.command))
        };
        for (i, scene) in self.scenes.iter().enumerate() {
            for (j, step) in scene.steps.iter().enumerate() {
                if let Some(message) = step_problem(step) {
                    problems.push(Problem {
                        at: alloc::format!("scenes[{i}].steps[{j}]"),
                        message,
                    });
                }
            }
        }

        for (i, act) in self.activities.iter().enumerate() {
            if let Err(message) = act.setup.validate(self) {
                problems.push(Problem {
                    at: alloc::format!("activities[{i}].setup"),
                    message: message.into(),
                });
            }
            if self.room(&act.room).is_none() {
                problems.push(Problem {
                    at: alloc::format!("activities[{i}].room"),
                    message: alloc::format!("no room \"{}\"", act.room),
                });
            }
            if let Some(src) = &act.source {
                if !device_ids.contains(&src) {
                    problems.push(Problem {
                        at: alloc::format!("activities[{i}].source"),
                        message: alloc::format!("no device \"{src}\""),
                    });
                }
            }
            for (j, binding) in act.buttons.iter().enumerate() {
                let valid = !act.buttons[..j]
                    .iter()
                    .any(|b| b.button == binding.button && b.gesture == binding.gesture)
                    && (binding.gesture == crate::buttons::Gesture::Short
                        || binding.button.supports_long())
                    && binding.action.as_ref().map_or(true, |action| {
                        self.devices()
                            .find(|(_, d)| d.id == action.device)
                            .is_some_and(|(_, d)| {
                                crate::commands::Function::parse(&action.command)
                                    .is_some_and(|f| f.supports_device(d, self))
                            })
                    });
                if !valid {
                    problems.push(Problem {
                        at: alloc::format!("activities[{i}].buttons[{j}]"),
                        message: "Choose one mapping per button and a supported device function"
                            .into(),
                    });
                }
            }
            for (j, step) in act.steps.iter().enumerate() {
                if let Some(message) = step_problem(step) {
                    problems.push(Problem {
                        at: alloc::format!("activities[{i}].steps[{j}]"),
                        message,
                    });
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { problems })
        }
    }
}

impl Config {
    /// What is wrong with a device saved as a child of a connection, if
    /// anything. The kind has to be one the connection's package declares, so
    /// with no such package (every shipped build: protocol 3 is switched off
    /// and no accepted manifest can declare children) every child is refused.
    fn child_problem(
        &self,
        connection: &Id,
        resource: &str,
        child: &crate::ChildSnapshot,
        device_kind: crate::DeviceKind,
    ) -> Option<&'static str> {
        let Some(connection) = self.connection(connection) else {
            return Some("This device refers to a missing connection");
        };
        let crate::Provider::Plugin { children, .. } = &connection.provider else {
            return Some("Only a connection to an integration package has devices of its own");
        };
        let Some(kind) = children.iter().find(|kind| kind.kind == child.kind) else {
            return Some("This integration does not offer that kind of device");
        };
        if kind.component == crate::ChildComponent::Scene {
            return Some("A package scene belongs to a room's scenes, not to its devices");
        }
        if kind.device_kind != device_kind {
            return Some("This device has to be the kind of device its integration says it is");
        }
        // Stricter than the rule for a connection that is one device, which
        // has to keep loading whatever was saved before children existed.
        if !crate::valid_resource(resource) {
            return Some("Choose a valid device from this connection");
        }
        (!child.fits(kind.component))
            .then_some("What this device can do does not fit the kind of device it is")
    }
}

fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_plugin_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control)
}

fn valid_plugin_component(
    component: &crate::PluginComponent,
    capabilities: &[crate::PluginCapability],
    supports_inputs: bool,
    actions: &[crate::PluginActionSchema],
) -> bool {
    let declared = |command: &str| {
        capabilities
            .iter()
            .any(|capability| capability.id == command)
    };
    match component {
        crate::PluginComponent::CommandGroup { title, commands } => {
            let mut seen = Vec::new();
            valid_plugin_label(title)
                && !commands.is_empty()
                && commands.len() <= 32
                && commands.iter().all(|command| {
                    let unique = !seen.contains(&command);
                    seen.push(command);
                    unique && declared(command)
                })
        }
        crate::PluginComponent::StatusText { label, .. } => valid_plugin_label(label),
        crate::PluginComponent::Toggle {
            label,
            state,
            on,
            off,
        } => {
            valid_plugin_label(label)
                && state.is_boolean()
                && on != off
                && declared(on)
                && declared(off)
        }
        crate::PluginComponent::VolumeDbControl { label } => {
            valid_plugin_label(label)
                && crate::PluginActionSchema::find(actions, crate::volume::ActionKind::SetVolumeDb)
                    .is_some_and(|schema| schema.is_valid())
        }
        crate::PluginComponent::InputSelector { label } => {
            valid_plugin_label(label) && supports_inputs
        }
        // Each is drawn over one typed action, as the decibel control is.
        crate::PluginComponent::Light { label } => {
            valid_plugin_label(label) && declares(actions, crate::ActionKind::SetLight)
        }
        crate::PluginComponent::Cover { label } => {
            valid_plugin_label(label) && declares(actions, crate::ActionKind::SetCover)
        }
        crate::PluginComponent::Climate { label } => {
            valid_plugin_label(label) && declares(actions, crate::ActionKind::SetClimate)
        }
        // The mirror of the manifest's own rules, so a saved snapshot is held
        // to exactly what the package was admitted under. The model does not
        // depend on couch-plugin, so the rules live in both places and the
        // crossload states are what keeps them the same.
        crate::PluginComponent::MediaPlayer {
            layout,
            artwork,
            lists,
            up_next,
            navigation,
            refresh_ms,
            keys,
        } => {
            let mut seen_roles = Vec::new();
            let mut seen_lists = Vec::new();
            let mut seen_keys = Vec::new();
            // Couch owns the key map; the manifest only chooses between the
            // two modes, so navigation needs every key that mode sends.
            const NAVIGATION: [&str; 8] =
                ["up", "down", "left", "right", "ok", "back", "home", "menu"];
            // Sources, modes, up next and the declared lists all open a sheet,
            // and the screen holds three.
            let sheets = usize::from(supports_inputs)
                + usize::from(declares(actions, crate::ActionKind::SetMode))
                + usize::from(*up_next)
                + lists.len();
            let declared = |command: &str| {
                capabilities
                    .iter()
                    .any(|capability| capability.id == command)
            };
            (declared("play-pause") || (declared("play") && declared("pause")))
                && (!*up_next || declared("next"))
                && (!*navigation || NAVIGATION.iter().all(|key| declared(key)))
                && sheets <= 3
                && (crate::MIN_REFRESH_MS..=crate::MAX_REFRESH_MS).contains(refresh_ms)
                && artwork.len() <= 3
                && artwork.iter().all(|role| {
                    let distinct = !seen_roles.contains(&role);
                    seen_roles.push(role);
                    distinct && role.fits(*layout)
                })
                && lists.iter().all(|list| {
                    let distinct = !seen_lists.contains(&&list.id);
                    seen_lists.push(&list.id);
                    distinct && valid_list_id(&list.id) && valid_plugin_label(&list.label)
                })
                && keys.len() <= crate::MAX_MEDIA_KEYS
                && keys.iter().all(|key| {
                    let distinct = !seen_keys.contains(&key.key);
                    seen_keys.push(key.key);
                    distinct && declared(&key.command)
                })
        }
        crate::PluginComponent::VolumePercentControl { label } => {
            valid_plugin_label(label) && declares(actions, crate::ActionKind::SetVolumePercent)
        }
    }
}

/// A list's own name, as the package spells it: an identifier it can also use
/// in a URL, never a path.
fn valid_list_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn declares(actions: &[crate::PluginActionSchema], kind: crate::ActionKind) -> bool {
    crate::PluginActionSchema::find(actions, kind).is_some()
}

fn check_entity<'a>(
    problems: &mut Vec<Problem>,
    seen: &mut Vec<&'a Id>,
    collection: &str,
    index: usize,
    id: &'a Id,
    name: &str,
) {
    let at = alloc::format!("{collection}[{index}]");
    if id.is_empty() {
        problems.push(Problem {
            at: at.clone(),
            message: "blank id".to_string(),
        });
    }
    if name.trim().is_empty() {
        problems.push(Problem {
            at: at.clone(),
            message: "blank name".to_string(),
        });
    }
    if seen.contains(&id) {
        problems.push(Problem {
            at,
            message: alloc::format!("duplicate id \"{id}\""),
        });
    } else {
        seen.push(id);
    }
}

/// `<node_id>/<endpoint>`: both decimal, node IDs are operational (non-zero)
/// and endpoint 0 is the root node rather than a controllable device.
pub fn valid_matter_resource(id: &str) -> bool {
    let Some((node, endpoint)) = id.split_once('/') else {
        return false;
    };
    node.len() <= 20
        && node.bytes().all(|b| b.is_ascii_digit())
        && node.parse::<u64>().is_ok_and(|n| n != 0)
        && endpoint.parse::<u16>().is_ok_and(|e| e != 0)
}

fn valid_ha_resource(id: &str, kind: crate::DeviceKind) -> bool {
    let Some((domain, name)) = id.split_once('.') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && matches!(
            (domain, kind),
            ("light", crate::DeviceKind::Light)
                | ("cover", crate::DeviceKind::Blind)
                | ("climate", crate::DeviceKind::Thermostat)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Area, Device, DeviceKind, Room};
    use alloc::vec;

    fn room(id: &str, name: &str) -> Room {
        Room {
            id: Id::new(id),
            name: name.to_string(),
            icon: None,
            devices: vec![],
        }
    }

    #[test]
    fn direct_sonos_integrations_require_literal_ipv4_addresses() {
        for (host, valid) in [
            ("192.0.2.1", true),
            ("speaker.local", false),
            ("::1", false),
            ("", false),
            ("256.1.1.1", false),
        ] {
            let mut room = room("living", "Living room");
            let mut device = Device::new(Id::new("speaker"), "Sonos", DeviceKind::Speaker);
            device.integration = crate::Integration::Sonos { host: host.into() };
            room.devices.push(device);
            let cfg = Config {
                rooms: vec![room],
                ..Config::default()
            };
            if valid {
                assert!(cfg.validate().is_ok(), "{host}");
            } else {
                let error = cfg.validate().unwrap_err();
                assert!(error
                    .problems
                    .iter()
                    .any(|p| p.at == "rooms.living.devices.speaker"
                        && p.message == "Sonos needs an IPv4 address"));
            }
        }
    }

    #[test]
    fn dangling_area_room_is_rejected() {
        let cfg = Config {
            areas: vec![Area {
                id: Id::new("a"),
                name: "A".to_string(),
                icon: None,
                rooms: vec![Id::new("nowhere")],
                scenes: vec![],
                activities: vec![],
                shortcuts: vec![],
            }],
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems[0].at, "areas[0].rooms[0]");
    }

    #[test]
    fn dangling_area_activity_is_rejected() {
        let cfg = Config {
            areas: vec![Area {
                id: Id::new("a"),
                name: "A".to_string(),
                icon: None,
                rooms: vec![],
                scenes: vec![],
                activities: vec![Id::new("nothing")],
                shortcuts: vec![],
            }],
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems[0].at, "areas[0].activities[0]");
    }

    #[test]
    fn duplicate_device_ids_across_rooms_are_rejected() {
        let mut a = room("a", "A");
        let mut b = room("b", "B");
        a.devices
            .push(Device::new(Id::new("dup"), "One", DeviceKind::Light));
        b.devices
            .push(Device::new(Id::new("dup"), "Two", DeviceKind::Light));
        let cfg = Config {
            rooms: vec![a, b],
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_scene_step_must_name_a_real_device() {
        let cfg = Config {
            scenes: vec![crate::Scene {
                hue: None,
                rooms: vec![],
                id: Id::new("s"),
                name: "S".to_string(),
                icon: None,
                steps: vec![Action::new(Id::new("ghost"), "on")],
                resource: None,
            }],
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_step_command_the_executor_cannot_parse_is_rejected() {
        let mut living = room("living", "Living room");
        living
            .devices
            .push(Device::new(Id::new("lamp"), "Lamp", DeviceKind::Light));
        for (command, valid) in [
            ("on", true),
            ("toggle", true),
            ("dim:30", true),
            ("dim:101", false),
            ("dim:", false),
            ("bright", false),
        ] {
            let steps = vec![Action::new(Id::new("lamp"), command)];
            let cfg = Config {
                rooms: vec![living.clone()],
                scenes: vec![crate::Scene {
                    hue: None,
                    rooms: vec![],
                    id: Id::new("s"),
                    name: "S".to_string(),
                    icon: None,
                    steps: steps.clone(),
                    resource: None,
                }],
                activities: vec![crate::Activity {
                    setup: Default::default(),
                    id: Id::new("a"),
                    name: "A".to_string(),
                    kind: Default::default(),
                    room: Id::new("living"),
                    source: None,
                    buttons: vec![],
                    steps,
                }],
                ..Config::default()
            };
            if valid {
                assert!(cfg.validate().is_ok(), "{command}");
            } else {
                let problems = cfg.validate().unwrap_err().problems;
                let at: Vec<&str> = problems.iter().map(|p| p.at.as_str()).collect();
                assert_eq!(
                    at,
                    ["scenes[0].steps[0]", "activities[0].steps[0]"],
                    "{command}"
                );
                assert!(
                    problems.iter().all(|p| p.message.contains(command)),
                    "{command}"
                );
            }
        }
    }

    /// A home with a packaged device (`player`, through a connection),
    /// another saved in its resolved form (`direct`), and a built-in lamp.
    fn packaged(capabilities: &[&str]) -> Config {
        let capabilities: Vec<crate::PluginCapability> = capabilities
            .iter()
            .map(|id| crate::PluginCapability {
                id: (*id).into(),
                label: "Label".into(),
            })
            .collect();
        let mut living = room("living", "Living room");
        living.devices = vec![
            Device::new(Id::new("player"), "Player", DeviceKind::MediaPlayer).with_integration(
                crate::Integration::Connection {
                    connection_id: Id::new("package"),
                    resource_id: "".into(),
                    child: None,
                },
            ),
            Device::new(Id::new("direct"), "Direct", DeviceKind::MediaPlayer).with_integration(
                crate::Integration::Plugin {
                    id: "sample".into(),
                    connection_id: Id::new("package"),
                    resource_id: "".into(),
                    capabilities: capabilities.clone(),
                    supports_inputs: false,
                    presentation: vec![],
                    actions: vec![],
                    child: None,
                },
            ),
            Device::new(Id::new("lamp"), "Lamp", DeviceKind::Light).with_integration(
                crate::Integration::Hue {
                    light_id: "id".into(),
                },
            ),
            Device::new(Id::new("tv"), "TV", DeviceKind::Tv).with_integration(
                crate::Integration::Ir {
                    codeset: "tv".into(),
                },
            ),
        ];
        Config {
            connections: vec![crate::Connection {
                id: Id::new("package"),
                name: "Package".into(),
                provider: crate::Provider::Plugin {
                    id: "sample".into(),
                    label: "Sample".into(),
                    capabilities,
                    supports_inputs: false,
                    presentation: vec![],
                    actions: vec![],
                    children: vec![],
                },
            }],
            rooms: vec![living],
            scenes: vec![crate::Scene {
                hue: None,
                rooms: vec![],
                id: Id::new("s"),
                name: "S".to_string(),
                icon: None,
                steps: vec![],
                resource: None,
            }],
            activities: vec![crate::Activity {
                setup: Default::default(),
                id: Id::new("a"),
                name: "A".to_string(),
                kind: Default::default(),
                room: Id::new("living"),
                source: None,
                buttons: vec![],
                steps: vec![],
            }],
            ..Config::default()
        }
    }

    #[test]
    fn a_package_named_button_is_valid_only_for_a_device_whose_package_declares_it() {
        // Every place a command is saved, against every kind of device.
        for (device, command, valid) in [
            ("player", "x:info", true),
            ("direct", "x:info", true),
            ("player", "x:osd", false),
            ("direct", "x:osd", false),
            ("lamp", "x:info", false),
            ("tv", "x:info", false),
            ("player", "x:Info", false),
            ("player", "x:", false),
        ] {
            let action = Action::new(Id::new(device), command);
            type Place = fn(&mut Config, Action);
            let sites: [(&str, Place); 6] = [
                ("scenes[0].steps[0]", |c, a| c.scenes[0].steps = vec![a]),
                ("activities[0].steps[0]", |c, a| {
                    c.activities[0].steps = vec![a]
                }),
                ("activities[0].buttons[0]", |c, a| {
                    c.activities[0].buttons = vec![crate::buttons::Binding {
                        button: crate::buttons::Button::Red,
                        gesture: Default::default(),
                        action: Some(a),
                    }]
                }),
                ("activities[0].setup", |c, a| {
                    c.activities[0].setup.on = vec![crate::SequenceStep::Command { action: a }]
                }),
                ("activities[0].setup", |c, a| {
                    c.activities[0].setup.off = vec![crate::SequenceStep::Command { action: a }]
                }),
                ("activities[0].setup", |c, a| {
                    c.activities[0].setup.pages = vec![crate::ActivityPage {
                        title: "Page".into(),
                        widgets: vec![crate::ActivityWidget {
                            label: "Button".into(),
                            icon: None,
                            action: a,
                        }],
                    }]
                }),
            ];
            for (index, (at, place)) in sites.into_iter().enumerate() {
                let mut cfg = packaged(&["menu", "x:info"]);
                cfg.activities[0].setup.devices = vec![Id::new(device)];
                place(&mut cfg, action.clone());
                match cfg.validate() {
                    Ok(()) => assert!(valid, "{command} on {device} at site {index}"),
                    Err(e) => {
                        assert!(!valid, "{command} on {device} at site {index}: {e}");
                        assert_eq!(e.problems.len(), 1, "{e}");
                        assert_eq!(e.problems[0].at, at);
                    }
                }
            }
        }
        // A word Couch knows is still a step for any device, as before: a
        // scene says "on" to a device whose catalog has no such key.
        let mut cfg = packaged(&["menu"]);
        cfg.scenes[0].steps = vec![
            Action::new(Id::new("player"), "on"),
            Action::new(Id::new("tv"), "dim:30"),
        ];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn a_package_names_at_most_thirty_two_buttons_of_its_own() {
        let ids: Vec<String> = (0..=crate::commands::MAX_CUSTOM_FUNCTIONS)
            .map(|n| alloc::format!("x:key-{n}"))
            .collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert!(packaged(&refs[..crate::commands::MAX_CUSTOM_FUNCTIONS])
            .validate()
            .is_ok());
        let problems = packaged(&refs).validate().unwrap_err().problems;
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].at, "connections[0].provider.capabilities");
        // Spelled outside the grammar, or twice, it is not a capability at all.
        for ids in [&["x:Info"][..], &["x:info", "x:info"], &["x:"]] {
            assert!(packaged(ids).validate().is_err(), "{ids:?}");
        }
    }

    #[test]
    fn typed_actions_are_a_set_of_distinct_kinds_and_the_db_control_needs_its_own() {
        let schema = crate::PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        let invalid = crate::PluginActionSchema::SetVolumeDb {
            min_tenths: 0,
            max_tenths: 0,
            step_tenths: 5,
        };
        let control = crate::PluginComponent::VolumeDbControl {
            label: "Volume".into(),
        };
        for (actions, presentation, valid) in [
            (vec![], vec![], true),
            (vec![schema], vec![], true),
            (vec![schema], vec![control.clone()], true),
            (vec![], vec![control.clone()], false),
            (vec![invalid], vec![control.clone()], false),
            (vec![invalid], vec![], false),
            (vec![schema, schema], vec![control.clone()], false),
            (vec![schema; 9], vec![], false),
        ] {
            let mut cfg = packaged(&["menu"]);
            if let crate::Provider::Plugin {
                actions: a,
                presentation: p,
                ..
            } = &mut cfg.connections[0].provider
            {
                *a = actions.clone();
                *p = presentation.clone();
            }
            assert_eq!(
                cfg.validate().is_ok(),
                valid,
                "{actions:?} {presentation:?}"
            );
        }
    }

    /// The mirror of the manifest's own rules for a packaged media player
    /// (protocol 3, unreleased). Nothing here reaches a released core: see
    /// `storage::v2_projection` and `tools/tests/config-crossload.rs`.
    #[test]
    fn a_packaged_player_declares_a_screen_it_can_actually_drive() {
        use crate::{
            ActionKind, ArtRole, ColourKey, MediaKey, MediaLayout, MediaList, PlayMode,
            PlayModeSet, PluginActionSchema, PluginComponent,
        };
        const TRANSPORT: &[&str] = &["play-pause", "next", "previous"];
        const NAVIGATION: &[&str] = &[
            "play-pause",
            "next",
            "up",
            "down",
            "left",
            "right",
            "ok",
            "back",
            "home",
            "menu",
        ];
        let player = |layout, artwork: &[ArtRole], lists: Vec<MediaList>, up_next, navigation| {
            PluginComponent::MediaPlayer {
                layout,
                artwork: artwork.to_vec(),
                lists,
                up_next,
                navigation,
                refresh_ms: crate::DEFAULT_REFRESH_MS,
                keys: vec![],
            }
        };
        let list = |id: &str| MediaList {
            id: id.into(),
            label: "Sheet".into(),
            choose: true,
        };
        let music = player(MediaLayout::Music, &[ArtRole::Cover], vec![], false, false);
        let modes = PluginActionSchema::SetMode {
            modes: PlayModeSet::new().with(PlayMode::Shuffle),
        };
        let percent = PluginActionSchema::SetVolumePercent { max_percent: 100 };
        let check = |capabilities: &[&str],
                     presentation: Vec<PluginComponent>,
                     actions: Vec<PluginActionSchema>,
                     supports_inputs: bool| {
            let mut cfg = packaged(capabilities);
            if let crate::Provider::Plugin {
                actions: a,
                presentation: p,
                supports_inputs: i,
                ..
            } = &mut cfg.connections[0].provider
            {
                *a = actions;
                *p = presentation;
                *i = supports_inputs;
            }
            cfg.validate().is_ok()
        };
        // The transport it is drawn over has to exist: `play-pause`, or both
        // halves of it.
        assert!(check(TRANSPORT, vec![music.clone()], vec![], false));
        assert!(check(
            &["play", "pause", "next"],
            vec![music.clone()],
            vec![],
            false
        ));
        assert!(!check(
            &["play", "next"],
            vec![music.clone()],
            vec![],
            false
        ));
        assert!(!check(&["menu"], vec![music.clone()], vec![], false));
        // One screen, one player.
        assert!(!check(
            TRANSPORT,
            vec![music.clone(), music.clone()],
            vec![],
            false
        ));
        // A picture belongs to the layout that has somewhere to put it, once.
        for (layout, roles, valid) in [
            (MediaLayout::Music, &[ArtRole::Cover][..], true),
            (MediaLayout::Music, &[ArtRole::Backdrop], false),
            (MediaLayout::Music, &[ArtRole::Logo], false),
            (
                MediaLayout::Video,
                &[ArtRole::Backdrop, ArtRole::Logo],
                true,
            ),
            (MediaLayout::Video, &[ArtRole::Cover], false),
            (
                MediaLayout::Video,
                &[ArtRole::Backdrop, ArtRole::Backdrop],
                false,
            ),
        ] {
            assert_eq!(
                check(
                    TRANSPORT,
                    vec![player(layout, roles, vec![], false, false)],
                    vec![],
                    false
                ),
                valid,
                "{layout:?} {roles:?}"
            );
        }
        // Up next is built from what the player says is next, so it needs the
        // command that plays it.
        assert!(check(
            TRANSPORT,
            vec![player(MediaLayout::Music, &[], vec![], true, false)],
            vec![],
            false
        ));
        assert!(!check(
            &["play-pause"],
            vec![player(MediaLayout::Music, &[], vec![], true, false)],
            vec![],
            false
        ));
        // Navigation needs every key that mode sends, and Couch chooses which
        // those are.
        assert!(check(
            NAVIGATION,
            vec![player(MediaLayout::Video, &[], vec![], false, true)],
            vec![],
            false
        ));
        for missing in ["menu", "ok", "back", "home", "up"] {
            let short: Vec<&str> = NAVIGATION
                .iter()
                .copied()
                .filter(|k| *k != missing)
                .collect();
            assert!(
                !check(
                    &short,
                    vec![player(MediaLayout::Video, &[], vec![], false, true)],
                    vec![],
                    false
                ),
                "navigation without {missing}"
            );
        }
        // The screen holds three sheets, whichever they are.
        for (lists, up_next, inputs, mode_sheet, valid) in [
            (
                vec!["chapters", "audio", "subtitles"],
                false,
                false,
                false,
                true,
            ),
            (vec!["chapters", "audio"], true, false, false, true),
            (vec!["chapters"], true, true, false, true),
            (vec![], true, true, true, true),
            (
                vec!["chapters", "audio", "subtitles"],
                true,
                false,
                false,
                false,
            ),
            (vec!["chapters", "audio"], true, true, false, false),
            (vec!["chapters"], true, true, true, false),
        ] {
            let sheets: Vec<MediaList> = lists.iter().map(|id| list(id)).collect();
            assert_eq!(
                check(
                    TRANSPORT,
                    vec![player(MediaLayout::Video, &[], sheets, up_next, false)],
                    if mode_sheet { vec![modes] } else { vec![] },
                    inputs
                ),
                valid,
                "{lists:?} {up_next} {inputs} {mode_sheet}"
            );
        }
        // A list is named once, by an identifier.
        for ids in [
            &["chapters", "chapters"][..],
            &["Chapters"],
            &[""],
            &["../etc"],
        ] {
            let sheets: Vec<MediaList> = ids.iter().map(|id| list(id)).collect();
            assert!(
                !check(
                    TRANSPORT,
                    vec![player(MediaLayout::Video, &[], sheets, false, false)],
                    vec![],
                    false
                ),
                "{ids:?}"
            );
        }
        // A re-read that never stops, or one every millisecond, is neither.
        for (refresh_ms, valid) in [(999, false), (1000, true), (30_000, true), (30_001, false)] {
            let component = PluginComponent::MediaPlayer {
                layout: MediaLayout::Music,
                artwork: vec![],
                lists: vec![],
                up_next: false,
                navigation: false,
                refresh_ms,
                keys: vec![],
            };
            assert_eq!(
                check(TRANSPORT, vec![component], vec![], false),
                valid,
                "{refresh_ms}"
            );
        }
        // The four colour keys, each once, each naming something the package
        // declares - a word Couch knows or one of its own.
        let keyed = |keys: Vec<MediaKey>| PluginComponent::MediaPlayer {
            layout: MediaLayout::Video,
            artwork: vec![],
            lists: vec![],
            up_next: false,
            navigation: false,
            refresh_ms: crate::DEFAULT_REFRESH_MS,
            keys,
        };
        let key = |key, command: &str| MediaKey {
            key,
            command: command.into(),
        };
        let named = &["play-pause", "next", "stop", "x:info"][..];
        assert!(check(
            named,
            vec![keyed(vec![
                key(ColourKey::Red, "x:info"),
                key(ColourKey::Blue, "stop"),
            ])],
            vec![],
            false
        ));
        assert!(check(
            named,
            vec![keyed(vec![
                key(ColourKey::Red, "x:info"),
                key(ColourKey::Green, "stop"),
                key(ColourKey::Yellow, "next"),
                key(ColourKey::Blue, "play-pause"),
            ])],
            vec![],
            false
        ));
        for keys in [
            vec![key(ColourKey::Red, "x:osd")],
            vec![key(ColourKey::Red, "rewind")],
            vec![key(ColourKey::Red, "x:info"), key(ColourKey::Red, "stop")],
        ] {
            assert!(
                !check(named, vec![keyed(keys.clone())], vec![], false),
                "{keys:?}"
            );
        }
        // The percentage control is drawn over its own action, exactly as the
        // decibel one is.
        let control = PluginComponent::VolumePercentControl {
            label: "Volume".into(),
        };
        assert!(check(
            TRANSPORT,
            vec![control.clone()],
            vec![percent],
            false
        ));
        assert!(!check(TRANSPORT, vec![control.clone()], vec![], false));
        assert!(!check(
            TRANSPORT,
            vec![control.clone()],
            vec![PluginActionSchema::SetVolumePercent { max_percent: 0 }],
            false
        ));
        assert!(!check(
            TRANSPORT,
            vec![control],
            vec![PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5,
            }],
            false
        ));
        assert_eq!(percent.kind(), ActionKind::SetVolumePercent);
    }

    /// A player, its lists, its keys and its schemas survive a save and a
    /// load, and every field a package did not fill vanishes from the bytes.
    #[test]
    fn a_player_round_trips_and_writes_only_what_the_package_asked_for() {
        use crate::{ArtRole, ColourKey, MediaKey, MediaLayout, MediaList, PluginComponent};
        let bare = PluginComponent::MediaPlayer {
            layout: MediaLayout::Music,
            artwork: vec![],
            lists: vec![],
            up_next: false,
            navigation: false,
            refresh_ms: crate::DEFAULT_REFRESH_MS,
            keys: vec![],
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"kind":"media_player","layout":"music"}),
            "nothing a package left alone is written, so an old reader sees no new word"
        );
        assert_eq!(
            serde_json::from_value::<PluginComponent>(
                serde_json::json!({"kind":"media_player","layout":"music"})
            )
            .unwrap(),
            bare
        );
        let full = PluginComponent::MediaPlayer {
            layout: MediaLayout::Video,
            artwork: vec![ArtRole::Backdrop, ArtRole::Logo],
            lists: vec![
                MediaList {
                    id: "chapters".into(),
                    label: "Chapters".into(),
                    choose: true,
                },
                MediaList {
                    id: "audio".into(),
                    label: "Audio".into(),
                    choose: false,
                },
            ],
            up_next: true,
            navigation: true,
            refresh_ms: 2_000,
            keys: vec![MediaKey {
                key: ColourKey::Yellow,
                command: "x:subtitle-next".into(),
            }],
        };
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"kind":"media_player","layout":"video",
                "artwork":["backdrop","logo"],
                "lists":[{"id":"chapters","label":"Chapters","choose":true},
                         {"id":"audio","label":"Audio"}],
                "up_next":true,"navigation":true,"refresh_ms":2000,
                "keys":[{"key":"yellow","command":"x:subtitle-next"}]})
        );
        assert_eq!(
            serde_json::from_value::<PluginComponent>(value).unwrap(),
            full
        );
        for bad in [
            serde_json::json!({"kind":"media_player"}),
            serde_json::json!({"kind":"media_player","layout":"radio"}),
            serde_json::json!({"kind":"media_player","layout":"music","seek":true}),
            serde_json::json!({"kind":"media_player","layout":"music","artwork":["poster"]}),
            serde_json::json!({"kind":"media_player","layout":"music",
                "lists":[{"id":"a","label":"A","pick":true}]}),
            serde_json::json!({"kind":"media_player","layout":"music",
                "keys":[{"key":"power","command":"stop"}]}),
            serde_json::json!({"kind":"media_player","layout":"music",
                "keys":[{"key":"red"}]}),
            serde_json::json!({"kind":"volume_percent_control"}),
        ] {
            assert!(
                serde_json::from_value::<PluginComponent>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn home_assistant_blinds_and_thermostats_can_be_bound_to_a_key() {
        let mut living = room("living", "Living room");
        for (id, name, kind, entity) in [
            ("blind", "Blind", DeviceKind::Blind, "cover.office"),
            (
                "stat",
                "Thermostat",
                DeviceKind::Thermostat,
                "climate.office",
            ),
        ] {
            living
                .devices
                .push(Device::new(Id::new(id), name, kind).with_integration(
                    crate::Integration::Connection {
                        connection_id: Id::new("ha"),
                        resource_id: entity.to_string(),
                        child: None,
                    },
                ));
        }
        let base = Config {
            connections: vec![crate::Connection {
                id: Id::new("ha"),
                name: "HA".to_string(),
                provider: crate::Provider::HomeAssistant,
            }],
            rooms: vec![living],
            activities: vec![crate::Activity {
                setup: Default::default(),
                id: Id::new("a"),
                name: "A".to_string(),
                kind: Default::default(),
                room: Id::new("living"),
                source: None,
                buttons: vec![],
                steps: vec![],
            }],
            ..Config::default()
        };
        for (device, command, valid) in [
            ("blind", "open", true),
            ("blind", "close", true),
            ("blind", "stop", true),
            ("blind", "position:70", true),
            ("blind", "position:101", false),
            ("blind", "mode:heat", false),
            ("blind", "on", false),
            ("stat", "mode:heat", true),
            ("stat", "mode:off", true),
            ("stat", "temperature-up", true),
            ("stat", "temperature-down", true),
            ("stat", "open", false),
            ("stat", "dim:30", false),
        ] {
            let mut config = base.clone();
            config.activities[0].buttons = vec![crate::buttons::Binding {
                button: crate::buttons::Button::Lights,
                gesture: crate::buttons::Gesture::Short,
                action: Some(Action::new(Id::new(device), command)),
            }];
            assert_eq!(config.validate().is_ok(), valid, "{device} {command}");
        }
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let cfg = Config {
            schema_version: SCHEMA_VERSION + 1,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }
    #[test]
    fn ha_resources_require_supported_domains_and_matching_kinds() {
        for (entity, kind) in [
            ("light.office", DeviceKind::Light),
            ("cover.office", DeviceKind::Blind),
            ("climate.office", DeviceKind::Thermostat),
        ] {
            assert!(valid_ha_resource(entity, kind));
        }
        for entity in [
            "cover.",
            "cover.a/b",
            "cover.a.b",
            "switch.office",
            "cover.Office",
        ] {
            assert!(!valid_ha_resource(entity, DeviceKind::Blind));
        }
        assert!(!valid_ha_resource("cover.office", DeviceKind::Light));
        assert!(!valid_ha_resource("climate.office", DeviceKind::Blind));
    }
}

/// Protocol 3 (unreleased): children of a packaged connection as room devices,
/// package scenes, and what a child can be told.
#[cfg(test)]
mod child_tests {
    use crate::buttons::{Binding, Button};
    use crate::commands::Function;
    use crate::{
        Action, ActionKind, ChildComponent, ChildSnapshot, ClimateMode, ClimateTraits, Config,
        Connection, CoverTraits, DeviceKind, Id, Integration, LightTraits, PluginActionSchema,
        PluginCapability, PluginChildKind, PluginComponent, Provider, Scene, SceneResource,
        Shortcut, ShortcutAction, TempUnit,
    };
    use alloc::{string::String, vec, vec::Vec};

    fn named(ids: &[&str]) -> Vec<PluginCapability> {
        ids.iter()
            .map(|id| PluginCapability {
                id: (*id).into(),
                label: "Label".into(),
            })
            .collect()
    }

    fn kinds() -> Vec<PluginChildKind> {
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
                &["on", "off", "toggle"],
                vec![PluginActionSchema::SetLight {}],
            ),
            kind(
                "plug",
                DeviceKind::Switch,
                ChildComponent::Light,
                &["on", "off"],
                vec![],
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
                &["open", "close", "toggle"],
                vec![PluginActionSchema::SetCover {}],
            ),
            kind(
                "thermostat",
                DeviceKind::Thermostat,
                ChildComponent::Climate,
                &["temperature-up"],
                vec![PluginActionSchema::SetClimate {}],
            ),
        ]
    }

    fn snapshot(kind: &str) -> ChildSnapshot {
        ChildSnapshot {
            kind: kind.into(),
            light: None,
            cover: None,
            climate: None,
        }
    }

    fn lamp(dimmable: bool) -> ChildSnapshot {
        ChildSnapshot {
            light: Some(LightTraits {
                dimmable,
                mirek: None,
                color: false,
            }),
            ..snapshot("light")
        }
    }

    const LAMP: &str = "5f0c9a52-7d1e-4a63-9b0e-2f6d1c3a8e41";

    /// The seed with a bridge-like package, and `living-lamp` as one of its
    /// lamps.
    fn home() -> Config {
        let mut config = Config::seed();
        config.connections.push(Connection {
            id: Id::new("bridge"),
            name: "Bridge".into(),
            provider: Provider::Plugin {
                id: "echo".into(),
                label: "Echo".into(),
                capabilities: named(&["power-on", "dim:30"]),
                supports_inputs: true,
                presentation: vec![],
                actions: vec![],
                children: kinds(),
            },
        });
        config.rooms[0].devices[4].integration = child(LAMP, lamp(true));
        config
    }

    fn child(resource: &str, snapshot: ChildSnapshot) -> Integration {
        Integration::Connection {
            connection_id: Id::new("bridge"),
            resource_id: resource.into(),
            child: Some(snapshot),
        }
    }

    fn problem(config: &Config) -> String {
        match config.validate() {
            Ok(()) => String::new(),
            Err(error) => alloc::format!("{error}"),
        }
    }

    #[test]
    fn a_child_resolves_to_what_its_kind_can_do_never_to_what_the_connection_can() {
        let config = home();
        assert_eq!(problem(&config), "");
        let resolved = config
            .resolve_integration(&config.rooms[0].devices[4].integration)
            .unwrap();
        assert_eq!(
            resolved,
            Integration::Plugin {
                id: "echo".into(),
                connection_id: Id::new("bridge"),
                resource_id: LAMP.into(),
                capabilities: named(&["on", "off", "toggle"]),
                supports_inputs: false,
                presentation: vec![],
                actions: vec![PluginActionSchema::SetLight {}],
                child: Some(lamp(true)),
            }
        );
        assert_eq!(
            crate::buttons::function_choices(&resolved),
            vec![
                ("on".into(), "Label".into()),
                ("off".into(), "Label".into()),
                ("toggle".into(), "Label".into())
            ],
            "a picker offers the kind's commands"
        );
        for (command, supported) in [
            ("toggle", true),
            ("dim:30", true),
            ("dim:0", true),
            ("power-on", false),
            ("input:HDMI1", false),
            ("position:40", false),
            ("mode:heat", false),
            ("volume:30", false),
            ("x:blink", false),
        ] {
            assert_eq!(
                Function::parse(command).unwrap().supports(&resolved),
                supported,
                "{command}"
            );
        }
        // The connection as a device of its own is what it always was.
        let whole = config
            .resolve_integration(&Integration::Connection {
                connection_id: Id::new("bridge"),
                resource_id: "zone1".into(),
                child: None,
            })
            .unwrap();
        assert!(matches!(
            &whole,
            Integration::Plugin { capabilities, supports_inputs: true, child: None, .. }
                if capabilities == &named(&["power-on", "dim:30"])
        ));
        assert!(Function::PowerOn.supports(&whole));
        assert!(!Function::Toggle.supports(&whole));
        assert!(
            Function::Dim(30).supports(&whole) && !Function::Dim(31).supports(&whole),
            "a level spelt out as a capability is still exactly that, as in every release"
        );

        // A kind the package no longer declares: the device stays, inert.
        let gone = config
            .resolve_integration(&child(LAMP, snapshot("strip")))
            .unwrap();
        assert!(matches!(
            &gone,
            Integration::Plugin { capabilities, actions, child: Some(_), .. }
                if capabilities.is_empty() && actions.is_empty()
        ));
        assert!(crate::buttons::function_choices(&gone).is_empty());
        for command in ["on", "toggle", "dim:30", "power-on"] {
            assert!(!Function::parse(command).unwrap().supports(&gone));
        }
    }

    #[test]
    fn a_level_needs_the_action_its_kind_declares_and_a_child_that_can_take_it() {
        let mut config = home();
        let blind = ChildSnapshot {
            cover: Some(CoverTraits {
                position: true,
                stop: false,
            }),
            ..snapshot("blind")
        };
        let thermostat = ChildSnapshot {
            climate: Some(ClimateTraits {
                min_tenths: 70,
                max_tenths: 300,
                step_tenths: 5,
                unit: TempUnit::Celsius,
                modes: vec![ClimateMode::Off, ClimateMode::HeatCool],
                range: true,
            }),
            ..snapshot("thermostat")
        };
        let resolve = |config: &Config, integration: &Integration| {
            config.resolve_integration(integration).unwrap()
        };
        let supports = |integration: &Integration, command: &str| {
            Function::parse(command).unwrap().supports(integration)
        };
        let cover = resolve(&config, &child("cover/1", blind.clone()));
        assert!(supports(&cover, "position:40"));
        assert!(supports(&cover, "open"));
        assert!(!supports(&cover, "dim:30"));
        let fixed = ChildSnapshot {
            cover: Some(CoverTraits::default()),
            ..blind
        };
        assert!(!supports(
            &resolve(&config, &child("cover/1", fixed)),
            "position:40"
        ));
        let climate = resolve(&config, &child("climate/1", thermostat));
        assert!(supports(&climate, "mode:heat_cool"));
        assert!(supports(&climate, "mode:off"));
        assert!(!supports(&climate, "mode:heat"));
        assert!(supports(&climate, "temperature-up"));
        assert!(!supports(&climate, "temperature-down"));

        assert!(!supports(
            &resolve(&config, &child(LAMP, lamp(false))),
            "dim:30"
        ));
        assert!(!supports(
            &resolve(&config, &child(LAMP, snapshot("light"))),
            "dim:30"
        ));
        // Dimmable, but of a kind that declares no set_light.
        let plug = ChildSnapshot {
            kind: "plug".into(),
            ..lamp(true)
        };
        assert!(!supports(&resolve(&config, &child(LAMP, plug)), "dim:30"));

        // And a key is held to it.
        let bind = |config: &mut Config, command: &str| {
            config.activities[0].buttons = vec![Binding {
                button: Button::Red,
                gesture: Default::default(),
                action: Some(Action::new("living-lamp", command)),
            }];
        };
        bind(&mut config, "dim:30");
        assert_eq!(problem(&config), "");
        config.rooms[0].devices[4].integration = child(LAMP, lamp(false));
        assert!(problem(&config).contains("activities[0].buttons[0]"));
        bind(&mut config, "toggle");
        assert_eq!(problem(&config), "");
        bind(&mut config, "power-on");
        assert!(problem(&config).contains("activities[0].buttons[0]"));
    }

    #[test]
    fn a_key_toggles_a_light_or_cover_child_whose_kind_declares_toggle() {
        let mut config = home();
        config.areas[0].shortcuts = vec![Shortcut {
            button: Button::Lights,
            action: ShortcutAction::Toggle {
                device: Id::new("living-lamp"),
            },
        }];
        assert_eq!(problem(&config), "");
        assert!(config.can_toggle(&config.rooms[0].devices[4]));
        let set = |config: &mut Config, kind: DeviceKind, integration: Integration| {
            config.rooms[0].devices[4].kind = kind;
            config.rooms[0].devices[4].integration = integration;
        };
        set(
            &mut config,
            DeviceKind::Blind,
            child("cover/1", snapshot("blind")),
        );
        assert_eq!(problem(&config), "");
        // `plug` has no toggle; a thermostat is not something a key switches;
        // the connection as a whole never was.
        set(
            &mut config,
            DeviceKind::Switch,
            child(LAMP, snapshot("plug")),
        );
        assert!(problem(&config).contains("areas[0].shortcuts[0]"));
        if let Provider::Plugin { children, .. } =
            &mut config.connections.last_mut().unwrap().provider
        {
            children[4].capabilities = named(&["toggle"]);
        }
        set(
            &mut config,
            DeviceKind::Thermostat,
            child("climate/1", snapshot("thermostat")),
        );
        assert!(problem(&config).contains("areas[0].shortcuts[0]"));
        set(
            &mut config,
            DeviceKind::Light,
            Integration::Connection {
                connection_id: Id::new("bridge"),
                resource_id: "zone1".into(),
                child: None,
            },
        );
        assert!(problem(&config).contains("areas[0].shortcuts[0]"));
        // The resolved form saved on the device answers from its own copy.
        set(
            &mut config,
            DeviceKind::Light,
            Integration::Plugin {
                id: "echo".into(),
                connection_id: Id::new("bridge"),
                resource_id: LAMP.into(),
                capabilities: named(&["on", "toggle"]),
                supports_inputs: false,
                presentation: vec![],
                actions: vec![],
                child: Some(lamp(true)),
            },
        );
        assert_eq!(problem(&config), "");
    }

    #[test]
    fn a_saved_child_has_to_be_one_its_package_could_have_offered() {
        let at = "rooms.living-room.devices.living-lamp.child";
        let cases: Vec<(&str, DeviceKind, Integration, &str)> = vec![
            (
                "an undeclared kind",
                DeviceKind::Light,
                child(LAMP, snapshot("strip")),
                "does not offer that kind",
            ),
            (
                "a scene kind as a device",
                DeviceKind::Other,
                child("scene/1", snapshot("scene")),
                "belongs to a room's scenes",
            ),
            (
                "another kind of device than its kind says",
                DeviceKind::Speaker,
                child(LAMP, lamp(true)),
                "has to be the kind of device",
            ),
            (
                "a switch saved as a light",
                DeviceKind::Light,
                child(LAMP, snapshot("plug")),
                "has to be the kind of device",
            ),
            (
                "traits of another component",
                DeviceKind::Light,
                child(
                    LAMP,
                    ChildSnapshot {
                        cover: Some(CoverTraits::default()),
                        ..lamp(true)
                    },
                ),
                "does not fit",
            ),
            (
                "traits outside the global bounds",
                DeviceKind::Light,
                child(
                    LAMP,
                    ChildSnapshot {
                        light: Some(LightTraits {
                            dimmable: true,
                            mirek: Some((50, 500)),
                            color: false,
                        }),
                        ..snapshot("light")
                    },
                ),
                "does not fit",
            ),
        ];
        for (name, kind, integration, message) in cases {
            let mut config = home();
            config.rooms[0].devices[4].kind = kind;
            config.rooms[0].devices[4].integration = integration;
            let problem = problem(&config);
            assert!(
                problem.contains(at) && problem.contains(message),
                "{name}: {problem}"
            );
        }
        // A child's id is held to the strict grammar; a connection that is one
        // device keeps the rule every release has had, so `zone1`, an empty id
        // and even `a//b` keep loading.
        for id in ["", "../x", "a/../b", "a//b", "/a", "a/", "room:1", "a b"] {
            let mut config = home();
            config.rooms[0].devices[4].integration = child(id, lamp(true));
            assert!(problem(&config).contains(at), "{id:?}");
        }
        for id in ["", "zone1", "a//b", "../x"] {
            let mut config = home();
            config.rooms[0].devices[4].integration = Integration::Connection {
                connection_id: Id::new("bridge"),
                resource_id: id.into(),
                child: None,
            };
            assert_eq!(problem(&config), "", "{id:?}");
        }

        // Not a package at all, or one that declares no children (which is
        // every package a shipped build accepts).
        let mut config = home();
        config.connections.push(Connection {
            id: Id::new("player"),
            name: "Player".into(),
            provider: Provider::Kodi {
                host: "kodi.invalid".into(),
                port: 9090,
            },
        });
        config.rooms[0].devices[4].integration = Integration::Connection {
            connection_id: Id::new("player"),
            resource_id: String::new(),
            child: Some(lamp(true)),
        };
        assert!(problem(&config).contains("Only a connection to an integration package"));
        let mut config = home();
        if let Provider::Plugin { children, .. } =
            &mut config.connections.last_mut().unwrap().provider
        {
            children.clear();
        }
        assert!(problem(&config).contains("does not offer that kind"));
        // The resolved form is held to the same rules.
        let mut config = home();
        config.rooms[0].devices[4].integration = Integration::Plugin {
            id: "echo".into(),
            connection_id: Id::new("bridge"),
            resource_id: "../x".into(),
            capabilities: vec![],
            supports_inputs: false,
            presentation: vec![],
            actions: vec![],
            child: Some(lamp(true)),
        };
        assert!(problem(&config).contains(at));
        if let Integration::Plugin { resource_id, .. } = &mut config.rooms[0].devices[4].integration
        {
            *resource_id = LAMP.into();
        }
        assert_eq!(problem(&config), "");
        if let Integration::Plugin {
            supports_inputs, ..
        } = &mut config.rooms[0].devices[4].integration
        {
            *supports_inputs = true;
        }
        assert!(problem(&config).contains("no inputs or screen of its own"));
        if let Integration::Plugin {
            connection_id,
            supports_inputs,
            ..
        } = &mut config.rooms[0].devices[4].integration
        {
            *supports_inputs = false;
            *connection_id = Id::new("nowhere");
        }
        assert!(problem(&config).contains("missing connection"));
    }

    #[test]
    fn a_package_scene_names_a_scene_kind_and_is_nothing_else() {
        let scene = |resource: SceneResource| Scene {
            id: Id::new("relax"),
            name: "Relax".into(),
            icon: None,
            steps: vec![],
            hue: None,
            resource: Some(resource),
            rooms: vec![Id::new("living-room")],
        };
        let resource = |connection: &str, id: &str, kind: &str| SceneResource {
            connection_id: Id::new(connection),
            resource_id: id.into(),
            kind: kind.into(),
        };
        let mut config = home();
        config
            .scenes
            .push(scene(resource("bridge", "scene/1", "scene")));
        assert_eq!(problem(&config), "");
        let saved = serde_json::to_value(config.scenes.last().unwrap()).unwrap();
        assert_eq!(
            saved["resource"],
            serde_json::json!({"connection_id": "bridge", "resource_id": "scene/1", "kind": "scene"})
        );
        assert!(saved.get("hue").is_none());
        let at = alloc::format!("scenes[{}].resource", config.scenes.len() - 1);
        for (name, broken) in [
            (
                "a kind that is not a scene",
                resource("bridge", "scene/1", "light"),
            ),
            ("an undeclared kind", resource("bridge", "scene/1", "mood")),
            (
                "no such connection",
                resource("nowhere", "scene/1", "scene"),
            ),
            ("an id that climbs", resource("bridge", "../1", "scene")),
            ("no id", resource("bridge", "", "scene")),
        ] {
            *config.scenes.last_mut().unwrap() = scene(broken);
            assert!(problem(&config).contains(&at), "{name}");
        }
        let mut with_steps = scene(resource("bridge", "scene/1", "scene"));
        with_steps.steps = vec![Action::new("living-lamp", "on")];
        *config.scenes.last_mut().unwrap() = with_steps;
        assert!(problem(&config).contains(&at));
        let mut with_hue = scene(resource("bridge", "scene/1", "scene"));
        with_hue.hue = Some(crate::HueScene {
            connection_id: Id::new("bridge"),
            scene_id: "00000000-0000-0000-0000-000000000001".into(),
        });
        *config.scenes.last_mut().unwrap() = with_hue;
        assert!(problem(&config).contains(&at));
    }

    #[test]
    fn a_package_declares_valid_kinds_and_at_most_32_buttons_of_its_own_in_all() {
        let mut config = home();
        let provider =
            |config: &mut Config,
             edit: &dyn Fn(&mut Vec<PluginCapability>, &mut Vec<PluginChildKind>)| {
                if let Provider::Plugin {
                    capabilities,
                    children,
                    ..
                } = &mut config.connections.last_mut().unwrap().provider
                {
                    edit(capabilities, children);
                }
            };
        let own = |range: core::ops::Range<usize>| -> Vec<PluginCapability> {
            range
                .map(|n| PluginCapability {
                    id: alloc::format!("x:own-{n}"),
                    label: "Own".into(),
                })
                .collect()
        };
        // 16 on the connection, 16 on a kind, and the same 16 again on another
        // kind: 32 names.
        provider(&mut config, &|capabilities, children| {
            capabilities.extend(own(0..16));
            children[0].capabilities.extend(own(16..32));
            children[3].capabilities.extend(own(16..32));
        });
        assert_eq!(problem(&config), "");
        provider(&mut config, &|_, children| {
            children[1].capabilities.extend(own(32..33));
        });
        assert!(problem(&config).contains("too many buttons of its own"));

        let mut config = home();
        provider(&mut config, &|_, children| {
            let again = children[0].clone();
            children.push(again);
        });
        assert!(problem(&config).contains("provider.children"));

        // A connection that is itself a lamp needs the action its control is
        // drawn over, as the decibel control always has.
        for (component, schema) in [
            (
                PluginComponent::Light {
                    label: "Lamp".into(),
                },
                PluginActionSchema::SetLight {},
            ),
            (
                PluginComponent::Cover {
                    label: "Blind".into(),
                },
                PluginActionSchema::SetCover {},
            ),
            (
                PluginComponent::Climate {
                    label: "Heating".into(),
                },
                PluginActionSchema::SetClimate {},
            ),
        ] {
            let mut config = home();
            let set = |config: &mut Config, actions: Vec<PluginActionSchema>| {
                if let Provider::Plugin {
                    presentation,
                    actions: declared,
                    ..
                } = &mut config.connections.last_mut().unwrap().provider
                {
                    *presentation = vec![component.clone()];
                    *declared = actions;
                }
            };
            set(&mut config, vec![schema]);
            assert_eq!(problem(&config), "", "{component:?}");
            set(&mut config, vec![]);
            assert!(
                problem(&config).contains("presentation is invalid"),
                "{component:?}"
            );
            assert_eq!(
                PluginActionSchema::find(&[schema], schema.kind()).map(|s| s.kind()),
                Some(schema.kind())
            );
        }
        assert_ne!(ActionKind::SetLight, ActionKind::SetCover);
    }

    #[test]
    fn nothing_new_is_written_for_a_configuration_that_has_nothing_new() {
        let connection = Integration::Connection {
            connection_id: Id::new("bridge"),
            resource_id: "zone1".into(),
            child: None,
        };
        assert_eq!(
            serde_json::to_string(&connection).unwrap(),
            r#"{"via":"connection","connection_id":"bridge","resource_id":"zone1"}"#
        );
        let mut config = home();
        if let Provider::Plugin {
            children,
            capabilities,
            supports_inputs,
            ..
        } = &mut config.connections.last_mut().unwrap().provider
        {
            children.clear();
            capabilities.clear();
            *supports_inputs = false;
        }
        assert_eq!(
            serde_json::to_string(&config.connections.last().unwrap().provider).unwrap(),
            r#"{"kind":"plugin","id":"echo","label":"Echo"}"#
        );
        let scene = serde_json::to_string(&Config::seed().scenes[0]).unwrap();
        assert!(!scene.contains("\"resource\"") && !scene.contains("\"hue\""));
        // And the old shapes read back as "not a child".
        let old: Integration =
            serde_json::from_str(r#"{"via":"plugin","id":"echo","connection_id":"bridge"}"#)
                .unwrap();
        assert!(matches!(old, Integration::Plugin { child: None, .. }));
        assert_eq!(
            serde_json::from_str::<Integration>(
                r#"{"via":"connection","connection_id":"bridge","resource_id":"zone1"}"#
            )
            .unwrap(),
            connection
        );
    }
}
