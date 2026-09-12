//! Native Rust Apple TV Companion client. PIN pairing, authenticated encrypted
//! sessions and physical remote commands; no Python helper or Apple ID required.
//! Experimental until exercised with a physical Apple TV. Modern now-playing
//! metadata uses the separately paired AirPlay 2/MRP `metadata` client.
mod crypto;
pub mod metadata;
pub mod opack;
use crypto::{field, sequence, tlv, untlv};
use opack::Value;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fmt,
    io::{Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    time::{Duration, Instant},
};
/// Bonjour service used to discover this protocol; discovery is caller-owned.
pub const MDNS_SERVICE: &str = "_companion-link._tcp.local.";

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub id: String,
    pub title: String,
}

fn launchable_apps(response: &Value) -> Result<Vec<App>> {
    // Companion returns _c as bundle identifier -> display name (pyatv's
    // CompanionApps.app_list). Do not forward arbitrary protocol fields.
    let Some(Value::Dict(items)) = response.get("_c") else {
        return Err(Error::Protocol);
    };
    if items.len() > 512 {
        return Err(Error::Protocol);
    }
    let mut seen = std::collections::HashSet::new();
    let mut apps = Vec::with_capacity(items.len());
    for (id, title) in items {
        let title = title.string()?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._/-+".contains(&b))
            || title.trim().is_empty()
            || title.len() > 256
            || title.chars().any(char::is_control)
            || !seen.insert(id)
        {
            return Err(Error::Protocol);
        }
        apps.push(App {
            id: id.clone(),
            title: title.to_owned(),
        });
    }
    apps.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(apps)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Transport,
    Timeout,
    Protocol,
    Authentication,
    InvalidPin,
    Crypto,
    Rejected,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "Invalid Apple TV configuration",
            Self::Transport => "Apple TV connection failed",
            Self::Timeout => "Apple TV response timed out",
            Self::Protocol => "Invalid Apple TV response",
            Self::Authentication => "Apple TV authentication failed; pair again",
            Self::InvalidPin => "Enter the four-digit PIN shown on the Apple TV",
            Self::Crypto => "Apple TV cryptographic operation failed",
            Self::Rejected => "Apple TV rejected the command",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub address: IpAddr,
    pub companion_port: u16,
}
/// Companion uses a Bonjour-advertised port, not a fixed universal port.
impl Settings {
    pub fn new(address: IpAddr, companion_port: u16) -> Result<Self> {
        if companion_port == 0 || address.is_unspecified() || address.is_multicast() {
            return Err(Error::Configuration);
        }
        Ok(Self {
            address,
            companion_port,
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub client_id: Vec<u8>,
    pub client_secret: [u8; 32],
    pub device_id: Vec<u8>,
    pub device_public: [u8; 32],
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([redacted])")
    }
}
struct Keys {
    output: [u8; 32],
    input: [u8; 32],
    sent: u64,
    received: u64,
}
struct Connection {
    stream: TcpStream,
    buffer: Vec<u8>,
    keys: Option<Keys>,
    xid: u64,
    events: VecDeque<Value>,
}
impl Connection {
    fn connect(settings: &Settings) -> Result<Self> {
        Settings::new(settings.address, settings.companion_port)?;
        let stream = TcpStream::connect_timeout(
            &SocketAddr::new(settings.address, settings.companion_port),
            Duration::from_secs(5),
        )
        .map_err(|_| Error::Transport)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| Error::Transport)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            stream,
            buffer: vec![],
            keys: None,
            xid: 1,
            events: VecDeque::new(),
        })
    }
    fn send(&mut self, kind: u8, value: Value) -> Result<u64> {
        let xid = self.xid;
        self.xid = self.xid.checked_add(1).ok_or(Error::Protocol)?;
        let Value::Dict(mut values) = value else {
            return Err(Error::Protocol);
        };
        values.push(("_x".into(), xid.into()));
        let mut payload = opack::encode(&Value::Dict(values))?;
        let size = payload.len() + if self.keys.is_some() { 16 } else { 0 };
        if size > 1_048_576 {
            return Err(Error::Protocol);
        }
        let header = [kind, (size >> 16) as u8, (size >> 8) as u8, size as u8];
        if let Some(keys) = self.keys.as_mut() {
            let mut nonce = [0; 12];
            nonce[..8].copy_from_slice(&keys.sent.to_le_bytes());
            keys.sent = keys.sent.checked_add(1).ok_or(Error::Protocol)?;
            payload = crypto::seal(&keys.output, &nonce, &payload, &header)?;
        }
        if self
            .stream
            .write_all(&[header.as_slice(), &payload].concat())
            .is_err()
        {
            let _ = self.stream.shutdown(Shutdown::Both);
            return Err(Error::Transport);
        }
        Ok(xid)
    }
    fn receive(&mut self) -> Result<Option<(u8, Value)>> {
        if self.buffer.len() < 4
            || self.buffer.len()
                < 4 + ((self.buffer[1] as usize) << 16
                    | ((self.buffer[2] as usize) << 8)
                    | self.buffer[3] as usize)
        {
            let mut buf = [0; 4096];
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(Error::Transport),
                Ok(n) => self.buffer.extend(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None)
                }
                Err(_) => return Err(Error::Transport),
            }
        }
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let size = (self.buffer[1] as usize) << 16
            | ((self.buffer[2] as usize) << 8)
            | self.buffer[3] as usize;
        if size > 1_048_576 {
            let _ = self.stream.shutdown(Shutdown::Both);
            return Err(Error::Protocol);
        }
        if self.buffer.len() < size + 4 {
            return Ok(None);
        }
        let frame: Vec<u8> = self.buffer.drain(..size + 4).collect();
        if size == 0 {
            return Ok(None);
        }
        let mut payload = frame[4..].to_vec();
        if let Some(keys) = self.keys.as_mut() {
            if frame[0] != 8 {
                let _ = self.stream.shutdown(Shutdown::Both);
                return Err(Error::Protocol);
            }
            let mut nonce = [0; 12];
            nonce[..8].copy_from_slice(&keys.received.to_le_bytes());
            match crypto::open(&keys.input, &nonce, &payload, &frame[..4]) {
                Ok(data) => payload = data,
                Err(e) => {
                    let _ = self.stream.shutdown(Shutdown::Both);
                    return Err(e);
                }
            }
            keys.received = keys.received.checked_add(1).ok_or(Error::Protocol)?;
        }
        Ok(Some((frame[0], opack::decode(&payload)?)))
    }
    fn exchange(&mut self, kind: u8, value: Value) -> Result<Value> {
        let xid = self.send(kind, value)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some((received, value)) = self.receive()? {
                if kind == 8 && value.get("_t").and_then(|v| v.uint().ok()) == Some(1) {
                    if self.events.len() == 64 {
                        self.events.pop_front();
                    }
                    self.events.push_back(value);
                } else if received
                    == if kind == 3 {
                        4
                    } else if kind == 5 {
                        6
                    } else {
                        kind
                    }
                {
                    if kind != 8 || value.get("_x").and_then(|v| v.uint().ok()) == Some(xid) {
                        if value.get("_em").is_some() || value.get("_ec").is_some() {
                            return Err(Error::Rejected);
                        }
                        if kind == 8 && value.get("_t").and_then(|v| v.uint().ok()) != Some(3) {
                            return Err(Error::Protocol);
                        }
                        return Ok(value);
                    }
                } else {
                    return Err(Error::Protocol);
                }
            }
            if Instant::now() >= deadline {
                let _ = self.stream.shutdown(Shutdown::Both);
                return Err(Error::Timeout);
            }
        }
    }
    fn auth(
        &mut self,
        kind: u8,
        data: Vec<u8>,
        extra: Option<(&'static str, Value)>,
        expected: u8,
    ) -> Result<Vec<(u8, Vec<u8>)>> {
        let mut values = vec![("_pd", Value::Data(data))];
        if let Some(extra) = extra {
            values.push(extra)
        }
        let reply = self.exchange(kind, Value::dict(values))?;
        let data = reply.get("_pd").ok_or(Error::Protocol)?.data()?;
        let fields = untlv(data)?;
        sequence(&fields, expected)?;
        Ok(fields)
    }
    fn command(&mut self, name: &str, content: Value) -> Result<Value> {
        self.exchange(
            8,
            Value::dict([("_i", name.into()), ("_t", 2.into()), ("_c", content)]),
        )
    }
}
/// Keeps the pairing connection alive while the user reads the four-digit PIN.
pub struct Pairing {
    connection: Connection,
    salt: Vec<u8>,
    server: Vec<u8>,
}
impl Pairing {
    pub fn begin(settings: &Settings) -> Result<Self> {
        let mut connection = Connection::connect(settings)?;
        let reply = connection.auth(
            3,
            tlv(&[(0, &[0]), (6, &[1])]),
            Some(("_pwTy", 1.into())),
            2,
        )?;
        Ok(Self {
            salt: field(&reply, 2)?.into(),
            server: field(&reply, 3)?.into(),
            connection,
        })
    }
    pub fn finish(mut self, pin: &str) -> Result<Credentials> {
        let setup = crypto::Setup::new(pin, &self.salt, &self.server)?;
        let reply = self.connection.auth(
            4,
            tlv(&[(6, &[3]), (3, &setup.public), (4, &setup.proof)]),
            Some(("_pwTy", 1.into())),
            4,
        )?;
        let encrypted = setup.finish_proof(field(&reply, 4)?)?;
        let reply = self.connection.auth(
            4,
            tlv(&[(6, &[5]), (5, &encrypted)]),
            Some(("_pwTy", 1.into())),
            6,
        )?;
        setup.finish(field(&reply, 5)?)
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Back,
    Ok,
    Home,
    VolumeUp,
    VolumeDown,
    PlayPause,
    PowerOn,
    PowerOff,
    ChannelUp,
    ChannelDown,
}
impl Button {
    fn code(self) -> u64 {
        match self {
            Self::Up => 1,
            Self::Down => 2,
            Self::Left => 3,
            Self::Right => 4,
            Self::Back => 5,
            Self::Ok => 6,
            Self::Home => 7,
            Self::VolumeUp => 8,
            Self::VolumeDown => 9,
            Self::PowerOff => 12,
            Self::PowerOn => 13,
            Self::PlayPause => 14,
            Self::ChannelUp => 15,
            Self::ChannelDown => 16,
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Playback {
    Play,
    Pause,
    Next,
    Previous,
    FastForwardStart,
    FastForwardStop,
    RewindStart,
    RewindStop,
}
impl Playback {
    fn code(self) -> u64 {
        match self {
            Self::Play => 1,
            Self::Pause => 2,
            Self::Next => 3,
            Self::Previous => 4,
            Self::FastForwardStart => 8,
            Self::FastForwardStop => 9,
            Self::RewindStart => 10,
            Self::RewindStop => 11,
        }
    }
}
pub struct Remote {
    connection: Connection,
    session: u64,
}
impl Remote {
    pub fn connect(settings: &Settings, credentials: &Credentials) -> Result<Self> {
        if credentials.client_id.is_empty()
            || credentials.client_id.len() > 128
            || credentials.device_id.is_empty()
            || credentials.device_id.len() > 128
        {
            return Err(Error::Configuration);
        }
        let mut connection = Connection::connect(settings)?;
        let verify = crypto::Verify::new();
        let reply = connection.auth(
            5,
            tlv(&[(6, &[1]), (3, &verify.public)]),
            Some(("_auTy", 4.into())),
            2,
        )?;
        let (data, output, input) =
            verify.reply(credentials, field(&reply, 3)?, field(&reply, 5)?)?;
        connection.auth(6, tlv(&[(6, &[3]), (5, &data)]), None, 4)?;
        connection.keys = Some(Keys {
            output,
            input,
            sent: 0,
            received: 0,
        });
        connection.command(
            "_systemInfo",
            Value::dict([
                ("_bf", 0.into()),
                ("_cf", 512.into()),
                ("_clFl", 128.into()),
                (
                    "_i",
                    Value::String(String::from_utf8_lossy(&credentials.client_id).into_owned()),
                ),
                ("_idsID", Value::Data(credentials.client_id.clone())),
                (
                    "_pubID",
                    Value::String(String::from_utf8_lossy(&credentials.client_id).into_owned()),
                ),
                ("_sf", 256.into()),
                ("_sv", "170.18".into()),
                ("model", "Couch HA100".into()),
                ("name", "couch.".into()),
            ]),
        )?;
        connection.command(
            "_touchStart",
            Value::dict([
                ("_height", Value::Float(1000.)),
                ("_width", Value::Float(1000.)),
                ("_tFl", 0.into()),
            ]),
        )?;
        let local = rand::random::<u32>() as u64;
        let reply = connection.command(
            "_sessionStart",
            Value::dict([
                ("_srvT", "com.apple.tvremoteservices".into()),
                ("_sid", local.into()),
            ]),
        )?;
        let remote = reply
            .get("_c")
            .and_then(|v| v.get("_sid"))
            .ok_or(Error::Protocol)?
            .uint()?;
        if remote > u32::MAX as u64 {
            return Err(Error::Protocol);
        }
        connection
            .stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            connection,
            session: remote << 32 | local,
        })
    }
    pub fn press(&mut self, button: Button) -> Result<()> {
        for state in [1, 2] {
            self.connection.command(
                "_hidC",
                Value::dict([("_hBtS", state.into()), ("_hidC", button.code().into())]),
            )?;
        }
        Ok(())
    }
    pub fn playback(&mut self, command: Playback) -> Result<()> {
        self.connection
            .command("_mcc", Value::dict([("_mcc", command.code().into())]))?;
        Ok(())
    }
    pub fn launch(&mut self, bundle_id: &str) -> Result<()> {
        if bundle_id.is_empty() || bundle_id.len() > 256 || bundle_id.chars().any(char::is_control)
        {
            return Err(Error::Configuration);
        }
        self.connection
            .command("_launchApp", Value::dict([("_bundleID", bundle_id.into())]))?;
        Ok(())
    }
    pub fn apps(&mut self) -> Result<Value> {
        self.connection
            .command("FetchLaunchableApplicationsEvent", Value::Dict(vec![]))
    }
    pub fn launchable_apps(&mut self) -> Result<Vec<App>> {
        launchable_apps(&self.apps()?)
    }
    /// Drain bounded pushed events; never performs a state-changing operation.
    pub fn poll(&mut self) -> Result<Option<Value>> {
        if let Some(event) = self.connection.events.pop_front() {
            return Ok(Some(event));
        }
        if let Some((_, event)) = self.connection.receive()? {
            return Ok(Some(event));
        }
        Ok(None)
    }
    pub fn close(mut self) -> Result<()> {
        self.connection.command(
            "_sessionStop",
            Value::dict([
                ("_srvT", "com.apple.tvremoteservices".into()),
                ("_sid", self.session.into()),
            ]),
        )?;
        // A peer may close immediately after acknowledging session stop.
        let _ = self.connection.stream.shutdown(Shutdown::Both);
        Ok(())
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
    use std::{net::TcpListener, thread};
    use x25519_dalek::{PublicKey, StaticSecret};
    fn nonce(label: &[u8; 8]) -> [u8; 12] {
        let mut n = [0; 12];
        n[4..].copy_from_slice(label);
        n
    }
    fn read(socket: &mut TcpStream, key: Option<&[u8; 32]>, counter: u64) -> (u8, Value) {
        let mut h = [0; 4];
        socket.read_exact(&mut h).unwrap();
        let n = ((h[1] as usize) << 16) | ((h[2] as usize) << 8) | h[3] as usize;
        assert!(n < 65536);
        let mut d = vec![0; n];
        socket.read_exact(&mut d).unwrap();
        if let Some(key) = key {
            let mut nonce = [0; 12];
            nonce[..8].copy_from_slice(&counter.to_le_bytes());
            d = crypto::open(key, &nonce, &d, &h).unwrap();
        }
        (h[0], opack::decode(&d).unwrap())
    }
    fn send(socket: &mut TcpStream, kind: u8, value: Value, key: Option<&[u8; 32]>, counter: u64) {
        let mut d = opack::encode(&value).unwrap();
        let n = d.len() + if key.is_some() { 16 } else { 0 };
        let h = [kind, (n >> 16) as u8, (n >> 8) as u8, n as u8];
        if let Some(key) = key {
            let mut nonce = [0; 12];
            nonce[..8].copy_from_slice(&counter.to_le_bytes());
            d = crypto::seal(key, &nonce, &d, &h).unwrap();
        }
        socket.write_all(&[h.as_slice(), &d].concat()).unwrap();
    }
    #[test]
    fn app_catalog_rejects_malformed_or_duplicate_entries() {
        for content in [
            Value::Null,
            Value::dict([("com.example", 1.into())]),
            Value::dict([("com.example", "One".into()), ("com.example", "Two".into())]),
            Value::dict([("bad id", "Name".into())]),
            Value::dict([("com.example", "\n".into())]),
            Value::Dict(
                (0..513)
                    .map(|i| (format!("com.example.{i}"), "Name".into()))
                    .collect(),
            ),
        ] {
            assert_eq!(
                launchable_apps(&Value::dict([("_c", content)])),
                Err(Error::Protocol)
            );
        }
        assert_eq!(launchable_apps(&Value::Dict(vec![])), Err(Error::Protocol));
        assert!(launchable_apps(&Value::dict([("_c", Value::Dict(vec![]))]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn verified_encrypted_session_routes_navigation_playback_and_apps() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let settings = Settings::new(
            "127.0.0.1".parse().unwrap(),
            listener.local_addr().unwrap().port(),
        )
        .unwrap();
        let server_signing = SigningKey::from_bytes(&[22; 32]);
        let client_signing = SigningKey::from_bytes(&[11; 32]);
        let credentials = Credentials {
            client_id: b"couch-test-client".to_vec(),
            client_secret: client_signing.to_bytes(),
            device_id: b"test-apple-tv".to_vec(),
            device_public: server_signing.verifying_key().to_bytes(),
        };
        let server_creds = credentials.clone();
        let peer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (kind, request) = read(&mut socket, None, 0);
            assert_eq!(kind, 5);
            let fields = untlv(request.get("_pd").unwrap().data().unwrap()).unwrap();
            sequence(&fields, 1).unwrap();
            let client_public: [u8; 32] = field(&fields, 3).unwrap().try_into().unwrap();
            let secret = StaticSecret::from([33; 32]);
            let public = PublicKey::from(&secret).to_bytes();
            let shared = secret.diffie_hellman(&PublicKey::from(client_public));
            let key = crypto::derive(
                shared.as_bytes(),
                "Pair-Verify-Encrypt-Salt",
                "Pair-Verify-Encrypt-Info",
            )
            .unwrap();
            let signature = server_signing
                .sign(&[public.as_slice(), &server_creds.device_id, &client_public].concat())
                .to_bytes();
            let encrypted = crypto::seal(
                &key,
                &nonce(b"PV-Msg02"),
                &tlv(&[(1, &server_creds.device_id), (10, &signature)]),
                &[],
            )
            .unwrap();
            send(
                &mut socket,
                6,
                Value::dict([(
                    "_pd",
                    Value::Data(tlv(&[(6, &[2]), (3, &public), (5, &encrypted)])),
                )]),
                None,
                0,
            );
            let (kind, request) = read(&mut socket, None, 0);
            assert_eq!(kind, 6);
            let fields = untlv(request.get("_pd").unwrap().data().unwrap()).unwrap();
            sequence(&fields, 3).unwrap();
            let decrypted =
                crypto::open(&key, &nonce(b"PV-Msg03"), field(&fields, 5).unwrap(), &[]).unwrap();
            let fields = untlv(&decrypted).unwrap();
            assert_eq!(field(&fields, 1).unwrap(), server_creds.client_id);
            VerifyingKey::from_bytes(&client_signing.verifying_key().to_bytes())
                .unwrap()
                .verify_strict(
                    &[client_public.as_slice(), &server_creds.client_id, &public].concat(),
                    &Signature::from_slice(field(&fields, 10).unwrap()).unwrap(),
                )
                .unwrap();
            send(
                &mut socket,
                6,
                Value::dict([("_pd", Value::Data(tlv(&[(6, &[4])])))]),
                None,
                0,
            );
            let input = crypto::derive(shared.as_bytes(), "", "ClientEncrypt-main").unwrap();
            let output = crypto::derive(shared.as_bytes(), "", "ServerEncrypt-main").unwrap();
            for (counter, expected) in [
                "_systemInfo",
                "_touchStart",
                "_sessionStart",
                "_hidC",
                "_hidC",
                "_mcc",
                "_launchApp",
                "FetchLaunchableApplicationsEvent",
                "_sessionStop",
            ]
            .into_iter()
            .enumerate()
            {
                let (kind, request) = read(&mut socket, Some(&input), counter as u64);
                assert_eq!(kind, 8);
                assert_eq!(request.get("_i").unwrap().string().unwrap(), expected);
                assert_eq!(request.get("_t").unwrap().uint().unwrap(), 2);
                let content = request.get("_c").unwrap();
                if expected == "_hidC" {
                    assert_eq!(content.get("_hidC").unwrap().uint().unwrap(), 6);
                    assert_eq!(
                        content.get("_hBtS").unwrap().uint().unwrap(),
                        if counter == 3 { 1 } else { 2 }
                    );
                }
                if expected == "_mcc" {
                    assert_eq!(content.get("_mcc").unwrap().uint().unwrap(), 1);
                }
                if expected == "_launchApp" {
                    assert_eq!(
                        content.get("_bundleID").unwrap().string().unwrap(),
                        "com.example.video"
                    );
                }
                send(
                    &mut socket,
                    8,
                    Value::dict([
                        ("_x", request.get("_x").unwrap().clone()),
                        ("_t", 3.into()),
                        (
                            "_c",
                            if expected == "_sessionStart" {
                                Value::dict([("_sid", 42.into())])
                            } else if expected == "FetchLaunchableApplicationsEvent" {
                                Value::dict([("com.example.video", "Example Video".into())])
                            } else {
                                Value::Dict(vec![])
                            },
                        ),
                    ]),
                    Some(&output),
                    counter as u64,
                );
            }
        });
        let mut remote = Remote::connect(&settings, &credentials).unwrap();
        remote.press(Button::Ok).unwrap();
        remote.playback(Playback::Play).unwrap();
        remote.launch("com.example.video").unwrap();
        assert_eq!(
            remote.launchable_apps().unwrap(),
            vec![App {
                id: "com.example.video".into(),
                title: "Example Video".into()
            }]
        );
        remote.close().unwrap();
        peer.join().unwrap();
    }
}
