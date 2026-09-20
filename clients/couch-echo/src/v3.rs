//! A protocol 3 fixture. Not a package, and not the template: copy `EchoTv`.
//!
//! Protocol 3 is unreleased and no shipped Couch accepts its manifest, so this
//! exists only behind the `protocol-3-preview` feature, for the tests that run
//! the SDK's `serve` loop end to end with the switch on. It is the same
//! television with three things an older package cannot say:
//!
//! - a button of its own, `x:info`;
//! - the key phase, which it passes to the device so a test can read it back
//!   from the fake: a tap is `CMD volume-up` as ever, anything else is
//!   `CMD volume-up repeat` or `CMD x:info long_press`;
//! - reasons: a refusal carries the device's words, `ERR unpaired` becomes
//!   [`Error::Unpaired`], and a bad port names the `port` setting.

use crate::{EchoTv, Settings};
use couch_sdk::{
    couch_model::commands::{Function, KeyPhase},
    Capability, ClientSettings, DeviceClient, Error, Reason, Result, Selectable, Status,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettingsV3(pub Settings);

impl ClientSettings for SettingsV3 {
    const FILE_PREFIX: &'static str = "echo-v3";

    fn validate(&self) -> Result<()> {
        if self.0.port == 0 {
            return Err(Error::Invalid.because(Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }));
        }
        self.0.validate()
    }
}

pub struct EchoTvV3(EchoTv);

impl DeviceClient for EchoTvV3 {
    type Settings = SettingsV3;

    const KIND: &'static str = "echo-v3";
    const LABEL: &'static str = "Echo TV (protocol 3 fixture)";

    fn capabilities() -> &'static [Capability] {
        &[
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK / select"),
            ("back", "Back"),
            ("home", "Home"),
            ("power-on", "Power on"),
            ("power-off", "Power off"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Toggle mute"),
            ("play-pause", "Play / pause"),
            ("x:info", "Info"),
        ]
    }

    fn connect(settings: &SettingsV3) -> Result<Self> {
        settings.validate()?;
        EchoTv::connect(&settings.0).map(Self)
    }

    fn execute(&mut self, function: &Function) -> Result<()> {
        self.execute_phased(function, KeyPhase::Tap)
    }

    fn execute_phased(&mut self, function: &Function, phase: KeyPhase) -> Result<()> {
        let line = match phase {
            KeyPhase::Tap => format!("CMD {}", function.id()),
            KeyPhase::Repeat => format!("CMD {} repeat", function.id()),
            KeyPhase::LongPress => format!("CMD {} long_press", function.id()),
        };
        let reply = self.0.request(&line)?;
        match reply.as_str() {
            "OK" => Ok(()),
            "ERR unpaired" => Err(Error::Unpaired.because(Reason::Message {
                text: "Pair this TV again".into(),
            })),
            rejected if rejected.starts_with("ERR ") => {
                Err(Error::Rejected.because(Reason::Message {
                    text: rejected[4..].to_string(),
                }))
            }
            _ => Err(Error::Protocol),
        }
    }

    fn status(&mut self) -> Result<Status> {
        self.0.status()
    }

    fn inputs(&mut self) -> Result<Vec<Selectable>> {
        self.0.inputs()
    }

    fn supports_input(id: &str) -> bool {
        EchoTv::supports_input(id)
    }
}

// ---------------------------------------------------------------------------
// A connection with children: the fake bridge.
// ---------------------------------------------------------------------------
//
// Not a package either, and not a template: it is the fixture the host's
// paging, its gate and its limits are proved against, so it is deliberately
// larger than any real one has to be. Seventy lamps, three room groups, five
// scenes, a blind and a thermostat: eighty children, which is three pages.
// Everything is in memory, there is no device to reach, and the answer to a
// write is the state the child is in afterwards, which is what a panel needs
// to draw the slider it has just moved.
//
// `hostile` is how a package that answers nonsense is written down. Each
// setting is one way a bridge can fail to end a listing, and each of them must
// cost the package its child process.

use couch_sdk::{
    couch_model::{PluginCapability, TempUnit},
    Child, ChildPage, ClimateMode, ClimateState, ClimateTraits, CoverState, CoverTraits,
    LightState, LightTraits, PluginActionSchema, PluginChildKind, TypedAction,
};
use std::{collections::BTreeMap, sync::LazyLock};

pub const LAMPS: usize = 70;
pub const GROUPS: usize = 3;
pub const SCENES: usize = 5;
/// Seventy lamps, three groups, five scenes, one blind and one thermostat.
pub const CHILDREN: usize = LAMPS + GROUPS + SCENES + 2;

/// Which way this bridge misbehaves, if it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hostile {
    #[default]
    None,
    /// Every page hands back the cursor it was given: a listing with no end.
    Cycle,
    /// One page with more children than a page may carry.
    Oversized,
    /// A child of a kind the manifest never declared.
    Undeclared,
    /// The second page repeats a child from the first.
    Duplicate,
}
impl Hostile {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "" | "none" => Self::None,
            "cycle" => Self::Cycle,
            "oversized" => Self::Oversized,
            "undeclared" => Self::Undeclared,
            "duplicate" => Self::Duplicate,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BridgeSettings {
    /// Empty, or one of `cycle`, `oversized`, `undeclared`, `duplicate`.
    #[serde(default)]
    pub hostile: String,
}

impl ClientSettings for BridgeSettings {
    const FILE_PREFIX: &'static str = "echo-bridge";

    fn validate(&self) -> Result<()> {
        if Hostile::parse(&self.hostile).is_none() {
            return Err(Error::Invalid.because(Reason::InvalidSetting {
                field: "hostile".into(),
                text: "Not a way this bridge knows how to misbehave".into(),
            }));
        }
        Ok(())
    }
}

fn capability(id: &str, label: &str) -> PluginCapability {
    PluginCapability {
        id: id.into(),
        label: label.into(),
    }
}

/// The kinds of child, exactly as `plugin-bridge-v3.json` declares them.
pub static KINDS: LazyLock<Vec<PluginChildKind>> = LazyLock::new(|| {
    use couch_sdk::couch_model::{ChildComponent, DeviceKind};
    let switchable = || {
        vec![
            capability("on", "On"),
            capability("off", "Off"),
            capability("toggle", "Toggle"),
        ]
    };
    vec![
        PluginChildKind {
            kind: "light".into(),
            label: "Lamp".into(),
            device_kind: DeviceKind::Light,
            component: ChildComponent::Light,
            capabilities: switchable(),
            actions: vec![PluginActionSchema::SetLight {}],
        },
        PluginChildKind {
            kind: "group".into(),
            label: "Room group".into(),
            device_kind: DeviceKind::Light,
            component: ChildComponent::Light,
            capabilities: switchable(),
            actions: vec![PluginActionSchema::SetLight {}],
        },
        PluginChildKind {
            kind: "scene".into(),
            label: "Scene".into(),
            device_kind: DeviceKind::Other,
            component: ChildComponent::Scene,
            capabilities: vec![capability("on", "Recall")],
            actions: vec![],
        },
        PluginChildKind {
            kind: "blind".into(),
            label: "Blind".into(),
            device_kind: DeviceKind::Blind,
            component: ChildComponent::Cover,
            capabilities: vec![
                capability("open", "Open"),
                capability("close", "Close"),
                capability("stop", "Stop"),
            ],
            actions: vec![PluginActionSchema::SetCover {}],
        },
        PluginChildKind {
            kind: "thermostat".into(),
            label: "Thermostat".into(),
            device_kind: DeviceKind::Thermostat,
            component: ChildComponent::Climate,
            capabilities: vec![capability("on", "On"), capability("off", "Off")],
            actions: vec![PluginActionSchema::SetClimate {}],
        },
    ]
});

fn lamp_traits(index: usize) -> LightTraits {
    LightTraits {
        dimmable: index % 7 != 0,
        mirek: (index % 3 == 0).then_some((153, 500)),
        color: index % 5 == 0,
    }
}

fn blind_traits() -> CoverTraits {
    CoverTraits {
        position: true,
        stop: true,
    }
}

fn thermostat_traits() -> ClimateTraits {
    ClimateTraits {
        min_tenths: 70,
        max_tenths: 300,
        step_tenths: 5,
        unit: TempUnit::Celsius,
        modes: vec![
            ClimateMode::Off,
            ClimateMode::Heat,
            ClimateMode::Cool,
            ClimateMode::HeatCool,
        ],
        range: true,
    }
}

/// Every child of the bridge, in the one order it ever lists them.
pub fn catalogue() -> Vec<Child> {
    let mut all = Vec::with_capacity(CHILDREN);
    for index in 0..LAMPS {
        all.push(
            Child::new(format!("lamp-{index:02}"), "light", format!("Lamp {index}"))
                .in_room(["Kitchen", "Study", "Hall"][index % 3])
                .with_light(lamp_traits(index)),
        );
    }
    for index in 0..GROUPS {
        let room = ["Kitchen", "Study", "Hall"][index];
        all.push(
            Child::new(format!("room/{index}"), "group", format!("All of {room}"))
                .in_room(room)
                .with_light(LightTraits {
                    dimmable: true,
                    mirek: Some((153, 500)),
                    color: true,
                }),
        );
    }
    for index in 0..SCENES {
        all.push(Child::new(
            format!("scene/{index}"),
            "scene",
            format!("Scene {index}"),
        ));
    }
    all.push(
        Child::new("blind-1", "blind", "Study blind")
            .in_room("Study")
            .with_cover(blind_traits()),
    );
    all.push(
        Child::new("thermostat-1", "thermostat", "Hall thermostat")
            .in_room("Hall")
            .with_climate(thermostat_traits()),
    );
    all
}

/// What one child is doing, kept in memory: this bridge has no device.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Light(LightState),
    Cover(CoverState),
    Climate(ClimateState),
    /// A scene is never in a state; recalling it is all there is.
    Scene,
}
impl State {
    fn status(&self) -> Status {
        match self {
            Self::Light(light) => Status::default().with_light(*light),
            Self::Cover(cover) => Status::default().with_cover(*cover),
            Self::Climate(climate) => Status::default().with_climate(*climate),
            Self::Scene => Status::default(),
        }
    }
}

/// A bridge with many children and no device behind it.
pub struct EchoBridge {
    hostile: Hostile,
    /// The cursor cycle needs a counter so that a looping listing does not
    /// also repeat an id, which would be caught for the wrong reason.
    looped: usize,
    children: Vec<Child>,
    state: BTreeMap<String, State>,
}

impl EchoBridge {
    fn start(&mut self, id: &str) -> Result<&mut State> {
        let child = self
            .children
            .iter()
            .find(|child| child.id == id)
            .ok_or(Error::Invalid)?;
        let fresh = match (&child.light, &child.cover, &child.climate) {
            (Some(_), _, _) => State::Light(LightState {
                on: Some(false),
                brightness: Some(50),
                mirek: Some(366),
                xy: None,
            }),
            (_, Some(_), _) => State::Cover(CoverState {
                open: Some(false),
                position: Some(0),
            }),
            (_, _, Some(_)) => State::Climate(ClimateState {
                available: true,
                mode: Some(ClimateMode::Heat),
                heating: Some(false),
                current_tenths: Some(195),
                target_tenths: Some(210),
                low_tenths: None,
                high_tenths: None,
            }),
            _ => State::Scene,
        };
        Ok(self.state.entry(id.to_owned()).or_insert(fresh))
    }

    fn hostile_page(&mut self, cursor: Option<&str>) -> Option<ChildPage> {
        match self.hostile {
            Hostile::None => None,
            Hostile::Cycle => {
                self.looped += 1;
                Some(ChildPage {
                    children: vec![Child::new(
                        format!("lamp-loop-{}", self.looped),
                        "light",
                        "Round and round",
                    )
                    .with_light(LightTraits::default())],
                    next: Some("loop".into()),
                })
            }
            Hostile::Oversized => Some(ChildPage {
                children: self
                    .children
                    .iter()
                    .take(couch_sdk::MAX_PAGE + 1)
                    .cloned()
                    .collect(),
                next: None,
            }),
            Hostile::Undeclared => Some(ChildPage {
                children: vec![Child::new("ghost-1", "ghost", "Not a kind it declared")],
                next: None,
            }),
            Hostile::Duplicate => {
                let mut page = ChildPage::fill(self.children.clone(), cursor).ok()?;
                // The second page opens with the first page's first child: one
                // page is still well formed, the listing as a whole is not.
                if cursor.is_some() {
                    page.children.insert(0, self.children[0].clone());
                    page.children.truncate(couch_sdk::MAX_PAGE);
                }
                Some(page)
            }
        }
    }
}

impl DeviceClient for EchoBridge {
    type Settings = BridgeSettings;

    const KIND: &'static str = "echo-bridge";
    const LABEL: &'static str = "Echo bridge (protocol 3 fixture)";

    /// The connection itself does nothing: everything is a child.
    fn capabilities() -> &'static [Capability] {
        &[]
    }

    fn child_kinds() -> &'static [PluginChildKind] {
        &KINDS
    }

    fn connect(settings: &BridgeSettings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            hostile: Hostile::parse(&settings.hostile).ok_or(Error::Invalid)?,
            looped: 0,
            children: catalogue(),
            state: BTreeMap::new(),
        })
    }

    fn execute(&mut self, _function: &Function) -> Result<()> {
        Err(Error::Unsupported)
    }

    fn children(&mut self, cursor: Option<&str>) -> Result<ChildPage> {
        if let Some(page) = self.hostile_page(cursor) {
            return Ok(page);
        }
        ChildPage::fill(self.children.clone(), cursor)
    }

    fn child_status(&mut self, resource: &str) -> Result<Status> {
        Ok(self.start(resource)?.status())
    }

    fn child_command(
        &mut self,
        resource: &str,
        function: &Function,
        _phase: KeyPhase,
    ) -> Result<Option<Status>> {
        let state = self.start(resource)?;
        match (state, function) {
            (State::Scene, Function::On) => return Ok(None),
            (State::Light(light), Function::On) => light.on = Some(true),
            (State::Light(light), Function::Off) => light.on = Some(false),
            (State::Light(light), Function::Toggle) => light.on = Some(light.on != Some(true)),
            (State::Cover(cover), Function::Open) => {
                *cover = CoverState {
                    open: Some(true),
                    position: Some(100),
                }
            }
            (State::Cover(cover), Function::Close) => {
                *cover = CoverState {
                    open: Some(false),
                    position: Some(0),
                }
            }
            (State::Cover(_), Function::Stop) => return Ok(None),
            (State::Climate(climate), Function::On) => climate.mode = Some(ClimateMode::Heat),
            (State::Climate(climate), Function::Off) => climate.mode = Some(ClimateMode::Off),
            _ => return Err(Error::Unsupported),
        }
        // The acknowledgement is the state, so a panel that has just moved a
        // slider does not have to ask again.
        Ok(Some(self.start(resource)?.status()))
    }

    fn child_action(&mut self, resource: &str, action: TypedAction) -> Result<Option<Status>> {
        let state = self.start(resource)?;
        match (state, action) {
            (
                State::Light(light),
                TypedAction::SetLight {
                    on,
                    brightness,
                    mirek,
                    xy,
                },
            ) => {
                // An absent field is left alone; brightness 0 is off.
                if let Some(on) = on {
                    light.on = Some(on);
                }
                if let Some(brightness) = brightness {
                    light.brightness = Some(brightness);
                    light.on = Some(brightness > 0);
                }
                if let Some(mirek) = mirek {
                    light.mirek = Some(mirek);
                    light.xy = None;
                }
                if let Some(xy) = xy {
                    light.xy = Some(xy);
                    light.mirek = None;
                }
            }
            (State::Cover(cover), TypedAction::SetCover { position }) => {
                *cover = CoverState {
                    open: Some(position > 0),
                    position: Some(position),
                }
            }
            (
                State::Climate(climate),
                TypedAction::SetClimate {
                    target_tenths,
                    low_tenths,
                    high_tenths,
                    mode,
                },
            ) => {
                if let Some(target) = target_tenths {
                    climate.target_tenths = Some(target);
                    climate.low_tenths = None;
                    climate.high_tenths = None;
                }
                if low_tenths.is_some() || high_tenths.is_some() {
                    climate.low_tenths = low_tenths;
                    climate.high_tenths = high_tenths;
                    climate.target_tenths = None;
                }
                if let Some(mode) = mode {
                    climate.mode = Some(mode);
                }
                climate.heating = Some(climate.mode != Some(ClimateMode::Off));
            }
            _ => return Err(Error::Unsupported),
        }
        Ok(Some(self.start(resource)?.status()))
    }
}
