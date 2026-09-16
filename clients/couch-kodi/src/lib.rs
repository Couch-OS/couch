//! A Kodi JSON-RPC client for Couch.
//!
//! Kodi speaks JSON-RPC 2.0 over two transports and this crate speaks both
//! behind one `Kodi` handle: a generic `call`, plus typed wrappers for the
//! handful of methods a remote control actually presses.
//!
//! Prefer the TCP one. Port 9090 is on out of the box, the connection is
//! persistent, and Kodi pushes notifications down it - which is what lets the
//! UI stop polling. The HTTP interface on 8080 has to be switched on by a
//! human, costs a handshake per keypress and can only answer questions.
//!
//! Every call is bounded by a timeout. The daemon that will own this feeds the
//! on-screen UI, so a media box that has slept or changed address must fail,
//! not block - a wedged read is a frozen screen with no way back.

mod error;
mod http;
mod net;
pub mod playback;
pub mod settings;
mod tcp;

pub use error::{Error, Result};

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub const DEFAULT_TCP_PORT: u16 = 9090;
pub const DEFAULT_HTTP_PORT: u16 = 8080;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(4);

enum Transport {
    Tcp(tcp::Tcp),
    Http(http::Http),
}

impl Transport {
    fn endpoint(&self) -> String {
        match self {
            Transport::Tcp(t) => t.endpoint(),
            Transport::Http(h) => h.endpoint(),
        }
    }

    fn exchange(&self, request: &Value, id: u64) -> Result<Value> {
        match self {
            Transport::Tcp(t) => t.exchange(request, id),
            Transport::Http(h) => h.exchange(request, id),
        }
    }

    fn set_timeout(&mut self, timeout: Duration) {
        match self {
            Transport::Tcp(t) => t.timeout = timeout,
            Transport::Http(h) => h.timeout = timeout,
        }
    }
}

/// A handle on one media box. Not `Sync`: the TCP transport is a single
/// connection with a single read position, and a daemon that wants to both
/// call and listen does it from one loop.
pub struct Kodi {
    transport: Transport,
    host: String,
    /// Artwork is served by the web interface even when control goes over the
    /// TCP port, so its port is tracked separately.
    web_port: u16,
    next_id: AtomicU64,
}

// Derived, this would print the encoded password on any log line that used
// {:?} on a handle.
impl fmt::Debug for Kodi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kodi")
            .field("endpoint", &self.transport.endpoint())
            .field("notifications", &self.supports_notifications())
            .finish()
    }
}

impl Kodi {
    /// The raw JSON-RPC port, which is what you want unless you have a reason.
    pub fn tcp(host: impl Into<String>, port: u16) -> Self {
        let host = host.into();
        Kodi {
            transport: Transport::Tcp(tcp::Tcp::new(host.clone(), port, DEFAULT_TIMEOUT)),
            host,
            web_port: DEFAULT_HTTP_PORT,
            next_id: AtomicU64::new(1),
        }
    }

    /// The web interface. Answers calls, never pushes anything.
    pub fn http(host: impl Into<String>, port: u16) -> Self {
        let host = host.into();
        Kodi {
            transport: Transport::Http(http::Http {
                host: host.clone(),
                port,
                auth: None,
                timeout: DEFAULT_TIMEOUT,
            }),
            host,
            web_port: port,
            next_id: AtomicU64::new(1),
        }
    }

    /// Credentials for the web interface. The TCP port has no authentication
    /// of any kind, so this does nothing there - which is worth knowing in the
    /// other direction too: anything that can reach 9090 controls the box.
    pub fn with_auth(mut self, user: &str, pass: &str) -> Self {
        if let Transport::Http(h) = &mut self.transport {
            h.auth = Some(http::basic_auth(user, pass));
        }
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.transport.set_timeout(timeout);
        self
    }

    /// Where `image_url` points. Only useful on a TCP handle, whose control
    /// port says nothing about where the web interface is.
    pub fn with_web_port(mut self, port: u16) -> Self {
        self.web_port = port;
        self
    }

    pub fn endpoint(&self) -> String {
        self.transport.endpoint()
    }

    /// Whether this transport can push. False for HTTP, where
    /// `next_notification` is always an immediate `None`.
    pub fn supports_notifications(&self) -> bool {
        matches!(self.transport, Transport::Tcp(_))
    }

    /// Block for up to `timeout` for the next event Kodi pushes - OnPlay,
    /// OnPause, OnVolumeChanged and the rest. `None` if none arrived.
    ///
    /// Events that turn up while a `call` is waiting for its reply are queued,
    /// not dropped, so a caller alternating between the two sees everything.
    pub fn next_notification(&self, timeout: Duration) -> Result<Option<Notification>> {
        let Transport::Tcp(t) = &self.transport else {
            return Ok(None);
        };
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(raw) = t.next_notification(left)? else {
                return Ok(None);
            };
            if let Some(n) = Notification::from_value(raw) {
                return Ok(Some(n));
            }
            // Something without a method and without an id. Not ours to
            // explain; keep waiting out the caller's budget.
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// The escape hatch: any method, any params, the raw `result` back. Pass
    /// `Value::Null` for params and the member is left out entirely, which some
    /// Kodi methods are fussier about than a null.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut request = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_null() {
            request["params"] = params;
        }

        let reply: Reply = serde_json::from_value(self.transport.exchange(&request, id)?)?;

        if let Some(e) = reply.error {
            return Err(Error::Rpc {
                method: method.to_string(),
                code: e.code,
                message: e.message,
            });
        }
        reply.result.ok_or_else(|| Error::Protocol {
            endpoint: self.transport.endpoint(),
            detail: format!("the reply to {method} carried neither a result nor an error"),
        })
    }

    /// `call`, with the result deserialised into `T`.
    pub fn call_as<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        Ok(serde_json::from_value(self.call(method, params)?)?)
    }

    pub fn ping(&self) -> Result<()> {
        match self.call("JSONRPC.Ping", Value::Null)? {
            Value::String(s) if s == "pong" => Ok(()),
            other => Err(Error::Protocol {
                endpoint: self.transport.endpoint(),
                detail: format!("JSONRPC.Ping answered {other} rather than \"pong\""),
            }),
        }
    }

    pub fn active_players(&self) -> Result<Vec<Player>> {
        self.call_as("Player.GetActivePlayers", Value::Null)
    }

    /// The item and its progress in one struct. `None` when nothing is playing.
    ///
    /// Three round trips, because Kodi splits "what is it" from "how far in are
    /// we" and will not say which player without being asked. On a LAN that is
    /// a couple of milliseconds; batching them would buy the daemon nothing it
    /// can see and cost the batch handling.
    pub fn now_playing(&self) -> Result<Option<NowPlaying>> {
        let Some(player) = self.active_players()?.into_iter().next() else {
            return Ok(None);
        };

        let item: ItemReply = self.call_as(
            "Player.GetItem",
            json!({
                "playerid": player.id,
                "properties": ["title", "showtitle", "season", "episode", "year",
                               "art", "runtime", "duration"],
            }),
        )?;
        let item = item.item;

        let props: Props = self.call_as(
            "Player.GetProperties",
            json!({
                "playerid": player.id,
                "properties": ["time", "totaltime", "percentage", "speed"],
            }),
        )?;

        // Kodi uses -1 and 0 rather than omitting these when they do not apply
        // to the media type, so a music track would otherwise claim to be
        // season -1 of nothing.
        let positive = |v: Option<i64>| v.filter(|n| *n > 0);
        let title = item
            .title
            .filter(|s| !s.is_empty())
            .or(item.label)
            .unwrap_or_else(|| "unknown".into());

        Ok(Some(NowPlaying {
            player_id: player.id,
            player_kind: player.kind,
            media_type: item.media_type.unwrap_or_default(),
            title,
            show: item.showtitle.filter(|s| !s.is_empty()),
            season: positive(item.season),
            episode: positive(item.episode),
            year: positive(item.year),
            art: item.art,
            // "runtime" is the video field, "duration" the audio one; both are
            // whole seconds, and either can be absent or zero for a stream.
            length_secs: item.runtime.or(item.duration).filter(|d| *d > 0.0),
            position_secs: props.time.secs(),
            total_secs: props.totaltime.secs(),
            percentage: props.percentage,
            speed: props.speed,
            paused: props.speed == 0.0,
        }))
    }

    // --- transport ----------------------------------------------------------

    /// Toggle play/pause; the new paused state comes back. `None` when there
    /// was no player to toggle.
    pub fn play_pause(&self) -> Result<Option<bool>> {
        let Some(id) = self.first_player()? else {
            return Ok(None);
        };
        let s: Speed = self.call_as("Player.PlayPause", json!({ "playerid": id }))?;
        Ok(Some(s.speed == 0.0))
    }

    /// The remaining transport calls return false when nothing was playing, in
    /// which case nothing was sent.
    pub fn stop(&self) -> Result<bool> {
        self.on_player("Player.Stop", json!({}))
    }

    pub fn next(&self) -> Result<bool> {
        self.on_player("Player.GoTo", json!({ "to": "next" }))
    }

    pub fn previous(&self) -> Result<bool> {
        self.on_player("Player.GoTo", json!({ "to": "previous" }))
    }

    /// Jump to a percentage of the item. Kodi wants an object here, not a bare
    /// number - the flat form was dropped after v17.
    pub fn seek_percent(&self, percent: f64) -> Result<bool> {
        let percent = percent.clamp(0.0, 100.0);
        self.on_player("Player.Seek", json!({ "value": { "percentage": percent } }))
    }

    // --- navigation ---------------------------------------------------------

    /// Any Input.* method by its bare name, e.g. "Up" or "ContextMenu".
    pub fn input(&self, action: &str) -> Result<()> {
        self.call(&format!("Input.{action}"), Value::Null)
            .map(|_| ())
    }

    pub fn up(&self) -> Result<()> {
        self.input("Up")
    }
    pub fn down(&self) -> Result<()> {
        self.input("Down")
    }
    pub fn left(&self) -> Result<()> {
        self.input("Left")
    }
    pub fn right(&self) -> Result<()> {
        self.input("Right")
    }
    /// Contextual OK: let Kodi resolve the remote key against its active window.
    /// Input.Select bypasses keymaps and cannot open the fullscreen player OSD.
    pub fn select(&self) -> Result<()> {
        self.call(
            "Input.ButtonEvent",
            json!({"button": "select", "keymap": "R1", "holdtime": 0}),
        )
        .map(|_| ())
    }
    pub fn back(&self) -> Result<()> {
        self.input("Back")
    }
    pub fn home(&self) -> Result<()> {
        self.input("Home")
    }
    pub fn context_menu(&self) -> Result<()> {
        self.input("ContextMenu")
    }

    // --- volume -------------------------------------------------------------

    pub fn volume(&self) -> Result<Volume> {
        self.call_as(
            "Application.GetProperties",
            json!({ "properties": ["volume", "muted"] }),
        )
    }

    /// Returns the level Kodi settled on, which is not always the one asked for.
    pub fn set_volume(&self, level: i64) -> Result<i64> {
        let level = level.clamp(0, 100);
        let v = self.call("Application.SetVolume", json!({ "volume": level }))?;
        Ok(v.as_i64().unwrap_or(level))
    }

    /// Returns the new muted state.
    pub fn toggle_mute(&self) -> Result<bool> {
        let v = self.call("Application.SetMute", json!({ "mute": "toggle" }))?;
        Ok(v.as_bool().unwrap_or(false))
    }

    // --- artwork ------------------------------------------------------------

    /// Turn one of Kodi's `image://...` strings into a URL the GUI can GET.
    ///
    /// The string is already percent-encoded once - it wraps the original file
    /// or http URL - and Kodi's /image/ handler decodes exactly one layer, so
    /// encoding the whole thing again is correct rather than sloppy.
    ///
    /// If the box needs credentials, so does this URL: whoever fetches it has
    /// to send the same Basic header.
    pub fn image_url(&self, art: &str) -> Option<String> {
        if art.is_empty() {
            return None;
        }
        Some(format!(
            "http://{}/image/{}",
            net::authority(&self.host, self.web_port),
            percent_encode(art)
        ))
    }

    /// The fetchable URL for a named piece of an item's art, e.g. "fanart" or
    /// "clearlogo".
    pub fn art_url(&self, item: &NowPlaying, name: &str) -> Option<String> {
        self.image_url(item.art_raw(name)?)
    }

    // --- internals ----------------------------------------------------------

    fn first_player(&self) -> Result<Option<i64>> {
        Ok(self.active_players()?.first().map(|p| p.id))
    }

    /// Aim a player method at whichever player is active, if any.
    fn on_player(&self, method: &str, mut params: Value) -> Result<bool> {
        let Some(id) = self.first_player()? else {
            return Ok(false);
        };
        params["playerid"] = json!(id);
        self.call(method, params)?;
        Ok(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Player {
    #[serde(rename = "playerid")]
    pub id: i64,
    /// "video", "audio" or "picture".
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Volume {
    pub volume: i64,
    pub muted: bool,
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub player_id: i64,
    pub player_kind: String,
    /// "episode", "movie", "song", "unknown"...
    pub media_type: String,
    pub title: String,
    pub show: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
    /// Raw `image://` strings, keyed as Kodi keys them.
    pub art: BTreeMap<String, String>,
    /// The library's idea of the item's length, absent for live streams.
    pub length_secs: Option<f64>,
    pub position_secs: f64,
    pub total_secs: f64,
    pub percentage: f64,
    /// 1.0 for normal playback, 0.0 paused, 2.0 and up fast-forwarding.
    pub speed: f64,
    pub paused: bool,
}

impl NowPlaying {
    /// Art by name, still as an `image://` string. The plain key wins; the
    /// "tvshow." one is only a fallback.
    ///
    /// Art inherited from a parent is also published under a prefix, so an
    /// episode's series backdrop appears as "tvshow.fanart". Measured against
    /// a real library (Kodi 22, episode via the Jellyfin plugin) both forms
    /// were present and identical, but a native TV scraper does not always
    /// fill in the plain key, so the fallback stays.
    ///
    /// Note that the prefixed key is not always a duplicate: "thumb" is the
    /// episode still and "tvshow.thumb" the series poster, two different
    /// images. Falling back between those two would be wrong, which is why
    /// this asks for one name and never guesses a "best" image.
    pub fn art_raw(&self, name: &str) -> Option<&str> {
        self.art
            .get(name)
            .or_else(|| self.art.get(&format!("tvshow.{name}")))
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

/// One of the events Kodi pushes down the TCP connection unasked.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    /// "Player.OnPause", "Application.OnVolumeChanged" and so on.
    pub method: String,
    /// Which subsystem announced it; "xbmc" for Kodi's own.
    pub sender: Option<String>,
    /// The event's payload. Shapes differ per method, so it stays a Value.
    pub data: Value,
    /// The whole object as it arrived, for logging.
    pub raw: Value,
}

impl Notification {
    fn from_value(raw: Value) -> Option<Self> {
        let method = raw.get("method")?.as_str()?.to_string();
        let params = raw.get("params");
        Some(Notification {
            sender: params
                .and_then(|p| p.get("sender"))
                .and_then(Value::as_str)
                .map(str::to_string),
            data: params
                .and_then(|p| p.get("data"))
                .cloned()
                .unwrap_or(Value::Null),
            method,
            raw,
        })
    }
}

#[derive(Deserialize)]
struct Reply {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Speed {
    speed: f64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Props {
    time: KTime,
    totaltime: KTime,
    percentage: f64,
    speed: f64,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default)]
struct KTime {
    hours: u32,
    minutes: u32,
    seconds: u32,
    milliseconds: u32,
}

impl KTime {
    fn secs(self) -> f64 {
        f64::from(self.hours * 3600 + self.minutes * 60 + self.seconds)
            + f64::from(self.milliseconds) / 1000.0
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ItemReply {
    item: RawItem,
}

// Every field optional and defaulted: Kodi returns a different subset for a
// film, an episode, a track and a live channel, and a missing field must not
// turn into an error the GUI has to render.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawItem {
    label: Option<String>,
    title: Option<String>,
    showtitle: Option<String>,
    season: Option<i64>,
    episode: Option<i64>,
    year: Option<i64>,
    runtime: Option<f64>,
    duration: Option<f64>,
    #[serde(rename = "type")]
    media_type: Option<String>,
    art: BTreeMap<String, String>,
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_sends_one_contextual_remote_press_and_propagates_rejection() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for rejected in [false, true] {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "Input.ButtonEvent");
                assert_eq!(
                    request["params"],
                    json!({"button":"select","keymap":"R1","holdtime":0})
                );
                let reply = if rejected {
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}})
                } else {
                    json!({"jsonrpc":"2.0","id":request["id"],"result":"OK"})
                };
                writeln!(stream, "{reply}").unwrap();
            }
        });
        let client = Kodi::tcp("127.0.0.1", port);
        client.select().unwrap();
        assert!(matches!(client.select(), Err(Error::Rpc { .. })));
        server.join().unwrap();
    }

    #[test]
    fn image_urls_are_encoded_once_more() {
        let k = Kodi::http("10.0.0.5", 8080);
        let art = "image://http%3a%2f%2fexample%2ffanart.jpg/";
        assert_eq!(
            k.image_url(art).unwrap(),
            "http://10.0.0.5:8080/image/image%3A%2F%2Fhttp%253a%252f%252fexample%252ffanart.jpg%2F"
        );
        assert!(k.image_url("").is_none());
        // A TCP handle controls 9090 but artwork still comes off the web port.
        let t = Kodi::tcp("10.0.0.5", 9090);
        assert!(t
            .image_url(art)
            .unwrap()
            .starts_with("http://10.0.0.5:8080/image/"));
        assert!(t
            .with_web_port(8090)
            .image_url(art)
            .unwrap()
            .starts_with("http://10.0.0.5:8090/"));
    }

    #[test]
    fn inherited_art_is_found_under_its_prefix() {
        let mut art = BTreeMap::new();
        art.insert("fanart".to_string(), "image://plain/".to_string());
        art.insert("tvshow.fanart".to_string(), "image://prefixed/".to_string());
        art.insert("tvshow.clearlogo".to_string(), "image://x/".to_string());
        art.insert("thumb".to_string(), String::new());
        let np = NowPlaying {
            player_id: 1,
            player_kind: "video".into(),
            media_type: "episode".into(),
            title: "Ozymandias".into(),
            show: None,
            season: None,
            episode: None,
            year: None,
            art,
            length_secs: None,
            position_secs: 0.0,
            total_secs: 0.0,
            percentage: 0.0,
            speed: 1.0,
            paused: false,
        };
        // Plain first, prefixed only as a fallback.
        assert_eq!(np.art_raw("fanart"), Some("image://plain/"));
        assert_eq!(np.art_raw("clearlogo"), Some("image://x/"));
        assert_eq!(np.art_raw("thumb"), None);
        assert_eq!(np.art_raw("discart"), None);
    }

    #[test]
    fn items_survive_the_fields_kodi_leaves_out() {
        let raw = r#"{"item":{"label":"BBC One","type":"unknown","art":{}}}"#;
        let reply: ItemReply = serde_json::from_str(raw).unwrap();
        assert_eq!(reply.item.label.as_deref(), Some("BBC One"));
        assert!(reply.item.title.is_none());
        assert!(reply.item.runtime.is_none());
    }

    #[test]
    fn times_fold_down_to_seconds() {
        let p: Props = serde_json::from_str(
            r#"{"time":{"hours":1,"minutes":2,"seconds":3,"milliseconds":500},"speed":0}"#,
        )
        .unwrap();
        assert_eq!(p.time.secs(), 3723.5);
        assert_eq!(p.totaltime.secs(), 0.0);
        assert_eq!(p.speed, 0.0);
    }
}
