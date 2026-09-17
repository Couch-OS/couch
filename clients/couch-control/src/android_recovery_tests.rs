//! Loopback-only mutual-TLS peers exercising the real broker lane and Android codec.
use super::*;
use prost::Message;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{
    net::{TcpListener, TcpStream},
    thread,
};
#[allow(dead_code)]
#[path = "../../couch-androidtv/src/wire.rs"]
mod wire;

type Peer = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;
fn fixture() -> (TcpListener, Arc<rustls::ServerConfig>, Spec) {
    let identity = couch_androidtv::Identity::generate().unwrap();
    let server = couch_androidtv::Identity::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![CertificateDer::from(server.certificate_der.clone())],
            PrivatePkcs8KeyDer::from(server.private_key_pkcs8).into(),
        )
        .unwrap();
    let settings = couch_androidtv::Settings {
        address: "127.0.0.1".parse().unwrap(),
        pairing_port: 6467,
        remote_port: listener.local_addr().unwrap().port(),
    };
    let spec = Spec::Streaming(StreamingConnection::AndroidTv {
        settings,
        credentials: couch_androidtv::Credentials {
            identity,
            server_certificate_der: server.certificate_der,
        },
    });
    (listener, Arc::new(config), spec)
}
fn accept(listener: &TcpListener, config: &Arc<rustls::ServerConfig>) -> Peer {
    let (socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    rustls::StreamOwned::new(
        rustls::ServerConnection::new(config.clone()).unwrap(),
        socket,
    )
}
fn send(peer: &mut Peer, message: wire::Remote) {
    peer.write_all(&message.encode_length_delimited_to_vec())
        .unwrap();
    peer.flush().unwrap();
}
fn read(peer: &mut Peer) -> wire::Remote {
    let mut size = 0;
    for shift in (0..28).step_by(7) {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        size |= ((byte[0] & 127) as usize) << shift;
        if byte[0] & 128 == 0 {
            assert!(size < 65536);
            let mut body = vec![0; size];
            peer.read_exact(&mut body).unwrap();
            return wire::Remote::decode(body.as_slice()).unwrap();
        }
    }
    panic!("invalid fixture frame")
}
fn negotiate(peer: &mut Peer, delay: Duration) {
    send(
        peer,
        wire::Remote {
            configure: Some(wire::Configure {
                features: 1 | 2 | 32 | 64 | 512,
                device: None,
            }),
            ..Default::default()
        },
    );
    assert!(read(peer).configure.is_some());
    thread::sleep(delay);
    send(
        peer,
        wire::Remote {
            start: Some(wire::Started { on: true }),
            ..Default::default()
        },
    );
}
fn submit(tx: &mpsc::Sender<Job>, spec: &Spec, op: Op) -> mpsc::Receiver<Result<Value>> {
    let (reply, receive) = mpsc::sync_channel(1);
    tx.send(Job {
        packet: Packet {
            spec: spec.clone(),
            lease: 77,
            op,
        },
        at: Instant::now(),
        reply,
    })
    .unwrap();
    receive
}
fn result(rx: mpsc::Receiver<Result<Value>>) -> Result<Value> {
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn lane() -> (mpsc::Sender<Job>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let task = thread::spawn(move || {
        run_lane(
            rx,
            Arc::new(Counters::default()),
            &super::Retirement::default(),
        )
    });
    (tx, task)
}

#[test]
fn slow_reconnection_expires_original_and_queued_keys_without_replay() {
    let (listener, config, spec) = fixture();
    let (opening, opened) = mpsc::channel();
    let peer = thread::spawn(move || {
        // First connection fails before negotiation. No command can have run.
        drop(accept(&listener, &config));
        let mut peer = accept(&listener, &config);
        opening.send(()).unwrap();
        negotiate(&mut peer, Duration::from_millis(950));
        // Expired OK and queued Right must not appear. Only a fresh Home is sent.
        let request = read(&mut peer);
        assert_eq!(request.key.unwrap().code, 3);
        peer.sock
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let mut extra = [0];
        assert!(peer.read(&mut extra).is_err());
    });
    let (tx, worker) = lane();
    assert!(result(submit(&tx, &spec, Op::Open)).is_err());
    let stale = submit(&tx, &spec, Op::StreamingCommand("ok".into()));
    opened.recv_timeout(Duration::from_secs(5)).unwrap();
    let queued = submit(&tx, &spec, Op::StreamingCommand("right".into()));
    assert!(result(stale)
        .unwrap_err()
        .to_string()
        .contains("expired during connection setup"));
    assert!(result(queued)
        .unwrap_err()
        .to_string()
        .contains("expired in queue"));
    result(submit(&tx, &spec, Op::StreamingCommand("home".into()))).unwrap();
    peer.join().unwrap();
    drop(tx);
    worker.join().unwrap();
}

#[test]
fn continuous_key_traffic_services_keepalives_and_dropped_socket_is_not_replayed() {
    let (listener, config, spec) = fixture();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let peer = thread::spawn(move || {
        let mut peer = accept(&listener, &config);
        negotiate(&mut peer, Duration::ZERO);
        for value in 1..=40 {
            send(
                &mut peer,
                wire::Remote {
                    ping: Some(wire::Number { value }),
                    ..Default::default()
                },
            );
            ready_tx.send(()).unwrap();
            // Both the pong and the one requested key must arrive, without an idle gap.
            let a = read(&mut peer);
            let b = read(&mut peer);
            let (pong, key) = if a.pong.is_some() { (a, b) } else { (b, a) };
            assert_eq!(pong.pong.unwrap().value, value);
            assert_eq!(key.key.unwrap().code, 23);
        }
        peer.conn.send_close_notify();
        peer.flush().unwrap();
        ready_tx.send(()).unwrap();
        go_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(peer);
        // Explicit later request reconnects and sends exactly Home, never old OK.
        let mut next = accept(&listener, &config);
        negotiate(&mut next, Duration::ZERO);
        assert_eq!(read(&mut next).key.unwrap().code, 3);
    });
    let (tx, worker) = lane();
    result(submit(&tx, &spec, Op::Open)).unwrap();
    for _ in 1..=40 {
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        result(submit(&tx, &spec, Op::StreamingCommand("ok".into()))).unwrap();
    }
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // EOF is observed before another key is written; that request is not retried.
    assert!(result(submit(&tx, &spec, Op::StreamingCommand("ok".into()))).is_err());
    go_tx.send(()).unwrap();
    result(submit(&tx, &spec, Op::StreamingCommand("home".into()))).unwrap();
    peer.join().unwrap();
    drop(tx);
    worker.join().unwrap();
}
