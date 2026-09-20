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

// ---------------------------------------------------------------------------
// A television that has to be paired: the pairing fixture.
// ---------------------------------------------------------------------------
//
// Three prompts, one fake device, and the whole of `PairFlow`. The television
// speaks four more lines than the one above:
//
//   -> PAIR button           <- PROMPT button
//   -> PAIR button           <- WAIT
//   -> PAIR button           <- PAIRED {"key":"0f1e2d"}
//   -> PAIR code             <- PROMPT code 4 digits
//   -> PAIR code 0417        <- ERR wrong code
//   -> PAIR cancel           <- OK
//
// `ERR expired` is the device's own window closing, `ERR refused` is a no. The
// key never reaches the settings, the log or the wire in either direction
// except as `Credential`, which prints as `Credential(..)`.

use couch_sdk::{CodeAlphabet, Credential, PairFailure, PairFlow, PairInput, PairPrompt, PairStep};

/// How long Couch waits between two polls of this television. Well inside the
/// bounds the host enforces, so the fixture is never the thing that breaks
/// them; `couch-plugin-echo-pair-hostile` is what does that on purpose.
pub const POLL_MS: u32 = 500;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairingSettings {
    pub host: String,
    #[serde(default = "crate::default_port")]
    pub port: u16,
    /// Which prompt this television asks for: `button`, `approve` or `code`.
    #[serde(default)]
    pub mode: String,
    /// Whether it hands out a new key on the next reading.
    #[serde(default)]
    pub rotate: bool,
}

impl PairingSettings {
    /// The address this television actually answers on. A person may type it
    /// absolutely (`avr.local.`) or in any case; the set is reached either way.
    fn address(&self) -> String {
        self.host.trim_end_matches('.').to_lowercase()
    }
    fn television(&self) -> Settings {
        Settings {
            host: self.address(),
            port: self.port,
            token: String::new(),
        }
    }
    /// The settings this television would rather Couch saved: its own name in
    /// the one spelling it uses. A real package corrects a port, or an address
    /// it was redirected to; this one returns the address it actually reached,
    /// which is enough to prove Couch takes them and re-validates them.
    fn normalised(&self) -> serde_json::Value {
        serde_json::json!({
            "host": self.address(),
            "port": self.port,
            "mode": self.mode,
            "rotate": self.rotate,
        })
    }
}

impl ClientSettings for PairingSettings {
    const FILE_PREFIX: &'static str = "echo-pair";

    fn validate(&self) -> Result<()> {
        if !matches!(self.mode.as_str(), "button" | "approve" | "code") {
            return Err(Error::Invalid.because(Reason::InvalidSetting {
                field: "mode".into(),
                text: "Not a way this television knows how to pair".into(),
            }));
        }
        self.television().validate()
    }
}

/// Come back in half a second, or - for a code - when the person submits it.
fn waiting(prompt: PairPrompt) -> PairStep {
    let after = if prompt.is_code() { 0 } else { POLL_MS };
    PairStep::waiting(prompt, after)
}

/// One pairing conversation with one television.
pub struct EchoPairFlow {
    tv: EchoTv,
    settings: PairingSettings,
    /// The last prompt, so a `WAIT` can repeat it without the device having
    /// to say it again.
    prompt: Option<PairPrompt>,
}

impl EchoPairFlow {
    fn interpret(&mut self, reply: String) -> Result<PairStep> {
        if let Some(rest) = reply.strip_prefix("PROMPT ") {
            let mut words = rest.split_whitespace();
            let prompt = match words.next() {
                Some("button") => {
                    PairPrompt::press_button().saying("The PAIR button is under the screen")
                }
                Some("approve") => {
                    PairPrompt::approve_on_device().saying("Say yes on the television")
                }
                Some("code") => {
                    let length: u8 = words
                        .next()
                        .and_then(|word| word.parse().ok())
                        .ok_or(Error::Protocol)?;
                    let alphabet = match words.next() {
                        Some("digits") => CodeAlphabet::Digits,
                        Some("hex") => CodeAlphabet::Hex,
                        Some("alphanumeric") => CodeAlphabet::Alphanumeric,
                        _ => return Err(Error::Protocol),
                    };
                    PairPrompt::enter_code(length, alphabet).saying("The code is on the screen")
                }
                _ => return Err(Error::Protocol),
            };
            self.prompt = Some(prompt.clone());
            return Ok(waiting(prompt));
        }
        if reply == "WAIT" {
            let prompt = self.prompt.clone().ok_or(Error::Protocol)?;
            return Ok(waiting(prompt));
        }
        if let Some(body) = reply.strip_prefix("PAIRED ") {
            let value: serde_json::Value =
                serde_json::from_str(body).map_err(|_| Error::Protocol)?;
            let credential = Credential::new(value)?;
            return Ok(
                PairStep::done(credential, format!("Paired with {}", self.settings.host))
                    .with_settings(self.settings.normalised()),
            );
        }
        Ok(match reply.as_str() {
            "ERR expired" => {
                PairStep::failed(PairFailure::TimedOut).because("The television stopped waiting")
            }
            "ERR wrong code" => PairStep::failed(PairFailure::WrongCode)
                .because("That was not the code on the screen"),
            "ERR refused" => {
                PairStep::failed(PairFailure::Refused).because("The television said no")
            }
            "ERR unreachable" => PairStep::failed(PairFailure::Unreachable),
            other => match other.strip_prefix("ERR ") {
                Some(text) => PairStep::failed(PairFailure::Refused).because(text),
                None => return Err(Error::Protocol),
            },
        })
    }
}

impl PairFlow for EchoPairFlow {
    fn step(&mut self, input: Option<PairInput>) -> Result<PairStep> {
        let line = match input {
            Some(PairInput::Code { code }) => format!("PAIR code {code}"),
            None => format!("PAIR {}", self.settings.mode),
        };
        let reply = self.tv.request(&line)?;
        self.interpret(reply)
    }

    fn cancel(&mut self) {
        // Best effort, and never an error: nothing is stored either way.
        let _ = self.tv.request("PAIR cancel");
    }
}

/// A television Couch has to be paired with before it will answer.
pub struct EchoPairTv {
    tv: EchoTv,
    /// The key Couch handed over on configure. Without one this television
    /// answers `unpaired`, which is how a case proves the key arrives.
    credential: Option<Credential>,
    /// Whether the next reading hands out a new key, and the one it hands out.
    rotate: bool,
    rotated: Option<Credential>,
}

impl DeviceClient for EchoPairTv {
    type Settings = PairingSettings;

    const KIND: &'static str = "echo-pair";
    const LABEL: &'static str = "Echo TV that pairs (protocol 3 fixture)";

    fn capabilities() -> &'static [Capability] {
        &[("power-on", "Power on"), ("power-off", "Power off")]
    }

    fn connect(settings: &PairingSettings) -> Result<Self> {
        Self::connect_with(settings, None)
    }

    fn connect_with(settings: &PairingSettings, credential: Option<&Credential>) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            tv: EchoTv::connect(&settings.television())?,
            credential: credential.cloned(),
            rotate: settings.rotate,
            rotated: None,
        })
    }

    fn pair_start(
        settings: &PairingSettings,
        _existing: Option<&Credential>,
    ) -> Result<Box<dyn PairFlow>> {
        settings.validate()?;
        Ok(Box::new(EchoPairFlow {
            tv: EchoTv::connect(&settings.television())?,
            settings: settings.clone(),
            prompt: None,
        }))
    }

    fn execute(&mut self, function: &Function) -> Result<()> {
        self.paired()?;
        match self.tv.request(&format!("CMD {}", function.id()))?.as_str() {
            "OK" => Ok(()),
            "ERR unpaired" => Err(Error::Unpaired.because(Reason::Message {
                text: "Pair this television again".into(),
            })),
            _ => Err(Error::Rejected),
        }
    }

    fn status(&mut self) -> Result<Status> {
        self.paired()?;
        let status = self.tv.status()?;
        // The television issued a new key while answering. It is handed to
        // Couch beside this reply and never put in the reply itself, and it
        // is issued once: a package that offered one on every reply would
        // have Couch rewriting the same file for ever.
        if self.rotate {
            self.rotate = false;
            self.rotated = Some(Credential::new(serde_json::json!({"key": "rotated-0002"}))?);
        }
        Ok(status)
    }

    fn take_credential(&mut self) -> Option<Credential> {
        self.rotated.take()
    }
}

impl EchoPairTv {
    fn paired(&self) -> Result<()> {
        if self.credential.is_none() {
            return Err(Error::Unpaired.because(Reason::Message {
                text: "This television has not been paired".into(),
            }));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// A package that answers nonsense: the hostile pairing fixture.
// ---------------------------------------------------------------------------
//
// Written by hand rather than through `couch_plugin::serve`, and that is the
// point. `serve` cannot emit any of this - its constructors clamp a step, it
// mints the session itself and it never attaches a key to a reply that may not
// carry one - so a fixture built on it could not stand in for the adversary
// the host is defending against. A package may be written in any language, and
// this one is the shape of one that was written badly, or maliciously.
//
// Five ways, and the split matters. The host sees the first four in the single
// answer and retires the child itself. The fifth it cannot see at all: a
// package that copies its own key into a reading is leaking it, and only
// someone who knows the key - the harness, the daemon - can tell.

use couch_plugin::{read_frame, write_frame, Credential as WireCredential, MAX_CREDENTIAL_BYTES};
use serde_json::json;

/// Which way this package misbehaves. Named in its `hostile` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostilePairing {
    #[default]
    None,
    /// A key larger than Couch will store.
    Oversized,
    /// A step that names a session nobody asked about.
    WrongSession,
    /// A wait no dialog could draw: poll again in no time at all.
    BadPoll,
    /// A key attached to the reply to `configure`, which may never carry one.
    CredentialOnConfigure,
    /// The key itself, copied into a later reading. The host cannot see this;
    /// the harness can.
    LeaksCredential,
}

impl HostilePairing {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "" | "none" => Self::None,
            "oversized" => Self::Oversized,
            "wrong_session" => Self::WrongSession,
            "bad_poll" => Self::BadPoll,
            "credential_on_configure" => Self::CredentialOnConfigure,
            "leaks_credential" => Self::LeaksCredential,
            _ => return None,
        })
    }
}

/// The key this package issues, so a test can look for it where it must not be.
pub const HOSTILE_KEY: &str = "leaked-0f1e2d";

fn hostile_credential(mode: HostilePairing) -> serde_json::Value {
    match mode {
        // Comfortably over the limit, and still a well formed object.
        HostilePairing::Oversized => json!({"key": "a".repeat(MAX_CREDENTIAL_BYTES)}),
        _ => json!({ "key": HOSTILE_KEY }),
    }
}

/// Serve the hostile fixture: the protocol by hand, over stdin and stdout.
///
/// Returns when the host closes the stream or sends something this fixture
/// does not answer, exactly as `serve` does.
pub fn serve_hostile(manifest_json: &str) -> std::result::Result<(), ()> {
    let manifest: serde_json::Value = serde_json::from_str(manifest_json).map_err(|_| ())?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut mode = HostilePairing::None;
    let mut session = String::new();
    loop {
        let frame: serde_json::Value = read_frame(&mut input).map_err(|_| ())?;
        let id = frame["id"].clone();
        let body = &frame["body"];
        let mut store: Option<serde_json::Value> = None;
        let response = match body["method"].as_str() {
            Some("hello") => json!({"type":"hello","manifest": manifest}),
            Some("configure") => {
                mode = body["settings"]["hostile"]
                    .as_str()
                    .and_then(HostilePairing::parse)
                    .unwrap_or_default();
                if mode == HostilePairing::CredentialOnConfigure {
                    // A key on the one reply that may never carry one. This
                    // also covers a package that simply echoes back the key it
                    // was just configured with.
                    store = Some(hostile_credential(mode));
                }
                json!({"type":"ok"})
            }
            Some("pair_start") => {
                session = "p1".into();
                json!({"type":"pairing","session":"p1","step": first_step(mode)})
            }
            Some("pair_continue") => {
                let named = match mode {
                    // A session nobody is having, which is the whole trick.
                    HostilePairing::WrongSession => "p9".to_owned(),
                    _ => session.clone(),
                };
                json!({"type":"pairing","session": named, "step": json!({
                    "step":"done",
                    "credential": hostile_credential(mode),
                    "summary":"Paired with the hostile set"
                })})
            }
            Some("pair_cancel") => json!({"type":"ok"}),
            Some("status") => {
                let status = if mode == HostilePairing::LeaksCredential {
                    // The key, in a field meant for what is on the screen.
                    json!({"on": true, "title": format!("key={HOSTILE_KEY}")})
                } else {
                    json!({ "on": true })
                };
                json!({"type":"status","status": status})
            }
            Some("command") => json!({"type":"ok"}),
            _ => json!({"type":"error","code":"unsupported"}),
        };
        let mut reply = json!({"id": id, "body": response});
        if let Some(store) = store {
            reply["store_credential"] = store;
        }
        write_frame(&mut output, &reply).map_err(|_| ())?;
    }
}

fn first_step(mode: HostilePairing) -> serde_json::Value {
    match mode {
        // A key too large, straight away.
        HostilePairing::Oversized => json!({
            "step":"done",
            "credential": hostile_credential(mode),
            "summary":"Paired with the hostile set"
        }),
        // Come back in no time at all, for a prompt that resolves by itself.
        HostilePairing::BadPoll => json!({
            "step":"waiting",
            "prompt":{"kind":"press_button"},
            "poll_after_ms":0
        }),
        _ => json!({
            "step":"waiting",
            "prompt":{"kind":"press_button"},
            "poll_after_ms":500
        }),
    }
}

/// So a test can build the very credential this fixture issues.
pub fn hostile_key() -> WireCredential {
    WireCredential::new(json!({ "key": HOSTILE_KEY })).expect("a small credential")
}
