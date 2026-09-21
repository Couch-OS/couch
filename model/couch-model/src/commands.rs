//! Typed executable functions. Strings exist only at persisted/UI boundaries;
//! execution matches this enum and capability checks use the same vocabulary.
use crate::Integration;
use alloc::{
    format,
    string::{String, ToString},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Function {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Home,
    Menu,
    VolumeUp,
    VolumeDown,
    Mute,
    MuteOn,
    MuteOff,
    PowerOn,
    PowerOff,
    ChannelUp,
    ChannelDown,
    Red,
    Green,
    Blue,
    Yellow,
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    Rewind,
    FastForward,
    On,
    Off,
    Toggle,
    Open,
    Close,
    /// One advertised increment of a thermostat's target temperature.
    TemperatureUp,
    TemperatureDown,
    Input(String),
    /// A thermostat's operating mode, `mode:heat_cool`. The value is Home
    /// Assistant's own token, the way `input:` carries the provider's.
    Mode(String),
    App(String),
    /// A percentage, 0..=100: brightness, absolute volume, cover position.
    /// Carried in the id (`dim:30`) because no fixed variant can hold a level.
    Dim(u8),
    Volume(u8),
    Position(u8),
    /// Protocol 3 (unreleased): a button a package names itself, `x:info`,
    /// for the words Couch has none for. Couch never interprets it. It is only
    /// ever supported by a packaged device whose saved capabilities list that
    /// exact id, and it never repeats while held.
    Custom(String),
}
/// How many `x:` ids one package may declare.
pub const MAX_CUSTOM_FUNCTIONS: usize = 32;
/// What kind of key event a command is. A package that speaks protocol 1 or 2
/// is only ever given `Tap`, and `Tap` is never written out, so the bytes those
/// packages read do not change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPhase {
    /// A fresh press.
    #[default]
    Tap,
    /// The key is still down and the panel's key repeat fired.
    Repeat,
    /// The key was held past the long-press threshold.
    LongPress,
}
impl KeyPhase {
    /// For `skip_serializing_if`, hence the reference.
    pub fn is_tap(&self) -> bool {
        *self == Self::Tap
    }
}
impl Function {
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(id) = value.strip_prefix("x:").filter(|s| valid_custom_id(s)) {
            return Some(Self::Custom(id.into()));
        }
        if let Some(id) = value.strip_prefix("input:").filter(|s| valid_input_id(s)) {
            return Some(Self::Input(id.into()));
        }
        if let Some(id) = value
            .strip_prefix("app:")
            .filter(|s| valid_id(s) || crate::valid_app_url(s))
        {
            return Some(Self::App(id.into()));
        }
        if let Some(id) = value
            .strip_prefix("mode:")
            .filter(|s| HVAC_MODES.contains(s))
        {
            return Some(Self::Mode(id.into()));
        }
        if let Some(p) = percent(value, "dim:") {
            return Some(Self::Dim(p));
        }
        if let Some(p) = percent(value, "volume:") {
            return Some(Self::Volume(p));
        }
        if let Some(p) = percent(value, "position:") {
            return Some(Self::Position(p));
        }
        Some(match value {
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "ok" => Self::Ok,
            "back" => Self::Back,
            "home" => Self::Home,
            "menu" => Self::Menu,
            "volume-up" => Self::VolumeUp,
            "volume-down" => Self::VolumeDown,
            "mute" => Self::Mute,
            "mute-on" => Self::MuteOn,
            "mute-off" => Self::MuteOff,
            "power-on" => Self::PowerOn,
            "power-off" => Self::PowerOff,
            "channel-up" => Self::ChannelUp,
            "channel-down" => Self::ChannelDown,
            "red" => Self::Red,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "yellow" => Self::Yellow,
            "play" => Self::Play,
            "pause" => Self::Pause,
            "play-pause" => Self::PlayPause,
            "stop" => Self::Stop,
            "next" => Self::Next,
            "previous" => Self::Previous,
            "rewind" => Self::Rewind,
            "fast-forward" => Self::FastForward,
            "on" => Self::On,
            "off" => Self::Off,
            "toggle" => Self::Toggle,
            "open" => Self::Open,
            "close" => Self::Close,
            "temperature-up" => Self::TemperatureUp,
            "temperature-down" => Self::TemperatureDown,
            _ => return None,
        })
    }
    pub fn id(&self) -> String {
        match self {
            Self::Input(id) => format!("input:{id}"),
            Self::App(id) => format!("app:{id}"),
            Self::Mode(m) => format!("mode:{m}"),
            Self::Dim(p) => format!("dim:{p}"),
            Self::Volume(p) => format!("volume:{p}"),
            Self::Position(p) => format!("position:{p}"),
            Self::Custom(id) => format!("x:{id}"),
            _ => match self {
                Self::Up => "up",
                Self::Down => "down",
                Self::Left => "left",
                Self::Right => "right",
                Self::Ok => "ok",
                Self::Back => "back",
                Self::Home => "home",
                Self::Menu => "menu",
                Self::VolumeUp => "volume-up",
                Self::VolumeDown => "volume-down",
                Self::Mute => "mute",
                Self::MuteOn => "mute-on",
                Self::MuteOff => "mute-off",
                Self::PowerOn => "power-on",
                Self::PowerOff => "power-off",
                Self::ChannelUp => "channel-up",
                Self::ChannelDown => "channel-down",
                Self::Red => "red",
                Self::Green => "green",
                Self::Blue => "blue",
                Self::Yellow => "yellow",
                Self::Play => "play",
                Self::Pause => "pause",
                Self::PlayPause => "play-pause",
                Self::Stop => "stop",
                Self::Next => "next",
                Self::Previous => "previous",
                Self::Rewind => "rewind",
                Self::FastForward => "fast-forward",
                Self::On => "on",
                Self::Off => "off",
                Self::Toggle => "toggle",
                Self::Open => "open",
                Self::Close => "close",
                Self::TemperatureUp => "temperature-up",
                Self::TemperatureDown => "temperature-down",
                _ => unreachable!(),
            }
            .to_string(),
        }
    }
    pub fn supports(&self, integration: &Integration) -> bool {
        if let Integration::Plugin {
            capabilities,
            supports_inputs,
            actions,
            child,
            ..
        } = integration
        {
            if let Self::Input(id) = self {
                return *supports_inputs && valid_input_id(id);
            }
            // Protocol 3 (unreleased). A level sent to a child is a typed
            // action the host makes from it, so what decides is the action its
            // kind declares and what this particular child can do, never a
            // capability spelt `dim:30`.
            if let Some(child) = child {
                let declares = |kind| crate::PluginActionSchema::find(actions, kind).is_some();
                match self {
                    Self::Dim(_) => {
                        return declares(crate::ActionKind::SetLight)
                            && child.light.is_some_and(|traits| traits.dimmable)
                    }
                    Self::Position(_) => {
                        return declares(crate::ActionKind::SetCover)
                            && child.cover.is_some_and(|traits| traits.position)
                    }
                    Self::Mode(mode) => {
                        return declares(crate::ActionKind::SetClimate)
                            && child.climate.as_ref().is_some_and(|traits| {
                                crate::ClimateMode::from_name(mode)
                                    .is_some_and(|mode| traits.modes.contains(&mode))
                            })
                    }
                    _ => {}
                }
            }
            // Protocol 3 (unreleased). A percentage sent to the connection
            // itself is the typed action the one host gate makes from it, so
            // what decides is the declared schema and its bound, never a
            // capability spelt `volume:30`. Additive on purpose: a package
            // may also declare that literal, which the released core accepts,
            // and taking it away here would stop the projection being the
            // identity on a file that core can write.
            if child.is_none() {
                if let Self::Volume(percent) = self {
                    if crate::PluginActionSchema::find(actions, crate::ActionKind::SetVolumePercent)
                        .is_some_and(|schema| {
                            schema
                                .accepts(crate::TypedAction::SetVolumePercent { percent: *percent })
                        })
                    {
                        return true;
                    }
                }
            }
            if matches!(self, Self::Custom(id) if !valid_custom_id(id)) {
                return false;
            }
            let id = self.id();
            return capabilities.iter().any(|capability| capability.id == id);
        }
        match self {
            Self::Input(id) => match integration {
                Integration::WebOs => valid_id(id),
                // Samsung source keys are fixed; there is no input list to discover.
                Integration::Tizen => TIZEN_INPUTS.contains(&id.as_str()),
                Integration::LegacyDenon { .. } => {
                    valid_input_id(id)
                        && id.len() <= 25
                        && id.bytes().all(|b| {
                            b.is_ascii_uppercase() || b.is_ascii_digit() || b" /+-".contains(&b)
                        })
                }
                _ => false,
            },
            Self::App(id) => {
                if matches!(integration, Integration::AndroidTv) {
                    crate::valid_app_url(id)
                } else {
                    matches!(
                        integration,
                        Integration::WebOs | Integration::AppleTv | Integration::Tizen
                    ) && valid_id(id)
                }
            }
            // Not catalog rows: a picker has to collect the number, so the
            // table lives here. Listed only where a client sets the level
            // today, and only for the Home Assistant domain that has it.
            Self::Dim(_) => match integration {
                // Matter carries the level on the endpoint's Level Control
                // cluster; an endpoint without one refuses the command.
                Integration::Hue { .. } | Integration::Matter { .. } => true,
                Integration::HomeAssistant { entity_id } => {
                    crate::buttons::ha_domain(entity_id) == "light"
                }
                _ => false,
            },
            Self::Volume(_) => matches!(
                integration,
                Integration::Sonos { .. } | Integration::Kodi { .. } | Integration::WebOs
            ),
            Self::Position(_) => {
                matches!(integration, Integration::HomeAssistant { entity_id } if crate::buttons::ha_domain(entity_id) == "cover")
            }
            // Only a package can give one of these a meaning, and a package
            // was handled above. No built-in, infrared or Bluetooth catalog
            // may ever answer to it.
            Self::Custom(_) => false,
            _ => crate::buttons::functions(integration)
                .iter()
                .any(|f| f.0 == self.id()),
        }
    }
    /// Configuration capability only; the executor must resolve the exact IR
    /// assignment before transmitting, and otherwise use network support.
    pub fn supports_device(&self, device: &crate::Device, config: &crate::Config) -> bool {
        crate::ALL_TRANSPORTS
            .iter()
            .any(|t| self.supports_transport(device, config, *t))
    }
    /// Whether one of the device's transports can carry this function: the
    /// network integration's own catalog, the IR catalog for a device with a
    /// codeset (the exact code is checked at send time), or the Bluetooth
    /// consumer-control catalog for a bonded device.
    pub fn supports_transport(
        &self,
        device: &crate::Device,
        config: &crate::Config,
        transport: crate::Transport,
    ) -> bool {
        transport
            .marker(device, config)
            .is_some_and(|integration| self.supports(&integration))
    }
    pub fn repeatable(&self) -> bool {
        matches!(
            self,
            Self::Up
                | Self::Down
                | Self::Left
                | Self::Right
                | Self::VolumeUp
                | Self::VolumeDown
                | Self::ChannelUp
                | Self::ChannelDown
        )
    }
}
/// Mirrors `couch_tizen::INPUTS`; kept here so the wasm build stays free of
/// the client crates.
pub const TIZEN_INPUTS: &[&str] = &["tv", "hdmi", "hdmi1", "hdmi2", "hdmi3", "hdmi4"];
/// Mirrors `couch_ha::entities::valid_mode`, for the same reason. A thermostat
/// advertises its own subset; `climate_command` refuses one it does not have.
pub const HVAC_MODES: &[&str] = &[
    "off",
    "heat",
    "cool",
    "heat_cool",
    "auto",
    "dry",
    "fan_only",
];
/// `dim:030` and `dim:+5` are refused so a rendered id parses back to the same
/// value; `dim:` and `dim:101` are not levels at all.
fn percent(value: &str, prefix: &str) -> Option<u8> {
    let digits = value.strip_prefix(prefix)?;
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse::<u8>().ok().filter(|p| *p <= 100)
}
/// Persisted input IDs preserve internal ASCII spaces exactly. Boundary
/// whitespace, controls, delimiters and non-ASCII whitespace are not tokens.
pub fn valid_input_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with(' ')
        && !id.ends_with(' ')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._/-+".contains(&b))
}
/// The part after `x:`: 1 to 48 bytes of lowercase letters, digits, `-`, `_`.
/// Narrower than the other grammars on purpose: the id is never shown, so it
/// needs no capitals or spaces, and one spelling per id keeps the saved
/// capability, the binding and the wire request comparable as plain strings.
pub fn valid_custom_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-+".contains(&b))
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn input_tokens_preserve_internal_spaces_without_expanding_other_grammars() {
        for id in ["SAT/CBL", "HD RADIO", "IPOD DIRECT", "A  B"] {
            let text = alloc::format!("input:{id}");
            let function = Function::parse(&text).unwrap();
            assert_eq!(function.id(), text);
            assert!(function.supports(&Integration::LegacyDenon {
                host: "avr".into(),
                port: 23
            }));
        }
        for id in [
            "",
            " HD RADIO",
            "HD RADIO ",
            "HD\tRADIO",
            "HD\rMV98",
            "HD\nRADIO",
            "HD\u{a0}RADIO",
            "HD:RADIO",
            "HD;RADIO",
        ] {
            assert!(Function::parse(&alloc::format!("input:{id}")).is_none());
        }
        assert!(Function::parse("app:HD RADIO").is_none());
        assert!(Function::parse("input:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_none());
    }

    #[test]
    fn all_catalog_entries_round_trip_and_dynamic_functions_are_provider_checked() {
        for integration in [
            Integration::Kodi {
                host: "host".into(),
                port: 9090,
            },
            Integration::WebOs,
            Integration::LegacyDenon {
                host: "host".into(),
                port: 23,
            },
            Integration::Hue {
                light_id: "id".into(),
            },
            Integration::Matter {
                device: "matter/7/1".into(),
            },
            Integration::HomeAssistant {
                entity_id: "light.test".into(),
            },
            Integration::HomeAssistant {
                entity_id: "ha-one/cover.test".into(),
            },
            Integration::HomeAssistant {
                entity_id: "ha-one/climate.test".into(),
            },
        ] {
            for (id, _) in crate::buttons::functions(&integration) {
                let f = Function::parse(id).unwrap();
                assert_eq!(f.id(), *id);
                assert!(f.supports(&integration));
            }
        }
        assert!(Function::parse("input:HDMI_1")
            .unwrap()
            .supports(&Integration::WebOs));
        assert!(!Function::parse("input:HDMI_1")
            .unwrap()
            .supports(&Integration::LegacyDenon {
                host: "h".into(),
                port: 23
            }));
        assert!(Function::parse("input:BD\rMV98").is_none());
        assert!(Function::parse("arbitrary-rpc").is_none());
    }
    /// A small deterministic generator: the model has no property-testing
    /// dependency and its lockfile is shared by three targets.
    pub(crate) struct Lcg(pub u64);
    impl Lcg {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        pub(crate) fn custom_id(&mut self) -> alloc::string::String {
            const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-_";
            let len = 1 + self.below(48);
            (0..len)
                .map(|_| ALPHABET[self.below(ALPHABET.len())] as char)
                .collect()
        }
    }

    #[test]
    fn package_named_buttons_round_trip_and_nothing_outside_the_grammar_parses() {
        let mut random = Lcg(3);
        let mut lengths = [false; 49];
        for _ in 0..4000 {
            let id = random.custom_id();
            lengths[id.len()] = true;
            let text = alloc::format!("x:{id}");
            let function = Function::parse(&text).unwrap();
            assert_eq!(function, Function::Custom(id.clone()));
            assert_eq!(function.id(), text);
            assert_eq!(Function::parse(&function.id()), Some(function.clone()));
            assert!(!function.repeatable(), "{text}");
            assert!(!crate::buttons::repeatable(&text), "{text}");
        }
        assert!(
            lengths[1] && lengths[48],
            "both ends of the length range ran"
        );
        // Every single byte outside the alphabet, at every position.
        for byte in 0u8..=127 {
            let allowed =
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_';
            for text in [
                alloc::format!("x:{}", byte as char),
                alloc::format!("x:a{}", byte as char),
                alloc::format!("x:{}a", byte as char),
            ] {
                assert_eq!(Function::parse(&text).is_some(), allowed, "{text:?}");
            }
        }
        let longest = alloc::format!("x:{}", "a".repeat(48));
        assert!(Function::parse(&longest).is_some());
        for text in [
            "x:",
            "x",
            "X:info",
            "x:Info",
            "x: info",
            "x:info ",
            "x:in fo",
            "x:in:fo",
            "x:in/fo",
            "x:in.fo",
            "x:inf\u{f6}",
            "x:x:info",
            "xx:info",
            " x:info",
            &alloc::format!("{longest}a"),
        ] {
            assert!(Function::parse(text).is_none(), "{text:?}");
        }
        // The prefix is taken: no other grammar may start to mean something
        // under it, and it cannot shadow a word Couch already has.
        assert_eq!(
            Function::parse("x:volume-up"),
            Some(Function::Custom("volume-up".into()))
        );
        assert_ne!(Function::parse("x:volume-up"), Some(Function::VolumeUp));
    }

    #[test]
    fn a_package_named_button_belongs_only_to_the_package_that_declares_it() {
        let plugin = |ids: &[&str]| Integration::Plugin {
            id: "sample".into(),
            connection_id: "player".into(),
            resource_id: "".into(),
            capabilities: ids
                .iter()
                .map(|id| crate::PluginCapability {
                    id: (*id).into(),
                    label: "Label".into(),
                })
                .collect(),
            supports_inputs: true,
            presentation: alloc::vec![],
            actions: alloc::vec![],
            child: None,
        };
        let info = Function::parse("x:info").unwrap();
        assert!(info.supports(&plugin(&["x:info"])));
        assert!(!info.supports(&plugin(&["x:osd", "info", "menu"])));
        assert!(!info.supports(&plugin(&[])));
        // A hand-built value outside the grammar is refused even if a saved
        // capability list somehow spells the same thing.
        assert!(!Function::Custom("Info".into()).supports(&plugin(&["x:Info"])));
        for integration in [
            Integration::None,
            Integration::Kodi {
                host: "host".into(),
                port: 9090,
            },
            Integration::Sonos {
                host: "192.0.2.1".into(),
            },
            Integration::WebOs,
            Integration::AndroidTv,
            Integration::AppleTv,
            Integration::Tizen,
            Integration::BluetoothTv,
            Integration::LegacyDenon {
                host: "host".into(),
                port: 23,
            },
            Integration::Hue {
                light_id: "id".into(),
            },
            Integration::Matter {
                device: "matter/7/1".into(),
            },
            Integration::HomeAssistant {
                entity_id: "light.test".into(),
            },
            Integration::Ir {
                codeset: "tv".into(),
            },
            Integration::Connection {
                connection_id: "player".into(),
                resource_id: "".into(),
                child: None,
            },
        ] {
            assert!(!info.supports(&integration), "{}", integration.via());
        }
    }

    #[test]
    fn key_phase_is_snake_case_defaults_to_tap_and_a_tap_is_never_written() {
        use serde_json::json;
        for (phase, text) in [
            (KeyPhase::Tap, "tap"),
            (KeyPhase::Repeat, "repeat"),
            (KeyPhase::LongPress, "long_press"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(text));
            assert_eq!(
                serde_json::from_value::<KeyPhase>(json!(text)).unwrap(),
                phase
            );
            assert_eq!(phase.is_tap(), phase == KeyPhase::Tap);
        }
        assert_eq!(KeyPhase::default(), KeyPhase::Tap);
        for text in ["Tap", "long-press", "longpress", "hold", ""] {
            assert!(serde_json::from_value::<KeyPhase>(json!(text)).is_err());
        }
        // The shape the wire request takes in the next pull request.
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Command {
            function: alloc::string::String,
            #[serde(default, skip_serializing_if = "KeyPhase::is_tap")]
            phase: KeyPhase,
        }
        let tap = Command {
            function: "ok".into(),
            phase: KeyPhase::Tap,
        };
        assert_eq!(
            serde_json::to_string(&tap).unwrap(),
            r#"{"function":"ok"}"#,
            "what a protocol 1 or 2 package reads today, byte for byte"
        );
        assert_eq!(
            serde_json::from_str::<Command>(r#"{"function":"ok"}"#).unwrap(),
            tap
        );
        assert_eq!(
            serde_json::to_string(&Command {
                function: "ok".into(),
                phase: KeyPhase::LongPress
            })
            .unwrap(),
            r#"{"function":"ok","phase":"long_press"}"#
        );
    }

    #[test]
    fn thermostat_modes_take_home_assistants_own_tokens_and_nothing_else() {
        let climate = Integration::HomeAssistant {
            entity_id: "ha-one/climate.office".into(),
        };
        for mode in HVAC_MODES {
            let id = alloc::format!("mode:{mode}");
            let parsed = Function::parse(&id).unwrap();
            assert_eq!(parsed.id(), id);
            assert!(parsed.supports(&climate), "{id}");
        }
        for id in [
            "mode:",
            "mode:heat-cool",
            "mode:HEAT",
            "mode:eco",
            "mode:off/on",
        ] {
            assert!(Function::parse(id).is_none(), "{id}");
        }
        // A cover's keys are a cover's; a thermostat gets no open or on.
        let cover = Integration::HomeAssistant {
            entity_id: "ha-one/cover.office".into(),
        };
        for (function, on_cover, on_climate) in [
            (Function::Open, true, false),
            (Function::Close, true, false),
            (Function::Stop, true, false),
            (Function::Position(70), true, false),
            (Function::TemperatureUp, false, true),
            (Function::TemperatureDown, false, true),
            (Function::Mode("heat".into()), false, true),
            (Function::On, false, false),
        ] {
            assert_eq!(
                function.supports(&cover),
                on_cover,
                "{} on a cover",
                function.id()
            );
            assert_eq!(
                function.supports(&climate),
                on_climate,
                "{} on a climate",
                function.id()
            );
        }
    }
    #[test]
    fn levels_round_trip_and_refuse_anything_that_is_not_a_percentage() {
        for (id, expected) in [
            ("dim:0", Function::Dim(0)),
            ("dim:100", Function::Dim(100)),
            ("volume:20", Function::Volume(20)),
            ("position:7", Function::Position(7)),
        ] {
            let parsed = Function::parse(id).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.id(), id);
            assert!(!parsed.repeatable());
        }
        for id in [
            "dim:",
            "dim:101",
            "volume:101",
            "position:255",
            "dim:256",
            "dim:1000",
            "dim:-5",
            "dim:+5",
            "dim:030",
            "dim: 30",
            "dim:3 0",
            "dim:30%",
            "dim:thirty",
            "dim",
            "level:30",
        ] {
            assert!(Function::parse(id).is_none(), "{id}");
        }
    }
    #[test]
    fn levels_are_offered_only_where_a_client_sets_one() {
        let ha = |entity: &str| Integration::HomeAssistant {
            entity_id: entity.into(),
        };
        for (function, integration, supported) in [
            (
                Function::Dim(30),
                Integration::Hue {
                    light_id: "id".into(),
                },
                true,
            ),
            (Function::Dim(30), ha("light.office"), true),
            (Function::Dim(30), ha("ha-one/light.office"), true),
            (
                Function::Dim(30),
                Integration::Matter {
                    device: "matter/7/1".into(),
                },
                true,
            ),
            (Function::Dim(30), ha("cover.office"), false),
            (Function::Dim(30), ha("climate.office"), false),
            (Function::Dim(30), Integration::WebOs, false),
            (Function::Position(30), ha("cover.office"), true),
            (Function::Position(30), ha("ha-one/cover.office"), true),
            (Function::Position(30), ha("light.office"), false),
            (
                Function::Position(30),
                Integration::Sonos {
                    host: "192.0.2.1".into(),
                },
                false,
            ),
            (
                Function::Volume(30),
                Integration::Sonos {
                    host: "192.0.2.1".into(),
                },
                true,
            ),
            (
                Function::Volume(30),
                Integration::Kodi {
                    host: "h".into(),
                    port: 9090,
                },
                true,
            ),
            (Function::Volume(30), Integration::WebOs, true),
            // Denon sets volume in dB, not percent, so there is nothing to send.
            (
                Function::Volume(30),
                Integration::LegacyDenon {
                    host: "h".into(),
                    port: 23,
                },
                false,
            ),
            (Function::Volume(30), ha("light.office"), false),
            (
                Function::Volume(30),
                Integration::Ir {
                    codeset: "tv".into(),
                },
                false,
            ),
            // A Matter endpoint is a light: no volume and no cover position.
            (
                Function::Volume(30),
                Integration::Matter {
                    device: "matter/7/1".into(),
                },
                false,
            ),
            (
                Function::Position(30),
                Integration::Matter {
                    device: "matter/7/1".into(),
                },
                false,
            ),
        ] {
            assert_eq!(
                function.supports(&integration),
                supported,
                "{} on {}",
                function.id(),
                integration.via()
            );
        }
    }

    /// Protocol 3 (unreleased). A percentage sent to a packaged connection is
    /// the typed action the one host gate makes from it, so the declared
    /// schema decides, and its bound decides which numbers may be saved.
    #[test]
    fn a_percentage_reaches_a_package_only_where_it_declares_the_action() {
        use alloc::vec;
        use alloc::vec::Vec;
        let plugin =
            |actions: Vec<crate::PluginActionSchema>,
             capabilities: Vec<crate::PluginCapability>| Integration::Plugin {
                id: "player".into(),
                connection_id: "speaker".into(),
                resource_id: "".into(),
                capabilities,
                supports_inputs: false,
                presentation: vec![],
                actions,
                child: None,
            };
        let named = |id: &str| crate::PluginCapability {
            id: id.into(),
            label: "Named".into(),
        };
        let percent = crate::PluginActionSchema::SetVolumePercent { max_percent: 100 };
        let short = crate::PluginActionSchema::SetVolumePercent { max_percent: 60 };
        let decibels = crate::PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        for (actions, capabilities, command, supported) in [
            (vec![percent], vec![], "volume:30", true),
            (vec![percent], vec![], "volume:0", true),
            (vec![percent], vec![], "volume:100", true),
            // No schema, no percentage: this is what `.188` does everywhere,
            // and what the v2 projection has to leave behind.
            (vec![], vec![], "volume:30", false),
            (vec![decibels], vec![], "volume:30", false),
            // A speaker whose scale stops short refuses the rest of it.
            (vec![short], vec![], "volume:60", true),
            (vec![short], vec![], "volume:61", false),
            // A package may still name the literal as one of its buttons, and
            // then it is an ordinary command that every release accepts.
            (vec![], vec![named("volume:30")], "volume:30", true),
            (vec![], vec![named("volume:30")], "volume:40", false),
            // Nothing else changes: a step key is still a declared capability.
            (vec![percent], vec![], "volume-up", false),
            (vec![percent], vec![named("volume-up")], "volume-up", true),
            (vec![percent], vec![], "dim:30", false),
            (vec![percent], vec![], "position:30", false),
        ] {
            let integration = plugin(actions.clone(), capabilities.clone());
            assert_eq!(
                Function::parse(command).unwrap().supports(&integration),
                supported,
                "{command} with {actions:?} and {capabilities:?}"
            );
        }
        // And the editor offers the level exactly where it can be saved.
        assert_eq!(
            crate::buttons::levels(&plugin(vec![percent], vec![])),
            vec![("volume", "Volume")]
        );
        assert!(crate::buttons::levels(&plugin(vec![], vec![])).is_empty());
        // A level is never a catalog row: the picker has to collect a number
        // first, so nothing here leaks into the list of complete commands.
        assert!(crate::buttons::function_choices(&plugin(vec![percent], vec![])).is_empty());
        assert_eq!(
            crate::buttons::levels(&Integration::HomeAssistant {
                entity_id: "light.office".into()
            }),
            vec![("dim", "Brightness")]
        );
    }
}
