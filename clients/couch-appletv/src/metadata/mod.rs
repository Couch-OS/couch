//! Modern tvOS now playing over authenticated AirPlay 2 / MediaRemote.
//!
//! Pair separately from Companion using the `_airplay._tcp.local.` port. Keep
//! calling `Client::poll` on a worker at least once per second for event replies
//! and feedback. Errors invalidate the session; reconnect explicitly. No media
//! is loaded, played, or launched by this client. Hardware validation is pending.
mod proto;
mod state;
mod store;
pub use store::StoredConnection;
mod transport;
use crate::{
    crypto::{self, field, sequence, tlv, untlv},
    Error, Result,
};
use plist::Value;
use prost::Message;
pub use state::{NowPlaying, PlaybackState};
use std::{
    collections::VecDeque,
    io::Cursor,
    net::IpAddr,
    time::{Duration, Instant},
};
use transport::{Channel, Http, LIMIT};
pub const MDNS_SERVICE: &str = "_airplay._tcp.local.";
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub address: IpAddr,
    pub airplay_port: u16,
}
impl Settings {
    pub fn new(address: IpAddr, airplay_port: u16) -> Result<Self> {
        crate::Settings::new(address, airplay_port)?;
        Ok(Self {
            address,
            airplay_port,
        })
    }
}
/// Separate type prevents accidentally treating Companion pairing as AirPlay pairing.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Credentials(crate::Credentials);
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AirPlayCredentials([redacted])")
    }
}
pub struct Pairing {
    http: Http,
    salt: Vec<u8>,
    public: Vec<u8>,
    started: Instant,
}
impl Pairing {
    /// Requests a PIN on the TV. Caller must explicitly initiate pairing.
    pub fn begin(settings: &Settings) -> Result<Self> {
        let mut http = Http::connect(settings)?;
        http.request("POST", "/pair-pin-start", &[], true)?;
        let response = http.request("POST", "/pair-setup", &tlv(&[(0, &[0]), (6, &[1])]), true)?;
        let values = untlv(&response)?;
        sequence(&values, 2)?;
        Ok(Self {
            http,
            salt: field(&values, 2)?.to_vec(),
            public: field(&values, 3)?.to_vec(),
            started: Instant::now(),
        })
    }
    pub fn finish(mut self, pin: &str) -> Result<Credentials> {
        if self.started.elapsed() > Duration::from_secs(120) {
            return Err(Error::Timeout);
        }
        let setup = crypto::Setup::new_airplay(pin, &self.salt, &self.public)?;
        let response = self.http.request(
            "POST",
            "/pair-setup",
            &tlv(&[(6, &[3]), (3, &setup.public), (4, &setup.proof)]),
            true,
        )?;
        let values = untlv(&response)?;
        sequence(&values, 4)?;
        let encrypted = setup.finish_proof(field(&values, 4)?)?;
        let response = self.http.request(
            "POST",
            "/pair-setup",
            &tlv(&[(6, &[5]), (5, &encrypted)]),
            true,
        )?;
        let values = untlv(&response)?;
        sequence(&values, 6)?;
        setup.finish(field(&values, 5)?).map(Credentials)
    }
}
fn dict(items: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    value
        .to_writer_binary(&mut output)
        .map_err(|_| Error::Protocol)?;
    Ok(output)
}
fn decode(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > LIMIT || !bytes.starts_with(b"bplist00") {
        return Err(Error::Protocol);
    }
    // Validate streaming expansion before constructing a recursive Value. A small
    // binary plist can reference the same large object many times.
    use plist::stream::{BinaryReader, Event};
    let (mut depth, mut expanded) = (0usize, 0usize);
    for (count, event) in BinaryReader::new(Cursor::new(bytes)).enumerate() {
        if count >= 4096 {
            return Err(Error::Protocol);
        }
        match event.map_err(|_| Error::Protocol)? {
            Event::StartArray(_) | Event::StartDictionary(_) => {
                depth += 1;
                if depth > 16 {
                    return Err(Error::Protocol);
                }
            }
            Event::EndCollection => {
                depth = depth.checked_sub(1).ok_or(Error::Protocol)?;
            }
            Event::Data(data) => {
                expanded += data.len();
            }
            Event::String(data) => {
                expanded += data.len();
            }
            _ => {}
        }
        if expanded > LIMIT {
            return Err(Error::Protocol);
        }
    }
    Value::from_reader(Cursor::new(bytes)).map_err(|_| Error::Protocol)
}
fn port(value: &Value, key: &str) -> Result<u16> {
    value
        .as_dictionary()
        .and_then(|d| d.get(key))
        .and_then(Value::as_unsigned_integer)
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(Error::Protocol)
}
fn envelope(kind: i32) -> proto::Envelope {
    proto::Envelope {
        kind: Some(kind),
        unique_id: Some(uuid::Uuid::new_v4().to_string().to_uppercase()),
        ..Default::default()
    }
}
fn frame(kind: &[u8; 4], command: &[u8; 4], seq: u64, body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > LIMIT - 32 {
        return Err(Error::Protocol);
    }
    let mut out = Vec::with_capacity(32 + body.len());
    out.extend(((32 + body.len()) as u32).to_be_bytes());
    out.extend(kind);
    out.extend([0; 8]);
    out.extend(command);
    out.extend(seq.to_be_bytes());
    out.extend([0; 4]);
    out.extend(body);
    Ok(out)
}
fn messages(data: &[u8]) -> Result<Vec<proto::Envelope>> {
    if data.len() > LIMIT {
        return Err(Error::Protocol);
    }
    let mut input = data;
    let mut output = vec![];
    while !input.is_empty() {
        if output.len() >= 128 {
            return Err(Error::Protocol);
        }
        // AirPlay ConfigureConnection is sometimes sent without the varint prefix.
        let message = if input[0] == 8 {
            let value = proto::Envelope::decode(input).map_err(|_| Error::Protocol)?;
            input = &[][..];
            value
        } else {
            proto::Envelope::decode_length_delimited(&mut input).map_err(|_| Error::Protocol)?
        };
        if message.kind.is_none() {
            return Err(Error::Protocol);
        }
        output.push(message);
    }
    Ok(output)
}
struct Session {
    control: Http,
    event: Channel,
    data: Channel,
    seq: u64,
    last_feedback: Instant,
    pending: VecDeque<proto::Envelope>,
}
impl Session {
    fn connect(settings: &Settings, credentials: &Credentials) -> Result<Self> {
        let mut control = Http::connect(settings)?;
        let verify = crypto::Verify::new();
        let response = control.request(
            "POST",
            "/pair-verify",
            &tlv(&[(6, &[1]), (3, &verify.public)]),
            true,
        )?;
        let fields = untlv(&response)?;
        sequence(&fields, 2)?;
        let (reply, mut shared) =
            verify.reply_shared(&credentials.0, field(&fields, 3)?, field(&fields, 5)?)?;
        // Keep shared-secret lifetime scoped even if any subsequent step fails.
        let result = (|| {
            let response = control.request(
                "POST",
                "/pair-verify",
                &tlv(&[(6, &[3]), (5, &reply)]),
                true,
            )?;
            sequence(&untlv(&response)?, 4)?;
            control.channel.enable(
                &shared,
                "Control-Salt",
                "Control-Write-Encryption-Key",
                "Control-Read-Encryption-Key",
            )?;
            let uri = control.uri.clone();
            let device_id = String::from_utf8(credentials.0.client_id.clone())
                .map_err(|_| Error::Configuration)?;
            let setup = dict([
                ("isRemoteControlOnly", true.into()),
                ("osName", "iPhone OS".into()),
                ("sourceVersion", "550.10".into()),
                ("timingProtocol", "None".into()),
                ("model", "iPhone14,3".into()),
                ("deviceID", device_id.clone().into()),
                ("osVersion", "17.0".into()),
                ("osBuildVersion", "21A329".into()),
                ("macAddress", "02:00:00:00:00:01".into()),
                (
                    "sessionUUID",
                    uuid::Uuid::new_v4().to_string().to_uppercase().into(),
                ),
                ("name", "Couch".into()),
            ]);
            let response = decode(&control.request("SETUP", &uri, &encode(&setup)?, false)?)?;
            let mut event = Channel::connect(settings, port(&response, "eventPort")?)?;
            event.enable(
                &shared,
                "Events-Salt",
                "Events-Read-Encryption-Key",
                "Events-Write-Encryption-Key",
            )?;
            control.request("RECORD", &uri, &[], false)?;
            let seed = rand::random::<u64>();
            let stream = dict([
                ("controlType", 2u64.into()),
                (
                    "channelID",
                    uuid::Uuid::new_v4().to_string().to_uppercase().into(),
                ),
                ("seed", seed.into()),
                (
                    "clientUUID",
                    uuid::Uuid::new_v4().to_string().to_uppercase().into(),
                ),
                ("type", 130u64.into()),
                ("wantsDedicatedSocket", true.into()),
                (
                    "clientTypeUUID",
                    "1910A70F-DBC0-4242-AF95-115DB30604E1".into(),
                ),
            ]);
            let setup = dict([("streams", Value::Array(vec![stream]))]);
            let response = decode(&control.request("SETUP", &uri, &encode(&setup)?, false)?)?;
            let streams = response
                .as_dictionary()
                .and_then(|d| d.get("streams"))
                .and_then(Value::as_array)
                .ok_or(Error::Protocol)?;
            if streams.len() != 1 {
                return Err(Error::Protocol);
            }
            let mut data = Channel::connect(settings, port(&streams[0], "dataPort")?)?;
            data.enable(
                &shared,
                &format!("DataStream-Salt{seed}"),
                "DataStream-Output-Encryption-Key",
                "DataStream-Input-Encryption-Key",
            )?;
            Ok(Self {
                control,
                event,
                data,
                seq: 0x100000000 + u64::from(rand::random::<u32>()),
                last_feedback: Instant::now(),
                pending: VecDeque::new(),
            })
        })();
        shared.fill(0);
        result
    }
    fn send(&mut self, message: &proto::Envelope) -> Result<()> {
        let payload = encode(&dict([(
            "params",
            dict([(
                "data",
                Value::Data(message.encode_length_delimited_to_vec()),
            )]),
        )]))?;
        self.data
            .send(&frame(b"sync", b"comm", self.seq, &payload)?)?;
        self.seq = self.seq.checked_add(1).ok_or(Error::Protocol)?;
        Ok(())
    }
    fn service(&mut self) -> Result<()> {
        self.event.read(Duration::from_millis(1))?;
        for _ in 0..32 {
            let Some(request) = transport::take_http(&mut self.event.buffer)? else {
                break;
            };
            let protocol = request
                .first
                .split_whitespace()
                .last()
                .filter(|s| matches!(*s, "HTTP/1.1" | "RTSP/1.0"))
                .ok_or(Error::Protocol)?;
            let mut reply =
                format!("{protocol} 200 OK\r\nContent-Length: 0\r\nAudio-Latency: 0\r\n");
            if let Some(cseq) = request.headers.get("cseq") {
                reply.push_str(&format!("CSeq: {cseq}\r\n"));
            }
            reply.push_str("\r\n");
            self.event.send(reply.as_bytes())?;
        }
        if self.last_feedback.elapsed() >= Duration::from_secs(2) {
            self.control.request("POST", "/feedback", &[], false)?;
            self.last_feedback = Instant::now();
        }
        self.data.read(Duration::from_millis(100))?;
        while self.data.buffer.len() >= 32 {
            let bytes = &self.data.buffer;
            let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            if !(32..=LIMIT).contains(&size) {
                return Err(Error::Protocol);
            }
            if bytes.len() < size {
                break;
            }
            let sync = &bytes[4..8] == b"sync";
            let seq = u64::from_be_bytes(bytes[20..28].try_into().unwrap());
            if size > 32 {
                let body = decode(&bytes[32..size])?;
                if let Some(data) = body
                    .as_dictionary()
                    .and_then(|d| d.get("params"))
                    .and_then(Value::as_dictionary)
                    .and_then(|p| p.get("data"))
                    .and_then(Value::as_data)
                {
                    let messages = messages(data)?;
                    if self.pending.len() + messages.len() > 128 {
                        return Err(Error::Protocol);
                    }
                    self.pending.extend(messages);
                }
            }
            self.data.buffer.drain(..size);
            if sync {
                self.data.send(&frame(b"rply", &[0; 4], seq, &[])?)?;
            }
        }
        Ok(())
    }
    fn exchange(&mut self, mut message: proto::Envelope, state: &mut state::State) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        message.identifier = Some(id.clone());
        self.send(&message)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(message) = self.pending.pop_front() {
                if message.identifier.as_ref() == Some(&id) {
                    if message.error.unwrap_or(0) != 0 {
                        return Err(Error::Rejected);
                    }
                    return Ok(());
                }
                state.apply(&message)?;
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            self.service()?;
        }
    }
}
/// Independent metadata connection. On any poll error all sockets and cached
/// metadata are cleared, so callers cannot mistake stale data for a live session.
pub struct Client {
    session: Option<Session>,
    state: state::State,
    last: NowPlaying,
}
impl Client {
    pub fn connect(settings: &Settings, credentials: &Credentials) -> Result<Self> {
        let mut session = Session::connect(settings, credentials)?;
        let mut state = state::State::default();
        let mut info = envelope(15);
        info.device = Some(proto::DeviceInfo {
            id: Some(
                String::from_utf8(credentials.0.client_id.clone())
                    .map_err(|_| Error::Configuration)?,
            ),
            name: Some("Couch".into()),
            model: Some("iPhone".into()),
            build: Some("21A329".into()),
            bundle: Some("com.apple.TVRemote".into()),
            version: Some("344.28".into()),
            protocol: Some(1),
            last_message: Some(108),
            system_pairing: Some(true),
            allows_pairing: Some(true),
            media_app: Some("com.apple.TVMusic".into()),
            acl: Some(true),
            shared_queue: Some(true),
            extended_motion: Some(false),
            queue_version: Some(2),
            device_class: Some(1),
            logical_count: Some(1),
        });
        session.exchange(info, &mut state)?;
        let mut connected = envelope(38);
        connected.connection = Some(proto::ConnectionState { state: Some(2) });
        session.send(&connected)?;
        let mut updates = envelope(16);
        updates.updates = Some(proto::Updates {
            artwork: Some(true),
            now_playing: Some(true),
            volume: Some(false),
            keyboard: Some(false),
            output: Some(false),
        });
        session.exchange(updates, &mut state)?;
        Ok(Self {
            session: Some(session),
            state,
            last: NowPlaying::default(),
        })
    }
    pub fn now_playing(&self) -> NowPlaying {
        self.state.snapshot()
    }
    /// Services push events and feedback. Returns a snapshot only when metadata
    /// changes; use NowPlaying::position_at for smooth progress between updates.
    /// Usually waits at most 101 ms; a due feedback exchange can take five seconds.
    pub fn poll(&mut self) -> Result<Option<NowPlaying>> {
        let result = (|| {
            let session = self.session.as_mut().ok_or(Error::Transport)?;
            session.service()?;
            while let Some(message) = session.pending.pop_front() {
                self.state.apply(&message)?;
            }
            let current = self.state.snapshot();
            if current != self.last {
                self.last = current.clone();
                Ok(Some(current))
            } else {
                Ok(None)
            }
        })();
        if result.is_err() {
            self.session = None;
            self.state = state::State::default();
            self.last = NowPlaying::default();
        }
        result
    }
    pub fn close(mut self) {
        self.session = None;
    }
}
#[cfg(test)]
mod tests;
