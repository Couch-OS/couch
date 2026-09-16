//! Typed executable functions. Strings exist only at persisted/UI boundaries;
//! execution matches this enum and capability checks use the same vocabulary.
use crate::Integration;
use alloc::{
    format,
    string::{String, ToString},
};
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
}
impl Function {
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(id) = value.strip_prefix("input:").filter(|s| valid_id(s)) {
            return Some(Self::Input(id.into()));
        }
        if let Some(id) = value.strip_prefix("app:").filter(|s| valid_id(s) || crate::valid_app_url(s)) {
            return Some(Self::App(id.into()));
        }
        if let Some(id) = value.strip_prefix("mode:").filter(|s| HVAC_MODES.contains(s)) {
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
        match self {
            Self::Input(id) => match integration {
                Integration::WebOs => valid_id(id),
                // Samsung source keys are fixed; there is no input list to discover.
                Integration::Tizen => TIZEN_INPUTS.contains(&id.as_str()),
                Integration::Denon { .. } => {
                    id.len() <= 25
                        && id.bytes().all(|b| {
                            b.is_ascii_uppercase() || b.is_ascii_digit() || b" /+-".contains(&b)
                        })
                }
                _ => false,
            },
            Self::App(id) => if matches!(integration, Integration::AndroidTv) { crate::valid_app_url(id) } else { matches!(integration, Integration::WebOs | Integration::AppleTv | Integration::Tizen) && valid_id(id) },
            // Not catalog rows: a picker has to collect the number, so the
            // table lives here. Listed only where a client sets the level
            // today, and only for the Home Assistant domain that has it.
            Self::Dim(_) => match integration {
                // Matter carries the level on the endpoint's Level Control
                // cluster; an endpoint without one refuses the command.
                Integration::Hue { .. } | Integration::Matter { .. } => true,
                Integration::HomeAssistant { entity_id } => crate::buttons::ha_domain(entity_id) == "light",
                _ => false,
            },
            Self::Volume(_) => matches!(integration, Integration::Sonos { .. } | Integration::Kodi { .. } | Integration::WebOs),
            Self::Position(_) => matches!(integration, Integration::HomeAssistant { entity_id } if crate::buttons::ha_domain(entity_id) == "cover"),
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
pub const HVAC_MODES: &[&str] = &["off", "heat", "cool", "heat_cool", "auto", "dry", "fan_only"];
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
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-+".contains(&b))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_catalog_entries_round_trip_and_dynamic_functions_are_provider_checked() {
        for integration in [
            Integration::Kodi {
                host: "host".into(),
                port: 9090,
            },
            Integration::WebOs,
            Integration::Denon {
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
            .supports(&Integration::Denon {
                host: "h".into(),
                port: 23
            }));
        assert!(Function::parse("input:BD\rMV98").is_none());
        assert!(Function::parse("arbitrary-rpc").is_none());
    }
    #[test]
    fn thermostat_modes_take_home_assistants_own_tokens_and_nothing_else() {
        let climate = Integration::HomeAssistant { entity_id: "ha-one/climate.office".into() };
        for mode in HVAC_MODES {
            let id = alloc::format!("mode:{mode}");
            let parsed = Function::parse(&id).unwrap();
            assert_eq!(parsed.id(), id);
            assert!(parsed.supports(&climate), "{id}");
        }
        for id in ["mode:", "mode:heat-cool", "mode:HEAT", "mode:eco", "mode:off/on"] {
            assert!(Function::parse(id).is_none(), "{id}");
        }
        // A cover's keys are a cover's; a thermostat gets no open or on.
        let cover = Integration::HomeAssistant { entity_id: "ha-one/cover.office".into() };
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
            assert_eq!(function.supports(&cover), on_cover, "{} on a cover", function.id());
            assert_eq!(function.supports(&climate), on_climate, "{} on a climate", function.id());
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
            "dim:", "dim:101", "volume:101", "position:255", "dim:256", "dim:1000", "dim:-5",
            "dim:+5", "dim:030", "dim: 30", "dim:3 0", "dim:30%", "dim:thirty", "dim", "level:30",
        ] {
            assert!(Function::parse(id).is_none(), "{id}");
        }
    }
    #[test]
    fn levels_are_offered_only_where_a_client_sets_one() {
        let ha = |entity: &str| Integration::HomeAssistant { entity_id: entity.into() };
        for (function, integration, supported) in [
            (Function::Dim(30), Integration::Hue { light_id: "id".into() }, true),
            (Function::Dim(30), ha("light.office"), true),
            (Function::Dim(30), ha("ha-one/light.office"), true),
            (Function::Dim(30), Integration::Matter { device: "matter/7/1".into() }, true),
            (Function::Dim(30), ha("cover.office"), false),
            (Function::Dim(30), ha("climate.office"), false),
            (Function::Dim(30), Integration::WebOs, false),
            (Function::Position(30), ha("cover.office"), true),
            (Function::Position(30), ha("ha-one/cover.office"), true),
            (Function::Position(30), ha("light.office"), false),
            (Function::Position(30), Integration::Sonos { host: "192.0.2.1".into() }, false),
            (Function::Volume(30), Integration::Sonos { host: "192.0.2.1".into() }, true),
            (Function::Volume(30), Integration::Kodi { host: "h".into(), port: 9090 }, true),
            (Function::Volume(30), Integration::WebOs, true),
            // Denon sets volume in dB, not percent, so there is nothing to send.
            (Function::Volume(30), Integration::Denon { host: "h".into(), port: 23 }, false),
            (Function::Volume(30), ha("light.office"), false),
            (Function::Volume(30), Integration::Ir { codeset: "tv".into() }, false),
            // A Matter endpoint is a light: no volume and no cover position.
            (Function::Volume(30), Integration::Matter { device: "matter/7/1".into() }, false),
            (Function::Position(30), Integration::Matter { device: "matter/7/1".into() }, false),
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
}
