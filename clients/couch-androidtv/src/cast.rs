//! Read-only Cast v2 metadata alongside Android Remote v2 controls.
//!
//! Sources: Chromium cast_channel.proto and Google's MediaStatus/MediaInformation
//! schema; attachment follows PyChromecast's receiver/media controllers. Only an
//! already running application advertising the media namespace is observed.
//! This module cannot launch apps, load content, seek, or change playback.
use crate::{Error, Result};
use prost::Message;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const MEDIA: &str = "urn:x-cast:com.google.cast.media";
const CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const SENDER: &str = "sender-couch-observer";
const MAX_FRAME: usize = 256 * 1024;
const STALE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub connected: bool,
    /// False while awaiting the first authoritative media observation for an
    /// active transport. True with no media means idle, unavailable, or expired.
    #[serde(default)]
    pub media_status_known: bool,
    pub app_name: Option<String>,
    pub now_playing: Option<NowPlaying>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NowPlaying {
    pub session_id: i64,
    pub title: String,
    pub subtitle: String,
    pub artwork_url: Option<String>,
    pub duration: Option<f64>,
    /// Projected from the last received report while PLAYING, never from polling.
    pub position: f64,
    pub position_known: bool,
    pub player_state: String,
    pub playback_rate: f64,
    pub live: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct AppSession {
    transport: String,
    session: String,
    app: String,
}
#[derive(Default)]
struct State {
    status: Status,
    app: Option<AppSession>,
    content: Option<String>,
    item: Option<i64>,
    updated: Option<Instant>,
    retired: Vec<i64>,
}
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").chars().take(512).collect()
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0. && *v < 1e10)
}
// Standard Cast Image dimensions are optional. Prefer a supplied landscape
// image for the fullscreen backdrop without inventing URLs or dropping posters.
fn backdrop(images: &Value) -> Option<String> {
    let mut fallback = None;
    for image in images.as_array()? {
        let Some(url) = artwork(&image["url"]) else {
            continue;
        };
        let landscape = number(&image["width"])
            .zip(number(&image["height"]))
            .is_some_and(|(width, height)| height > 0. && width > height);
        if landscape {
            return Some(url);
        }
        if fallback.is_none() {
            fallback = Some(url);
        }
    }
    fallback
}
fn artwork(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if raw.len() > 4096 {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
impl State {
    fn clear_media(&mut self) {
        self.status.now_playing = None;
        self.updated = None;
        self.content = None;
        self.item = None;
    }
    fn disconnect(&mut self) {
        *self = Self::default();
    }
    fn snapshot(&self, now: Instant) -> Status {
        let mut status = self.status.clone();
        let Some(at) = self.updated else {
            status.now_playing = None;
            return status;
        };
        let age = now.saturating_duration_since(at);
        if age >= STALE {
            status.now_playing = None;
            return status;
        }
        if let Some(media) = status.now_playing.as_mut() {
            if media.position_known && media.player_state == "PLAYING" {
                media.position += age.as_secs_f64() * media.playback_rate;
            }
            if let Some(duration) = media.duration {
                media.position = media.position.min(duration);
            }
        }
        status
    }
    /// Returns true when a new active media transport must be attached.
    fn receive(
        &mut self,
        source: &str,
        namespace: &str,
        value: &Value,
        now: Instant,
    ) -> Result<bool> {
        if namespace == RECEIVER && source == "receiver-0" && value["type"] == "RECEIVER_STATUS" {
            let status = value
                .get("status")
                .and_then(Value::as_object)
                .ok_or(Error::Protocol)?;
            let apps = status.get("applications").and_then(Value::as_array);
            self.status.app_name = apps
                .and_then(|a| a.first())
                .map(|v| text(&v["displayName"]))
                .filter(|s| !s.is_empty());
            let app = apps
                .into_iter()
                .flatten()
                .find(|a| {
                    a["namespaces"]
                        .as_array()
                        .is_some_and(|n| n.iter().any(|v| v["name"] == MEDIA))
                })
                .and_then(|a| {
                    let transport = a["transportId"].as_str()?;
                    let session = a["sessionId"].as_str()?;
                    let id = a["appId"].as_str()?;
                    if [transport, session, id]
                        .iter()
                        .any(|s| s.is_empty() || s.len() > 512)
                    {
                        return None;
                    }
                    self.status.app_name = Some(text(&a["displayName"]));
                    Some(AppSession {
                        transport: transport.into(),
                        session: session.into(),
                        app: id.into(),
                    })
                });
            if self.app != app {
                self.clear_media();
                self.retired.clear();
                self.app = app;
                self.status.media_status_known = self.app.is_none();
                return Ok(self.app.is_some());
            }
            if self.app.is_none() {
                self.status.media_status_known = true;
            }
        } else if namespace == MEDIA
            && self.app.as_ref().is_some_and(|a| a.transport == source)
            && value["type"] == "MEDIA_STATUS"
        {
            let statuses = value["status"].as_array().ok_or(Error::Protocol)?;
            if statuses.is_empty() {
                self.status.media_status_known = true;
                if let Some(old) = self.status.now_playing.as_ref() {
                    self.retired.push(old.session_id);
                    if self.retired.len() > 8 {
                        self.retired.remove(0);
                    }
                }
                self.clear_media();
                return Ok(false);
            }
            let status = statuses.first().unwrap();
            let id = status["mediaSessionId"]
                .as_i64()
                .filter(|id| *id >= 0)
                .ok_or(Error::Protocol)?;
            if self.retired.contains(&id) {
                return Ok(false);
            }
            let media = status.get("media");
            let content = media
                .and_then(|m| m["contentId"].as_str())
                .map(str::to_string);
            let item = status["currentItemId"].as_i64();
            let changed = self
                .status
                .now_playing
                .as_ref()
                .is_some_and(|old| old.session_id != id)
                || content
                    .as_ref()
                    .is_some_and(|c| self.content.as_ref().is_some_and(|old| old != c))
                || item.is_some_and(|i| self.item.is_some_and(|old| old != i));
            if changed {
                if let Some(old) = self.status.now_playing.as_ref() {
                    if old.session_id != id {
                        self.retired.push(old.session_id);
                        if self.retired.len() > 8 {
                            self.retired.remove(0);
                        }
                    }
                }
                self.clear_media();
            }
            if status["playerState"] == "IDLE" {
                self.status.media_status_known = true;
                self.retired.push(id);
                if self.retired.len() > 8 {
                    self.retired.remove(0);
                }
                self.clear_media();
                return Ok(false);
            }
            let mut next = self
                .snapshot(now)
                .now_playing
                .unwrap_or_else(|| NowPlaying {
                    session_id: id,
                    playback_rate: 1.,
                    ..NowPlaying::default()
                });
            if let Some(state) = status["playerState"].as_str() {
                if !matches!(state, "PLAYING" | "PAUSED" | "BUFFERING" | "IDLE") {
                    return Err(Error::Protocol);
                }
                next.player_state = state.into();
            }
            if let Some(position) = number(&status["currentTime"]) {
                next.position = position;
                next.position_known = true;
            }
            if let Some(rate) = number(&status["playbackRate"]).filter(|r| *r <= 16.) {
                next.playback_rate = rate;
            }
            if let Some(media) = media {
                if let Some(metadata) = media.get("metadata") {
                    next.title = text(&metadata["title"]);
                    next.subtitle = ["subtitle", "artist", "seriesTitle", "albumName"]
                        .into_iter()
                        .map(|key| text(&metadata[key]))
                        .find(|s| !s.is_empty())
                        .unwrap_or_default();
                    next.artwork_url = backdrop(&metadata["images"]);
                }
                if media.get("duration").is_some() {
                    next.duration = number(&media["duration"]);
                }
                if let Some(kind) = media["streamType"].as_str() {
                    next.live = kind == "LIVE";
                }
            }
            if content.is_some() {
                self.content = content;
            }
            if item.is_some() {
                self.item = item;
            }
            self.status.now_playing = Some(next);
            self.status.media_status_known = true;
            self.updated = Some(now);
        }
        Ok(false)
    }
}

// Cast v2 uses a fixed big-endian u32 frame length, unlike Remote v2's varint.
#[derive(Clone, PartialEq, Message)]
struct Envelope {
    #[prost(int32, required, tag = "1")]
    version: i32,
    #[prost(string, required, tag = "2")]
    source: String,
    #[prost(string, required, tag = "3")]
    destination: String,
    #[prost(string, required, tag = "4")]
    namespace: String,
    #[prost(int32, required, tag = "5")]
    payload_type: i32,
    #[prost(string, optional, tag = "6")]
    payload: Option<String>,
    #[prost(bytes = "vec", optional, tag = "7")]
    binary: Option<Vec<u8>>,
}
#[derive(Default)]
struct Frames {
    bytes: Vec<u8>,
}
impl Frames {
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if self.bytes.len() + bytes.len() > MAX_FRAME + 4 + 8192 {
            return Err(Error::Protocol);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn take(&mut self) -> Result<Option<Envelope>> {
        if self.bytes.len() < 4 {
            return Ok(None);
        }
        let size = u32::from_be_bytes(self.bytes[..4].try_into().unwrap()) as usize;
        if size == 0 || size > MAX_FRAME {
            return Err(Error::Protocol);
        }
        if self.bytes.len() < size + 4 {
            return Ok(None);
        }
        let frame = Envelope::decode(&self.bytes[4..size + 4]).map_err(|_| Error::Protocol)?;
        self.bytes.drain(..size + 4);
        Ok(Some(frame))
    }
}

/// A separate, read-only TLS8009 connection. Never share its blocking poll with
/// the Remote v2 input lane. Cast uses its own self-signed certificate; this
/// observer verifies handshake signatures but does not assert device identity.
pub struct Observer {
    stream: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    frames: Frames,
    state: State,
    request: u32,
    next_status: Instant,
    next_ping: Instant,
    last_rx: Instant,
    pending_app: Option<u32>,
}
impl Observer {
    pub fn connect(address: IpAddr, timeout: Duration) -> Result<Self> {
        Self::connect_port(address, 8009, timeout)
    }
    fn connect_port(address: IpAddr, port: u16, timeout: Duration) -> Result<Self> {
        if address.is_unspecified()
            || address.is_multicast()
            || timeout.is_zero()
            || timeout > Duration::from_secs(10)
        {
            return Err(Error::Configuration);
        }
        let deadline = Instant::now() + timeout;
        let mut socket = TcpStream::connect_timeout(&SocketAddr::new(address, port), timeout)
            .map_err(|_| Error::Transport)?;
        socket
            .set_read_timeout(Some(timeout))
            .map_err(|_| Error::Transport)?;
        socket
            .set_write_timeout(Some(timeout))
            .map_err(|_| Error::Transport)?;
        // A fresh, empty pin: the cast channel accepts whatever certificate the
        // TV shows on this connection and holds it for that connection only.
        let cfg = couch_sdk::tls::pinned_client_config(Arc::new(couch_sdk::tls::Pin::new(
            Arc::new(Mutex::new(Vec::new())),
            crate::CERTIFICATE_CHANGED,
        )))
        .map_err(|_| Error::Crypto)?;
        let mut connection =
            rustls::ClientConnection::new(Arc::new(cfg), ServerName::IpAddress(address.into()))
                .map_err(|_| Error::Crypto)?;
        while connection.is_handshaking() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout);
            }
            socket
                .set_read_timeout(Some(left.min(Duration::from_millis(100))))
                .map_err(|_| Error::Transport)?;
            socket
                .set_write_timeout(Some(left.min(Duration::from_millis(100))))
                .map_err(|_| Error::Transport)?;
            if connection.wants_write() {
                connection
                    .write_tls(&mut socket)
                    .map_err(|_| Error::Transport)?;
            }
            if connection.wants_read() {
                match connection.read_tls(&mut socket) {
                    Ok(0) => return Err(Error::Transport),
                    Ok(_) => {
                        connection
                            .process_new_packets()
                            .map_err(|_| Error::Protocol)?;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => return Err(Error::Transport),
                }
            }
        }
        socket
            .set_write_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| Error::Transport)?;
        let now = Instant::now();
        let mut this = Self {
            stream: rustls::StreamOwned::new(connection, socket),
            frames: Frames::default(),
            state: State::default(),
            request: 0,
            next_status: now,
            next_ping: now + Duration::from_secs(5),
            last_rx: now,
            pending_app: None,
        };
        this.state.status.connected = true;
        this.send(
            "receiver-0",
            CONNECTION,
            json!({"type":"CONNECT","origin":{},"userAgent":"Couch read-only observer"}),
        )?;
        this.request_status()?;
        Ok(this)
    }
    pub fn status(&self) -> Status {
        self.state.snapshot(Instant::now())
    }
    /// Returns a projected snapshot. Any I/O/protocol error clears cached media;
    /// create a new observer to retry. The caller controls reconnection backoff.
    pub fn poll(&mut self, budget: Duration) -> Result<Status> {
        let result = self.poll_inner(budget.min(Duration::from_secs(1)));
        if result.is_err() {
            self.state.disconnect();
        }
        result.map(|_| self.status())
    }
    fn send(&mut self, to: &str, namespace: &str, payload: Value) -> Result<()> {
        let frame = Envelope {
            version: 0,
            source: SENDER.into(),
            destination: to.into(),
            namespace: namespace.into(),
            payload_type: 0,
            payload: Some(payload.to_string()),
            binary: None,
        }
        .encode_to_vec();
        self.stream
            .write_all(&(frame.len() as u32).to_be_bytes())
            .and_then(|_| self.stream.write_all(&frame))
            .and_then(|_| self.stream.flush())
            .map_err(|_| Error::Transport)
    }
    fn query(&mut self, to: &str, namespace: &str) -> Result<u32> {
        self.request = self.request.wrapping_add(1);
        self.send(
            to,
            namespace,
            json!({"type":"GET_STATUS","requestId":self.request}),
        )?;
        Ok(self.request)
    }
    fn request_status(&mut self) -> Result<()> {
        self.query("receiver-0", RECEIVER)?;
        if let Some(app) = self.state.app.clone() {
            let request = self.query(&app.transport, MEDIA)?;
            if self.pending_app.is_some() {
                self.pending_app = Some(request);
            }
        }
        self.next_status = Instant::now() + Duration::from_secs(3);
        Ok(())
    }
    fn poll_inner(&mut self, budget: Duration) -> Result<()> {
        if !self.state.status.connected {
            return Err(Error::Transport);
        }
        if self.last_rx.elapsed() >= STALE {
            return Err(Error::Timeout);
        }
        if Instant::now() >= self.next_status {
            self.request_status()?;
        }
        if Instant::now() >= self.next_ping {
            self.send("receiver-0", HEARTBEAT, json!({"type":"PING"}))?;
            self.next_ping = Instant::now() + Duration::from_secs(5);
        }
        let end = Instant::now() + budget;
        for _ in 0..64 {
            if let Some(frame) = self.frames.take()? {
                if frame.version != 0 || frame.payload_type != 0 {
                    return Err(Error::Protocol);
                }
                if frame.destination != SENDER && frame.destination != "*" {
                    continue;
                }
                let value: Value =
                    serde_json::from_str(frame.payload.as_deref().ok_or(Error::Protocol)?)
                        .map_err(|_| Error::Protocol)?;
                self.last_rx = Instant::now();
                if frame.namespace == HEARTBEAT && value["type"] == "PING" {
                    self.send(&frame.source, HEARTBEAT, json!({"type":"PONG"}))?;
                }
                if frame.namespace == CONNECTION && value["type"] == "CLOSE" {
                    if frame.source == "receiver-0" {
                        return Err(Error::Transport);
                    }
                    if self
                        .state
                        .app
                        .as_ref()
                        .is_some_and(|a| a.transport == frame.source)
                    {
                        self.state.clear_media();
                        self.state.app = None;
                        self.pending_app = None;
                    }
                    continue;
                }
                if frame.namespace == MEDIA {
                    if !self
                        .state
                        .app
                        .as_ref()
                        .is_some_and(|app| app.transport == frame.source)
                    {
                        continue;
                    }
                    if let Some(request) = self.pending_app {
                        if value["requestId"].as_u64() != Some(request as u64) {
                            continue;
                        }
                        self.pending_app = None;
                    }
                }
                if self
                    .state
                    .receive(&frame.source, &frame.namespace, &value, Instant::now())?
                {
                    let target = self.state.app.as_ref().unwrap().transport.clone();
                    self.send(&target, CONNECTION, json!({"type":"CONNECT","origin":{}}))?;
                    self.pending_app = Some(self.query(&target, MEDIA)?);
                }
                continue;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.stream
                .sock
                .set_read_timeout(Some(left.min(Duration::from_millis(50))))
                .map_err(|_| Error::Transport)?;
            let mut bytes = [0; 8192];
            match self.stream.read(&mut bytes) {
                Ok(0) => return Err(Error::Transport),
                Ok(n) => self.frames.push(&bytes[..n])?,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(_) => return Err(Error::Transport),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receiver(transport: &str, session: &str) -> Value {
        json!({"type":"RECEIVER_STATUS","status":{"applications":[{"appId":"test-app","displayName":"Test player","transportId":transport,"sessionId":session,"namespaces":[{"name":MEDIA}]}]}})
    }
    fn media(id: i64) -> Value {
        json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":id,"playerState":"PLAYING","playbackRate":1.,"currentTime":10.,"media":{"contentId":format!("movie-{id}"),"duration":100.,"streamType":"BUFFERED","metadata":{"title":"Movie","subtitle":"Episode","images":[{"url":"https://example.test/art.jpg"}]}}}]})
    }
    fn setup(now: Instant) -> State {
        let mut state = State::default();
        state.status.connected = true;
        assert!(state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-a", "session-a"),
                now
            )
            .unwrap());
        state.receive("transport-a", MEDIA, &media(1), now).unwrap();
        state
    }
    #[test]
    fn projects_only_playing_reports_and_expires_without_resetting_on_reads() {
        let now = Instant::now();
        let mut state = setup(now);
        assert_eq!(
            state
                .snapshot(now + Duration::from_secs(2))
                .now_playing
                .unwrap()
                .position,
            12.
        );
        assert_eq!(
            state
                .snapshot(now + Duration::from_secs(4))
                .now_playing
                .unwrap()
                .position,
            14.
        );
        assert!(state.snapshot(now + STALE).now_playing.is_none());
        for mode in ["PAUSED", "BUFFERING"] {
            let mut v = media(1);
            v["status"][0]["playerState"] = json!(mode);
            state.receive("transport-a", MEDIA, &v, now).unwrap();
            assert_eq!(
                state
                    .snapshot(now + Duration::from_secs(4))
                    .now_playing
                    .unwrap()
                    .position,
                10.
            );
        }
        let mut v = media(1);
        v["status"][0]["currentTime"] = json!(99.);
        state.receive("transport-a", MEDIA, &v, now).unwrap();
        assert_eq!(
            state
                .snapshot(now + Duration::from_secs(4))
                .now_playing
                .unwrap()
                .position,
            100.
        );
    }
    #[test]
    fn missing_position_never_becomes_a_synthetic_timeline() {
        let now = Instant::now();
        let mut state = setup(now);
        let mut update = media(2);
        update["status"][0]
            .as_object_mut()
            .unwrap()
            .remove("currentTime");
        state.receive("transport-a", MEDIA, &update, now).unwrap();
        let next = state
            .snapshot(now + Duration::from_secs(2))
            .now_playing
            .unwrap();
        assert!(!next.position_known);
        assert_eq!(next.position, 0.);
        update["status"][0]["currentTime"] = json!(30.);
        state.receive("transport-a", MEDIA, &update, now).unwrap();
        assert!(state.snapshot(now).now_playing.unwrap().position_known);
        update["status"][0]
            .as_object_mut()
            .unwrap()
            .remove("currentTime");
        state
            .receive("transport-a", MEDIA, &update, now + Duration::from_secs(1))
            .unwrap();
        assert!(
            state
                .snapshot(now + Duration::from_secs(1))
                .now_playing
                .unwrap()
                .position_known
        );
    }
    #[test]
    fn app_session_changes_and_disconnect_clear_artwork() {
        let now = Instant::now();
        let mut state = setup(now);
        assert!(state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-b", "session-b"),
                now
            )
            .unwrap());
        assert!(state.snapshot(now).now_playing.is_none());
        state.receive("transport-a", MEDIA, &media(1), now).unwrap();
        assert!(state.snapshot(now).now_playing.is_none());
        state.receive("transport-b", MEDIA, &media(2), now).unwrap();
        assert!(state.snapshot(now).now_playing.is_some());
        state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-b", "session-c"),
                now,
            )
            .unwrap();
        assert!(state.snapshot(now).now_playing.is_none());
        state.disconnect();
        assert_eq!(state.snapshot(now), Status::default());
    }
    #[test]
    fn late_old_session_and_idle_reports_never_resurrect_a_movie() {
        let now = Instant::now();
        let mut state = setup(now);
        let v = json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":2,"playerState":"PAUSED","currentTime":0.}]});
        state.receive("transport-a", MEDIA, &v, now).unwrap();
        let next = state.snapshot(now).now_playing.unwrap();
        assert!(next.title.is_empty());
        assert!(next.artwork_url.is_none());
        state.receive("transport-a", MEDIA, &media(1), now).unwrap();
        assert_eq!(state.snapshot(now).now_playing.unwrap().session_id, 2);
        state.receive("transport-a",MEDIA,&json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":2,"playerState":"IDLE"}]}),now).unwrap();
        state.receive("transport-a", MEDIA, &media(2), now).unwrap();
        assert!(state.snapshot(now).now_playing.is_none());
    }
    #[test]
    fn partial_reports_keep_metadata_but_new_content_drops_it() {
        let now = Instant::now();
        let mut state = setup(now);
        state.receive("transport-a",MEDIA,&json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":1,"playerState":"PAUSED","currentTime":20.}]}),now).unwrap();
        assert_eq!(state.snapshot(now).now_playing.unwrap().title, "Movie");
        state.receive("transport-a",MEDIA,&json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":1,"playerState":"PLAYING","media":{"contentId":"new-content","streamType":"LIVE"}}]}),now).unwrap();
        let next = state.snapshot(now).now_playing.unwrap();
        assert!(next.live);
        assert!(next.title.is_empty());
        assert!(next.duration.is_none());
        assert!(next.artwork_url.is_none());
    }
    #[test]
    fn media_readiness_distinguishes_loading_from_authoritative_empty_status() {
        let now = Instant::now();
        let mut state = State::default();
        assert!(!state.snapshot(now).media_status_known);
        state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-a", "session-a"),
                now,
            )
            .unwrap();
        assert!(!state.snapshot(now).media_status_known);
        state.receive("transport-a", MEDIA, &media(1), now).unwrap();
        assert!(state.snapshot(now).media_status_known);
        assert!(state.snapshot(now + STALE).media_status_known);
        assert!(state.snapshot(now + STALE).now_playing.is_none());
        state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-b", "session-b"),
                now,
            )
            .unwrap();
        assert!(!state.snapshot(now).media_status_known);
        // A late report from the previous transport cannot finish this load.
        state.receive("transport-a", MEDIA, &media(1), now).unwrap();
        assert!(!state.snapshot(now).media_status_known);
        state
            .receive(
                "transport-b",
                MEDIA,
                &json!({"type":"MEDIA_STATUS","status":[]}),
                now,
            )
            .unwrap();
        assert!(state.snapshot(now).media_status_known);
        assert!(state.snapshot(now).now_playing.is_none());
        state.disconnect();
        assert!(!state.snapshot(now).media_status_known);
        state
            .receive(
                "receiver-0",
                RECEIVER,
                &receiver("transport-a", "session-a"),
                now,
            )
            .unwrap();
        state.receive("transport-a", MEDIA, &json!({"type":"MEDIA_STATUS","status":[{"mediaSessionId":2,"playerState":"IDLE"}]}), now).unwrap();
        assert!(state.snapshot(now).media_status_known);
        assert!(state.snapshot(now).now_playing.is_none());
    }
    #[test]
    fn receiver_without_media_namespace_confirms_unavailable_media() {
        let now = Instant::now();
        for applications in [json!([]), json!([{"displayName":"Home","namespaces":[]}])] {
            let mut state = State::default();
            state
                .receive(
                    "receiver-0",
                    RECEIVER,
                    &json!({"type":"RECEIVER_STATUS","status":{"applications":applications}}),
                    now,
                )
                .unwrap();
            assert!(state.snapshot(now).media_status_known);
            assert!(state.snapshot(now).now_playing.is_none());
        }
    }
    #[test]
    fn standard_image_dimensions_prefer_landscape_over_first_poster() {
        let now = Instant::now();
        let mut state = setup(now);
        let mut report = media(1);
        report["status"][0]["media"]["metadata"]["images"] = json!([
            {"url":"https://example.test/poster.jpg","width":1000,"height":1500},
            {"url":"https://example.test/backdrop.jpg","width":1920,"height":1080}
        ]);
        state.receive("transport-a", MEDIA, &report, now).unwrap();
        assert_eq!(
            state
                .snapshot(now)
                .now_playing
                .unwrap()
                .artwork_url
                .as_deref(),
            Some("https://example.test/backdrop.jpg")
        );
    }
    #[test]
    fn artwork_selection_keeps_valid_fallbacks_and_rejects_invalid_urls() {
        let images = json!([
            {"url":"https://example.test/unknown.jpg"},
            {"url":"file:///landscape.jpg","width":1920,"height":1080},
            {"url":"https://example.test/invalid-size.jpg","width":1920,"height":0},
            {"url":"https://example.test/poster.jpg","width":1000,"height":1500}
        ]);
        assert_eq!(
            backdrop(&images).as_deref(),
            Some("https://example.test/unknown.jpg")
        );
        assert_eq!(
            backdrop(&json!([images[3].clone()])).as_deref(),
            Some("https://example.test/poster.jpg")
        );
        assert!(backdrop(&json!([images[1].clone()])).is_none());
        assert!(backdrop(&Value::Null).is_none());
        assert!(backdrop(&json!([])).is_none());
    }
    #[test]
    fn limits_malformed_fields_and_rejects_non_http_artwork() {
        for url in [
            "file:///tmp/art",
            "javascript:alert(1)",
            "https://user:pass@example.test/a",
            "http:///",
            "data:image/png;base64,a",
        ] {
            assert!(artwork(&json!(url)).is_none());
        }
        assert!(artwork(&json!("http://192.0.2.1/art.jpg")).is_some());
        let now = Instant::now();
        let mut state = setup(now);
        assert!(state
            .receive(
                "transport-a",
                MEDIA,
                &json!({"type":"MEDIA_STATUS","status":"bad"}),
                now
            )
            .is_err());
        assert!(state
            .receive(
                "transport-a",
                MEDIA,
                &json!({"type":"MEDIA_STATUS","status":[{}]}),
                now
            )
            .is_err());
    }
    #[test]
    fn frames_handle_every_fragment_boundary_and_reject_oversize() {
        let e = Envelope {
            version: 0,
            source: "receiver-0".into(),
            destination: SENDER.into(),
            namespace: RECEIVER.into(),
            payload_type: 0,
            payload: Some(receiver("a", "b").to_string()),
            binary: None,
        };
        let data = e.encode_to_vec();
        let mut wire = (data.len() as u32).to_be_bytes().to_vec();
        wire.extend(&data);
        for boundary in 0..wire.len() {
            let mut frames = Frames::default();
            frames.push(&wire[..boundary]).unwrap();
            assert!(frames.take().unwrap().is_none());
            frames.push(&wire[boundary..]).unwrap();
            assert_eq!(frames.take().unwrap(), Some(e.clone()));
            assert!(frames.take().unwrap().is_none());
        }
        let mut frames = Frames::default();
        for byte in &wire {
            frames.push(&[*byte]).unwrap();
        }
        assert!(frames.take().unwrap().is_some());
        for size in [0, MAX_FRAME as u32 + 1, u32::MAX] {
            let mut frames = Frames::default();
            frames.push(&size.to_be_bytes()).unwrap();
            assert!(frames.take().is_err());
        }
        let mut frames = Frames::default();
        frames.push(&[0, 0, 0, 1, 255]).unwrap();
        assert!(frames.take().is_err());
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    #[test]
    fn tls_peer_observes_only_active_transport_and_clears_on_malformed_wire() {
        use rustls::pki_types::PrivatePkcs8KeyDer;
        use std::net::TcpListener;
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (observed, wait) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket.set_nodelay(true).unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(cfg)).unwrap(),
                socket,
            );
            fn read(stream: &mut impl Read) -> (Envelope, Value) {
                let mut len = [0; 4];
                stream.read_exact(&mut len).unwrap();
                let len = u32::from_be_bytes(len) as usize;
                assert!(len < MAX_FRAME);
                let mut bytes = vec![0; len];
                stream.read_exact(&mut bytes).unwrap();
                let envelope = Envelope::decode(bytes.as_slice()).unwrap();
                let payload: Value =
                    serde_json::from_str(envelope.payload.as_deref().unwrap()).unwrap();
                assert!(["CONNECT", "GET_STATUS", "PING", "PONG"]
                    .contains(&payload["type"].as_str().unwrap()));
                (envelope, payload)
            }
            fn send(stream: &mut impl Write, source: &str, namespace: &str, payload: Value) {
                let bytes = Envelope {
                    version: 0,
                    source: source.into(),
                    destination: SENDER.into(),
                    namespace: namespace.into(),
                    payload_type: 0,
                    payload: Some(payload.to_string()),
                    binary: None,
                }
                .encode_to_vec();
                let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
                frame.extend(bytes);
                for fragment in frame.chunks(7) {
                    stream.write_all(fragment).unwrap();
                    stream.flush().unwrap();
                }
            }
            let (connect, p) = read(&mut stream);
            assert_eq!(connect.destination, "receiver-0");
            assert_eq!(p["type"], "CONNECT");
            let (_, query) = read(&mut stream);
            assert_eq!(query["type"], "GET_STATUS");
            send(
                &mut stream,
                "receiver-0",
                RECEIVER,
                json!({"type":"RECEIVER_STATUS","status":{"applications":[{"appId":"existing","sessionId":"session","transportId":"transport","displayName":"Existing app","namespaces":[{"name":MEDIA}]}]}}),
            );
            let (connect, p) = read(&mut stream);
            assert_eq!(connect.destination, "transport");
            assert_eq!(p["type"], "CONNECT");
            let (_, query) = read(&mut stream);
            let request = query["requestId"].clone();
            let media = json!({"type":"MEDIA_STATUS","requestId":request,"status":[{"mediaSessionId":7,"playerState":"PAUSED","currentTime":12.,"media":{"duration":90.,"metadata":{"title":"Existing film"}}}]});
            send(&mut stream, "old-transport", MEDIA, media.clone());
            send(&mut stream, "transport", MEDIA, media);
            send(&mut stream, "receiver-0", HEARTBEAT, json!({"type":"PING"}));
            let (_, pong) = read(&mut stream);
            assert_eq!(pong["type"], "PONG");
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            stream.write_all(&[0, 0, 0, 1, 255]).unwrap();
            stream.flush().unwrap();
        });
        let mut observer =
            Observer::connect_port("127.0.0.1".parse().unwrap(), port, Duration::from_secs(2))
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reported = false;
        while Instant::now() < deadline {
            let status = observer.poll(Duration::from_millis(50)).unwrap();
            if let Some(media) = status.now_playing {
                assert_eq!(media.title, "Existing film");
                assert_eq!(media.position, 12.);
                reported = true;
                break;
            }
        }
        assert!(reported);
        observed.send(()).unwrap();
        while Instant::now() < deadline {
            if observer.poll(Duration::from_millis(50)).is_err() {
                break;
            }
        }
        assert!(!observer.status().connected);
        assert!(observer.status().now_playing.is_none());
        peer.join().unwrap();
    }
}
