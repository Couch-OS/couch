//! Samsung Tizen LAN control over the Smart View remote WebSocket
//! (`/api/v2/channels/samsung.remote.control`). One owner drives a persistent
//! socket. Key presses have no acknowledgement and no command is retried.
//!
//! Written without a TV to test against: the wire format follows the
//! samsungtvws library and Home Assistant's Tizen bridge, which this crate
//! cites in `docs/samsung-tizen.md`. Anything a real TV disagrees with is a
//! bug here, not a reason to guess harder.
pub mod discovery;
pub mod rest;
pub mod settings;
use base64::Engine;
use couch_sdk::tls::{self, Socket};
use serde_json::{json, Value};
pub use settings::Settings;
use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};
use url::Url;
pub type Result<T> = std::result::Result<T, Error>;
/// Shown on the TV's "Allow" prompt and in its device list.
pub const CLIENT_NAME: &str = "couch.";
/// Token-authenticated TLS endpoint on 2017+ TVs.
pub const SECURE_PORT: u16 = 8002;
/// Plain endpoint; 2016 TVs only offer this one and issue no token.
pub const LEGACY_PORT: u16 = 8001;
const CHANNEL: &str = "/api/v2/channels/samsung.remote.control";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Transport,
    Protocol,
    Timeout,
    PairingRequired,
    PairingRejected,
    Rejected,
    Certificate,
    Unsupported,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "Invalid Samsung TV configuration or argument",
            Self::Transport => "TV connection failed",
            Self::Protocol => "Invalid response from TV",
            Self::Timeout => "TV did not reply before the deadline",
            Self::PairingRequired => "Pair with the TV and allow Couch on its screen",
            Self::PairingRejected => "The TV denied access or its prompt timed out",
            Self::Rejected => "TV rejected the command",
            Self::Certificate => "TV certificate is missing or has changed; pair again",
            Self::Unsupported => "This TV does not support that operation",
        })
    }
}
impl std::error::Error for Error {}
/// Remote keys the TV understands as `KEY_*` names. Only keys documented by
/// the reference clients are listed; nothing is synthesised from a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Return,
    Exit,
    Home,
    Menu,
    Info,
    Guide,
    Source,
    Tools,
    ChannelUp,
    ChannelDown,
    VolumeUp,
    VolumeDown,
    Mute,
    /// Toggles on every Tizen generation; there is no reliable discrete off.
    Power,
    Play,
    Pause,
    Stop,
    Rewind,
    FastForward,
    Red,
    Green,
    Yellow,
    /// Samsung labels the fourth colour key cyan.
    Blue,
    Tv,
    Hdmi,
    Hdmi1,
    Hdmi2,
    Hdmi3,
    Hdmi4,
}
impl Key {
    pub fn name(self) -> &'static str {
        match self {
            Self::Up => "KEY_UP",
            Self::Down => "KEY_DOWN",
            Self::Left => "KEY_LEFT",
            Self::Right => "KEY_RIGHT",
            Self::Enter => "KEY_ENTER",
            Self::Return => "KEY_RETURN",
            Self::Exit => "KEY_EXIT",
            Self::Home => "KEY_HOME",
            Self::Menu => "KEY_MENU",
            Self::Info => "KEY_INFO",
            Self::Guide => "KEY_GUIDE",
            Self::Source => "KEY_SOURCE",
            Self::Tools => "KEY_TOOLS",
            Self::ChannelUp => "KEY_CHUP",
            Self::ChannelDown => "KEY_CHDOWN",
            Self::VolumeUp => "KEY_VOLUP",
            Self::VolumeDown => "KEY_VOLDOWN",
            Self::Mute => "KEY_MUTE",
            Self::Power => "KEY_POWER",
            Self::Play => "KEY_PLAY",
            Self::Pause => "KEY_PAUSE",
            Self::Stop => "KEY_STOP",
            Self::Rewind => "KEY_REWIND",
            Self::FastForward => "KEY_FF",
            Self::Red => "KEY_RED",
            Self::Green => "KEY_GREEN",
            Self::Yellow => "KEY_YELLOW",
            Self::Blue => "KEY_CYAN",
            Self::Tv => "KEY_TV",
            Self::Hdmi => "KEY_HDMI",
            Self::Hdmi1 => "KEY_HDMI1",
            Self::Hdmi2 => "KEY_HDMI2",
            Self::Hdmi3 => "KEY_HDMI3",
            Self::Hdmi4 => "KEY_HDMI4",
        }
    }
    /// Couch's shared device-command vocabulary. Power is deliberately absent:
    /// `power-on` is Wake-on-LAN and `power-off` needs the Frame TV rule, so
    /// both are handled by the caller rather than mapped to a bare key.
    pub fn for_function(function: &str) -> Option<Self> {
        Some(match function {
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "ok" => Self::Enter,
            "back" => Self::Return,
            "exit" => Self::Exit,
            "home" => Self::Home,
            "menu" => Self::Menu,
            "info" => Self::Info,
            "guide" => Self::Guide,
            "channel-up" => Self::ChannelUp,
            "channel-down" => Self::ChannelDown,
            "volume-up" => Self::VolumeUp,
            "volume-down" => Self::VolumeDown,
            "mute" => Self::Mute,
            "play" => Self::Play,
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "rewind" => Self::Rewind,
            "fast-forward" => Self::FastForward,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "input:tv" => Self::Tv,
            "input:hdmi" => Self::Hdmi,
            "input:hdmi1" => Self::Hdmi1,
            "input:hdmi2" => Self::Hdmi2,
            "input:hdmi3" => Self::Hdmi3,
            "input:hdmi4" => Self::Hdmi4,
            _ => return None,
        })
    }
}
/// Source keys offered as `input:` commands, with labels for pickers. Whether
/// a given HDMI key exists depends on the TV; a missing input is ignored by
/// the TV rather than reported.
pub const INPUTS: &[(&str, &str)] = &[
    ("tv", "TV / tuner"),
    ("hdmi", "HDMI (cycle)"),
    ("hdmi1", "HDMI 1"),
    ("hdmi2", "HDMI 2"),
    ("hdmi3", "HDMI 3"),
    ("hdmi4", "HDMI 4"),
];
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct App {
    pub id: String,
    pub name: String,
    /// 2 = web app launched by deep link, 4 = native app. Anything else is
    /// launched as a deep link, which is what the reference client defaults to.
    pub app_type: u64,
}
/// A base URL: `ws://IP:8001/` or `wss://IP:8002/`, nothing else.
pub(crate) fn endpoint(raw: &str) -> Result<(Url, IpAddr, u16)> {
    let u = Url::parse(raw).map_err(|_| Error::Configuration)?;
    if !matches!(u.scheme(), "ws" | "wss")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err(Error::Configuration);
    }
    let host = u
        .host_str()
        .ok_or(Error::Configuration)?
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .map_err(|_| Error::Configuration)?;
    if host.is_unspecified() || host.is_multicast() {
        return Err(Error::Configuration);
    }
    let port = u.port_or_known_default().ok_or(Error::Configuration)?;
    if port == 0 {
        return Err(Error::Configuration);
    }
    Ok((u, host, port))
}
pub fn base_url(address: IpAddr, secure: bool) -> String {
    let host = match address {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    if secure {
        format!("wss://{host}:{SECURE_PORT}/")
    } else {
        format!("ws://{host}:{LEGACY_PORT}/")
    }
}
/// The TV reads its client name from a base64 query parameter.
fn channel_url(base: &str, token: Option<&str>) -> Result<String> {
    let (url, _, _) = endpoint(base)?;
    if url.path() != "/" || url.query().is_some() {
        return Err(Error::Configuration);
    }
    let mut channel = format!(
        "{}{CHANNEL}?name={}",
        base.trim_end_matches('/'),
        base64::engine::general_purpose::STANDARD.encode(CLIENT_NAME)
    );
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        if token.len() > 256 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::Configuration);
        }
        channel.push_str("&token=");
        channel.push_str(token);
    }
    Ok(channel)
}
/// Reported by the pinned verifier and shown to the user, so it names the TV.
const CERTIFICATE_CHANGED: &str = "Samsung TV certificate changed; pair again";
fn connect_socket(
    base: &str,
    token: Option<&str>,
    pin: Arc<Mutex<Vec<u8>>>,
    timeout: Duration,
) -> Result<WebSocket<Socket>> {
    let (url, host, port) = endpoint(base)?;
    let channel = channel_url(base, token)?;
    let tcp = TcpStream::connect_timeout(&SocketAddr::new(host, port), timeout)
        .map_err(|_| Error::Transport)?;
    tcp.set_read_timeout(Some(timeout))
        .map_err(|_| Error::Transport)?;
    tcp.set_write_timeout(Some(timeout))
        .map_err(|_| Error::Transport)?;
    let socket = if url.scheme() == "wss" {
        let cfg = tls::pinned_client_config(Arc::new(tls::Pin::new(pin, CERTIFICATE_CHANGED)))
            .map_err(|_| Error::Transport)?;
        let name = rustls::pki_types::ServerName::IpAddress(host.into());
        let conn =
            rustls::ClientConnection::new(Arc::new(cfg), name).map_err(|_| Error::Transport)?;
        Socket::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        Socket::Plain(tcp)
    };
    let mut cfg = tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(1024 * 1024);
    cfg.max_frame_size = Some(1024 * 1024);
    tungstenite::client::client_with_config(channel.as_str(), socket, Some(cfg))
        .map(|(s, _)| s)
        .map_err(|e| match e {
            // A pin mismatch surfaces from rustls as an I/O error whose text
            // is the verifier's own message.
            tungstenite::HandshakeError::Failure(tungstenite::Error::Io(io))
                if io.to_string().contains("certificate") =>
            {
                Error::Certificate
            }
            _ => Error::Transport,
        })
}
pub struct Client {
    socket: WebSocket<Socket>,
    base: String,
    token: Option<String>,
    timeout: Duration,
    apps: Option<Vec<App>>,
    /// Events read while waiting for something else. Bounded; the TV chatters
    /// about app-list updates and IME state that nobody here consumes.
    events: VecDeque<Value>,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TizenClient")
            .field("endpoint", &self.base)
            .finish_non_exhaustive()
    }
}
impl Client {
    /// Explicit pairing only. The TV shows an Allow / Deny prompt for the
    /// client name; the user has about thirty seconds to answer it. Returned
    /// settings must be saved by the caller after adding REST details.
    pub fn pair(base: &str) -> Result<(Self, Settings)> {
        let pin = Arc::new(Mutex::new(Vec::new()));
        let mut client = Self::open(base, None, pin.clone())?;
        let token = client.handshake(Duration::from_secs(60))?;
        let secure = client.base.starts_with("wss://");
        if secure && token.is_none() {
            // A 2017+ TV that lets the socket through without issuing a token
            // will prompt on every connection. Do not save that as a pairing.
            return Err(Error::PairingRequired);
        }
        let settings = Settings {
            url: base.into(),
            token: token.clone().unwrap_or_default(),
            certificate: pin.lock().unwrap().clone(),
            mac: None,
            frame_tv: false,
            model: String::new(),
            name: String::new(),
        };
        client.token = token;
        Ok((client, settings))
    }
    pub fn connect(settings: &Settings) -> Result<Self> {
        settings.validate()?;
        let pin = Arc::new(Mutex::new(settings.certificate.clone()));
        let token = (!settings.token.is_empty()).then(|| settings.token.clone());
        let mut client = Self::open(&settings.url, token.as_deref(), pin)?;
        let issued = client.handshake(Duration::from_secs(8))?;
        client.token = issued.or(token);
        Ok(client)
    }
    /// The token the TV last confirmed. It differs from the saved one only
    /// when the user approved a fresh prompt; callers should persist it.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
    fn open(base: &str, token: Option<&str>, pin: Arc<Mutex<Vec<u8>>>) -> Result<Self> {
        let timeout = Duration::from_secs(4);
        Ok(Self {
            socket: connect_socket(base, token, pin, timeout)?,
            base: base.into(),
            token: None,
            timeout,
            apps: None,
            events: VecDeque::new(),
        })
    }
    fn write(&mut self, value: Value) -> Result<()> {
        self.socket
            .get_ref()
            .timeout(self.timeout)
            .map_err(|_| Error::Transport)?;
        self.socket
            .send(Message::Text(value.to_string().into()))
            .map_err(|_| Error::Transport)
    }
    fn read(&mut self, until: Instant) -> Result<Value> {
        loop {
            let left = until
                .checked_duration_since(Instant::now())
                .ok_or(Error::Timeout)?;
            self.socket
                .get_ref()
                .timeout(left)
                .map_err(|_| Error::Transport)?;
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return serde_json::from_str(&text).map_err(|_| Error::Protocol)
                }
                Ok(Message::Ping(_)) => {
                    self.socket.flush().map_err(|_| Error::Transport)?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return Err(Error::Transport),
                Ok(_) => return Err(Error::Protocol),
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(Error::Timeout)
                }
                Err(_) => return Err(Error::Transport),
            }
        }
    }
    /// Wait for `ms.channel.connect`. Anything the TV says about authorisation
    /// before that is final; other startup chatter is ignored.
    fn handshake(&mut self, wait: Duration) -> Result<Option<String>> {
        let until = Instant::now() + wait;
        loop {
            let value = self.read(until)?;
            match value["event"].as_str() {
                Some("ms.channel.connect") => {
                    let token = match &value["data"]["token"] {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    }
                    .filter(|t| !t.is_empty());
                    if token.as_ref().is_some_and(|t| {
                        t.len() > 256 || !t.bytes().all(|b| b.is_ascii_alphanumeric())
                    }) {
                        return Err(Error::Protocol);
                    }
                    return Ok(token);
                }
                Some("ms.channel.unauthorized") | Some("ms.channel.timeOut") => {
                    return Err(Error::PairingRejected)
                }
                Some("ms.error") => return Err(Error::Rejected),
                _ => {}
            }
        }
    }
    fn queue(&mut self, value: Value) {
        if self.events.len() >= 64 {
            self.events.pop_front();
        }
        self.events.push_back(value);
    }
    /// Wait for one event by name, queueing unrelated ones.
    fn wait_for(&mut self, event: &str, wait: Duration) -> Result<Value> {
        if let Some(i) = self.events.iter().position(|v| v["event"] == event) {
            return Ok(self.events.remove(i).unwrap());
        }
        let until = Instant::now() + wait;
        loop {
            let value = self.read(until)?;
            if value["event"] == event {
                return Ok(value);
            }
            if value["event"] == "ms.channel.unauthorized" {
                return Err(Error::PairingRequired);
            }
            self.queue(value);
        }
    }
    /// Service pings and detect a closed socket without blocking. Call this
    /// while idle; the TV drops silent clients and reports nothing on a key
    /// sent into a dead socket.
    pub fn idle(&mut self) -> Result<()> {
        let until = Instant::now() + Duration::from_millis(1);
        for _ in 0..8 {
            match self.read(until) {
                Ok(value) => self.queue(value),
                Err(Error::Timeout) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn remote(&mut self, command: &str, key: Key) -> Result<()> {
        self.write(json!({"method":"ms.remote.control","params":{"Cmd":command,"DataOfCmd":key.name(),"Option":"false","TypeOfRemote":"SendRemoteKey"}}))
    }
    /// One click. The protocol has no per-key acknowledgement.
    pub fn key(&mut self, key: Key) -> Result<()> {
        self.idle()?;
        self.remote("Click", key)
    }
    /// Press, hold, release. The Frame leaves Art Mode for standby only when
    /// power is held for about three seconds.
    pub fn hold_key(&mut self, key: Key, held: Duration) -> Result<()> {
        self.idle()?;
        self.remote("Press", key)?;
        std::thread::sleep(held.min(Duration::from_secs(5)));
        self.remote("Release", key)
    }
    /// Tizen has no discrete off key; this is the toggle, held on The Frame.
    pub fn power_toggle(&mut self, frame_tv: bool) -> Result<()> {
        if frame_tv {
            self.hold_key(Key::Power, Duration::from_secs(3))
        } else {
            self.key(Key::Power)
        }
    }
    /// Installed apps, when the TV supports the query (most 2017+ do).
    pub fn apps(&mut self) -> Result<Vec<App>> {
        self.write(json!({"method":"ms.channel.emit","params":{"event":"ed.installedApp.get","to":"host"}}))?;
        let reply = match self.wait_for("ed.installedApp.get", Duration::from_secs(6)) {
            Ok(reply) => reply,
            Err(Error::Timeout) => return Err(Error::Unsupported),
            Err(e) => return Err(e),
        };
        let apps = parse_apps(&reply)?;
        self.apps = Some(apps.clone());
        Ok(apps)
    }
    /// Launch by app ID. Web apps take a deep link and native apps a native
    /// launch; the type comes from the TV's own app list when it has one.
    pub fn launch_app(&mut self, id: &str) -> Result<()> {
        valid_id(id)?;
        if self.apps.is_none() {
            let _ = self.apps();
        }
        let native = self
            .apps
            .as_ref()
            .and_then(|apps| apps.iter().find(|a| a.id == id))
            .is_some_and(|a| a.app_type == 4);
        self.write(json!({"method":"ms.channel.emit","params":{"event":"ed.apps.launch","to":"host","data":{"appId":id,"action_type":if native {"NATIVE_LAUNCH"} else {"DEEP_LINK"},"metaTag":""}}}))?;
        // Newer firmware answers with a status; older firmware says nothing.
        match self.wait_for("ed.apps.launch", Duration::from_millis(1500)) {
            Ok(reply) => match reply["data"]
                .as_i64()
                .or_else(|| reply["data"]["status"].as_i64())
            {
                Some(200) | None => Ok(()),
                Some(_) => Err(Error::Rejected),
            },
            Err(Error::Timeout) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
fn parse_apps(reply: &Value) -> Result<Vec<App>> {
    let list = reply["data"]["data"].as_array().ok_or(Error::Protocol)?;
    let mut apps = Vec::new();
    for entry in list.iter().take(256) {
        let Some(id) = entry["appId"].as_str() else {
            continue;
        };
        if valid_id(id).is_err() {
            continue;
        }
        let name: String = entry["name"]
            .as_str()
            .unwrap_or(id)
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        if apps.iter().any(|a: &App| a.id == id) {
            continue;
        }
        apps.push(App {
            id: id.into(),
            name: if name.is_empty() { id.into() } else { name },
            app_type: entry["app_type"].as_u64().unwrap_or(2),
        });
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}
fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        Err(Error::Configuration)
    } else {
        Ok(())
    }
}
pub fn magic_packet(mac: &str) -> Result<[u8; 102]> {
    couch_sdk::wol::magic_packet(mac).ok_or(Error::Configuration)
}
/// A TV in standby answers nothing on the remote channel. Waking needs the
/// MAC the TV reported while paired and its network-standby setting enabled.
/// Sending the packet proves nothing about the TV; check its REST state after.
pub fn wake(mac: &str, broadcast: std::net::Ipv4Addr) -> Result<()> {
    couch_sdk::wol::wake(&magic_packet(mac)?, broadcast).map_err(|_| Error::Transport)
}
/// Wake the paired TV: limited broadcast plus a directed packet to its last
/// address, which reaches it while the router still has its ARP entry.
pub fn wake_paired(settings: &Settings) -> Result<()> {
    let mac = settings.mac.as_deref().ok_or(Error::Unsupported)?;
    wake(mac, std::net::Ipv4Addr::BROADCAST)?;
    if let Ok(IpAddr::V4(address)) = settings.address() {
        let _ = wake(mac, address);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]
    use super::*;
    use tungstenite::handshake::server::{Request, Response};
    fn fixture(
        f: impl FnOnce(WebSocket<TcpStream>, String) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut path = String::new();
            let ws = tungstenite::accept_hdr(
                listener.accept().unwrap().0,
                |req: &Request, res: Response| {
                    path = req.uri().to_string();
                    Ok(res)
                },
            )
            .unwrap();
            f(ws, path)
        });
        (url, handle)
    }
    fn receive(ws: &mut WebSocket<TcpStream>) -> Value {
        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
    }
    fn send(ws: &mut WebSocket<TcpStream>, v: Value) {
        ws.send(Message::Text(v.to_string().into())).unwrap();
    }
    fn connected(token: Option<&str>) -> Value {
        let mut data = json!({"clients":[{"attributes":{"name":"Y291Y2gu"},"connectTime":1,"deviceName":"Y291Y2gu","id":"a","isHost":false}],"id":"a"});
        if let Some(token) = token {
            data["token"] = json!(token);
        }
        json!({"event":"ms.channel.connect","data":data})
    }
    #[test]
    fn pairing_names_the_client_and_keeps_the_issued_token_private() {
        let (url, server) = fixture(|mut ws, path| {
            assert_eq!(
                path,
                "/api/v2/channels/samsung.remote.control?name=Y291Y2gu"
            );
            send(
                &mut ws,
                json!({"event":"ms.channel.clientConnect","data":{}}),
            );
            send(&mut ws, connected(Some("12345678")));
            let key = receive(&mut ws);
            assert_eq!(key["method"], "ms.remote.control");
            assert_eq!(key["params"]["Cmd"], "Click");
            assert_eq!(key["params"]["DataOfCmd"], "KEY_VOLUP");
            assert_eq!(key["params"]["TypeOfRemote"], "SendRemoteKey");
        });
        let (mut client, settings) = Client::pair(&url).unwrap();
        assert_eq!(settings.token, "12345678");
        assert_eq!(client.token(), Some("12345678"));
        assert!(!format!("{settings:?}").contains("12345678"));
        client.key(Key::VolumeUp).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn saved_token_is_sent_and_a_renewed_token_is_reported() {
        let (url, server) = fixture(|mut ws, path| {
            assert!(path.ends_with("?name=Y291Y2gu&token=oldtoken"));
            send(&mut ws, connected(Some("newtoken")));
        });
        let settings = Settings {
            url,
            token: "oldtoken".into(),
            certificate: vec![],
            mac: None,
            frame_tv: false,
            model: String::new(),
            name: String::new(),
        };
        let client = Client::connect(&settings).unwrap();
        assert_eq!(client.token(), Some("newtoken"));
        server.join().unwrap();
    }
    #[test]
    fn denied_prompt_is_rejected_and_legacy_pairing_needs_no_token() {
        let (url, server) = fixture(|mut ws, _| {
            send(
                &mut ws,
                json!({"event":"ms.channel.unauthorized","data":{}}),
            );
        });
        assert_eq!(Client::pair(&url).unwrap_err(), Error::PairingRejected);
        server.join().unwrap();
        let (url, server) = fixture(|mut ws, _| {
            send(&mut ws, connected(None));
        });
        let (_, settings) = Client::pair(&url).unwrap();
        assert_eq!(settings.token, "");
        assert!(settings.validate().is_ok());
        server.join().unwrap();
    }
    #[test]
    fn app_list_waits_through_unrelated_events_and_launch_uses_app_type() {
        let (url, server) = fixture(|mut ws, _| {
            send(&mut ws, connected(Some("t")));
            let request = receive(&mut ws);
            assert_eq!(request["method"], "ms.channel.emit");
            assert_eq!(request["params"]["event"], "ed.installedApp.get");
            assert_eq!(request["params"]["to"], "host");
            send(
                &mut ws,
                json!({"event":"ed.edenTV.update","from":"host","data":{"update_type":"ed.edenApp.update"}}),
            );
            send(
                &mut ws,
                json!({"event":"ed.installedApp.get","from":"host","data":{"data":[
                {"appId":"111299001912","app_type":2,"icon":"/x.png","is_lock":0,"name":"YouTube"},
                {"appId":"3201606009684","app_type":4,"icon":"/y.png","is_lock":0,"name":"Spotify"},
                {"appId":"bad id!","app_type":2,"name":"Nope"},
                {"appId":"111299001912","app_type":2,"name":"Duplicate"}]}}),
            );
            let launch = receive(&mut ws);
            assert_eq!(launch["params"]["event"], "ed.apps.launch");
            assert_eq!(launch["params"]["data"]["appId"], "3201606009684");
            assert_eq!(launch["params"]["data"]["action_type"], "NATIVE_LAUNCH");
            send(
                &mut ws,
                json!({"event":"ed.apps.launch","from":"host","data":200}),
            );
            let launch = receive(&mut ws);
            assert_eq!(launch["params"]["data"]["appId"], "111299001912");
            assert_eq!(launch["params"]["data"]["action_type"], "DEEP_LINK");
            send(
                &mut ws,
                json!({"event":"ed.apps.launch","from":"host","data":404}),
            );
        });
        let (mut client, _) = Client::pair(&url).unwrap();
        let apps = client.apps().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Spotify");
        assert_eq!(apps[1].id, "111299001912");
        client.launch_app("3201606009684").unwrap();
        assert_eq!(client.launch_app("111299001912"), Err(Error::Rejected));
        assert_eq!(client.launch_app("../etc"), Err(Error::Configuration));
        server.join().unwrap();
    }
    #[test]
    fn frame_power_is_held_and_ordinary_power_is_clicked() {
        let (url, server) = fixture(|mut ws, _| {
            send(&mut ws, connected(Some("t")));
            let click = receive(&mut ws);
            assert_eq!(click["params"]["Cmd"], "Click");
            assert_eq!(click["params"]["DataOfCmd"], "KEY_POWER");
            let press = receive(&mut ws);
            assert_eq!(press["params"]["Cmd"], "Press");
            let release = receive(&mut ws);
            assert_eq!(release["params"]["Cmd"], "Release");
            assert_eq!(release["params"]["DataOfCmd"], "KEY_POWER");
        });
        let (mut client, _) = Client::pair(&url).unwrap();
        client.power_toggle(false).unwrap();
        client
            .hold_key(Key::Power, Duration::from_millis(10))
            .unwrap();
        server.join().unwrap();
    }
    #[test]
    fn closed_socket_is_reported_before_the_next_key() {
        let (url, server) = fixture(|mut ws, _| {
            send(&mut ws, connected(Some("t")));
            ws.close(None).unwrap();
            // Drop without reading the client's close reply: the client is
            // still held by the test, so waiting here would deadlock.
        });
        let (mut client, _) = Client::pair(&url).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(client.key(Key::Home), Err(Error::Transport));
        server.join().unwrap();
    }
    #[test]
    fn functions_map_to_documented_keys_only() {
        assert_eq!(Key::for_function("ok"), Some(Key::Enter));
        assert_eq!(Key::for_function("back"), Some(Key::Return));
        assert_eq!(Key::for_function("blue").map(Key::name), Some("KEY_CYAN"));
        assert_eq!(Key::for_function("input:hdmi2"), Some(Key::Hdmi2));
        assert_eq!(Key::for_function("power-off"), None);
        assert_eq!(Key::for_function("power-on"), None);
        assert_eq!(Key::for_function("next"), None);
        for (id, _) in INPUTS {
            assert!(Key::for_function(&format!("input:{id}")).is_some());
        }
    }
    #[test]
    fn urls_are_strict_and_wake_packet_repeats_mac() {
        assert!(channel_url("wss://192.168.1.20:8002/", Some("abc"))
            .unwrap()
            .ends_with("&token=abc"));
        assert!(channel_url("wss://192.168.1.20:8002/", Some("a b")).is_err());
        assert!(channel_url("wss://192.168.1.20:8002/path", None).is_err());
        assert!(channel_url("http://192.168.1.20:8002/", None).is_err());
        assert!(channel_url("ws://0.0.0.0:8001/", None).is_err());
        assert_eq!(
            base_url("192.168.1.20".parse().unwrap(), true),
            "wss://192.168.1.20:8002/"
        );
        assert_eq!(
            base_url("192.168.1.20".parse().unwrap(), false),
            "ws://192.168.1.20:8001/"
        );
        let packet = magic_packet("8C:C8:CD:11:22:33").unwrap();
        assert_eq!(&packet[..6], &[255; 6]);
        for i in 0..16 {
            assert_eq!(
                &packet[6 + i * 6..12 + i * 6],
                &[0x8c, 0xc8, 0xcd, 0x11, 0x22, 0x33]
            );
        }
        assert!(magic_packet("none").is_err());
    }
    #[test]
    fn settings_are_private_bound_and_validated() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "couch-tizen-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("tizen-connection.json");
        let mut settings = Settings {
            url: "wss://192.0.2.1:8002/".into(),
            token: "12345678".into(),
            certificate: vec![],
            mac: Some("8C:C8:CD:11:22:33".into()),
            frame_tv: true,
            model: "QE55".into(),
            name: "[TV] Test".into(),
        };
        assert_eq!(settings.save(&path), Err(Error::Certificate));
        settings.certificate = vec![1, 2, 3];
        settings.save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.token, "12345678");
        assert!(loaded.frame_tv);
        assert_eq!(
            loaded.address().unwrap(),
            "192.0.2.1".parse::<IpAddr>().unwrap()
        );
        settings.mac = Some("not a mac".into());
        assert!(settings.validate().is_err());
        std::fs::write(&path, b"{}").unwrap();
        assert!(Settings::load(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
