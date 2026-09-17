//! `couch-denon` as a `couch-sdk` client.
//!
//! This is the adaptation that proves the SDK against a real integration
//! rather than only against the example. Nothing about the AVR transport
//! changed: [`Client`] still owns one socket, still confirms absolute commands
//! by reading the receiver's own report back, and still never retries. What
//! moved is the part that was never Denon-specific - the private settings file
//! is now written by the shared helper, and the function vocabulary is declared
//! once where a test can compare it with the catalog the button picker reads.
//!
//! The dispatch below is the same mapping the device GUI performs inline in
//! `ui/couch-gui/src/activity_buttons.rs`; having it here means a second
//! consumer does not have to reproduce it, and the mute toggle's
//! read-then-write pair stays in one place.

use couch_sdk::{
    couch_model::commands::Function, Capability, ClientSettings, DeviceClient, Selectable, Status,
};

use crate::{Client, Command, Error, Settings};

impl From<Error> for couch_sdk::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(_) => Self::Transport,
            Error::Invalid => Self::Invalid,
            Error::Timeout => Self::Timeout,
            Error::Protocol => Self::Protocol,
            // Not Transport: the receiver is not the thing that failed.
            Error::Storage(e) => Self::Remote(alloc_message(e)),
        }
    }
}

fn alloc_message(_: std::io::Error) -> String {
    "Cannot read or write the AVR connection settings".into()
}

impl ClientSettings for Settings {
    const FILE_PREFIX: &'static str = "denon";

    fn validate(&self) -> couch_sdk::Result<()> {
        Settings::validate(self).map_err(Into::into)
    }
}

impl DeviceClient for Client {
    type Settings = Settings;

    const KIND: &'static str = "denon";
    const LABEL: &'static str = "Denon AVR";

    /// Exactly `couch_model::buttons::functions(&Integration::Denon { .. })`,
    /// and `catalog_matches_the_model` fails if that stops being true.
    ///
    /// No transport keys: the main zone has no notion of play or pause, and
    /// offering them would put a key on screen that silently does nothing.
    fn capabilities() -> &'static [Capability] {
        &[
            ("power-on", "Main zone on"),
            ("power-off", "Main zone off"),
            ("volume-up", "Volume up (0.5 dB)"),
            ("volume-down", "Volume down (0.5 dB)"),
            ("mute", "Toggle mute"),
            ("mute-on", "Mute"),
            ("mute-off", "Unmute"),
        ]
    }

    fn actions() -> &'static [couch_sdk::PluginActionSchema] {
        &[couch_sdk::PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        }]
    }

    fn execute_action(&mut self, action: couch_sdk::TypedAction) -> couch_sdk::Result<()> {
        Self::validate_action(action)?;
        match action {
            couch_sdk::TypedAction::SetVolumeDb { tenths } => {
                Client::command(self, Command::VolumeDb(f32::from(tenths) / 10.0))?;
                Ok(())
            }
        }
    }

    fn connect(settings: &Settings) -> couch_sdk::Result<Self> {
        Client::connect(settings).map_err(Into::into)
    }

    fn execute(&mut self, function: &Function) -> couch_sdk::Result<()> {
        // Toggling is a read and a write, and both belong to this one owner:
        // reading the state from somewhere else would race a second consumer's
        // command between the two halves.
        if *function == Function::Mute {
            let muted = Client::status(self)?
                .muted
                .ok_or(couch_sdk::Error::Protocol)?;
            Client::command(self, Command::Mute(!muted))?;
            return Ok(());
        }
        let command = match function {
            Function::PowerOn => Command::Power(true),
            Function::PowerOff => Command::Power(false),
            Function::VolumeUp => Command::VolumeUp,
            Function::VolumeDown => Command::VolumeDown,
            Function::MuteOn => Command::Mute(true),
            Function::MuteOff => Command::Mute(false),
            Function::Input(id) => Command::Input(id.clone()),
            _ => return Err(couch_sdk::Error::Unsupported),
        };
        Client::command(self, command)?;
        Ok(())
    }

    /// Percentage volume stays absent; preserve the measured dB/minimum state.
    fn status(&mut self) -> couch_sdk::Result<Status> {
        let state = Client::status(self)?;
        let mut status = Status::default();
        status.on = state.on;
        status.muted = state.muted;
        status.input = state.input;
        status.volume_db = if state.volume_minimum {
            Some(couch_sdk::VolumeDb::Minimum)
        } else {
            state.volume_db.map(|db| couch_sdk::VolumeDb::Reading {
                tenths: (db * 10.0).round() as i16,
            })
        };
        Ok(status)
    }

    fn inputs(&mut self) -> couch_sdk::Result<Vec<Selectable>> {
        Ok(Client::sources(self)?
            .into_iter()
            .map(|(id, name)| Selectable::new(id, name))
            .collect())
    }

    /// The receiver's own source tokens. Same rule as
    /// `Function::supports(&Integration::Denon { .. })`, minus the empty
    /// string, which the wire encoder rejects anyway.
    fn supports_input(id: &str) -> bool {
        couch_sdk::couch_model::commands::valid_input_id(id)
            && id.len() <= 25
            && id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b" /+-".contains(&b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_sdk::couch_model::Integration;
    use couch_sdk::testing::{assert_contract, MockHost, Reply, Script};

    fn avr(script: Script) -> MockHost {
        MockHost::start(script.terminator(b'\r'))
    }

    fn settings(host: &MockHost) -> Settings {
        Settings {
            host: host.host().into(),
            port: host.port(),
        }
    }

    #[test]
    fn the_denon_client_keeps_the_contract() {
        let host = avr(Script::new());
        assert_contract::<Client>(&settings(&host), &host);
    }

    #[test]
    fn catalog_matches_the_model_so_the_button_picker_offers_what_this_client_implements() {
        let integration = Integration::Denon {
            host: "avr.local".into(),
            port: crate::DEFAULT_PORT,
        };
        assert_eq!(
            couch_sdk::catalog_differences::<Client>(&integration),
            Vec::<String>::new()
        );
        // And every declared function is one this registered provider accepts.
        for (id, _) in <Client as DeviceClient>::capabilities() {
            let function = Function::parse(id).unwrap();
            assert!(function.supports(&integration), "{id}");
        }
    }

    #[test]
    fn a_mute_toggle_reads_before_it_writes_and_an_undeclared_key_is_never_sent() {
        let host = avr(Script::new()
            .on("ZM?", Reply::line("ZMON"))
            .on("MV?", Reply::line("MV455"))
            .on("MU?", Reply::line("MUOFF"))
            .on("SI?", Reply::line("SIBD"))
            .on("MUON", Reply::line("MUON"))
            .on("MU?", Reply::line("MUON")));
        let mut client = <Client as DeviceClient>::connect(&settings(&host)).unwrap();

        assert_eq!(
            DeviceClient::command(&mut client, "mute"),
            Ok(()),
            "toggling an unmuted receiver mutes it"
        );
        assert!(
            host.requests().contains(&"MU?".to_string())
                && host.requests().contains(&"MUON".to_string()),
            "the toggle must observe the receiver before deciding: {:?}",
            host.requests()
        );

        let before = host.requests().len();
        assert_eq!(
            DeviceClient::command(&mut client, "play-pause"),
            Err(couch_sdk::Error::Unsupported),
            "the main zone has no transport keys"
        );
        assert_eq!(
            DeviceClient::command(&mut client, "input:BD\rMV98"),
            Err(couch_sdk::Error::Unsupported),
            "an input ID that could break the wire protocol is refused here"
        );
        assert_eq!(
            host.requests().len(),
            before,
            "refused commands must cost no round trip"
        );
    }

    #[test]
    fn a_reading_reports_what_the_receiver_said_and_nothing_else() {
        let host = avr(Script::new()
            .on("ZM?", Reply::line("ZMON"))
            .on("MV?", Reply::line("MV455"))
            .on("MU?", Reply::line("MUOFF"))
            .on("SI?", Reply::line("SIBD")));
        let mut client = <Client as DeviceClient>::connect(&settings(&host)).unwrap();
        let status = DeviceClient::status(&mut client).unwrap();
        assert_eq!(status.on, Some(true));
        assert_eq!(status.muted, Some(false));
        assert_eq!(status.input.as_deref(), Some("BD"));
        assert_eq!(
            status.volume, None,
            "a decibel reading must not be reported as a percentage"
        );
    }

    #[test]
    fn a_persisted_denon_input_binding_uses_the_receiver_token_unchanged() {
        let host = avr(Script::new()
            .on("SISAT/CBL", Reply::line("SISAT/CBL"))
            .on("SI?", Reply::line("SISAT/CBL")));
        let mut client = <Client as DeviceClient>::connect(&settings(&host)).unwrap();

        let function = Function::parse("input:SAT/CBL").expect("a persisted input binding");
        assert!(<Client as DeviceClient>::supports(&function));
        assert_eq!(
            DeviceClient::command(&mut client, "input:SAT/CBL"),
            Ok(()),
            "the SDK adapter must preserve a source token accepted by the native AVR client"
        );
        assert_eq!(
            host.requests(),
            ["SISAT/CBL", "SI?"],
            "the input is sent once and confirmed with its own status prefix"
        );
    }

    #[test]
    fn a_silent_receiver_times_out_rather_than_blocking_a_caller() {
        let host = avr(Script::new().otherwise(Reply::Silence));
        let mut client = <Client as DeviceClient>::connect(&settings(&host)).unwrap();
        assert_eq!(
            DeviceClient::command(&mut client, "power-on"),
            Err(couch_sdk::Error::Timeout)
        );
    }
}
