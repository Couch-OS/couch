//! A single network owner per endpoint, shared by GUI and web via a private Unix
//! socket. Each endpoint has its own bounded lane; requests are never retried.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
mod proxies;
mod streaming;
pub use proxies::{Kodi, WebOs};
pub use streaming::{StreamingConnection, StreamingTv};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    Protocol,
    Transport,
    Timeout,
    Rejected,
    Remote(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol => f.write_str("Invalid control response"),
            Self::Transport => f.write_str("Device connection failed"),
            Self::Timeout => f.write_str("Device did not reply before the deadline"),
            Self::Rejected => f.write_str("Device rejected the request"),
            Self::Remote(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Protocol
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Remote(format!("Control service: {e}"))
    }
}
impl From<couch_webos::Error> for Error {
    fn from(e: couch_webos::Error) -> Self {
        match e {
            couch_webos::Error::Transport => Self::Transport,
            couch_webos::Error::Timeout => Self::Timeout,
            couch_webos::Error::Rejected => Self::Rejected,
            _ => Self::Remote(e.to_string()),
        }
    }
}
impl From<couch_tizen::Error> for Error {
    fn from(e: couch_tizen::Error) -> Self {
        match e {
            couch_tizen::Error::Transport => Self::Transport,
            couch_tizen::Error::Timeout => Self::Timeout,
            couch_tizen::Error::Rejected => Self::Rejected,
            _ => Self::Remote(e.to_string()),
        }
    }
}
impl From<couch_kodi::Error> for Error {
    fn from(e: couch_kodi::Error) -> Self {
        match e {
            couch_kodi::Error::Io { .. } => Self::Transport,
            couch_kodi::Error::Rpc { .. } => Self::Rejected,
            _ => Self::Remote(e.to_string()),
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Serialize, Deserialize)]
enum Spec {
    Kodi {
        host: String,
        port: u16,
        http: bool,
        user: String,
        password: String,
        timeout_ms: u64,
    },
    WebOs(couch_webos::Settings),
    Streaming(StreamingConnection),
}
impl Spec {
    fn key(&self) -> String {
        match self {
            Self::Kodi { host, port, .. } => format!("kodi:{host}:{port}"),
            Self::WebOs(s) => format!("webos:{}", s.url),
            Self::Streaming(s) => format!("{}:{}:{}", s.kind(), s.address(), s.port()),
        }
    }
}
#[derive(Serialize, Deserialize)]
enum Op {
    Open,
    Release,
    KodiCall(String, Value),
    KodiPlayback,
    KodiSelect,
    KodiVolumeStep(i64),
    KodiChapters(i64),
    KodiNotification(u64),
    TvRequest(String, Value),
    TvButton(couch_webos::Button),
    TvPrepare,
    TvToggleMute,
    TvSubscribe(String),
    TvUnsubscribe(String),
    TvUpdate(u64),
    StreamingStatus,
    StreamingApps,
    StreamingCommand(String),
}
#[derive(Serialize, Deserialize)]
struct Packet {
    spec: Spec,
    lease: u64,
    op: Op,
}
struct Job {
    packet: Packet,
    at: Instant,
    reply: mpsc::SyncSender<std::result::Result<Value, Error>>,
}
#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    dropped: AtomicU64,
    queue_us: AtomicU64,
    max_queue_us: AtomicU64,
}
struct Lane {
    tx: mpsc::SyncSender<Job>,
    stats: Arc<Counters>,
    last_used: Instant,
    retirement: Arc<Retirement>,
}
/// An idle lane is asked to stop and stays listed until its socket is closed,
/// so a request that arrives meanwhile cannot open a second one beside it.
#[derive(Default)]
struct Retirement {
    requested: AtomicBool,
    stopped: Mutex<bool>,
}
#[derive(Default)]
struct PoolState {
    lanes: HashMap<String, Lane>,
}
#[derive(Default)]
struct Pool {
    state: Mutex<PoolState>,
}
static POOL: OnceLock<Pool> = OnceLock::new();
static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static LEASE: AtomicU64 = AtomicU64::new(1);
pub fn use_socket(path: PathBuf) {
    let _ = SOCKET.set(path);
}
fn pool() -> &'static Pool {
    POOL.get_or_init(Pool::default)
}
impl Pool {
    fn call(&self, packet: Packet) -> Result<Value> {
        let (reply, rx) = mpsc::sync_channel(1);
        let mut state = self.state.lock().unwrap();
        let lanes = &mut state.lanes;
        lanes.retain(|_, lane| {
            if lane.last_used.elapsed() < Duration::from_secs(60) {
                return true;
            }
            lane.retirement.requested.store(true, Ordering::SeqCst);
            // Keep the retirement handle until socket closure is acknowledged.
            !*lane.retirement.stopped.lock().unwrap()
        });
        if lanes
            .get(&packet.spec.key())
            .is_some_and(|lane| lane.retirement.requested.load(Ordering::SeqCst))
        {
            return Err(Error::Remote(
                "Device connection is stopping; try again".into(),
            ));
        }
        if lanes.len() >= 128 && !lanes.contains_key(&packet.spec.key()) {
            return Err(Error::Remote("Too many active connections".into()));
        }
        let lane = lanes.entry(packet.spec.key()).or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel(16);
            let stats = Arc::new(Counters::default());
            let metrics = stats.clone();
            let retirement = Arc::new(Retirement::default());
            let worker_retirement = retirement.clone();
            std::thread::spawn(move || {
                run_lane(rx, metrics, &worker_retirement);
                // run_lane has dropped its client/socket before acknowledging.
                *worker_retirement.stopped.lock().unwrap() = true;
            });
            Lane {
                tx,
                stats,
                last_used: Instant::now(),
                retirement,
            }
        });
        lane.last_used = Instant::now();
        lane.stats.requests.fetch_add(1, Ordering::Relaxed);
        if lane
            .tx
            .try_send(Job {
                packet,
                at: Instant::now(),
                reply,
            })
            .is_err()
        {
            lane.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Remote("Device command queue is full".into()));
        }
        drop(state);
        rx.recv_timeout(Duration::from_secs(15)).map_err(|_| {
            Error::Remote("Device control timed out; command was not retried".into())
        })?
    }
}
pub fn metrics() -> Value {
    Value::Array(pool().state.lock().unwrap().lanes.iter().map(|(key,l)|json!({"endpoint":key,"requests":l.stats.requests.load(Ordering::Relaxed),"dropped":l.stats.dropped.load(Ordering::Relaxed),"queue_us":l.stats.queue_us.load(Ordering::Relaxed),"max_queue_us":l.stats.max_queue_us.load(Ordering::Relaxed)})).collect())
}
enum Client {
    Kodi(couch_kodi::Kodi),
    Tv(couch_webos::Client),
    Streaming(streaming::Client),
}
impl Client {
    fn open(spec: &Spec) -> Result<Self> {
        Ok(match spec {
            Spec::Kodi {
                host,
                port,
                http,
                user,
                password,
                timeout_ms,
            } => Self::Kodi(
                if *http {
                    couch_kodi::Kodi::http(host, *port).with_auth(user, password)
                } else {
                    couch_kodi::Kodi::tcp(host, *port)
                }
                .with_timeout(Duration::from_millis(*timeout_ms)),
            ),
            Spec::WebOs(s) => Self::Tv(couch_webos::Client::connect(s)?),
            Spec::Streaming(s) => Self::Streaming(streaming::Client::open(s)?),
        })
    }
    fn execute(&mut self, op: Op) -> Result<Value> {
        macro_rules! encode {
            ($x:expr) => {
                serde_json::to_value($x.map_err(Error::from)?).map_err(Into::into)
            };
        }
        match (self, op) {
            (_, Op::Open) => Ok(Value::Null),
            (Self::Streaming(c), Op::StreamingStatus) => c.status(),
            (Self::Streaming(c), Op::StreamingApps) => c.apps(),
            (Self::Streaming(c), Op::StreamingCommand(name)) => {
                c.command(&name)?;
                Ok(Value::Null)
            }
            (Self::Kodi(c), Op::KodiCall(m, p)) => c.call(&m, p).map_err(Error::from),
            (Self::Kodi(c), Op::KodiPlayback) => encode!(c.playback()),
            (Self::Kodi(c), Op::KodiSelect) => encode!(c.select()),
            (Self::Kodi(c), Op::KodiVolumeStep(delta)) => {
                let current = c.volume().map_err(Error::from)?;
                encode!(c.set_volume((current.volume + delta).clamp(0, 100)))
            }
            (Self::Kodi(c), Op::KodiChapters(p)) => encode!(c.chapters(p)),
            (Self::Kodi(c), Op::KodiNotification(ms)) => {
                encode!(c.next_notification(Duration::from_millis(ms.min(20))))
            }
            (Self::Tv(c), Op::TvRequest(uri, p)) => c.request(&uri, p).map_err(Into::into),
            (Self::Tv(c), Op::TvButton(b)) => encode!(c.button(b)),
            (Self::Tv(c), Op::TvPrepare) => encode!(c.prepare_input()),
            (Self::Tv(c), Op::TvToggleMute) => {
                let state = c.volume()?;
                let state = if state["volumeStatus"].is_object() {
                    &state["volumeStatus"]
                } else {
                    &state
                };
                let muted = state["muteStatus"]
                    .as_bool()
                    .or(state["muted"].as_bool())
                    .ok_or(Error::Protocol)?;
                encode!(c.mute(!muted))
            }
            (Self::Tv(c), Op::TvSubscribe(uri)) => encode!(c.subscribe(&uri)),
            (Self::Tv(c), Op::TvUnsubscribe(id)) => encode!(c.unsubscribe(&id)),
            (Self::Tv(c), Op::TvUpdate(ms)) => {
                encode!(c.next_update(Duration::from_millis(ms.min(20))))
            }
            _ => Err(Error::Protocol),
        }
    }
}
fn run_lane(rx: mpsc::Receiver<Job>, stats: Arc<Counters>, retirement: &Retirement) {
    let mut client = None;
    let mut identity = Vec::new();
    let mut owners = HashMap::<u64, Instant>::new();
    loop {
        if retirement.requested.load(Ordering::SeqCst) {
            return;
        }
        let job = rx.recv_timeout(Duration::from_millis(100));
        owners.retain(|_, at| at.elapsed() < Duration::from_secs(30));
        if owners.is_empty() {
            client = None;
        }
        let job = match job {
            Ok(j) => j,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Android TV keepalives must be answered even with no command
                // queued; each endpoint already owns an independent lane.
                if let Some(Client::Streaming(c)) = client.as_mut() {
                    if c.idle().is_err() {
                        client = None;
                    }
                }
                continue;
            }
            Err(_) => return,
        };
        if retirement.requested.load(Ordering::SeqCst) {
            let _ = job
                .reply
                .send(Err(Error::Remote("Device connection is stopping".into())));
            return;
        }
        if matches!(job.packet.op, Op::Release) {
            owners.remove(&job.packet.lease);
            if owners.is_empty() {
                client = None;
            }
            let _ = job.reply.send(Ok(Value::Null));
            continue;
        }
        let us = job.at.elapsed().as_micros() as u64;
        stats.queue_us.fetch_add(us, Ordering::Relaxed);
        stats.max_queue_us.fetch_max(us, Ordering::Relaxed);
        if us > 750_000 {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
            let _ = job.reply.send(Err(Error::Remote(
                "Device command expired in queue; not sent".into(),
            )));
            continue;
        }
        let spec = serde_json::to_vec(&job.packet.spec).unwrap();
        if identity != spec {
            client = None;
            identity = spec;
            owners.clear();
        }
        owners.insert(job.packet.lease, Instant::now());
        let result = (|| {
            if client.is_none() {
                client = Some(Client::open(&job.packet.spec)?);
            }
            if !matches!(job.packet.op, Op::Open) {
                if let Some(Client::Streaming(streaming::Client::Android(c))) = client.as_mut() {
                    // Busy key traffic must also service pings and observe a
                    // dropped TLS socket before issuing another command.
                    c.poll().map_err(|e| Error::Remote(e.to_string()))?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            client = None;
            let _ = job.reply.send(Err(error));
            continue;
        }
        if retirement.requested.load(Ordering::SeqCst) {
            let _ = job
                .reply
                .send(Err(Error::Remote("Device connection is stopping".into())));
            return;
        }
        // Opening a replacement socket can take longer than the queue limit.
        // Never send that old key after a slow handshake. Keep the now-open
        // socket so a fresh user request can proceed without another reconnect.
        if !matches!(job.packet.op, Op::Open) && job.at.elapsed() > Duration::from_millis(750) {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
            let _ = job.reply.send(Err(Error::Remote(
                "Device command expired during connection setup; not sent".into(),
            )));
            continue;
        }
        let result = client.as_mut().unwrap().execute(job.packet.op);
        // A failed read/command is never retried. Reconnect on a future request.
        if result
            .as_ref()
            .is_err_and(|e| !matches!(e, Error::Rejected))
        {
            client = None;
        }
        let _ = job.reply.send(result);
    }
}
struct Handle {
    spec: Spec,
    lease: u64,
}
impl Handle {
    fn new(spec: Spec) -> Self {
        Self {
            spec,
            lease: ((std::process::id() as u64) << 32) | LEASE.fetch_add(1, Ordering::Relaxed),
        }
    }
    fn call(&self, op: Op) -> Result<Value> {
        dispatch(Packet {
            spec: self.spec.clone(),
            lease: self.lease,
            op,
        })
    }
    fn get<T: serde::de::DeserializeOwned>(&self, op: Op) -> Result<T> {
        Ok(serde_json::from_value(self.call(op)?)?)
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.call(Op::Release);
    }
}
fn dispatch(packet: Packet) -> Result<Value> {
    dispatch_via(SOCKET.get().map(PathBuf::as_path), packet)
}
fn dispatch_via(socket: Option<&Path>, packet: Packet) -> Result<Value> {
    // Once the GUI selects the daemon as owner, a missing/restarting daemon is
    // an error. Falling back to a GUI-local socket bypasses migration fencing.
    if let Some(path) = socket {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(16)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        write_frame(&mut stream, &packet)?;
        let result: std::result::Result<Value, Error> = read_frame(&mut stream)?;
        result
    } else {
        pool().call(packet)
    }
}
fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::Protocol);
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 4 * 1024 * 1024 {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
/// Only local privileged processes may reach this socket; credentials never
/// enter the public HTTP API. The daemon itself uses the same in-process pool.
pub fn serve(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(Error::Remote("Control socket path is not a socket".into()));
        }
        if UnixStream::connect(path).is_ok() {
            return Err(Error::Remote("Control service is already running".into()));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for mut socket in listener.incoming().flatten() {
            if active.fetch_add(1, Ordering::SeqCst) >= 32 {
                active.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let active = active.clone();
            std::thread::spawn(move || {
                let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
                if let Ok(packet) = read_frame::<Packet>(&mut socket) {
                    let result = pool().call(packet);
                    let _ = write_frame(&mut socket, &result);
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    /// A Kodi that answers every JSON-RPC call with `"OK"` on one connection,
    /// and can hold its first `Test.Block` until told to let go. The pool's
    /// properties are the same for every client; raw-TCP Kodi is the one whose
    /// wire a test can speak in a dozen lines.
    fn player(
        block: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    ) -> (u16, mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (closed, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut block = block;
            let mut pending = Vec::new();
            let mut chunk = [0; 512];
            loop {
                match c.read(&mut chunk) {
                    Ok(0) | Err(_) => {
                        let _ = closed.send(());
                        return;
                    }
                    Ok(n) => pending.extend_from_slice(&chunk[..n]),
                }
                // One request is outstanding at a time, so whatever parses is
                // the whole of it.
                let Ok(request) = serde_json::from_slice::<Value>(&pending) else {
                    continue;
                };
                pending.clear();
                if request["method"] == "Test.Block" {
                    if let Some((entered, release)) = block.take() {
                        entered.send(()).unwrap();
                        release.recv().unwrap();
                    }
                }
                let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":"OK"});
                c.write_all(reply.to_string().as_bytes()).unwrap();
            }
        });
        (port, rx, thread)
    }
    fn packet(port: u16, lease: u64, op: Op) -> Packet {
        Packet {
            spec: Spec::Kodi {
                host: "127.0.0.1".into(),
                port,
                http: false,
                user: String::new(),
                password: String::new(),
                timeout_ms: 5_000,
            },
            lease,
            op,
        }
    }
    fn ping() -> Op {
        Op::KodiCall("JSONRPC.Ping".into(), Value::Null)
    }
    fn held() -> Op {
        Op::KodiCall("Test.Block".into(), Value::Null)
    }
    #[test]
    fn blocked_device_does_not_delay_another_connection() {
        let pool = Arc::new(Pool::default());
        let (entered, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (slow, _, a) = player(Some((entered, blocked)));
        let (fast, _, b) = player(None);
        let p = pool.clone();
        let slow_job = std::thread::spawn(move || p.call(packet(slow, 1, held())).unwrap());
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let p = pool.clone();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || done.send(p.call(packet(fast, 2, ping()))).unwrap());
        assert!(
            finished
                .recv_timeout(Duration::from_millis(500))
                .unwrap()
                .is_ok(),
            "fast lane must finish before slow lane is released"
        );
        release.send(()).unwrap();
        slow_job.join().unwrap();
        pool.call(packet(slow, 1, Op::Release)).unwrap();
        pool.call(packet(fast, 2, Op::Release)).unwrap();
        a.join().unwrap();
        b.join().unwrap();
    }
    #[test]
    fn multiple_consumers_share_one_socket_until_the_last_lease_releases() {
        let pool = Pool::default();
        let (port, closed, server) = player(None);
        pool.call(packet(port, 11, ping())).unwrap();
        pool.call(packet(port, 12, ping())).unwrap();
        pool.call(packet(port, 11, Op::Release)).unwrap();
        assert!(closed.try_recv().is_err());
        pool.call(packet(port, 12, ping())).unwrap();
        pool.call(packet(port, 12, Op::Release)).unwrap();
        closed.recv_timeout(Duration::from_secs(1)).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn an_idle_lane_with_work_in_flight_drains_before_anything_replaces_it() {
        let pool = Arc::new(Pool::default());
        let (entered, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (port, closed, server) = player(Some((entered, blocked)));
        let p = pool.clone();
        let request = std::thread::spawn(move || p.call(packet(port, 71, held())));
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let key = format!("kodi:127.0.0.1:{port}");
        pool.state
            .lock()
            .unwrap()
            .lanes
            .get_mut(&key)
            .unwrap()
            .last_used = Instant::now() - Duration::from_secs(61);
        assert!(
            pool.call(packet(port, 72, ping())).is_err(),
            "idle eviction must retain a draining lane"
        );
        assert!(
            closed.try_recv().is_err(),
            "the issued request still owns the socket"
        );
        release.send(()).unwrap();
        request.join().unwrap().unwrap();
        closed.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn private_service_and_local_consumer_share_the_same_connection() {
        let path =
            std::env::temp_dir().join(format!("couch-control-test-{}.sock", std::process::id()));
        serve(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let (port, closed, server) = player(None);
        let remote = |lease, op| {
            let mut stream = UnixStream::connect(&path).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write_frame(&mut stream, &packet(port, lease, op)).unwrap();
            read_frame::<Result<Value>>(&mut stream).unwrap().unwrap()
        };
        remote(21, ping());
        pool().call(packet(port, 22, ping())).unwrap();
        remote(21, Op::Release);
        assert!(closed.try_recv().is_err());
        pool().call(packet(port, 22, Op::Release)).unwrap();
        closed.recv_timeout(Duration::from_secs(1)).unwrap();
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn private_protocol_preserves_rejection_and_rejects_oversized_frames() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_frame(
            &mut a,
            &std::result::Result::<Value, Error>::Err(Error::Rejected),
        )
        .unwrap();
        assert!(matches!(
            read_frame::<std::result::Result<Value, Error>>(&mut b).unwrap(),
            Err(Error::Rejected)
        ));
        a.write_all(&(5 * 1024 * 1024u32).to_be_bytes()).unwrap();
        assert!(matches!(read_frame::<Packet>(&mut b), Err(Error::Protocol)));
    }
    #[test]
    fn missing_selected_broker_does_not_fall_back_to_a_second_network_owner() {
        let receiver = TcpListener::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let path =
            std::env::temp_dir().join(format!("couch-absent-control-{}.sock", std::process::id()));
        assert!(!path.exists());
        assert!(dispatch_via(
            Some(&path),
            packet(receiver.local_addr().unwrap().port(), 91, ping())
        )
        .is_err());
        assert!(matches!(receiver.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }
}

#[cfg(test)]
mod android_recovery_tests;
