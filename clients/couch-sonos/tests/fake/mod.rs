//! A fake local Sonos Control API, as a `couch_plugin::testing::FakeDevice`.
//!
//! The real thing is HTTPS on port 1443 with a leaf certificate issued by a
//! device CA that is in no trust store. None of that can be reproduced on an
//! ephemeral loopback port, and none of it is what the admission cases are
//! about: they assert that configure performs no I/O, that a refusal costs no
//! round trip, that a timeout is sent once, and that a burst queues nothing
//! stale. So this serves plain HTTP/1.1 and the adapter is pointed at it
//! through the ordinary `api_root` setting, whose validator confines plain HTTP
//! to loopback. Nothing in the shipping client is relaxed for a test.
//!
//! The routing table is not consumed: a route answers every time it is asked,
//! which is what an HTTP API does and what the timeout case needs, since the
//! replacement child re-identifies itself before recovering.

use couch_plugin::testing::FakeDevice;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// The player this fixture pretends to be, matching the household listing.
pub const PLAYER: &str = "RINCON_TEST";
pub const GROUP: &str = "RINCON_TEST:1";

/// How the fake player answers one request.
#[derive(Clone, Debug)]
pub enum Answer {
    /// A JSON body under this status code.
    Json(u16, String),
    /// Accept the request and never answer it. The adapter must time out rather
    /// than block forever, and must not send the command a second time.
    Silence,
    /// Hang up without answering, which is a transport failure and not a
    /// protocol one.
    Close,
}

impl Answer {
    pub fn ok(body: impl Into<String>) -> Self {
        Self::Json(200, body.into())
    }
}

/// What the fake player answers, and to what. Requests are keyed exactly as
/// [`FakeSonos::requests`] reports them: `"GET /api/v1/households/local/groups"`.
#[derive(Clone, Debug)]
pub struct Plan {
    routes: Vec<(String, Answer)>,
    fallback: Answer,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            fallback: Answer::Json(
                404,
                json!({"_objectType": "globalError", "errorCode": "ERROR_RESOURCE_NOT_FOUND"})
                    .to_string(),
            ),
        }
    }
}

impl Plan {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn on(mut self, request: &str, answer: Answer) -> Self {
        self.routes.push((request.to_owned(), answer));
        self
    }
    /// What to do with a request no route matched. Defaults to a Sonos-shaped
    /// 404, so an unexpected request is visible as a refusal rather than a hang.
    pub fn otherwise(mut self, answer: Answer) -> Self {
        self.fallback = answer;
        self
    }
    /// Everything a healthy player answers: identity, one group it coordinates,
    /// its own volume, the household's favourites and playlists, and an
    /// acknowledgement for every write.
    pub fn healthy(state: &str) -> Self {
        Self::new()
            .on("GET /api/v1/players/local/info", Answer::ok(info()))
            .on(
                "GET /api/v1/households/local/groups",
                Answer::ok(groups(state)),
            )
            .on(
                &format!("GET /api/v1/players/{PLAYER}/playerVolume"),
                Answer::ok(volume(17, true)),
            )
            .on(
                "GET /api/v1/households/local/favorites",
                Answer::ok(favorites()),
            )
            .on(
                "GET /api/v1/households/local/playlists",
                Answer::ok(playlists()),
            )
            .otherwise(Answer::ok("{}"))
    }

    fn answer(&self, request: &str) -> Answer {
        self.routes
            .iter()
            .find(|(want, _)| want == request)
            .map(|(_, answer)| answer.clone())
            .unwrap_or_else(|| self.fallback.clone())
    }
}

/// A fake player on loopback. Stops when dropped.
pub struct FakeSonos {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl FakeSonos {
    pub fn start(plan: Plan) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let port = listener.local_addr().expect("local address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let device = Self {
            base: format!("http://127.0.0.1:{port}/api/v1"),
            requests: requests.clone(),
            stop: stop.clone(),
        };
        std::thread::spawn(move || {
            let mut sessions = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let (plan, requests, stop) = (plan.clone(), requests.clone(), stop.clone());
                        sessions.push(std::thread::spawn(move || {
                            serve(socket, plan, requests, stop)
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            for session in sessions {
                let _ = session.join();
            }
        });
        device
    }
}

impl FakeDevice for FakeSonos {
    /// `host` is the identity Couch stores and is never contacted, because
    /// `api_root` replaces the address the transport derives from it.
    fn settings(&self) -> Value {
        json!({
            "host": "192.0.2.10",
            "api_key": "fixture-key",
            "api_root": self.base,
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log").clone()
    }
}

impl Drop for FakeSonos {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// One request, one answer, one connection. `Connection: close` keeps this
/// short: there is no keep-alive state to get wrong, and the adapter opens a
/// fresh connection per request either way.
fn serve(
    mut socket: TcpStream,
    plan: Plan,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(25)));
    let mut buffer: Vec<u8> = Vec::new();
    // `end` is the byte offset of the blank line, kept rather than recomputed
    // from the decoded head: a lossy decode can change the length, and the body
    // offset below has to be exact.
    let (end, head) = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let Ok(head) = std::str::from_utf8(&buffer[..at]) else {
                return;
            };
            break (at, head.to_owned());
        }
        if buffer.len() > 16 * 1024 {
            return;
        }
        match read_more(&mut socket, &mut buffer) {
            Some(true) => (),
            Some(false) => continue,
            None => return,
        }
    };
    let mut lines = head.split("\r\n");
    let Some(request) = lines.next().and_then(request_line) else {
        return;
    };
    // The body is read and discarded: nothing here inspects what a write said,
    // and leaving it unread would reset the connection instead of answering it.
    let length: usize = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0);
    let wanted = end + 4 + length;
    while buffer.len() < wanted {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match read_more(&mut socket, &mut buffer) {
            Some(_) => (),
            None => return,
        }
    }
    let answer = plan.answer(&request);
    requests.lock().expect("request log").push(request);
    match answer {
        Answer::Json(status, body) => {
            let response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
            let _ = socket.shutdown(Shutdown::Write);
        }
        Answer::Close => {
            let _ = socket.shutdown(Shutdown::Both);
        }
        Answer::Silence => {
            // Hold the connection open until the harness retires the child or
            // the fixture stops. Answering nothing is the point; the read is
            // only how this thread notices the peer has gone.
            while !stop.load(Ordering::SeqCst) {
                if read_more(&mut socket, &mut buffer).is_none() {
                    return;
                }
            }
        }
    }
}

/// `Some(true)` read something, `Some(false)` timed out, `None` finished.
fn read_more(socket: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<bool> {
    let mut bytes = [0; 1024];
    match socket.read(&mut bytes) {
        Ok(0) => None,
        Ok(n) => {
            buffer.extend_from_slice(&bytes[..n]);
            Some(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

/// `"GET /api/v1/players/local/info"` from a request line.
fn request_line(line: &str) -> Option<String> {
    let mut parts = line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    if method.is_empty() || !target.starts_with('/') {
        return None;
    }
    Some(format!("{method} {target}"))
}

/// A player that coordinates its group and has both of its own inputs.
pub fn info() -> String {
    json!({
        "_objectType": "discoveryInfo",
        "playerId": PLAYER,
        "householdId": "Sonos_test",
        "groupId": GROUP,
        "restUrl": "https://192.0.2.10:1443/api",
        "device": {
            "_objectType": "deviceInfo",
            "id": PLAYER,
            "name": "Living room",
            "model": "S19",
            "modelDisplayName": "Arc",
            "capabilities": ["PLAYBACK", "HT_PLAYBACK", "LINE_IN"],
        },
    })
    .to_string()
}

/// One group, coordinated by this player, in `state`.
pub fn groups(state: &str) -> String {
    json!({
        "_objectType": "groups",
        "groups": [{
            "_objectType": "group",
            "id": GROUP,
            "name": "Living room",
            "coordinatorId": PLAYER,
            "playbackState": state,
            "playerIds": [PLAYER],
        }],
        "players": [{"_objectType": "player", "id": PLAYER, "name": "Living room"}],
    })
    .to_string()
}

pub fn volume(level: u16, muted: bool) -> String {
    json!({"_objectType": "playerVolume", "volume": level, "muted": muted, "fixed": false})
        .to_string()
}

pub fn favorites() -> String {
    json!({
        "version": "RINCON_TEST:14",
        "items": [
            {"id": "4", "name": "Dreaming", "description": "By Marshmello", "service": {"name": "Apple Music"}},
            {"id": "9", "name": "Hotel Lobby", "description": "", "service": {"name": "Apple Music"}},
        ],
    })
    .to_string()
}

pub fn playlists() -> String {
    json!({
        "version": "RINCON_TEST:6",
        "playlists": [{"id": "1", "name": "All Songs", "type": "playlist", "trackCount": 2760}],
    })
    .to_string()
}
