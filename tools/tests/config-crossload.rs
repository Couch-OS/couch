//! The program `config-crossload.sh` builds twice: once inside a copy of the
//! released model it checks against, once inside a copy of this tree's model.
//! It is dropped into `couch-model/examples/`, so it needs no manifest or
//! lockfile change in either copy, and it uses only the public API both have.
//!
//! Every command does what `couch-confd`'s `Store::open` and `Store::write` do
//! with the model: parse a `StoredConfig`, `into_config`, `validate`, `migrate`,
//! and serialise `StoredConfig::new` pretty-printed with a trailing newline.
use couch_model::buttons::{Binding, Button, Gesture};
use couch_model::{
    Action, ActivityPage, ActivityWidget, Config, Connection, DenonMigration, Id, Integration,
    PluginActionSchema, PluginCapability, PluginComponent, PluginStatusField, Provider,
    SequenceStep, Shortcut, ShortcutAction, StoredConfig,
};
use serde_json::json;
use std::{env, fs, process::exit};

const ENVELOPE: [(&str, &str); 3] = [
    ("integration_config", "v1"),
    ("integration_config_v2", "v2"),
    ("integration_config_v3", "v3"),
];

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("{}", message.as_ref());
    exit(1)
}

/// The three types a later protocol adds fields to are built from JSON, not
/// from struct literals: this one source has to compile against the released
/// model and against this tree's, and a literal stops compiling the moment
/// either side has a field the other lacks.
fn built<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|e| fail(format!("a state does not parse: {e}")))
}

fn capability(id: &str, label: &str) -> PluginCapability {
    PluginCapability {
        id: id.into(),
        label: label.into(),
    }
}

/// `level` 1 is what a protocol 1 package saves, 2 adds the decibel control and
/// an input whose name has a space, and `custom` adds package-named buttons
/// (`x:`) everywhere a command can be saved. `denon` puts all of it on a
/// connection the Denon pilot converted, receipt included.
fn state(name: &str) -> Config {
    let (level, custom, denon) = match name {
        "A" => return Config::seed(),
        "B" => (1, false, false),
        "C" => (2, false, false),
        "Cd" => (2, false, true),
        "D" => (2, true, false),
        "E" => (2, true, true),
        "F" => (1, true, false),
        "H" | "I" | "J" | "K" | "L" | "M" => return child_state(name),
        other => fail(format!("unknown state {other}")),
    };
    let mut config = Config::seed();
    let mut capabilities = vec![
        capability("power-on", "On"),
        capability("power-off", "Off"),
        capability("volume-up", "Louder"),
    ];
    let mut presentation = vec![
        PluginComponent::CommandGroup {
            title: "Power".into(),
            commands: vec!["power-on".into(), "power-off".into()],
        },
        PluginComponent::Toggle {
            label: "Power".into(),
            state: PluginStatusField::On,
            on: "power-on".into(),
            off: "power-off".into(),
        },
    ];
    let mut actions = vec![];
    if level == 2 {
        presentation.push(PluginComponent::InputSelector {
            label: "Source".into(),
        });
        presentation.push(PluginComponent::VolumeDbControl {
            label: "Volume".into(),
        });
        presentation.push(PluginComponent::StatusText {
            label: "Volume".into(),
            field: PluginStatusField::VolumeDb,
        });
        actions.push(PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        });
    }
    if custom {
        for (id, label) in [
            ("x:info", "Info"),
            ("x:osd", "On-screen display"),
            ("x:subtitles-on", "Subtitles on"),
            ("x:subtitles-off", "Subtitles off"),
        ] {
            capabilities.push(capability(id, label));
        }
        // One group that keeps a member an older Couch knows, one that does
        // not, and a switch made only of the package's own buttons.
        presentation.push(PluginComponent::CommandGroup {
            title: "Mixed".into(),
            commands: vec!["x:info".into(), "volume-up".into()],
        });
        presentation.push(PluginComponent::CommandGroup {
            title: "Extras".into(),
            commands: vec!["x:info".into(), "x:osd".into()],
        });
        presentation.push(PluginComponent::Toggle {
            label: "Subtitles".into(),
            state: PluginStatusField::Playing,
            on: "x:subtitles-on".into(),
            off: "x:subtitles-off".into(),
        });
    }
    let connection = Id::new("receiver");
    let package = if denon { "denon" } else { "echo" };
    let plugin: Provider = built(json!({
        "kind": "plugin",
        "id": package,
        "label": if denon { "Denon" } else { "Echo" },
        "capabilities": capabilities,
        "supports_inputs": level == 2,
        "presentation": presentation,
        "actions": actions,
    }));
    config.connections.push(Connection {
        id: connection.clone(),
        name: "Receiver".into(),
        provider: plugin,
    });
    if denon {
        config.denon_migrations.insert(
            connection.clone(),
            DenonMigration {
                host: "avr.invalid".into(),
                port: 23,
            },
        );
    }
    let device = config.rooms[0].devices[0].id.clone();
    config.rooms[0].devices[0].integration = built(json!({
        "via": "connection",
        "connection_id": connection,
        // A converted Denon connection keeps the empty resource it always had.
        "resource_id": if denon { "" } else { "zone1" },
    }));
    // The resolved form saved straight on a device takes the same path.
    let direct = config.rooms[0].devices[1].id.clone();
    config.rooms[0].devices[1].integration = built(json!({
        "via": "plugin",
        "id": package,
        "connection_id": connection,
        "resource_id": "zone2",
        "capabilities": capabilities,
        "supports_inputs": level == 2,
        "presentation": presentation,
        "actions": actions,
    }));

    let mut commands = vec!["power-on"];
    if level == 2 {
        commands.push("input:HD RADIO");
    }
    if custom {
        commands.push("x:info");
    }
    let actions: Vec<Action> = [&device, &direct]
        .into_iter()
        .flat_map(|device| {
            commands
                .iter()
                .map(move |command| Action::new(device.clone(), *command))
        })
        .collect();
    let activity = &mut config.activities[0];
    activity.buttons = actions
        .iter()
        .zip([
            Button::Red,
            Button::Green,
            Button::Blue,
            Button::Yellow,
            Button::Menu,
            Button::Power,
        ])
        .map(|(action, button)| Binding {
            button,
            gesture: Gesture::Short,
            action: Some(action.clone()),
        })
        .collect();
    activity.steps = actions.clone();
    activity.setup.devices = vec![device, direct];
    let sequence = |actions: &[Action]| -> Vec<SequenceStep> {
        actions
            .iter()
            .map(|action| SequenceStep::Command {
                action: action.clone(),
            })
            .chain([SequenceStep::Delay { ms: 250 }])
            .collect()
    };
    activity.setup.on = sequence(&actions);
    activity.setup.off = sequence(&actions);
    activity.setup.custom_screen = true;
    activity.setup.pages = vec![ActivityPage {
        title: "Receiver".into(),
        widgets: actions
            .iter()
            .map(|action| ActivityWidget {
                label: action.command.clone(),
                icon: None,
                action: action.clone(),
            })
            .collect(),
    }];
    config.scenes[0].steps = actions;
    config
}


const LIGHT: &str = "5f0c9a52-7d1e-4a63-9b0e-2f6d1c3a8e41";
const GROUP: &str = "room/9d2b7c10-35aa-4c0e-8a57-6e1f0b94d2c3";
const DIRECT: &str = "0b7e44d1-91c2-4f3a-a6d8-3c5e7f1a2b90";
const SCENE: &str = "scene/3a1f6c8e-2b4d-4e9a-b7c5-d0e1f2a3b4c5";
const COVER: &str = "cover/blind-1";
const CLIMATE: &str = "climate/hall";

/// The kinds of child a package declares (protocol 3, unreleased): what each
/// can do is saved with the connection so a binding validates offline.
fn child_kinds() -> serde_json::Value {
    let named = |ids: &[&str]| -> Vec<serde_json::Value> {
        ids.iter().map(|id| json!({"id": id, "label": id})).collect()
    };
    json!([
        {"kind": "light", "label": "Light", "device_kind": "light", "component": "light",
         "capabilities": named(&["on", "off", "toggle"]), "actions": [{"action": "set_light"}]},
        {"kind": "group", "label": "Room or zone", "device_kind": "light", "component": "light",
         "capabilities": named(&["on", "off", "toggle"]), "actions": [{"action": "set_light"}]},
        {"kind": "scene", "label": "Scene", "device_kind": "other", "component": "scene",
         "capabilities": named(&["on"])},
        {"kind": "blind", "label": "Blind", "device_kind": "blind", "component": "cover",
         "capabilities": named(&["open", "close", "stop", "toggle"]),
         "actions": [{"action": "set_cover"}]},
        {"kind": "thermostat", "label": "Thermostat", "device_kind": "thermostat",
         "component": "climate",
         "capabilities": named(&["temperature-up", "temperature-down"]),
         "actions": [{"action": "set_climate"}]},
    ])
}

/// One saved child: which seed device becomes it, and what is bound to it.
struct ChildDevice {
    device: &'static str,
    kind: &'static str,
    /// The resolved form saved straight on the device, as `state` also does.
    direct: bool,
    resource: &'static str,
    snapshot: serde_json::Value,
    commands: [&'static str; 2],
}

/// H to M: children of one connection saved as room devices, a package scene,
/// and a connection that is itself one lamp. Everything new is put in through
/// JSON because the released model has no such fields or variants to name.
fn child_state(name: &str) -> Config {
    let light = |device, kind, direct, resource| ChildDevice {
        device,
        kind: "light",
        direct,
        resource,
        snapshot: json!({"kind": kind, "light": {"dimmable": true, "mirek": [153, 500]}}),
        commands: ["dim:30", "toggle"],
    };
    let lights = || {
        vec![
            light("living-lamp", "light", false, LIGHT),
            light("kitchen-hue", "group", false, GROUP),
            light("bedroom-hue", "light", true, DIRECT),
        ]
    };
    // (the state it starts from, the connection, its children, a package scene
    // that an area lists)
    let (base, connection, children, listed) = match name {
        "H" => ("A", "bridge", lights(), false),
        "I" => ("C", "receiver", lights(), false),
        "L" => ("Cd", "receiver", lights(), false),
        "J" => (
            "A",
            "bridge",
            vec![
                ChildDevice {
                    device: "bedroom-blind",
                    kind: "blind",
                    direct: false,
                    resource: COVER,
                    snapshot: json!({"kind": "blind", "cover": {"position": true, "stop": true}}),
                    commands: ["position:40", "toggle"],
                },
                ChildDevice {
                    device: "study-hue",
                    kind: "thermostat",
                    direct: false,
                    resource: CLIMATE,
                    snapshot: json!({"kind": "thermostat", "climate": {
                        "min_tenths": 70, "max_tenths": 300, "step_tenths": 5, "unit": "celsius",
                        "modes": ["off", "heat", "heat_cool"], "range": true}}),
                    commands: ["mode:heat", "temperature-up"],
                },
            ],
            false,
        ),
        "K" => ("A", "bridge", vec![], true),
        "M" => ("A", "lamp", vec![], false),
        other => fail(format!("unknown state {other}")),
    };
    let mut config = state(base);
    let package = if base == "Cd" { "denon" } else { "echo" };
    if base == "A" {
        let provider = if name == "M" {
            // The connection is the lamp: no children, one light component.
            json!({"kind": "plugin", "id": package, "label": "Echo",
                "capabilities": [{"id": "on", "label": "On"}, {"id": "off", "label": "Off"},
                                 {"id": "toggle", "label": "Toggle"}],
                "presentation": [
                    {"kind": "command_group", "title": "Power", "commands": ["on", "off"]},
                    {"kind": "light", "label": "Lamp"},
                    {"kind": "cover", "label": "Blind"},
                    {"kind": "climate", "label": "Heating"}],
                "actions": [{"action": "set_light"}, {"action": "set_cover"},
                            {"action": "set_climate"}]})
        } else {
            json!({"kind": "plugin", "id": package, "label": "Echo"})
        };
        config.connections.push(Connection {
            id: Id::new(connection),
            name: "Bridge".into(),
            provider: built(provider),
        });
    }

    let actions: Vec<Action> = children
        .iter()
        .flat_map(|child| child.commands.iter().map(|c| Action::new(child.device, *c)))
        .collect();
    let activity = &mut config.activities[0];
    activity.buttons.extend(
        actions
            .iter()
            .zip([
                Button::Lights,
                Button::Activity,
                Button::Music,
                Button::Tv,
                Button::Back,
                Button::Home,
            ])
            .map(|(action, button)| Binding {
                button,
                gesture: Gesture::Short,
                action: Some(action.clone()),
            }),
    );
    activity.steps.extend(actions.iter().cloned());
    activity
        .setup
        .devices
        .extend(children.iter().map(|child| Id::new(child.device)));
    for steps in [&mut activity.setup.on, &mut activity.setup.off] {
        steps.extend(actions.iter().map(|action| SequenceStep::Command {
            action: action.clone(),
        }));
    }
    if !actions.is_empty() {
        activity.setup.custom_screen = true;
        activity.setup.pages.push(ActivityPage {
            title: "Children".into(),
            widgets: actions
                .iter()
                .map(|action| ActivityWidget {
                    label: action.command.clone(),
                    icon: None,
                    action: action.clone(),
                })
                .collect(),
        });
        config.scenes[1].steps = actions.clone();
        // One key that switches a child, which an older Couch would refuse,
        // and one that opens a child, which it accepts.
        config.areas[0].shortcuts = vec![
            Shortcut {
                button: Button::Lights,
                action: ShortcutAction::Toggle {
                    device: Id::new(children[0].device),
                },
            },
            Shortcut {
                button: Button::Red,
                action: ShortcutAction::Device {
                    device: Id::new(children[1].device),
                },
            },
        ];
    }
    if name == "M" {
        let lamp = Id::new("living-lamp");
        config.rooms[0].devices[4].integration = built(json!({
            "via": "connection", "connection_id": connection, "resource_id": ""}));
        let activity = &mut config.activities[0];
        activity.buttons.push(Binding {
            button: Button::Lights,
            gesture: Gesture::Short,
            action: Some(Action::new(lamp.clone(), "toggle")),
        });
        activity.steps.push(Action::new(lamp, "on"));
    }
    // A scene that belongs to the package. In K an area lists it, so taking it
    // away has to take the reference too.
    let scene = if listed {
        let id = config.areas[0].scenes[0].clone();
        let at = config.scenes.iter().position(|s| s.id == id).unwrap();
        config.scenes[at].steps.clear();
        Some(at)
    } else if name == "M" {
        None
    } else {
        let mut scene = config.scenes[0].clone();
        scene.id = Id::new("package-scene");
        scene.name = "Package scene".into();
        scene.steps.clear();
        config.scenes.push(scene);
        Some(config.scenes.len() - 1)
    };

    let mut value = serde_json::to_value(&config).unwrap();
    if name != "M" {
        let provider = value["connections"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == connection)
            .unwrap();
        provider["provider"]["children"] = child_kinds();
    }
    if let Some(at) = scene {
        value["scenes"][at]["resource"] =
            json!({"connection_id": connection, "resource_id": SCENE, "kind": "scene"});
    }
    for room in value["rooms"].as_array_mut().unwrap() {
        for device in room["devices"].as_array_mut().unwrap() {
            let Some(child) = children.iter().find(|c| device["id"] == c.device) else {
                continue;
            };
            device["kind"] = json!(child.kind);
            device["integration"] = if child.direct {
                // What `resolve_integration` makes of a child: the kind's own
                // commands and actions, never the connection's.
                json!({"via": "plugin", "id": package, "connection_id": connection,
                    "resource_id": child.resource, "child": child.snapshot,
                    "capabilities": [{"id": "on", "label": "on"}, {"id": "off", "label": "off"},
                                     {"id": "toggle", "label": "toggle"}],
                    "actions": [{"action": "set_light"}]})
            } else {
                json!({"via": "connection", "connection_id": connection,
                    "resource_id": child.resource, "child": child.snapshot})
            };
        }
    }
    built(value)
}

/// G: what no configuration may hold. `x:` means nothing to Couch itself, so a
/// step may only send it to a device whose package declares that exact id.
fn refused() -> Vec<(String, Config)> {
    let built_in = |config: &Config| {
        config
            .devices()
            .find(|(_, d)| {
                !matches!(
                    d.integration,
                    Integration::Connection { .. } | Integration::Plugin { .. }
                )
            })
            .map(|(_, d)| d.id.clone())
            .unwrap()
    };
    let packaged = |config: &Config| config.rooms[0].devices[0].id.clone();
    let mut cases = Vec::new();
    for (name, base, on_package, command) in [
        ("a step to a built-in device", "A", false, "x:info"),
        (
            "a step to a package that names other buttons",
            "D",
            true,
            "x:missing",
        ),
        (
            "a step to a package with no buttons of its own",
            "C",
            true,
            "x:info",
        ),
    ] {
        let config = state(base);
        let id = if on_package {
            packaged(&config)
        } else {
            built_in(&config)
        };
        let step = vec![Action::new(id, command)];
        let mut activity = config.clone();
        activity.activities[0].steps = step.clone();
        cases.push((format!("activity: {name}"), activity));
        let mut scene = config;
        scene.scenes[0].steps = step;
        cases.push((format!("scene:    {name}"), scene));
    }
    let mut config = state("A");
    let id = built_in(&config);
    config.activities[0].buttons = vec![Binding {
        button: Button::Red,
        gesture: Gesture::Short,
        action: Some(Action::new(id, "x:info")),
    }];
    cases.push(("key:      bound on a built-in device".into(), config));

    // Children. Each starts from H, which validates, and changes one thing.
    let edited = |edit: &dyn Fn(&mut serde_json::Value)| -> Config {
        let mut value = serde_json::to_value(state("H")).unwrap();
        edit(&mut value);
        built(value)
    };
    // `living-lamp`, the first child in H.
    fn lamp(value: &mut serde_json::Value) -> &mut serde_json::Value {
        &mut value["rooms"][0]["devices"][4]
    }
    cases.push((
        "child:    saved on a connection that is not a package".into(),
        edited(&|value| {
            value["connections"].as_array_mut().unwrap().push(json!({
                "id": "player", "name": "Player",
                "provider": {"kind": "kodi", "host": "kodi.invalid", "port": 9090}}));
            lamp(value)["integration"]["connection_id"] = json!("player");
            lamp(value)["integration"]["resource_id"] = json!("");
        }),
    ));
    cases.push((
        "child:    of a kind the package does not declare".into(),
        edited(&|value| lamp(value)["integration"]["child"]["kind"] = json!("lamp")),
    ));
    cases.push((
        "child:    a scene kind saved as a device".into(),
        edited(&|value| {
            lamp(value)["kind"] = json!("other");
            lamp(value)["integration"]["resource_id"] = json!(SCENE);
            lamp(value)["integration"]["child"] = json!({"kind": "scene"});
        }),
    ));
    cases.push((
        "child:    a device kind other than the one its kind declares".into(),
        edited(&|value| lamp(value)["kind"] = json!("speaker")),
    ));
    cases.push((
        "child:    an id that climbs out of the package's names".into(),
        edited(&|value| lamp(value)["integration"]["resource_id"] = json!("../x")),
    ));
    cases.push((
        "key:      dim: bound to a child that cannot be dimmed".into(),
        edited(&|value| {
            lamp(value)["integration"]["child"]["light"] = json!({"dimmable": false});
        }),
    ));
    cases.push((
        "scene:    a package scene that also has steps".into(),
        edited(&|value| {
            let scene = value["scenes"].as_array_mut().unwrap().last_mut().unwrap();
            scene["steps"] = json!([{"device": "living-lamp", "command": "on"}]);
        }),
    ));
    cases.push((
        "scene:    a package scene of a kind that is not a scene".into(),
        edited(&|value| {
            let scene = value["scenes"].as_array_mut().unwrap().last_mut().unwrap();
            scene["resource"]["kind"] = json!("light");
        }),
    ));
    cases
}

fn open(path: &str, without_v3: bool) -> Config {
    let bytes = fs::read(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let stored: StoredConfig = if without_v3 {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value.as_object_mut().unwrap().remove(ENVELOPE[2].0);
        serde_json::from_value(value)
    } else {
        serde_json::from_slice(&bytes)
    }
    .unwrap_or_else(|e| fail(format!("{path}: does not parse: {e}")));
    let config = stored
        .into_config()
        .unwrap_or_else(|e| fail(format!("{path}: {e}")));
    config
        .validate()
        .unwrap_or_else(|e| fail(format!("{path}: does not validate: {e}")));
    config
}

fn migrated(mut config: Config) -> Config {
    if config.migrate() {
        config
            .validate()
            .unwrap_or_else(|e| fail(format!("does not validate after migrate: {e}")));
    }
    config
}

fn save(path: &str, config: &Config) {
    let mut bytes = serde_json::to_vec_pretty(&StoredConfig::new(config)).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn print(config: &Config) {
    println!("{}", serde_json::to_string_pretty(config).unwrap());
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        // The configuration as it was built, and the file a save produces.
        ["write", name, path] => {
            let config = state(name);
            config
                .validate()
                .unwrap_or_else(|e| fail(format!("state {name} does not validate: {e}")));
            save(path, &config);
        }
        ["show", name] => print(&state(name)),
        ["load", path] => print(&open(path, false)),
        ["load-migrated", path] => print(&migrated(open(path, false))),
        // What this file says once the newest layer is taken away, which is
        // what a Couch that has never heard of that layer is given.
        ["load-without-v3", path] => print(&open(path, true)),
        ["load-without-v3-migrated", path] => print(&migrated(open(path, true))),
        // Start on this file, as the daemon does, then save.
        ["resave", from, to] => save(to, &migrated(open(from, false))),
        // Load and save with nothing in between: the bytes must not move.
        ["rewrite", from, to] => save(to, &open(from, false)),
        ["layers", path] => {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let present: Vec<&str> = ENVELOPE
                .iter()
                .filter(|(key, _)| value.get(key).is_some())
                .map(|(_, name)| *name)
                .collect();
            println!("{}", if present.is_empty() { "plain".into() } else { present.join("+") });
        }
        // Nothing an older Couch reads may mention a package-named button, a
        // child, a package scene or one of the actions protocol 3 adds.
        ["leaks", path] => {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value.as_object_mut().unwrap().remove(ENVELOPE[2].0);
            let visible = serde_json::to_string(&value).unwrap();
            for (needle, what) in [
                ("\"x:", "an x: id"),
                ("\"child\"", "a child snapshot"),
                ("\"children\"", "a package's child kinds"),
                ("\"resource\":{", "a package scene"),
                ("set_light", "the set_light action"),
                ("set_cover", "the set_cover action"),
                ("set_climate", "the set_climate action"),
            ] {
                if visible.contains(needle) {
                    fail(format!("{path}: {what} is visible outside integration_config_v3"));
                }
            }
        }
        ["refuses"] => {
            let mut accepted = 0;
            for (name, config) in refused() {
                match config.validate() {
                    Err(_) => println!("  refused   {name}"),
                    Ok(()) => {
                        accepted += 1;
                        println!("  ACCEPTED  {name}");
                    }
                }
            }
            if accepted > 0 {
                exit(1);
            }
        }
        _ => fail("usage: write|show|load|load-migrated|load-without-v3|load-without-v3-migrated|resave|rewrite|layers|leaks|refuses"),
    }
}
