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
    SequenceStep, StoredConfig,
};
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
    let plugin = Provider::Plugin {
        id: if denon { "denon" } else { "echo" }.into(),
        label: if denon { "Denon" } else { "Echo" }.into(),
        capabilities: capabilities.clone(),
        supports_inputs: level == 2,
        presentation: presentation.clone(),
        actions: actions.clone(),
    };
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
    config.rooms[0].devices[0].integration = Integration::Connection {
        connection_id: connection.clone(),
        // A converted Denon connection keeps the empty resource it always had.
        resource_id: if denon { "" } else { "zone1" }.into(),
    };
    // The resolved form saved straight on a device takes the same path.
    let direct = config.rooms[0].devices[1].id.clone();
    config.rooms[0].devices[1].integration = Integration::Plugin {
        id: if denon { "denon" } else { "echo" }.into(),
        connection_id: connection,
        resource_id: "zone2".into(),
        capabilities,
        supports_inputs: level == 2,
        presentation,
        actions,
    };

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
        // Nothing an older Couch reads may mention a package-named button.
        ["leaks", path] => {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value.as_object_mut().unwrap().remove(ENVELOPE[2].0);
            if serde_json::to_string(&value).unwrap().contains("\"x:") {
                fail(format!("{path}: an x: id is visible outside integration_config_v3"));
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
