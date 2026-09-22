//! The pinned bridge certificate is the only thing stopping another box on the
//! LAN from answering as the Hue bridge, so these tests drive the real client -
//! `Settings`, `Hue` and the ureq/rustls transport under them - against a
//! loopback TLS server, rather than checking the verifier in isolation.
//!
//! What they are really about is the *load* path. A saved pin that is corrupt,
//! truncated or empty has to refuse the bridge, and must never be quietly
//! forgotten and learned again on the next handshake: trust on first use
//! belongs to pairing and nowhere else. The bridge counts the requests it
//! serves, because the thing that matters is not the error the caller sees but
//! that nothing reached the far end.

use couch_hue::{settings::Settings, Error, Hue};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

const KEY: &str = "test-secret";
const LIGHT: &str = "00000000-0000-0000-0000-000000000001";

fn self_signed() -> (Vec<u8>, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    (
        certified.cert.der().to_vec(),
        certified.key_pair.serialize_der(),
    )
}

struct Bridge {
    url: String,
    certificate: Vec<u8>,
    port: u16,
    served: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Bridge {
    fn start() -> Self {
        let (certificate, key) = self_signed();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(certificate.clone())],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key).into(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, halt, config) = (served.clone(), stop.clone(), Arc::new(config));
        let worker = thread::spawn(move || {
            for socket in listener.incoming() {
                if halt.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(socket) = socket else { continue };
                let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
                let Ok(connection) = rustls::ServerConnection::new(config.clone()) else {
                    continue;
                };
                // A refused pin aborts the handshake here, so the read below
                // fails and nothing is counted.
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                let head = loop {
                    match stream.read(&mut byte) {
                        Ok(1) => request.push(byte[0]),
                        _ => break None,
                    }
                    if request.ends_with(b"\r\n\r\n") {
                        break Some(String::from_utf8_lossy(&request).into_owned());
                    }
                    if request.len() > 16 * 1024 {
                        break None;
                    }
                };
                let Some(head) = head else { continue };
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                if length > 0 && stream.read_exact(&mut body).is_err() {
                    continue;
                }
                count.fetch_add(1, Ordering::SeqCst);
                let payload = if head.starts_with("POST /api ") {
                    format!("[{{\"success\":{{\"username\":\"{KEY}\"}}}}]")
                } else {
                    format!(
                        "{{\"errors\":[],\"data\":[\
                         {{\"type\":\"light\",\"id\":\"{LIGHT}\",\"owner\":{{\"rid\":\"device\"}},\
                         \"metadata\":{{\"name\":\"Fixture light\"}},\"on\":{{\"on\":true}},\
                         \"dimming\":{{\"brightness\":40}}}},\
                         {{\"type\":\"zigbee_connectivity\",\"owner\":{{\"rid\":\"device\"}},\
                         \"status\":\"connected\"}}]}}"
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.flush();
            }
        });
        Self {
            url: format!("https://127.0.0.1:{port}"),
            certificate,
            port,
            served,
            stop,
            worker: Some(worker),
        }
    }
    fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept the worker is parked on.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("couch-hue-pin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_pinned_certificate_is_what_lets_the_bridge_answer() {
    let bridge = Bridge::start();
    let lights = Hue::new(&bridge.url, KEY, &bridge.certificate)
        .unwrap()
        .lights()
        .unwrap();
    assert_eq!(lights.len(), 1);
    assert_eq!(lights[0].name, "Fixture light");
    assert_eq!(bridge.served(), 1);
}

#[test]
fn another_devices_valid_certificate_is_refused_at_the_paired_address() {
    let paired = Bridge::start();
    // Same address, a perfectly valid self-signed certificate of its own, and
    // the same answers: an impostor that has taken over the bridge's place.
    let impostor = Bridge::start();
    let client = Hue::new(&impostor.url, KEY, &paired.certificate).unwrap();
    assert_eq!(client.lights().unwrap_err(), Error::Transport);
    assert_eq!(
        impostor.served(),
        0,
        "the request must not reach an unpinned device"
    );
}

#[test]
fn an_unusable_pin_is_refused_rather_than_treated_as_no_pin_yet() {
    let bridge = Bridge::start();
    let mut flipped = bridge.certificate.clone();
    flipped[0] ^= 1;
    let truncated = bridge.certificate[..bridge.certificate.len() / 2].to_vec();
    let garbage = vec![0xffu8; 64];
    for pin in [flipped, truncated, garbage] {
        let client = Hue::new(&bridge.url, KEY, &pin).unwrap();
        assert_eq!(client.lights().unwrap_err(), Error::Transport);
        // If the handshake had discarded the unusable pin and taken the
        // bridge's certificate instead, this second call would succeed.
        assert_eq!(client.lights().unwrap_err(), Error::Transport);
        assert_eq!(
            Hue::new(&bridge.url, KEY, &pin)
                .unwrap()
                .lights()
                .unwrap_err(),
            Error::Transport
        );
    }
    assert_eq!(bridge.served(), 0);
}

#[test]
fn a_corrupt_saved_pin_refuses_the_bridge_and_is_left_on_disk_as_it_was() {
    let bridge = Bridge::start();
    let dir = scratch("corrupt");
    let file = dir.join("hue-connection.json");
    let mut certificate = bridge.certificate.clone();
    certificate[0] ^= 1;
    Settings {
        url: bridge.url.clone(),
        token: KEY.into(),
        certificate,
    }
    .save(&file)
    .unwrap();
    let before = std::fs::read(&file).unwrap();
    for _ in 0..2 {
        let saved = Settings::load(&file).unwrap();
        assert_eq!(
            saved.client().unwrap().lights().unwrap_err(),
            Error::Transport
        );
        // Nothing re-pins a saved connection behind the user's back.
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }
    assert_eq!(bridge.served(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_empty_pin_is_refused_unless_the_connection_is_genuinely_unpaired() {
    let bridge = Bridge::start();
    // A saved connection without a certificate is not a pairing. Refuse it
    // before a socket is opened rather than trusting whatever answers.
    assert_eq!(
        Hue::new(&bridge.url, KEY, &[]).unwrap_err(),
        Error::Configuration
    );
    let dir = scratch("empty");
    let file = dir.join("hue-connection.json");
    std::fs::write(
        &file,
        serde_json::json!({"url": bridge.url, "token": KEY, "certificate": []}).to_string(),
    )
    .unwrap();
    assert_eq!(
        Settings::load(&file).unwrap().client().unwrap_err(),
        Error::Configuration
    );
    assert_eq!(bridge.served(), 0);

    // Pairing is the one moment an empty pin is allowed, and it records exactly
    // the certificate the bridge showed while the link button was held.
    let paired = Hue::pair(&bridge.url).unwrap();
    assert_eq!(paired.certificate, bridge.certificate);
    assert_eq!(paired.token, KEY);
    // The pairing POST, then the light read that confirms the key works.
    assert_eq!(bridge.served(), 2);
    paired.save(&file).unwrap();
    assert_eq!(
        Settings::load(&file).unwrap().certificate,
        bridge.certificate
    );
    std::fs::remove_dir_all(dir).unwrap();
}
