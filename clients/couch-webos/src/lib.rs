//! LG webOS LAN control using SSAP. One owner drives a persistent WebSocket;
//! subscriptions interleave with request replies. No command is automatically retried.
pub mod power;
pub mod settings;
use couch_sdk::tls::{self, Socket};
use serde_json::{json, Value};
pub use settings::Settings;
use std::{
    collections::{HashSet, VecDeque},
    fmt,
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};
use url::Url;
pub type Result<T> = std::result::Result<T, Error>;
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
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "Invalid webOS configuration or argument",
            Self::Transport => "TV connection failed",
            Self::Protocol => "Invalid response from TV",
            Self::Timeout => "TV did not reply before the deadline",
            Self::PairingRequired => "Pair with the TV and accept its on-screen request",
            Self::PairingRejected => "TV pairing was rejected or cancelled",
            Self::Rejected => "TV rejected the command",
            Self::Certificate => "TV certificate is missing or has changed; pair again",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    Home,
    Menu,
    Info,
    Exit,
    Red,
    Green,
    Yellow,
    Blue,
}
impl Button {
    fn name(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
            Self::Enter => "ENTER",
            Self::Back => "BACK",
            Self::Home => "HOME",
            Self::Menu => "MENU",
            Self::Info => "INFO",
            Self::Exit => "EXIT",
            Self::Red => "RED",
            Self::Green => "GREEN",
            Self::Yellow => "YELLOW",
            Self::Blue => "BLUE",
        }
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum Playback {
    Play,
    Pause,
    Stop,
    Rewind,
    FastForward,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Update {
    pub subscription: String,
    pub payload: Value,
}
fn endpoint(raw: &str) -> Result<(Url, IpAddr, u16)> {
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
/// The sentence the pinned verifier reports; it names the device rather than
/// the certificate, because it is shown on the panel and in the browser.
const CERTIFICATE_CHANGED: &str = "LG TV certificate changed; pair again";
fn connect_socket(
    raw: &str,
    pin: Arc<Mutex<Vec<u8>>>,
    timeout: Duration,
) -> Result<WebSocket<Socket>> {
    let (url, host, port) = endpoint(raw)?;
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
    tungstenite::client::client_with_config(raw, socket, Some(cfg))
        .map(|(s, _)| s)
        .map_err(|_| Error::Transport)
}
fn manifest(key: Option<&str>) -> Value {
    let mut payload = json!({"forcePairing":false,"pairingType":"PROMPT","manifest":{"manifestVersion":1,"appVersion":"1.0","permissions":["CONTROL_AUDIO","CONTROL_INPUT_TV","CONTROL_INPUT_MEDIA_PLAYBACK","CONTROL_MOUSE_AND_KEYBOARD","CONTROL_POWER","READ_POWER_STATE","READ_INPUT_DEVICE_LIST","READ_INSTALLED_APPS","READ_RUNNING_APPS","READ_CURRENT_CHANNEL","LAUNCH"]}});
    if let Some(key) = key {
        payload["client-key"] = json!(key)
    }
    json!({"id":"register","type":"register","payload":payload})
}
pub struct Client {
    socket: WebSocket<Socket>,
    pointer: Option<WebSocket<Socket>>,
    url: String,
    pin: Arc<Mutex<Vec<u8>>>,
    next: u64,
    timeout: Duration,
    subscriptions: HashSet<String>,
    updates: VecDeque<Update>,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebOsClient")
            .field("endpoint", &self.url)
            .finish_non_exhaustive()
    }
}
impl Client {
    /// Explicit pairing only. The caller must ask the user to approve on the TV.
    /// The returned credentials should be saved only after registration succeeds.
    pub fn pair(url: &str) -> Result<(Self, Settings)> {
        let (control, _, _) = endpoint(url)?;
        if control.path() != "/" || control.query().is_some() {
            return Err(Error::Configuration);
        }
        let pin = Arc::new(Mutex::new(Vec::new()));
        let mut client = Self::open(url, pin.clone())?;
        let key = client.register(None, Duration::from_secs(60))?;
        let settings = Settings {
            url: url.into(),
            client_key: key,
            certificate: pin.lock().unwrap().clone(),
        };
        Ok((client, settings))
    }
    pub fn connect(settings: &Settings) -> Result<Self> {
        settings.validate()?;
        let pin = Arc::new(Mutex::new(settings.certificate.clone()));
        let mut client = Self::open(&settings.url, pin)?;
        client.register(Some(&settings.client_key), Duration::from_secs(5))?;
        Ok(client)
    }
    fn open(url: &str, pin: Arc<Mutex<Vec<u8>>>) -> Result<Self> {
        let timeout = Duration::from_secs(4);
        Ok(Self {
            socket: connect_socket(url, pin.clone(), timeout)?,
            pointer: None,
            url: url.into(),
            pin,
            next: 0,
            timeout,
            subscriptions: HashSet::new(),
            updates: VecDeque::new(),
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
    fn register(&mut self, key: Option<&str>, timeout: Duration) -> Result<String> {
        self.write(manifest(key))?;
        let until = Instant::now() + timeout;
        loop {
            let value = self.read(until)?;
            if value["type"] == "error" {
                return Err(Error::PairingRejected);
            }
            if value["type"] == "registered" {
                return value["payload"]["client-key"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 4096)
                    .map(str::to_string)
                    .ok_or(Error::Protocol);
            }
            if key.is_some() && value["payload"]["pairingType"].is_string() {
                return Err(Error::PairingRequired);
            }
        }
    }
    fn queue(&mut self, value: Value) {
        if let Some(id) = value["id"].as_str() {
            if self.subscriptions.contains(id) {
                if self.updates.len() >= 128 {
                    self.updates.pop_front();
                }
                self.updates.push_back(Update {
                    subscription: id.into(),
                    payload: value["payload"].clone(),
                });
            }
        }
    }
    fn request_kind(&mut self, kind: &str, uri: &str, payload: Value) -> Result<(String, Value)> {
        if !uri.starts_with("ssap://") || uri.len() > 256 || uri.chars().any(char::is_control) {
            return Err(Error::Configuration);
        }
        self.next += 1;
        let id = format!("couch-{}", self.next);
        self.write(json!({"id":id,"type":kind,"uri":uri,"payload":payload}))?;
        let until = Instant::now() + self.timeout;
        loop {
            let value = self.read(until)?;
            if value["id"] == id {
                if value["type"] == "error" || value["payload"]["returnValue"] == false {
                    return Err(Error::Rejected);
                }
                if value["type"] != "response" {
                    return Err(Error::Protocol);
                }
                return Ok((id, value["payload"].clone()));
            }
            self.queue(value);
        }
    }
    pub fn request(&mut self, uri: &str, payload: Value) -> Result<Value> {
        self.request_kind("request", uri, payload).map(|(_, v)| v)
    }
    pub fn subscribe(&mut self, uri: &str) -> Result<Update> {
        let (id, payload) = self.request_kind("subscribe", uri, json!({}))?;
        self.subscriptions.insert(id.clone());
        Ok(Update {
            subscription: id,
            payload,
        })
    }
    pub fn unsubscribe(&mut self, id: &str) -> Result<()> {
        if !self.subscriptions.remove(id) {
            return Err(Error::Configuration);
        }
        self.updates.retain(|v| v.subscription != id);
        self.write(json!({"id":id,"type":"unsubscribe"}))
    }
    pub fn next_update(&mut self, timeout: Duration) -> Result<Option<Update>> {
        if let Some(v) = self.updates.pop_front() {
            return Ok(Some(v));
        }
        let until = Instant::now() + timeout;
        loop {
            match self.read(until) {
                Ok(v) => {
                    self.queue(v);
                    if let Some(v) = self.updates.pop_front() {
                        return Ok(Some(v));
                    }
                }
                Err(Error::Timeout) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
    pub fn volume(&mut self) -> Result<Value> {
        self.request("ssap://audio/getVolume", json!({}))
    }
    pub fn set_volume(&mut self, volume: u8) -> Result<()> {
        if volume > 100 {
            return Err(Error::Configuration);
        }
        self.request("ssap://audio/setVolume", json!({"volume":volume}))
            .map(|_| ())
    }
    pub fn volume_up(&mut self) -> Result<()> {
        self.request("ssap://audio/volumeUp", json!({})).map(|_| ())
    }
    pub fn volume_down(&mut self) -> Result<()> {
        self.request("ssap://audio/volumeDown", json!({}))
            .map(|_| ())
    }
    pub fn mute(&mut self, on: bool) -> Result<()> {
        self.request("ssap://audio/setMute", json!({"mute":on}))
            .map(|_| ())
    }
    pub fn power_state(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.service.tvpower/power/getPowerState",
            json!({}),
        )
    }
    pub fn power_off(&mut self) -> Result<()> {
        self.request("ssap://system/turnOff", json!({})).map(|_| ())
    }
    pub fn inputs(&mut self) -> Result<Value> {
        self.request("ssap://tv/getExternalInputList", json!({}))
    }
    pub fn select_input(&mut self, id: &str) -> Result<()> {
        valid_id(id)?;
        self.request("ssap://tv/switchInput", json!({"inputId":id}))
            .map(|_| ())
    }
    pub fn apps(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.applicationManager/listLaunchPoints",
            json!({}),
        )
    }
    pub fn launch_app(&mut self, id: &str) -> Result<()> {
        valid_id(id)?;
        self.request("ssap://system.launcher/launch", json!({"id":id}))
            .map(|_| ())
    }
    pub fn foreground_app(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            json!({}),
        )
    }
    pub fn playback(&mut self, command: Playback) -> Result<()> {
        self.request(
            match command {
                Playback::Play => "ssap://media.controls/play",
                Playback::Pause => "ssap://media.controls/pause",
                Playback::Stop => "ssap://media.controls/stop",
                Playback::Rewind => "ssap://media.controls/rewind",
                Playback::FastForward => "ssap://media.controls/fastForward",
            },
            json!({}),
        )
        .map(|_| ())
    }
    /// Open the navigation socket without sending a key.
    pub fn prepare_input(&mut self) -> Result<()> {
        if self.pointer.is_none() {
            let v = self.request(
                "ssap://com.webos.service.networkinput/getPointerInputSocket",
                json!({}),
            )?;
            let raw = v["socketPath"].as_str().ok_or(Error::Protocol)?;
            let (u, host, port) = endpoint(raw)?;
            let (_, own_host, own_port) = endpoint(&self.url)?;
            if host != own_host
                || port != own_port
                || u.scheme() != Url::parse(&self.url).unwrap().scheme()
            {
                return Err(Error::Protocol);
            }
            self.pointer = Some(connect_socket(raw, self.pin.clone(), self.timeout)?);
        }
        Ok(())
    }
    pub fn channel(&mut self, up: bool) -> Result<()> {
        self.request(
            if up {
                "ssap://tv/channelUp"
            } else {
                "ssap://tv/channelDown"
            },
            json!({}),
        )
        .map(|_| ())
    }
    pub fn button(&mut self, button: Button) -> Result<()> {
        self.prepare_input()?;
        let pointer = self.pointer.as_mut().unwrap();
        // Service idle pings and detect a closed pointer before sending a new key.
        pointer
            .get_ref()
            .timeout(Duration::from_millis(1))
            .map_err(|_| Error::Transport)?;
        for _ in 0..8 {
            match pointer.read() {
                Ok(Message::Ping(_)) => {
                    pointer.flush().map_err(|_| Error::Transport)?;
                }
                Ok(Message::Close(_)) => {
                    self.pointer = None;
                    return Err(Error::Transport);
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(_) => {
                    self.pointer = None;
                    return Err(Error::Transport);
                }
            }
        }
        pointer
            .get_ref()
            .timeout(self.timeout)
            .map_err(|_| Error::Transport)?;
        if pointer
            .send(Message::Text(
                format!("type:button\nname:{}\n\n", button.name()).into(),
            ))
            .is_err()
        {
            self.pointer = None;
            return Err(Error::Transport);
        }
        Ok(()) // Pointer protocol has no per-button acknowledgement.
    }
}
fn valid_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(Error::Configuration)
    } else {
        Ok(())
    }
}
pub fn magic_packet(mac: &str) -> Result<[u8; 102]> {
    couch_sdk::wol::magic_packet(mac).ok_or(Error::Configuration)
}
/// A sleeping TV cannot answer SSAP. WOL needs its MAC and an enabled TV setting.
pub fn wake(mac: &str, broadcast: std::net::Ipv4Addr) -> Result<()> {
    couch_sdk::wol::wake(&magic_packet(mac)?, broadcast).map_err(|_| Error::Transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(
        f: impl FnOnce(WebSocket<TcpStream>) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            f(tungstenite::accept(listener.accept().unwrap().0).unwrap())
        });
        (url, handle)
    }
    fn receive(ws: &mut WebSocket<TcpStream>) -> Value {
        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
    }
    fn send(ws: &mut WebSocket<TcpStream>, v: Value) {
        ws.send(Message::Text(v.to_string().into())).unwrap();
    }
    #[test]
    fn pairing_and_interleaved_subscription_do_not_confuse_command_reply() {
        let (url, server) = fixture(|mut ws| {
            assert_eq!(receive(&mut ws)["type"], "register");
            send(
                &mut ws,
                json!({"type":"registered","payload":{"client-key":"test-private-key"}}),
            );
            let sub = receive(&mut ws);
            assert_eq!(sub["type"], "subscribe");
            send(
                &mut ws,
                json!({"id":sub["id"],"type":"response","payload":{"volume":12}}),
            );
            let command = receive(&mut ws);
            assert_eq!(command["payload"]["mute"], false);
            send(
                &mut ws,
                json!({"id":sub["id"],"type":"response","payload":{"volume":13}}),
            );
            send(
                &mut ws,
                json!({"id":command["id"],"type":"response","payload":{"returnValue":true}}),
            );
        });
        let (mut c, s) = Client::pair(&url).unwrap();
        assert_eq!(s.client_key, "test-private-key");
        assert!(!format!("{s:?}").contains("test-private-key"));
        assert_eq!(
            c.subscribe("ssap://audio/getVolume").unwrap().payload["volume"],
            12
        );
        c.mute(false).unwrap();
        assert_eq!(
            c.next_update(Duration::from_millis(10))
                .unwrap()
                .unwrap()
                .payload["volume"],
            13
        );
        server.join().unwrap();
    }
    #[test]
    fn rejected_commands_are_not_reported_as_success_or_retried() {
        let (url, server) = fixture(|mut ws| {
            receive(&mut ws);
            send(
                &mut ws,
                json!({"type":"registered","payload":{"client-key":"test-key"}}),
            );
            let request = receive(&mut ws);
            assert_eq!(request["payload"]["volume"], 40);
            send(
                &mut ws,
                json!({"type":"response","id":request["id"],"payload":{"returnValue":false}}),
            );
        });
        let (mut c, _) = Client::pair(&url).unwrap();
        assert_eq!(c.set_volume(101), Err(Error::Configuration));
        assert_eq!(c.set_volume(40), Err(Error::Rejected));
        server.join().unwrap();
    }
    #[test]
    fn saved_key_prompt_requires_explicit_repair() {
        let (url, server) = fixture(|mut ws| {
            assert_eq!(receive(&mut ws)["payload"]["client-key"], "old-key");
            send(
                &mut ws,
                json!({"type":"response","payload":{"pairingType":"PROMPT"}}),
            );
        });
        let settings = Settings {
            url,
            client_key: "old-key".into(),
            certificate: vec![],
        };
        assert!(matches!(
            Client::connect(&settings),
            Err(Error::PairingRequired)
        ));
        server.join().unwrap();
    }
    #[test]
    fn navigation_uses_pointer_socket_and_channels_use_ssap() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let pointer_url = format!("{}pointer", url);
        let server = std::thread::spawn(move || {
            let mut control = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            receive(&mut control);
            send(
                &mut control,
                json!({"type":"registered","payload":{"client-key":"test-key"}}),
            );
            let request = receive(&mut control);
            assert!(request["uri"]
                .as_str()
                .unwrap()
                .ends_with("getPointerInputSocket"));
            send(
                &mut control,
                json!({"id":request["id"],"type":"response","payload":{"socketPath":pointer_url}}),
            );
            let mut pointer = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            assert_eq!(
                pointer.read().unwrap().to_text().unwrap(),
                "type:button\nname:ENTER\n\n"
            );
            assert_eq!(
                pointer.read().unwrap().to_text().unwrap(),
                "type:button\nname:BACK\n\n"
            );
            for uri in ["ssap://tv/channelUp", "ssap://tv/channelDown"] {
                let request = receive(&mut control);
                assert_eq!(request["uri"], uri);
                send(
                    &mut control,
                    json!({"id":request["id"],"type":"response","payload":{"returnValue":true}}),
                );
            }
        });
        let (mut client, _) = Client::pair(&url).unwrap();
        client.prepare_input().unwrap();
        client.button(Button::Enter).unwrap();
        client.button(Button::Back).unwrap();
        client.channel(true).unwrap();
        client.channel(false).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn wake_packet_repeats_mac_sixteen_times() {
        let packet = magic_packet("01:23:45:67:89:ab").unwrap();
        assert_eq!(&packet[..6], &[255; 6]);
        for chunk in packet[6..].chunks_exact(6) {
            assert_eq!(chunk, &[1, 35, 69, 103, 137, 171]);
        }
        assert!(magic_packet("bad address").is_err());
    }
}
