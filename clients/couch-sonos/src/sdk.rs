//! `couch-sonos` as a `couch-sdk` client.
//!
//! Nothing about the Control API transport changed: [`Client`] still reads the
//! household group list before a playback write, still refuses to forward a
//! command to another speaker, and still never retries. What this adds is the
//! part that was never Sonos-specific - the private per-connection settings
//! file written by the shared helper, and the function vocabulary declared once
//! where a test can compare it with the catalog the button picker reads.
//!
//! The dispatch below reuses the closed vocabulary [`Client::command`] already
//! accepts rather than restating it as a second table: the ids in
//! `capabilities()` are exactly the strings that method parses, and
//! `catalog_matches_the_model` is what keeps both equal to `couch-model`.

use std::net::Ipv4Addr;

use couch_sdk::{
    couch_model::commands::Function, Capability, ClientSettings, DeviceClient, Selectable, Status,
};
use serde::{Deserialize, Serialize};

use crate::{Client, Error, SourceId};

impl From<Error> for couch_sdk::Error {
    fn from(e: Error) -> Self {
        match &e {
            Error::Transport => Self::Transport,
            // It answered and the answer did not parse or did not confirm.
            Error::Response | Error::Http(_) => Self::Protocol,
            // The one variant that means the same thing on both sides.
            Error::Command => Self::Unsupported,
            // Everything else carries a message worth showing the person
            // holding the remote, and none of them can contain the API key.
            _ => Self::Remote(e.to_string()),
        }
    }
}

/// What Couch needs to reach one Sonos player.
///
/// The key is optional because it is a household credential rather than a
/// per-player one: a deployment may keep a single `sonos-api-key` file beside
/// `config.json` instead of repeating the same value in every connection. When
/// it is set here it is written to the per-connection file at mode 0600 like
/// any other credential.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The player's IPv4 address. Sonos serves the API on port 1443, so there
    /// is no port to configure.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// An explicit API root, replacing `https://<host>:1443/api/v1`.
    ///
    /// Two callers need it. A household whose players sit behind a reverse
    /// proxy on another port cannot be addressed by IPv4 alone, and an
    /// admission fixture is a loopback HTTP server on an ephemeral port. Both
    /// arrive through the same validated setting, so a fake device needs no
    /// relaxation in the client: [`crate::api_root_ok`] is the shipping rule,
    /// and it confines plain HTTP to loopback.
    ///
    /// `host` stays required even when this is set, because it is the identity
    /// the rest of Couch stores for the player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_root: Option<String>,
}

/// Written by hand so the promise that the key is never logged is structural:
/// a `{:?}` in a future error path cannot print it by accident.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("host", &self.host)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_root", &self.api_root)
            .finish()
    }
}

impl Settings {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            api_key: None,
            api_root: None,
        }
    }
    /// Environment first, then this connection's own settings, then the
    /// household file, then the placeholder. Never logged.
    pub fn key(&self) -> String {
        crate::choose_key([
            crate::key_from_env(),
            self.api_key.clone(),
            std::fs::read_to_string(crate::key_file()).ok(),
        ])
    }
    fn address(&self) -> couch_sdk::Result<Ipv4Addr> {
        self.host
            .trim()
            .parse()
            .map_err(|_| couch_sdk::Error::Invalid)
    }
    /// The API root this connection uses: the configured one if any, otherwise
    /// the pinned TLS root derived from the address.
    fn base(&self) -> couch_sdk::Result<String> {
        match self.api_root.as_deref().map(str::trim) {
            Some(base) if !base.is_empty() => Ok(base.to_owned()),
            _ => Ok(crate::api_root(self.address()?)),
        }
    }
}

impl ClientSettings for Settings {
    const FILE_PREFIX: &'static str = "sonos";

    fn validate(&self) -> couch_sdk::Result<()> {
        self.address()?;
        // A key that cannot be a header value is rejected here rather than at
        // the point of use, where it would become a request that never went out.
        // A blank field is not a configured key: a manifest setting the user
        // left empty arrives as an empty string, and it means the same as absent.
        if self
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|key| !key.is_empty() && !crate::key_ok(key))
        {
            return Err(couch_sdk::Error::Invalid);
        }
        // A root the connection would refuse is a settings mistake, not a
        // transport failure, so it is rejected here rather than at connect.
        if self
            .api_root
            .as_deref()
            .map(str::trim)
            .is_some_and(|base| !base.is_empty() && !crate::api_root_ok(base))
        {
            return Err(couch_sdk::Error::Invalid);
        }
        Ok(())
    }
}

impl DeviceClient for Client {
    type Settings = Settings;

    const KIND: &'static str = "sonos";
    const LABEL: &'static str = "Sonos";

    /// Exactly `couch_model::buttons::functions(&Integration::Sonos { .. })`,
    /// and `catalog_matches_the_model` fails if that stops being true.
    ///
    /// No power keys: a Sonos player has no power state to set or observe, and
    /// offering one would put a key on screen that silently does nothing.
    fn capabilities() -> &'static [Capability] {
        &[
            ("play", "Play"),
            ("pause", "Pause"),
            ("play-pause", "Play / pause"),
            ("stop", "Stop"),
            ("next", "Next"),
            ("previous", "Previous"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Mute"),
            ("mute-on", "Mute on"),
            ("mute-off", "Mute off"),
        ]
    }

    fn connect(settings: &Settings) -> couch_sdk::Result<Self> {
        settings.validate()?;
        Client::connect_url(&settings.base()?, &settings.key()).map_err(Into::into)
    }

    /// One dispatch, not two: the declared ids are the vocabulary
    /// [`Client::command`] parses, including the group check before a playback
    /// write and the read-then-write pair behind a mute toggle.
    ///
    /// `input:<id>` is the one addition. It is not a declared capability - the
    /// household's favourites are discovered, not compiled in - so it is
    /// gated by [`DeviceClient::supports_input`] above and translated back to
    /// the [`SourceId`] the picker offered.
    fn execute(&mut self, function: &Function) -> couch_sdk::Result<()> {
        if let Function::Input(id) = function {
            let source = source_id(id).ok_or(couch_sdk::Error::Unsupported)?;
            return Client::select_source(self, &source).map_err(Into::into);
        }
        let id = function.id();
        if !Self::capabilities().iter().any(|(name, _)| *name == id) {
            return Err(couch_sdk::Error::Unsupported);
        }
        Client::command(self, &id).map_err(Into::into)
    }

    /// `on` stays absent: a player reports no power state, and inventing one
    /// from "it answered" would be a different fact. Playback is the group's;
    /// volume and mute are this player's.
    fn status(&mut self) -> couch_sdk::Result<Status> {
        let state = Client::status(self)?;
        let status = Status::default()
            .with_muted(state.muted)
            .with_volume(state.volume)?;
        Ok(match state.transport.as_str() {
            "PLAYING" | "BUFFERING" => status.with_playing(true),
            "PAUSED" | "IDLE" => status.with_playing(false),
            // An unfamiliar transport state is not an assertion about playback.
            _ => status,
        })
    }

    /// The source picker, as the one list the plugin protocol carries.
    ///
    /// [`Client::sources`] already returns the player's own TV and line-in
    /// inputs plus the household's favourites and Sonos playlists, in the order
    /// the player lists them. Only the ids change shape: `input:<id>` ids are
    /// `couch_model`'s, whose alphabet has no colon, so the source kind is
    /// spelled with a dot.
    ///
    /// The row's `detail` text ("Sonos playlist · 12 tracks") is dropped:
    /// [`Selectable`] carries an id and a name and nothing else. A household id
    /// that cannot be an `input:` function is dropped too, rather than offered
    /// as a row the gate would refuse.
    fn inputs(&mut self) -> couch_sdk::Result<Vec<Selectable>> {
        Ok(Client::sources(self)?
            .into_iter()
            .map(|source| Selectable::new(input_id(&source.id), source.name))
            .filter(|input| offerable(&input.id))
            .collect())
    }

    /// Whether `input:<id>` is one of the four shapes [`input_id`] produces.
    ///
    /// Pure, so an id the household never offered is refused before a socket
    /// opens. Whether *this* player has a TV or line-in input is a different
    /// question, answered by [`Client::select_source`] from the capabilities it
    /// read while connecting.
    fn supports_input(id: &str) -> bool {
        source_id(id).is_some()
    }
}

/// A source's `input:<id>` spelling.
///
/// `couch_model`'s id alphabet is alphanumerics and `._/-+`, so the colon in
/// `favorite:4` cannot survive; the dot is both legal there and legal in a
/// Sonos URL path segment, so one id serves the button mapping and the wire.
fn input_id(source: &SourceId) -> String {
    match source {
        SourceId::HomeTheater => "tv".to_owned(),
        SourceId::LineIn => "line-in".to_owned(),
        SourceId::Favorite(id) => format!("favorite.{id}"),
        SourceId::Playlist(id) => format!("playlist.{id}"),
    }
}

/// Whether a picker row can be pressed at all: the id has to parse as the
/// `input:` function the panel will send, unchanged, and has to be one this
/// client accepts. Both halves, because either alone lets a row onto the screen
/// that does nothing.
fn offerable(id: &str) -> bool {
    Function::parse(&format!("input:{id}")).is_some_and(|f| f == Function::Input(id.to_owned()))
        && source_id(id).is_some()
}

/// [`input_id`] backwards. `None` for anything the picker could not have
/// offered, including a household id that would not survive as a URL segment.
fn source_id(id: &str) -> Option<SourceId> {
    let source = match id {
        "tv" => SourceId::HomeTheater,
        "line-in" => SourceId::LineIn,
        _ => match id.split_once('.') {
            Some(("favorite", item)) => SourceId::Favorite(item.to_owned()),
            Some(("playlist", item)) => SourceId::Playlist(item.to_owned()),
            _ => return None,
        },
    };
    // The household id becomes a path segment. Refuse here what the request
    // builder would refuse later, so the refusal costs nothing.
    match &source {
        SourceId::Favorite(item) | SourceId::Playlist(item) if !crate::segment_ok(item) => None,
        _ => Some(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{
        favorites, groups, info, info_with, playlists, server, volume, KEY, PLAYER,
    };
    use couch_sdk::couch_model::Integration;
    use couch_sdk::testing::{contract_findings, MockHost, Reply, Script};

    fn integration() -> Integration {
        Integration::Sonos {
            host: "192.0.2.10".into(),
        }
    }

    #[test]
    fn catalog_matches_the_model_so_the_button_picker_offers_what_this_client_implements() {
        assert_eq!(
            couch_sdk::catalog_differences::<Client>(&integration()),
            Vec::<String>::new()
        );
        // And every declared function is one this registered provider accepts.
        for (id, _) in <Client as DeviceClient>::capabilities() {
            let function = Function::parse(id).unwrap();
            assert!(function.supports(&integration()), "{id}");
        }
    }

    /// The whole contract, with nothing left over.
    ///
    /// The SDK's mock host is a scripted line protocol and this client speaks
    /// HTTP and JSON, so the checker used to stop at `connect failed`. An
    /// explicit API root points `connect` at the loopback fixture instead, and
    /// the mock host stays on as the observer the checker needs: a refusal that
    /// pinged a device would appear in its log, and the log is empty because
    /// every refusal here is decided before any I/O.
    #[test]
    fn the_contract_holds_completely_against_a_loopback_api_root() {
        let (base, thread) = server(vec![(200, info())]);
        let observer = MockHost::start(Script::new().otherwise(Reply::Close));
        let settings = Settings {
            host: "192.0.2.10".into(),
            api_key: Some("fixture-key".into()),
            api_root: Some(base),
        };
        let findings = contract_findings::<Client>(&settings, &observer);
        assert_eq!(findings, Vec::<String>::new());
        assert!(
            observer.requests().is_empty(),
            "nothing reached the observer: {:?}",
            observer.requests()
        );
        assert_eq!(
            thread.join().unwrap().len(),
            1,
            "only connect's identifying request: a refusal costs no round trip"
        );
    }

    /// The setting that makes a fixture reachable is still a validated setting:
    /// plain HTTP off loopback, a second origin or a trailing slash is a
    /// settings mistake, refused before anything is sent.
    #[test]
    fn an_explicit_api_root_is_validated_and_replaces_the_pinned_tls_root() {
        let mut settings = Settings::new("192.0.2.10");
        assert_eq!(settings.base().unwrap(), "https://192.0.2.10:1443/api/v1");
        for good in [
            "http://127.0.0.1:38211/api/v1",
            "http://localhost:38211/api/v1",
            "https://sonos.example:8443/api/v1",
        ] {
            settings.api_root = Some(good.into());
            assert!(settings.validate().is_ok(), "{good}");
            assert_eq!(settings.base().unwrap(), good, "{good}");
        }
        for bad in [
            "http://192.0.2.10:1443/api/v1",
            "https://192.0.2.10:1443/api/v1/",
            "ws://127.0.0.1/api/v1",
            "https://user@192.0.2.10/api/v1",
            "192.0.2.10:1443",
        ] {
            settings.api_root = Some(bad.into());
            assert_eq!(settings.validate(), Err(couch_sdk::Error::Invalid), "{bad}");
        }
        // Absent and blank both mean "use the player's own TLS root".
        for empty in ["", "  "] {
            settings.api_root = Some(empty.into());
            assert!(settings.validate().is_ok());
            assert_eq!(settings.base().unwrap(), "https://192.0.2.10:1443/api/v1");
        }
    }

    /// The picker's ids survive the trip out to a button mapping and back, and
    /// nothing else is accepted as an input.
    #[test]
    fn source_ids_round_trip_through_the_input_function_vocabulary() {
        for source in [
            SourceId::HomeTheater,
            SourceId::LineIn,
            SourceId::Favorite("4".into()),
            SourceId::Playlist("0".into()),
        ] {
            let id = input_id(&source);
            assert!(
                Function::parse(&format!("input:{id}")) == Some(Function::Input(id.clone()))
                    && <Client as DeviceClient>::supports_input(&id),
                "{id}"
            );
            assert_eq!(source_id(&id), Some(source));
        }
        for refused in [
            "",
            "hdmi1",
            "favorite",
            "favorite.",
            "favorite.../etc",
            "playlist.a/b",
            "station.4",
            "TV",
        ] {
            assert_eq!(source_id(refused), None, "{refused}");
            assert!(
                !<Client as DeviceClient>::supports_input(refused),
                "{refused}"
            );
        }
    }

    /// Two reads, one flat list, and the detail text that cannot travel. The
    /// stock favourites gain one whose household id no `input:` function can
    /// carry, which the picker must drop rather than offer as a dead row.
    #[test]
    fn the_source_picker_becomes_the_one_list_the_plugin_protocol_carries() {
        let mut items: serde_json::Value = serde_json::from_str(&favorites()).unwrap();
        items["items"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"id": "a:b", "name": "Unaddressable", "description": "", "service": {"name": "x"}}
            ));
        let (base, thread) = server(vec![
            (200, info_with(&["PLAYBACK", "HT_PLAYBACK", "LINE_IN"])),
            (200, items.to_string()),
            (200, playlists()),
        ]);
        let mut client = Client::connect_url(&base, KEY).unwrap();
        let inputs = DeviceClient::inputs(&mut client).unwrap();
        assert_eq!(
            inputs
                .iter()
                .map(|input| (input.id.as_str(), input.name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("tv", "TV"),
                ("line-in", "Line-in"),
                ("favorite.4", "Dreaming"),
                ("favorite.6", "Made for Spatial Audio"),
                ("favorite.9", "Hotel Lobby"),
                ("favorite.7", "Radio"),
                ("playlist.1", "All Songs"),
                ("playlist.0", "One"),
                ("playlist.2", "Unknown size"),
            ],
            "the favourite whose household id cannot be an input function is \
             dropped rather than offered as a row that would be refused"
        );
        thread.join().unwrap();
    }

    /// An input the household offered is dispatched as the source it names;
    /// nothing else reaches the player.
    #[test]
    fn selecting_an_input_loads_that_source_into_the_group() {
        let (base, thread) = server(vec![
            (200, info()),
            (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
            (200, "{}".into()),
        ]);
        let mut client = Client::connect_url(&base, KEY).unwrap();
        assert_eq!(
            DeviceClient::command(&mut client, "input:favorite.6"),
            Ok(())
        );
        let sent = thread.join().unwrap();
        assert_eq!(sent.len(), 3);

        // A player without HT_PLAYBACK refuses its own TV row, and the refusal
        // is the player's capability list rather than an invented rule.
        let (base, thread) = server(vec![(200, info())]);
        let mut client = Client::connect_url(&base, KEY).unwrap();
        assert_eq!(
            DeviceClient::command(&mut client, "input:tv"),
            Err(couch_sdk::Error::Unsupported)
        );
        assert_eq!(thread.join().unwrap().len(), 1);
    }

    #[test]
    fn an_undeclared_function_is_refused_before_any_request() {
        let (base, thread) = server(vec![(200, info())]);
        let mut client = Client::connect_url(&base, KEY).unwrap();
        for refused in [
            "power-off",
            "home",
            "wash-the-dishes",
            "volume",
            "app:spotify",
            "input:hdmi1",
        ] {
            assert_eq!(
                DeviceClient::command(&mut client, refused),
                Err(couch_sdk::Error::Unsupported),
                "{refused} is not declared and must be refused before any I/O"
            );
        }
        assert_eq!(
            thread.join().unwrap().len(),
            1,
            "only connect's identifying request: a refusal costs no round trip"
        );
    }

    #[test]
    fn a_refused_playback_command_is_reported_once_and_never_retried() {
        let refusal = r#"{"_objectType":"playbackError","errorCode":"ERROR_PLAYBACK_NO_CONTENT"}"#;
        let (base, thread) = server(vec![
            (200, info()),
            (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
            (499, refusal.into()),
        ]);
        let mut client = Client::connect_url(&base, KEY).unwrap();
        assert_eq!(
            DeviceClient::command(&mut client, "play"),
            Err(couch_sdk::Error::Remote(
                "Sonos API error ERROR_PLAYBACK_NO_CONTENT".into()
            )),
            "the player's own code reaches the person holding the remote"
        );
        assert_eq!(
            thread.join().unwrap().len(),
            3,
            "one topology read and one write, never a second attempt"
        );
    }

    #[test]
    fn a_reading_reports_what_the_group_and_the_player_said() {
        for (state, playing) in [
            ("PLAYBACK_STATE_PLAYING", Some(true)),
            ("PLAYBACK_STATE_BUFFERING", Some(true)),
            ("PLAYBACK_STATE_PAUSED", Some(false)),
            ("PLAYBACK_STATE_IDLE", Some(false)),
            ("PLAYBACK_STATE_INVENTED_BY_NEWER_FIRMWARE", None),
        ] {
            let (base, thread) = server(vec![
                (200, info()),
                (200, groups(PLAYER, state)),
                (200, volume(17, true)),
            ]);
            let mut client = Client::connect_url(&base, KEY).unwrap();
            let status = DeviceClient::status(&mut client).unwrap();
            assert_eq!(status.playing, playing, "{state}");
            assert_eq!(status.muted, Some(true));
            assert_eq!(status.volume, Some(17));
            assert_eq!(status.on, None, "a player has no power state to report");
            assert_eq!(status.input, None);
            thread.join().unwrap();
        }
    }

    #[test]
    fn settings_reject_what_cannot_address_a_player_or_sign_a_request() {
        assert!(Settings::new(" 192.0.2.10 ").validate().is_ok());
        for bad in [
            "",
            "sonos.local",
            "192.0.2.10:1443",
            "192.0.2",
            "2001:db8::1",
            "192.0.2.10 evil",
        ] {
            assert_eq!(
                Settings::new(bad).validate(),
                Err(couch_sdk::Error::Invalid),
                "{bad}"
            );
        }
        let mut settings = Settings::new("192.0.2.10");
        settings.api_key = Some("key\nX-Sonos-Api-Key: other".into());
        assert_eq!(settings.validate(), Err(couch_sdk::Error::Invalid));
    }

    #[test]
    fn debug_output_cannot_print_the_key() {
        let mut settings = Settings::new("192.0.2.10");
        settings.api_key = Some("s3cret-developer-key".into());
        let shown = format!("{settings:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(
            shown.contains("192.0.2.10") && shown.contains("<redacted>"),
            "{shown}"
        );
        assert_eq!(
            format!("{:?}", Settings::new("192.0.2.10")),
            "Settings { host: \"192.0.2.10\", api_key: None, api_root: None }"
        );
    }
    #[test]
    fn a_connection_key_is_preferred_to_the_household_file() {
        if std::env::var_os(crate::KEY_ENV).is_some() {
            return; // An operator override outranks both and would mask this.
        }
        let mut settings = Settings::new("192.0.2.10");
        assert_eq!(
            settings.key(),
            crate::api_key(),
            "without one of its own, a connection uses the household key"
        );
        settings.api_key = Some("per-connection".into());
        assert_eq!(settings.key(), "per-connection");
    }
}
