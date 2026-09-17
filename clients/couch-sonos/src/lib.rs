//! Blocking client for the official Sonos Control API, spoken directly to a
//! player on the local network (HTTPS, port 1443, no cloud gateway and no OAuth).
//! Run it on a worker thread, never the GUI thread. Playback affects the selected
//! coordinator's group; volume and mute address one player.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Players serve the Control API over TLS on 1443; 1400 was the legacy UPnP port.
const PORT: u16 = 1443;
const LIMIT: u64 = 512 * 1024;
/// Artwork is the one thing bigger than a JSON document; a player's proxied
/// cover is a few hundred KB, so anything past this is refused, not decoded.
const ART_LIMIT: u64 = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
const KEY_HEADER: &str = "X-Sonos-Api-Key";
/// One operator override for the whole remote, ahead of every file.
pub const KEY_ENV: &str = "COUCH_SONOS_API_KEY";
/// Moves the household key file, for every process that reads it.
pub const KEY_FILE_ENV: &str = "COUCH_SONOS_API_KEY_FILE";
const SERVICE: &str = "_sonos._tcp.local";
const MDNS: (&str, u16) = ("224.0.0.251", 5353);
/// File name, alongside the other connection settings, holding one API key for
/// the household. Keep real keys out of Git.
///
/// Every reader resolves it the same way: [`KEY_FILE_ENV`] if set, otherwise
/// this name in the directory that holds `config.json`.
pub const KEY_FILE: &str = "sonos-api-key";
/// Stand-in used when no key is configured. Players that allow guest access
/// currently accept any non-empty key; a real developer key from
/// integration.sonos.com replaces this through `COUCH_SONOS_API_KEY` or the
/// `sonos-api-key` file. This constant is not a credential.
pub const PLACEHOLDER_API_KEY: &str = "00000000-0000-4000-8000-000000000000";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Transport,
    Response,
    Unsupported,
    Http(u16),
    Api(String),
    Volume,
    Cancelled,
    Command,
    NotCoordinator { coordinator: String },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport => f.write_str("Cannot reach Sonos within the network timeout"),
            Self::Response => f.write_str("Invalid or oversized Sonos response"),
            Self::Unsupported => {
                f.write_str("Host is not a Sonos player with local playback control")
            }
            Self::Http(code) => write!(f, "Sonos HTTP error {code}"),
            Self::Api(code) => write!(f, "Sonos API error {code}"),
            Self::Cancelled => f.write_str("Sonos command expired before dispatch"),
            Self::Command => f.write_str("Unsupported Sonos command"),
            Self::Volume => f.write_str("Volume must be between 0 and 100"),
            Self::NotCoordinator { coordinator } => write!(
                f,
                "Select group coordinator {coordinator} explicitly for playback"
            ),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Player {
    pub uuid: String,
    pub name: String,
    pub model: String,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Status {
    pub player: Player,
    /// Coordinator player id, comparable with `player.uuid`.
    pub coordinator: String,
    /// Coordinator room name when the household listing supplies one, else its id.
    pub coordinator_name: String,
    /// Group playback state with the `PLAYBACK_STATE_` prefix removed.
    pub transport: String,
    pub volume: u8,
    pub muted: bool,
}
#[derive(Debug, Clone, Copy)]
pub enum Playback {
    Play,
    Pause,
    Stop,
    PlayPause,
    Next,
    Previous,
}

/// Something a group can be told to play from the remote's source picker.
/// Favourite and playlist ids are the household's own, taken from its listing
/// moments earlier; nothing here is typed in by a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceId {
    /// The player's own TV input (home theatre playback).
    HomeTheater,
    /// The player's own analogue line-in.
    LineIn,
    Favorite(String),
    Playlist(String),
}
/// One row of the source picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    /// Where it comes from ("Apple Music", "Sonos playlist · 12 tracks", "This player").
    pub detail: String,
}

/// What the group is playing, as far as the player will say. Every field is
/// optional at the wire; an empty string here means the player did not say.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Track {
    pub name: String,
    pub artist: String,
    pub album: String,
    /// Absolute URL of the artwork, usually served by the player itself on
    /// port 1400 as a proxy for the service's image. Fetch it with [`Client::artwork`].
    pub image_url: String,
    pub duration_ms: Option<u64>,
    pub service: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct NowPlaying {
    /// The playlist, station, queue or input the group is playing from.
    pub container: String,
    pub container_type: String,
    pub current: Option<Track>,
    pub next: Option<Track>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct PlayModes {
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_one: bool,
    pub crossfade: bool,
}
/// A partial change to the play modes: `None` leaves a mode as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayModeChange {
    pub shuffle: Option<bool>,
    pub repeat: Option<bool>,
    pub repeat_one: Option<bool>,
    pub crossfade: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct PlaybackStatus {
    /// Group playback state with the `PLAYBACK_STATE_` prefix removed.
    pub state: String,
    pub position_ms: u64,
    pub modes: PlayModes,
    pub can_seek: bool,
    pub can_skip: bool,
    pub can_skip_back: bool,
}
/// Everything the player screen shows, from one topology read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub status: Status,
    pub playback: PlaybackStatus,
    pub now_playing: NowPlaying,
}

// Wire types. Fields are optional at the parser so a firmware that renames or
// drops one is a refusal rather than a panic, and each field a decision depends
// on is then checked for presence: a defaulted empty string, `0` or `false` is
// not a reading, and must never be mistaken for one.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    model_display_name: String,
    #[serde(default)]
    capabilities: Vec<String>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DiscoveryInfo {
    #[serde(rename = "_objectType", default)]
    object: String,
    #[serde(default)]
    player_id: String,
    #[serde(default)]
    household_id: String,
    #[serde(default)]
    device: DeviceInfo,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Group {
    #[serde(default)]
    id: String,
    #[serde(default)]
    coordinator_id: String,
    #[serde(default)]
    playback_state: String,
    #[serde(default)]
    player_ids: Vec<String>,
}
#[derive(Deserialize, Default)]
struct Named {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}
#[derive(Deserialize, Default)]
struct Service {
    #[serde(default)]
    name: String,
}
#[derive(Deserialize, Default)]
struct Favorite {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    service: Service,
}
#[derive(Deserialize, Default)]
struct Favorites {
    #[serde(default)]
    items: Vec<Favorite>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Playlist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    track_count: Option<u32>,
}
#[derive(Deserialize, Default)]
struct Playlists {
    #[serde(default)]
    playlists: Vec<Playlist>,
}
#[derive(Deserialize, Default)]
struct NamedThing {
    #[serde(default)]
    name: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TrackBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: NamedThing,
    #[serde(default)]
    album: NamedThing,
    #[serde(default)]
    image_url: String,
    #[serde(default)]
    duration_millis: Option<u64>,
    #[serde(default)]
    service: NamedThing,
}
#[derive(Deserialize, Default)]
struct ItemBody {
    #[serde(default)]
    track: Option<TrackBody>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ContainerBody {
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MetadataBody {
    #[serde(default)]
    container: Option<ContainerBody>,
    #[serde(default)]
    current_item: Option<ItemBody>,
    #[serde(default)]
    next_item: Option<ItemBody>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PlayModesBody {
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    repeat_one: bool,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    crossfade: bool,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ActionsBody {
    #[serde(default)]
    can_seek: bool,
    #[serde(default)]
    can_skip: bool,
    #[serde(default)]
    can_skip_back: bool,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PlaybackBody {
    #[serde(default)]
    playback_state: String,
    #[serde(default)]
    position_millis: u64,
    #[serde(default)]
    play_modes: PlayModesBody,
    #[serde(default)]
    available_playback_actions: ActionsBody,
}
#[derive(Deserialize, Default)]
struct Groups {
    #[serde(default)]
    groups: Vec<Group>,
    #[serde(default)]
    players: Vec<Named>,
}
#[derive(Deserialize, Default)]
struct PlayerVolumeBody {
    #[serde(default)]
    volume: Option<u16>,
    #[serde(default)]
    muted: Option<bool>,
}
/// A player volume reading with both fields confirmed present and in range.
struct PlayerVolume {
    level: u8,
    muted: bool,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ApiError {
    #[serde(default)]
    error_code: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CoordinatorChanged {
    #[serde(rename = "_objectType", default)]
    object: String,
    #[serde(default)]
    group_name: String,
    #[serde(default)]
    player_id: String,
}
/// The group this player belongs to, as one household read.
struct Membership {
    id: String,
    coordinator: String,
    coordinator_name: String,
    transport: String,
}

/// A resolved API key, and whether a configured value had to be passed over to
/// reach it. The rejected value is never kept, returned or logged; a caller that
/// wants to tell someone has only the fact that it happened.
pub struct KeyChoice {
    pub key: String,
    pub rejected: bool,
}

/// Read the household API key: environment first, then the settings file, then
/// the placeholder. Never log the result.
pub fn api_key() -> String {
    api_key_choice().key
}
pub fn api_key_at(file: &Path) -> String {
    api_key_choice_at(file).key
}
/// The same resolution, keeping whether a configured key was unusable. A silent
/// fall back to the placeholder looks exactly like "no key is configured" and
/// then fails at the player, which is a long way from the mistake.
pub fn api_key_choice() -> KeyChoice {
    api_key_choice_at(&key_file())
}
pub fn api_key_choice_at(file: &Path) -> KeyChoice {
    resolve_key([key_from_env(), std::fs::read_to_string(file).ok()])
}
fn key_from_env() -> Option<String> {
    std::env::var(KEY_ENV).ok()
}
/// Default key location, mirroring where the GUI keeps connection settings.
/// For a process that already knows the configuration directory - the daemon
/// owns `config.json` - use [`key_file_in`] so both agree.
pub fn key_file() -> PathBuf {
    let root = std::env::var_os("COUCH_HOME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if Path::new("/mnt/alpine/opt/couch").is_dir() {
                "/mnt/alpine/opt/couch"
            } else {
                "/opt/couch"
            })
        });
    key_file_in(&root)
}
/// The household key file for a given configuration directory. The override
/// outranks the directory, so moving the key moves it for every reader rather
/// than only for the ones that guess the same path.
pub fn key_file_in(home: &Path) -> PathBuf {
    match std::env::var_os(KEY_FILE_ENV) {
        Some(path) => PathBuf::from(path),
        None => home.join(KEY_FILE),
    }
}
/// The developer key compiled into a release build, so a shipped remote
/// identifies itself to players without any file on the device. Set
/// `COUCH_SONOS_BUILT_IN_API_KEY` when cargo runs (the build scripts read it
/// from the gitignored `build/sonos-api-key`); the value lives in the binary,
/// never in the source tree. Runtime configuration still outranks it.
pub const BUILT_IN_API_KEY: Option<&str> = option_env!("COUCH_SONOS_BUILT_IN_API_KEY");

/// The first usable key in order of precedence, or the placeholder.
fn choose_key<I: IntoIterator<Item = Option<String>>>(candidates: I) -> String {
    resolve_key(candidates).key
}
fn resolve_key<I: IntoIterator<Item = Option<String>>>(candidates: I) -> KeyChoice {
    resolve_key_with(candidates, BUILT_IN_API_KEY)
}
/// `built_in` is consulted after every configured candidate and before the
/// placeholder; tests pass their own so they do not depend on how the crate was
/// compiled.
fn resolve_key_with<I: IntoIterator<Item = Option<String>>>(
    candidates: I,
    built_in: Option<&str>,
) -> KeyChoice {
    let mut rejected = false;
    for value in candidates
        .into_iter()
        .flatten()
        .chain(built_in.map(str::to_owned))
    {
        let value = value.trim();
        if key_ok(value) {
            return KeyChoice {
                key: value.to_owned(),
                rejected,
            };
        }
        // An unset variable or an empty file is "not configured"; anything else
        // is a value somebody meant to send, and this client will not send it.
        rejected |= !value.is_empty();
    }
    KeyChoice {
        key: PLACEHOLDER_API_KEY.to_owned(),
        rejected,
    }
}
/// A key reaches the wire as a header value: refuse anything that is not
/// printable ASCII rather than letting a stray newline split the request.
fn key_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| Error::Response)
}
fn transport(state: &str) -> String {
    state
        .strip_prefix("PLAYBACK_STATE_")
        .unwrap_or(state)
        .to_owned()
}
/// Device-supplied identifiers become URL path segments; keep them inside the
/// connected origin and out of the query and path-traversal alphabets.
fn segment(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 128
        // Path navigation, not an identifier, whichever alphabet spells it.
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:~".contains(&b))
    {
        return Err(Error::Response);
    }
    Ok(value)
}
/// Whether a device-supplied identifier can be a URL path segment here.
///
/// The same rule [`segment`] applies, exposed so a caller holding an id can
/// refuse it before it becomes a request that was never sent.
pub fn segment_ok(value: &str) -> bool {
    segment(value).is_ok()
}
/// Accept an API root on one origin. Plain HTTP is allowed only for loopback
/// fixtures; a real player is always HTTPS.
fn check_base(base: &str) -> Result<()> {
    let rest = match base.strip_prefix("https://") {
        Some(rest) => rest,
        None => {
            let rest = base.strip_prefix("http://").ok_or(Error::Unsupported)?;
            let host = rest.split(['/', ':']).next().unwrap_or_default();
            if host != "localhost" && !host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback()) {
                return Err(Error::Unsupported);
            }
            rest
        }
    };
    if rest.is_empty()
        || rest.ends_with('/')
        || !rest.is_ascii()
        || rest.contains(['@', '?', '#', ' '])
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}
/// Whether [`Client::connect_url`] will speak to this API root at all.
///
/// The same rule the connection itself applies, exposed so a caller holding a
/// configured root can refuse it while validating settings rather than at the
/// point of use. Plain HTTP stays confined to loopback, which is what makes a
/// local fixture reachable without relaxing anything for a real player: the
/// rule here is shipping behaviour, not a test-only bypass.
pub fn api_root_ok(base: &str) -> bool {
    check_base(base).is_ok()
}
/// Where a player's API lives. A real player is always TLS on 1443; the scheme
/// and port are pinned here so a change to either is one edit and one test.
fn api_root(address: Ipv4Addr) -> String {
    format!("https://{address}:{PORT}/api/v1")
}
fn action(command: Playback) -> &'static str {
    match command {
        Playback::Play => "play",
        // The Control API has no stop; pause is the closest non-destructive match.
        Playback::Pause | Playback::Stop => "pause",
        Playback::PlayPause => "togglePlayPause",
        Playback::Next => "skipToNextTrack",
        Playback::Previous => "skipToPreviousTrack",
    }
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
    key: String,
    player: Player,
    /// The player's advertised capabilities, which gate the source picker's
    /// TV and line-in rows; never an authority for volume or playback.
    capabilities: Vec<String>,
}
impl Client {
    pub fn connect(address: Ipv4Addr) -> Result<Self> {
        Self::connect_with_key(address, &api_key())
    }
    pub fn connect_with_key(address: Ipv4Addr, key: &str) -> Result<Self> {
        Self::connect_url(&api_root(address), key)
    }
    /// Connect to an explicit API root, e.g. `https://192.0.2.10:1443/api/v1`.
    ///
    /// Player certificates are leaves issued by the Sonos device CA, which is in
    /// no system trust store and is not sent in the chain, so peer verification
    /// is disabled: this is the same trust level as the legacy plain-HTTP
    /// protocol, namely a trusted LAN and no peer authentication.
    pub fn connect_url(base: &str, key: &str) -> Result<Self> {
        check_base(base)?;
        // Refuse a key we cannot put in a header rather than letting a stray
        // newline split the request; the player reports the same code itself.
        let key = key.trim().to_owned();
        if !key_ok(&key) {
            return Err(Error::Api("ERROR_API_KEY_VALIDATION_FAILED".into()));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            )
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        let client = Self {
            base: base.to_owned(),
            agent,
            key,
            player: Player {
                uuid: String::new(),
                name: String::new(),
                model: String::new(),
            },
            capabilities: Vec::new(),
        };
        let info: DiscoveryInfo = json(&client.request("/players/local/info", None)?)?;
        if info.object != "discoveryInfo"
            || info.household_id.is_empty()
            || info.device.name.is_empty()
            || !info.device.capabilities.iter().any(|c| c == "PLAYBACK")
        {
            return Err(Error::Unsupported);
        }
        let uuid = segment(&info.player_id)
            .map_err(|_| Error::Unsupported)?
            .to_owned();
        let model = if info.device.model_display_name.is_empty() {
            info.device.model
        } else {
            info.device.model_display_name
        };
        Ok(Self {
            player: Player {
                uuid,
                name: info.device.name,
                model,
            },
            capabilities: info.device.capabilities,
            ..client
        })
    }
    pub fn player(&self) -> &Player {
        &self.player
    }
    fn capable(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
    fn request(&self, path: &str, body: Option<&str>) -> Result<String> {
        let url = format!("{}{path}", self.base);
        let sent = match body {
            Some(body) => self
                .agent
                .post(&url)
                .header(KEY_HEADER, &self.key)
                .header("Content-Type", "application/json")
                .send(body),
            None => self.agent.get(&url).header(KEY_HEADER, &self.key).call(),
        };
        let mut response = sent.map_err(|_| Error::Transport)?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(LIMIT)
            .read_to_string()
            .map_err(|_| Error::Response)?;
        if (200..300).contains(&status) {
            return Ok(text);
        }
        // The group moved to another coordinator between the topology read and
        // this request; report it like any other member refusal.
        if let Ok(moved) = serde_json::from_str::<CoordinatorChanged>(&text) {
            if moved.object == "groupCoordinatorChanged" {
                let coordinator = if moved.group_name.is_empty() {
                    moved.player_id
                } else {
                    moved.group_name
                };
                if !coordinator.is_empty() {
                    return Err(Error::NotCoordinator { coordinator });
                }
            }
        }
        match serde_json::from_str::<ApiError>(&text) {
            Ok(error) if !error.error_code.is_empty() => Err(Error::Api(error.error_code)),
            _ => Err(Error::Http(status)),
        }
    }
    /// One household read: the group holding this player, its coordinator and
    /// that group's playback state.
    fn membership(&self) -> Result<Membership> {
        let body = self.request("/households/local/groups", None)?;
        let groups: Groups = json(&body)?;
        let mut matching = groups
            .groups
            .iter()
            .filter(|g| g.player_ids.contains(&self.player.uuid));
        let group = matching.next().ok_or(Error::Response)?;
        // Ambiguous topology fails closed rather than guessing a target group.
        if matching.next().is_some() || group.coordinator_id.is_empty() {
            return Err(Error::Response);
        }
        let coordinator_name = groups
            .players
            .iter()
            .find(|p| p.id == group.coordinator_id && !p.name.is_empty())
            .map(|p| p.name.clone())
            .unwrap_or_else(|| group.coordinator_id.clone());
        Ok(Membership {
            id: segment(&group.id)?.to_owned(),
            coordinator: group.coordinator_id.clone(),
            coordinator_name,
            transport: transport(&group.playback_state),
        })
    }
    pub fn coordinator(&self) -> Result<String> {
        Ok(self.membership()?.coordinator)
    }
    pub fn status(&self) -> Result<Status> {
        let group = self.membership()?;
        let volume = self.player_volume()?;
        Ok(Status {
            player: self.player.clone(),
            coordinator: group.coordinator,
            coordinator_name: group.coordinator_name,
            transport: group.transport,
            volume: volume.level,
            muted: volume.muted,
        })
    }
    pub fn playback(&self, command: Playback) -> Result<()> {
        self.playback_if_current(command, &|| true)
    }
    fn playback_if_current(&self, command: Playback, current: &dyn Fn() -> bool) -> Result<()> {
        let group = self.membership()?;
        if group.coordinator != self.player.uuid {
            return Err(Error::NotCoordinator {
                coordinator: group.coordinator_name,
            });
        }
        if !current() {
            return Err(Error::Cancelled);
        }
        self.request(
            &format!("/groups/{}/playback/{}", group.id, action(command)),
            Some("{}"),
        )
        .map(|_| ())
    }
    /// Execute a closed button vocabulary, checking freshness after preparatory
    /// reads. A sent command cannot be recalled. This method never retries a write.
    pub fn command_if_current(&self, command: &str, current: &dyn Fn() -> bool) -> Result<()> {
        if !current() {
            return Err(Error::Cancelled);
        }
        let playback = match command {
            "play" => Some(Playback::Play),
            "pause" => Some(Playback::Pause),
            "stop" => Some(Playback::Stop),
            "play-pause" => Some(Playback::PlayPause),
            "next" => Some(Playback::Next),
            "previous" => Some(Playback::Previous),
            _ => None,
        };
        if let Some(command) = playback {
            return self.playback_if_current(command, current);
        }
        match command {
            // One relative write: no read, so nothing can go stale in between.
            "volume-up" | "volume-down" => {
                self.nudge_volume(if command == "volume-up" { 1 } else { -1 })
            }
            "mute" | "mute-on" | "mute-off" => {
                let muted = if command == "mute" {
                    !self.player_volume()?.muted
                } else {
                    command == "mute-on"
                };
                if !current() {
                    return Err(Error::Cancelled);
                }
                self.set_muted(muted)
            }
            _ => Err(Error::Command),
        }
    }
    pub fn command(&self, command: &str) -> Result<()> {
        self.command_if_current(command, &|| true)
    }
    /// What the source picker offers for this player: its own TV and line-in
    /// inputs when the hardware has them, then the household's favourites and
    /// Sonos playlists in the order the player lists them. Two reads, no writes.
    pub fn sources(&self) -> Result<Vec<Source>> {
        let mut sources = Vec::new();
        if self.capable("HT_PLAYBACK") {
            sources.push(Source {
                id: SourceId::HomeTheater,
                name: "TV".into(),
                detail: format!("{} input", self.player.name),
            });
        }
        if self.capable("LINE_IN") {
            sources.push(Source {
                id: SourceId::LineIn,
                name: "Line-in".into(),
                detail: format!("{} input", self.player.name),
            });
        }
        let favorites: Favorites = json(&self.request("/households/local/favorites", None)?)?;
        for item in favorites.items {
            if item.id.is_empty() || item.name.is_empty() {
                continue;
            }
            let detail = match (item.description.is_empty(), item.service.name.is_empty()) {
                (false, false) if item.description != item.service.name => {
                    format!("{} · {}", item.description, item.service.name)
                }
                (false, _) => item.description,
                (true, false) => item.service.name,
                (true, true) => "Sonos favourite".into(),
            };
            sources.push(Source {
                id: SourceId::Favorite(item.id),
                name: item.name,
                detail,
            });
        }
        let playlists: Playlists = json(&self.request("/households/local/playlists", None)?)?;
        for item in playlists.playlists {
            if item.id.is_empty() || item.name.is_empty() {
                continue;
            }
            sources.push(Source {
                id: SourceId::Playlist(item.id),
                name: item.name,
                detail: match item.track_count {
                    Some(1) => "Sonos playlist · 1 track".into(),
                    Some(n) => format!("Sonos playlist · {n} tracks"),
                    None => "Sonos playlist".into(),
                },
            });
        }
        Ok(sources)
    }
    pub fn select_source(&self, source: &SourceId) -> Result<()> {
        self.select_source_if_current(source, &|| true)
    }
    /// Start playing a source. TV is the player's own and needs no topology;
    /// everything else loads into the group, so like playback it is refused on
    /// a member and checked for freshness after the topology read.
    pub fn select_source_if_current(
        &self,
        source: &SourceId,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !current() {
            return Err(Error::Cancelled);
        }
        if *source == SourceId::HomeTheater {
            if !self.capable("HT_PLAYBACK") {
                return Err(Error::Command);
            }
            return self
                .request(
                    &format!("/players/{}/homeTheater", self.player.uuid),
                    Some("{}"),
                )
                .map(|_| ());
        }
        if *source == SourceId::LineIn && !self.capable("LINE_IN") {
            return Err(Error::Command);
        }
        let group = self.membership()?;
        if group.coordinator != self.player.uuid {
            return Err(Error::NotCoordinator {
                coordinator: group.coordinator_name,
            });
        }
        if !current() {
            return Err(Error::Cancelled);
        }
        let (path, body) = match source {
            SourceId::LineIn => (
                "playback/lineIn".to_owned(),
                serde_json::json!({ "deviceId": self.player.uuid, "playOnCompletion": true }),
            ),
            SourceId::Favorite(id) => (
                "favorites".to_owned(),
                serde_json::json!({ "favoriteId": id, "playOnCompletion": true }),
            ),
            SourceId::Playlist(id) => (
                "playlists".to_owned(),
                serde_json::json!({ "playlistId": id, "playOnCompletion": true }),
            ),
            SourceId::HomeTheater => unreachable!("handled above"),
        };
        self.request(
            &format!("/groups/{}/{path}", group.id),
            Some(&body.to_string()),
        )
        .map(|_| ())
    }
    /// The group's playback status: state, position, play modes and which
    /// transport actions the player says are available right now.
    pub fn playback_status(&self) -> Result<PlaybackStatus> {
        let group = self.membership()?;
        self.playback_in(&group.id)
    }
    fn playback_in(&self, group: &str) -> Result<PlaybackStatus> {
        let body: PlaybackBody = json(&self.request(&format!("/groups/{group}/playback"), None)?)?;
        Ok(PlaybackStatus {
            state: transport(&body.playback_state),
            position_ms: body.position_millis,
            modes: PlayModes {
                shuffle: body.play_modes.shuffle,
                repeat: body.play_modes.repeat,
                repeat_one: body.play_modes.repeat_one,
                crossfade: body.play_modes.crossfade,
            },
            can_seek: body.available_playback_actions.can_seek,
            can_skip: body.available_playback_actions.can_skip,
            can_skip_back: body.available_playback_actions.can_skip_back,
        })
    }
    /// What the group is playing: container, current track and next track.
    /// TV, line-in and some streams report no track; the container then says
    /// what is on.
    pub fn now_playing(&self) -> Result<NowPlaying> {
        let group = self.membership()?;
        self.now_playing_in(&group.id)
    }
    fn now_playing_in(&self, group: &str) -> Result<NowPlaying> {
        let body: MetadataBody =
            json(&self.request(&format!("/groups/{group}/playbackMetadata"), None)?)?;
        let track = |item: Option<ItemBody>| {
            item.and_then(|i| i.track)
                .filter(|t| !t.name.is_empty())
                .map(|t| Track {
                    name: t.name,
                    artist: t.artist.name,
                    album: t.album.name,
                    image_url: t.image_url,
                    duration_ms: t.duration_millis.filter(|d| *d > 0),
                    service: t.service.name,
                })
        };
        let container = body.container.unwrap_or_default();
        Ok(NowPlaying {
            container: container.name,
            container_type: container.kind,
            current: track(body.current_item),
            next: track(body.next_item),
        })
    }
    /// One topology read, then the group's playback, its metadata and this
    /// player's volume: the player screen's whole picture in four requests.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let group = self.membership()?;
        let playback = self.playback_in(&group.id)?;
        let now_playing = self.now_playing_in(&group.id)?;
        let volume = self.player_volume()?;
        Ok(Snapshot {
            status: Status {
                player: self.player.clone(),
                coordinator: group.coordinator,
                coordinator_name: group.coordinator_name,
                transport: group.transport,
                volume: volume.level,
                muted: volume.muted,
            },
            playback,
            now_playing,
        })
    }
    pub fn seek(&self, position_ms: u64) -> Result<()> {
        self.seek_if_current(position_ms, &|| true)
    }
    /// Seek within the current track. A group write, so refused on a member
    /// and checked for freshness after the topology read, like playback.
    pub fn seek_if_current(&self, position_ms: u64, current: &dyn Fn() -> bool) -> Result<()> {
        let group = self.coordinated_group(current)?;
        self.request(
            &format!("/groups/{}/playback/seek", group.id),
            Some(&serde_json::json!({ "positionMillis": position_ms }).to_string()),
        )
        .map(|_| ())
    }
    pub fn set_play_modes(&self, change: PlayModeChange) -> Result<()> {
        self.set_play_modes_if_current(change, &|| true)
    }
    /// Change shuffle, repeat, repeat-one or crossfade; unset fields stay as
    /// they are. A group write like the others.
    pub fn set_play_modes_if_current(
        &self,
        change: PlayModeChange,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        let mut modes = serde_json::Map::new();
        for (key, value) in [
            ("shuffle", change.shuffle),
            ("repeat", change.repeat),
            ("repeatOne", change.repeat_one),
            ("crossfade", change.crossfade),
        ] {
            if let Some(value) = value {
                modes.insert(key.into(), serde_json::Value::Bool(value));
            }
        }
        if modes.is_empty() {
            return Err(Error::Command);
        }
        let group = self.coordinated_group(current)?;
        self.request(
            &format!("/groups/{}/playback/playMode", group.id),
            Some(&serde_json::json!({ "playModes": modes }).to_string()),
        )
        .map(|_| ())
    }
    /// The group, once this player is confirmed to coordinate it and the
    /// command is still wanted.
    fn coordinated_group(&self, current: &dyn Fn() -> bool) -> Result<Membership> {
        if !current() {
            return Err(Error::Cancelled);
        }
        let group = self.membership()?;
        if group.coordinator != self.player.uuid {
            return Err(Error::NotCoordinator {
                coordinator: group.coordinator_name,
            });
        }
        if !current() {
            return Err(Error::Cancelled);
        }
        Ok(group)
    }
    /// Fetch a track's artwork by the absolute URL the player gave. The
    /// player's own proxy on port 1400 is plain HTTP; a service's own image
    /// host is HTTPS with normal certificate checks. No API key travels with
    /// it, the size is bounded, and redirects are not followed off the host.
    pub fn artwork(&self, url: &str) -> Result<Vec<u8>> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::Unsupported);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(2)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        let mut response = agent.get(url).call().map_err(|_| Error::Transport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::Http(status));
        }
        response
            .body_mut()
            .with_config()
            .limit(ART_LIMIT)
            .read_to_vec()
            .map_err(|_| Error::Response)
    }
    /// A reading missing either field is a failure, not a zero volume and an
    /// unmuted speaker: a mute toggle decides its write from `muted`, and
    /// reporting 0 for "did not say" would invite someone to turn it up.
    fn player_volume(&self) -> Result<PlayerVolume> {
        let path = format!("/players/{}/playerVolume", self.player.uuid);
        let body: PlayerVolumeBody = json(&self.request(&path, None)?)?;
        let (Some(level), Some(muted)) = (body.volume, body.muted) else {
            return Err(Error::Response);
        };
        if level > 100 {
            return Err(Error::Response);
        }
        Ok(PlayerVolume {
            level: level as u8,
            muted,
        })
    }
    pub fn volume(&self) -> Result<u8> {
        Ok(self.player_volume()?.level)
    }
    /// Level and mute in one read, for feedback after a volume or mute write.
    pub fn volume_state(&self) -> Result<(u8, bool)> {
        let v = self.player_volume()?;
        Ok((v.level, v.muted))
    }
    pub fn muted(&self) -> Result<bool> {
        Ok(self.player_volume()?.muted)
    }
    /// The playerVolume commands bind to the object itself (setVolume) and to
    /// `/relative` and `/mute`; the command names are the websocket spelling.
    fn volume_write(&self, command: &str, body: serde_json::Value) -> Result<()> {
        self.request(
            &format!("/players/{}/playerVolume{command}", self.player.uuid),
            Some(&body.to_string()),
        )
        .map(|_| ())
    }
    pub fn set_volume(&self, volume: u8) -> Result<()> {
        if volume > 100 {
            return Err(Error::Volume);
        }
        self.volume_write("", serde_json::json!({ "volume": volume }))
    }
    pub fn nudge_volume(&self, delta: i8) -> Result<()> {
        self.volume_write("/relative", serde_json::json!({ "volumeDelta": delta }))
    }
    pub fn set_muted(&self, muted: bool) -> Result<()> {
        self.volume_write("/mute", serde_json::json!({ "muted": muted }))
    }
}

/// Discover IPv4 responders in at most three seconds with one multicast DNS
/// query for `_sonos._tcp.local`. Results are untrusted candidates; `connect`
/// verifies each one. The advertised TXT `location` URL is never fetched.
pub fn discover() -> Result<Vec<Ipv4Addr>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| Error::Transport)?;
    socket.set_multicast_ttl_v4(255).ok();
    socket
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|_| Error::Transport)?;
    socket
        .send_to(&query(SERVICE), MDNS)
        .map_err(|_| Error::Transport)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = BTreeSet::new();
    let mut buffer = [0; 9000];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if left.is_zero() {
            break;
        }
        socket
            .set_read_timeout(Some(left))
            .map_err(|_| Error::Transport)?;
        match socket.recv_from(&mut buffer) {
            Ok((len, SocketAddr::V4(peer))) => {
                for address in addresses(&buffer[..len], *peer.ip()) {
                    found.insert(address);
                }
                if found.len() >= 256 {
                    break;
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                break
            }
            Err(_) => return Err(Error::Transport),
        }
    }
    Ok(found.into_iter().take(256).collect())
}
/// One PTR question. Queries from a port other than 5353 are answered by unicast,
/// so a plain unbound socket receives the replies.
fn query(service: &str) -> Vec<u8> {
    let mut packet = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in service.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.extend_from_slice(&[0, 0, 12, 0, 1]);
    packet
}
/// Decompress one name. A pointer must point backwards, but that does not bound
/// the walk on its own - a backwards pointer can land on a label that runs
/// forward into the same pointer again - so the hop budget below and the
/// 255-byte name cap are what end it. The caller continues after the first
/// pointer.
fn read_name(message: &[u8], start: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut pos = start;
    let mut after = None;
    for _ in 0..128 {
        let length = *message.get(pos)? as usize;
        match length & 0xc0 {
            0 => {
                pos += 1;
                if length == 0 {
                    return Some((name, after.unwrap_or(pos)));
                }
                let label = message.get(pos..pos + length)?;
                if name.len() + length > 255 {
                    return None;
                }
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&String::from_utf8_lossy(label).to_ascii_lowercase());
                pos += length;
            }
            0xc0 => {
                let target = ((length & 0x3f) << 8) | *message.get(pos + 1)? as usize;
                after.get_or_insert(pos + 2);
                if target >= pos {
                    return None;
                }
                pos = target;
            }
            _ => return None,
        }
    }
    None
}
/// Owner name, record type and the bounds of the record data, for every record
/// in a response. Lengths are checked against the message before use.
fn records(message: &[u8]) -> Option<Vec<(String, u16, usize, usize)>> {
    let count =
        |i: usize| Some(u16::from_be_bytes([*message.get(i)?, *message.get(i + 1)?]) as usize);
    if message.len() < 12 || (count(2)? & 0x8000) == 0 {
        return None;
    }
    let mut pos = 12;
    for _ in 0..count(4)? {
        pos = read_name(message, pos)?.1 + 4;
    }
    let total = count(6)? + count(8)? + count(10)?;
    if total > 256 {
        return None;
    }
    let mut found = Vec::with_capacity(total);
    for _ in 0..total {
        let (name, next) = read_name(message, pos)?;
        let rtype = count(next)?;
        let length = count(next + 8)?;
        let start = next + 10;
        if message.len() < start + length {
            return None;
        }
        found.push((name, rtype as u16, start, length));
        pos = start + length;
    }
    Some(found)
}
/// PTR proves the responder offers the Sonos service; SRV names its host and the
/// A record gives that host's address. Responses without a usable A record fall
/// back to the responder itself, which is the player that answered.
fn addresses(message: &[u8], peer: Ipv4Addr) -> Vec<Ipv4Addr> {
    let Some(records) = records(message) else {
        return vec![];
    };
    if !records
        .iter()
        .any(|(name, rtype, ..)| *rtype == 12 && name == SERVICE)
    {
        return vec![];
    }
    let targets: Vec<String> = records
        .iter()
        .filter(|(name, rtype, ..)| *rtype == 33 && name.ends_with(SERVICE))
        .filter_map(|(_, _, start, _)| read_name(message, start + 6).map(|(name, _)| name))
        .collect();
    let mut found: Vec<Ipv4Addr> = records
        .iter()
        .filter(|(name, rtype, _, length)| {
            *rtype == 1 && *length == 4 && targets.iter().any(|target| target == name)
        })
        .map(|(_, _, start, _)| {
            Ipv4Addr::new(
                message[*start],
                message[start + 1],
                message[start + 2],
                message[start + 3],
            )
        })
        .filter(routable)
        .collect();
    if found.is_empty() && routable(&peer) {
        found.push(peer);
    }
    found
}
fn routable(address: &Ipv4Addr) -> bool {
    !address.is_unspecified()
        && !address.is_loopback()
        && !address.is_multicast()
        && !address.is_broadcast()
}

mod sdk;
pub use sdk::Settings;

#[cfg(test)]
mod tests;
