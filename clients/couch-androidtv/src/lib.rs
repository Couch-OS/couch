//! Android TV Remote v2: explicit PIN pairing, pinned mutual TLS, and a
//! persistent remote session. Call `poll` regularly to answer TV keepalives.
//! No command is retried automatically. Physical interoperability is pending.
pub mod cast;
mod wire;
use couch_sdk::tls;
use prost::Message;
use rsa::pkcs8::EncodePrivateKey;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use x509_parser::prelude::FromDer;

/// Bonjour service used to discover this protocol; discovery is caller-owned.
pub const MDNS_SERVICE: &str = "_androidtvremote2._tcp.local.";

/// What the pinned verifier reports; it names the device, not the certificate,
/// because it reaches the panel and the browser unchanged.
pub(crate) const CERTIFICATE_CHANGED: &str = "Android TV certificate changed; pair again";

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Transport,
    Timeout,
    Protocol,
    Certificate,
    PairingRequired,
    InvalidPin,
    Rejected,
    Crypto,
    Unsupported,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "Invalid Android TV configuration",
            Self::Transport => "Android TV connection failed",
            Self::Timeout => "Android TV response timed out",
            Self::Protocol => "Invalid Android TV response",
            Self::Certificate => "Android TV certificate changed; pair again",
            Self::PairingRequired => "Pair with the Android TV first",
            Self::InvalidPin => "Enter the six-character code shown on the TV",
            Self::Rejected => "Android TV rejected the request",
            Self::Crypto => "Android TV cryptographic operation failed",
            Self::Unsupported => "Android TV does not support this operation",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub certificate_der: Vec<u8>,
    pub private_key_pkcs8: Vec<u8>,
}
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Identity([redacted])")
    }
}
impl Identity {
    pub fn generate() -> Result<Self> {
        let key =
            rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).map_err(|_| Error::Crypto)?;
        let der = key.to_pkcs8_der().map_err(|_| Error::Crypto)?;
        let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.as_bytes()),
            &rcgen::PKCS_RSA_SHA256,
        )
        .map_err(|_| Error::Crypto)?;
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "couch.");
        let cert = params.self_signed(&pair).map_err(|_| Error::Crypto)?;
        Ok(Self {
            certificate_der: cert.der().to_vec(),
            private_key_pkcs8: der.as_bytes().to_vec(),
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub identity: Identity,
    pub server_certificate_der: Vec<u8>,
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([redacted])")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub address: IpAddr,
    #[serde(default = "pairing_port")]
    pub pairing_port: u16,
    #[serde(default = "remote_port")]
    pub remote_port: u16,
}
fn pairing_port() -> u16 {
    6467
}
fn remote_port() -> u16 {
    6466
}
impl Settings {
    pub fn new(address: IpAddr) -> Self {
        Self {
            address,
            pairing_port: 6467,
            remote_port: 6466,
        }
    }
}
type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;
struct Connection {
    stream: Tls,
    reader: Frames,
    pin: Arc<Mutex<Vec<u8>>>,
}
impl Connection {
    fn open(address: IpAddr, port: u16, identity: &Identity, pin: Vec<u8>) -> Result<Self> {
        if port == 0 || address.is_unspecified() || address.is_multicast() {
            return Err(Error::Configuration);
        }
        let pin = Arc::new(Mutex::new(pin));
        // Mutual TLS: the TV's certificate is pinned the way the other clients
        // pin theirs, and this is the one client that also presents its own.
        let config =
            tls::pinned_config_builder(Arc::new(tls::Pin::new(pin.clone(), CERTIFICATE_CHANGED)))
                .map_err(|_| Error::Crypto)?
                .with_client_auth_cert(
                    vec![CertificateDer::from(identity.certificate_der.clone())],
                    PrivatePkcs8KeyDer::from(identity.private_key_pkcs8.clone()).into(),
                )
                .map_err(|_| Error::Crypto)?;
        let socket =
            TcpStream::connect_timeout(&SocketAddr::new(address, port), Duration::from_secs(5))
                .map_err(|_| Error::Transport)?;
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| Error::Transport)?;
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| Error::Transport)?;
        let connection =
            rustls::ClientConnection::new(Arc::new(config), ServerName::IpAddress(address.into()))
                .map_err(|_| Error::Crypto)?;
        Ok(Self {
            stream: rustls::StreamOwned::new(connection, socket),
            reader: Frames::default(),
            pin,
        })
    }
    fn send(&mut self, message: impl Message) -> Result<()> {
        self.stream
            .write_all(&message.encode_length_delimited_to_vec())
            .map_err(|_| Error::Transport)?;
        self.stream.flush().map_err(|_| Error::Transport)
    }
    fn poll<M: Message + Default>(&mut self) -> Result<Option<M>> {
        if let Some(frame) = self.reader.take()? {
            return M::decode(frame.as_slice())
                .map(Some)
                .map_err(|_| Error::Protocol);
        }
        let mut buf = [0; 4096];
        match self.stream.read(&mut buf) {
            Ok(0) => return Err(Error::Transport),
            Ok(n) => self.reader.bytes.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(_) => return Err(Error::Transport),
        }
        self.reader
            .take()?
            .map(|v| M::decode(v.as_slice()).map_err(|_| Error::Protocol))
            .transpose()
    }
    fn receive<M: Message + Default>(&mut self) -> Result<M> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(m) = self.poll()? {
                return Ok(m);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
        }
    }
}
#[derive(Default)]
struct Frames {
    bytes: Vec<u8>,
}
impl Frames {
    fn take(&mut self) -> Result<Option<Vec<u8>>> {
        let mut size = 0usize;
        for (index, byte) in self.bytes.iter().copied().take(4).enumerate() {
            size |= ((byte & 127) as usize) << (7 * index);
            if byte & 128 == 0 {
                if size == 0 || size > 65536 {
                    return Err(Error::Protocol);
                }
                let end = index + 1 + size;
                if self.bytes.len() < end {
                    return Ok(None);
                }
                let frame = self.bytes[index + 1..end].to_vec();
                self.bytes.drain(..end);
                return Ok(Some(frame));
            }
        }
        if self.bytes.len() >= 4 {
            return Err(Error::Protocol);
        }
        Ok(None)
    }
}
fn pairing(payload: wire::Pair) -> wire::Pair {
    wire::Pair {
        version: 2,
        status: 200,
        ..payload
    }
}
fn accept(message: wire::Pair) -> Result<wire::Pair> {
    if message.version != 2 {
        return Err(Error::Protocol);
    }
    if message.status != 200 {
        return Err(Error::Rejected);
    }
    Ok(message)
}
/// Owns the live pairing socket while the user reads the TV's code.
pub struct Pairing {
    connection: Connection,
    identity: Identity,
}
impl Pairing {
    pub fn begin(settings: &Settings, identity: Identity, client_name: &str) -> Result<Self> {
        if client_name.is_empty() || client_name.len() > 80 {
            return Err(Error::Configuration);
        }
        let mut connection =
            Connection::open(settings.address, settings.pairing_port, &identity, vec![])?;
        connection.send(pairing(wire::Pair {
            request: Some(wire::Request {
                service: "atvremote".into(),
                client: client_name.into(),
            }),
            ..Default::default()
        }))?;
        if accept(connection.receive()?)?.request_ack.is_none() {
            return Err(Error::Protocol);
        }
        connection.send(pairing(wire::Pair {
            options: Some(wire::Options {
                input: vec![wire::Encoding { kind: 3, length: 6 }],
                role: 1,
                ..Default::default()
            }),
            ..Default::default()
        }))?;
        let options = accept(connection.receive()?)?
            .options
            .ok_or(Error::Protocol)?;
        if !options.output.iter().any(|e| e.kind == 3 && e.length == 6) {
            return Err(Error::Unsupported);
        }
        connection.send(pairing(wire::Pair {
            configuration: Some(wire::Configuration {
                encoding: Some(wire::Encoding { kind: 3, length: 6 }),
                role: 1,
            }),
            ..Default::default()
        }))?;
        if accept(connection.receive()?)?.configuration_ack.is_none() {
            return Err(Error::Protocol);
        }
        Ok(Self {
            connection,
            identity,
        })
    }
    pub fn finish(mut self, pin: &str) -> Result<Credentials> {
        let server = self
            .connection
            .pin
            .lock()
            .map_err(|_| Error::Certificate)?
            .clone();
        let secret = pairing_secret(&self.identity.certificate_der, &server, pin)?;
        self.connection.send(pairing(wire::Pair {
            secret: Some(wire::Secret {
                value: secret.to_vec(),
            }),
            ..Default::default()
        }))?;
        if accept(self.connection.receive()?)?.secret_ack.is_none() {
            return Err(Error::Protocol);
        }
        Ok(Credentials {
            identity: self.identity,
            server_certificate_der: server,
        })
    }
}
fn rsa_components(cert: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(cert)
        .map_err(|_| Error::Certificate)?;
    let key = cert.public_key().parsed().map_err(|_| Error::Certificate)?;
    match key {
        x509_parser::public_key::PublicKey::RSA(key) => {
            Ok((trim_zero(key.modulus), trim_zero(key.exponent)))
        }
        _ => Err(Error::Certificate),
    }
}
fn trim_zero(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().skip_while(|b| *b == 0).collect()
}
fn pairing_secret(client: &[u8], server: &[u8], pin: &str) -> Result<[u8; 32]> {
    if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::InvalidPin);
    }
    let code = (0..3)
        .map(|i| u8::from_str_radix(&pin[i * 2..i * 2 + 2], 16).map_err(|_| Error::InvalidPin))
        .collect::<Result<Vec<_>>>()?;
    let (cn, ce) = rsa_components(client)?;
    let (sn, se) = rsa_components(server)?;
    let hash: [u8; 32] = Sha256::new()
        .chain_update(cn)
        .chain_update(ce)
        .chain_update(sn)
        .chain_update(se)
        .chain_update(&code[1..])
        .finalize()
        .into();
    if hash[0] != code[0] {
        return Err(Error::InvalidPin);
    }
    Ok(hash)
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Button {
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
    Power,
    PowerOn,
    PowerOff,
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Rewind,
    FastForward,
    ChannelUp,
    ChannelDown,
}
impl Button {
    fn code(self) -> u32 {
        match self {
            Self::Up => 19,
            Self::Down => 20,
            Self::Left => 21,
            Self::Right => 22,
            Self::Ok => 23,
            Self::Back => 4,
            Self::Home => 3,
            Self::Menu => 82,
            Self::VolumeUp => 24,
            Self::VolumeDown => 25,
            Self::Mute => 164,
            Self::Power => 26,
            Self::PowerOn => 224,
            Self::PowerOff => 223,
            Self::PlayPause => 85,
            Self::Play => 126,
            Self::Pause => 127,
            Self::Stop => 86,
            Self::Next => 87,
            Self::Previous => 88,
            Self::Rewind => 89,
            Self::FastForward => 90,
            Self::ChannelUp => 166,
            Self::ChannelDown => 167,
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct State {
    pub on: Option<bool>,
    pub volume: Option<(u32, u32, bool)>,
    pub model: String,
    pub vendor: String,
}
pub struct Remote {
    connection: Connection,
    features: u32,
    pub state: State,
}
impl Remote {
    pub fn connect(settings: &Settings, credentials: &Credentials) -> Result<Self> {
        if credentials.server_certificate_der.is_empty() {
            return Err(Error::PairingRequired);
        }
        let mut remote = Self {
            connection: Connection::open(
                settings.address,
                settings.remote_port,
                &credentials.identity,
                credentials.server_certificate_der.clone(),
            )?,
            features: 1 | 2 | 32 | 64 | 512,
            state: State::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while remote.state.on.is_none() {
            remote.poll()?;
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
        }
        remote
            .connection
            .stream
            .sock
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|_| Error::Transport)?;
        Ok(remote)
    }
    /// Poll at least once per second while idle; replies to TV pings automatically.
    pub fn poll(&mut self) -> Result<bool> {
        let Some(msg) = self.connection.poll::<wire::Remote>()? else {
            return Ok(false);
        };
        if msg.error.is_some() {
            return Err(Error::Rejected);
        }
        if let Some(config) = msg.configure {
            self.features &= config.features;
            if self.features & 2 == 0 {
                return Err(Error::Unsupported);
            }
            if let Some(device) = config.device {
                self.state.model = device.model;
                self.state.vendor = device.vendor;
            }
            self.connection.send(wire::Remote {
                configure: Some(wire::Configure {
                    features: self.features,
                    device: Some(wire::Device {
                        unknown1: 1,
                        unknown2: "1".into(),
                        package: "couch".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            })?;
        }
        if msg.active.is_some() {
            self.connection.send(wire::Remote {
                active: Some(wire::Number {
                    value: self.features,
                }),
                ..Default::default()
            })?;
        }
        if let Some(ping) = msg.ping {
            self.connection.send(wire::Remote {
                pong: Some(ping),
                ..Default::default()
            })?;
        }
        if let Some(start) = msg.start {
            self.state.on = Some(start.on);
        }
        if let Some(volume) = msg.volume {
            self.state.volume = Some((volume.level, volume.max, volume.muted));
        }
        Ok(true)
    }
    pub fn press(&mut self, key: Button) -> Result<()> {
        self.connection.send(wire::Remote {
            key: Some(wire::Key {
                code: key.code(),
                direction: 3,
            }),
            ..Default::default()
        })
    }
    pub fn launch(&mut self, url: &str) -> Result<()> {
        if self.features & 512 == 0 {
            return Err(Error::Unsupported);
        }
        if url.is_empty() || url.len() > 2048 || url.contains('\0') {
            return Err(Error::Configuration);
        }
        self.connection.send(wire::Remote {
            app: Some(wire::AppLink { url: url.into() }),
            ..Default::default()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_and_coalesced_frames() {
        let a = wire::Remote {
            key: Some(wire::Key {
                code: 23,
                direction: 3,
            }),
            ..Default::default()
        }
        .encode_length_delimited_to_vec();
        let mut f = Frames::default();
        for byte in &a[..a.len() - 1] {
            f.bytes.push(*byte);
            assert!(f.take().unwrap().is_none());
        }
        f.bytes.push(*a.last().unwrap());
        f.bytes.extend_from_slice(&a);
        assert_eq!(
            wire::Remote::decode(f.take().unwrap().unwrap().as_slice())
                .unwrap()
                .key
                .unwrap()
                .code,
            23
        );
        assert!(f.take().unwrap().is_some());
        assert!(f.take().unwrap().is_none());
    }
    #[test]
    fn rejects_unbounded_frame_lengths() {
        for bytes in [vec![0], vec![255, 255, 255, 255], vec![129, 128, 4]] {
            assert_eq!(Frames { bytes }.take(), Err(Error::Protocol));
        }
    }
    #[test]
    fn pins_are_strict_hex_and_never_echoed() {
        for pin in ["", "12345", "12345G", "😎12"] {
            assert_eq!(pairing_secret(&[], &[], pin), Err(Error::InvalidPin));
        }
    }
    #[test]
    fn generated_identity_is_rsa_and_pairing_code_binds_both_certificates() {
        let client = Identity::generate().unwrap();
        let server = Identity::generate().unwrap();
        let (cn, ce) = rsa_components(&client.certificate_der).unwrap();
        let (sn, se) = rsa_components(&server.certificate_der).unwrap();
        assert_eq!(cn.len(), 256);
        assert_eq!(ce, vec![1, 0, 1]);
        let hash: [u8; 32] = Sha256::new()
            .chain_update(cn)
            .chain_update(ce)
            .chain_update(sn)
            .chain_update(se)
            .chain_update([0xab, 0xcd])
            .finalize()
            .into();
        let pin = format!("{:02X}ABCD", hash[0]);
        assert_eq!(
            pairing_secret(&client.certificate_der, &server.certificate_der, &pin).unwrap(),
            hash
        );
        assert!(!format!("{client:?}").contains("private_key"));
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    use std::{net::TcpListener, thread};
    fn read<M: Message + Default>(stream: &mut impl Read) -> M {
        let mut n = 0usize;
        for shift in (0..28).step_by(7) {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            n |= ((b[0] & 127) as usize) << shift;
            if b[0] & 128 == 0 {
                let mut bytes = vec![0; n];
                stream.read_exact(&mut bytes).unwrap();
                return M::decode(bytes.as_slice()).unwrap();
            }
        }
        panic!("bad frame")
    }
    fn send(stream: &mut impl Write, msg: impl Message) {
        stream
            .write_all(&msg.encode_length_delimited_to_vec())
            .unwrap();
        stream.flush().unwrap();
    }
    #[test]
    fn pair_then_control_over_mutual_tls_and_answer_keepalive() {
        let identity = Identity::generate().unwrap();
        let server_identity = Identity::generate().unwrap();
        let pairing_socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote_socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let settings = Settings {
            address: "127.0.0.1".parse().unwrap(),
            pairing_port: pairing_socket.local_addr().unwrap().port(),
            remote_port: remote_socket.local_addr().unwrap().port(),
        };
        let (cn, ce) = rsa_components(&identity.certificate_der).unwrap();
        let (sn, se) = rsa_components(&server_identity.certificate_der).unwrap();
        let digest: [u8; 32] = Sha256::new()
            .chain_update(cn)
            .chain_update(ce)
            .chain_update(sn)
            .chain_update(se)
            .chain_update([0xab, 0xcd])
            .finalize()
            .into();
        let pin = format!("{:02X}ABCD", digest[0]);
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(identity.certificate_der.clone()))
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_client_cert_verifier(verifier)
                .with_single_cert(
                    vec![CertificateDer::from(server_identity.certificate_der)],
                    PrivatePkcs8KeyDer::from(server_identity.private_key_pkcs8).into(),
                )
                .unwrap(),
        );
        let peer = thread::spawn(move || {
            let accept = |listener: TcpListener| {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    socket,
                )
            };
            let mut pair = accept(pairing_socket);
            assert_eq!(
                read::<wire::Pair>(&mut pair).request.unwrap().client,
                "Couch test"
            );
            send(
                &mut pair,
                pairing(wire::Pair {
                    request_ack: Some(wire::Empty {}),
                    ..Default::default()
                }),
            );
            assert_eq!(read::<wire::Pair>(&mut pair).options.unwrap().role, 1);
            send(
                &mut pair,
                pairing(wire::Pair {
                    options: Some(wire::Options {
                        output: vec![wire::Encoding { kind: 3, length: 6 }],
                        role: 2,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            );
            assert_eq!(read::<wire::Pair>(&mut pair).configuration.unwrap().role, 1);
            send(
                &mut pair,
                pairing(wire::Pair {
                    configuration_ack: Some(wire::Empty {}),
                    ..Default::default()
                }),
            );
            assert_eq!(read::<wire::Pair>(&mut pair).secret.unwrap().value, digest);
            send(
                &mut pair,
                pairing(wire::Pair {
                    secret_ack: Some(wire::Secret { value: vec![] }),
                    ..Default::default()
                }),
            );
            drop(pair);
            let mut remote = accept(remote_socket);
            send(
                &mut remote,
                wire::Remote {
                    configure: Some(wire::Configure {
                        features: 1 | 2 | 32 | 64 | 512,
                        device: Some(wire::Device {
                            model: "Fake Google TV".into(),
                            ..Default::default()
                        }),
                    }),
                    ..Default::default()
                },
            );
            assert_eq!(
                read::<wire::Remote>(&mut remote)
                    .configure
                    .unwrap()
                    .features,
                611
            );
            send(
                &mut remote,
                wire::Remote {
                    active: Some(wire::Number { value: 611 }),
                    ..Default::default()
                },
            );
            assert_eq!(read::<wire::Remote>(&mut remote).active.unwrap().value, 611);
            send(
                &mut remote,
                wire::Remote {
                    start: Some(wire::Started { on: true }),
                    ..Default::default()
                },
            );
            let key = read::<wire::Remote>(&mut remote).key.unwrap();
            assert_eq!((key.code, key.direction), (23, 3));
            send(
                &mut remote,
                wire::Remote {
                    ping: Some(wire::Number { value: 12345 }),
                    ..Default::default()
                },
            );
            assert_eq!(read::<wire::Remote>(&mut remote).pong.unwrap().value, 12345);
            assert_eq!(
                read::<wire::Remote>(&mut remote).app.unwrap().url,
                "https://example.test/video"
            );
        });
        let credentials = Pairing::begin(&settings, identity, "Couch test")
            .unwrap()
            .finish(&pin)
            .unwrap();
        let mut remote = Remote::connect(&settings, &credentials).unwrap();
        assert_eq!(remote.state.model, "Fake Google TV");
        remote.press(Button::Ok).unwrap();
        while !remote.poll().unwrap() {}
        remote.launch("https://example.test/video").unwrap();
        peer.join().unwrap();
    }
}
