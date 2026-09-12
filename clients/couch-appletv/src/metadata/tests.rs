use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
fn path(app: &str) -> proto::PlayerPath {
    proto::PlayerPath {
        client: Some(proto::ClientInfo {
            bundle: Some(app.into()),
            name: Some("Music".into()),
        }),
        player: Some(proto::PlayerInfo {
            id: Some("MediaRemote-DefaultPlayer".into()),
            name: None,
        }),
    }
}
fn active(app: &str) -> proto::Envelope {
    let mut m = envelope(46);
    m.active_client = Some(proto::ClientMessage {
        client: path(app).client,
    });
    m
}
fn playing(app: &str, title: &str) -> proto::Envelope {
    let mut m = envelope(4);
    m.state = Some(proto::SetState {
        path: Some(path(app)),
        playback_state: Some(1),
        queue: Some(proto::Queue {
            location: Some(0),
            items: vec![proto::Item {
                id: Some("track-1".into()),
                metadata: Some(proto::Metadata {
                    title: Some(title.into()),
                    artist: Some("Artist".into()),
                    elapsed: Some(10.0),
                    duration: Some(100.0),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }),
    });
    m
}
#[test]
fn tracks_active_app_and_merges_partial_updates_without_leaking_previous_item() {
    let mut state = state::State::default();
    state.apply(&playing("music", "First")).unwrap();
    assert_eq!(state.snapshot(), NowPlaying::default());
    state.apply(&active("music")).unwrap();
    assert_eq!(state.snapshot().title.as_deref(), Some("First"));
    let mut update = envelope(56);
    update.update_items = Some(proto::ItemUpdate {
        path: Some(path("music")),
        items: vec![proto::Item {
            id: Some("track-1".into()),
            metadata: Some(proto::Metadata {
                elapsed: Some(0.0),
                subtitle: Some("".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
    });
    state.apply(&update).unwrap();
    assert_eq!(state.snapshot().position, Some(0.0));
    assert_eq!(state.snapshot().artist.as_deref(), Some("Artist"));
    state.apply(&playing("video", "Second")).unwrap();
    assert_eq!(state.snapshot().title.as_deref(), Some("First"));
    state.apply(&active("video")).unwrap();
    assert_eq!(state.snapshot().title.as_deref(), Some("Second"));
    let mut empty = playing("video", "ignored");
    empty
        .state
        .as_mut()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .items
        .clear();
    state.apply(&empty).unwrap();
    assert_eq!(state.snapshot().title, None);
    let mut remove = envelope(53);
    remove.remove_client = Some(proto::ClientMessage {
        client: path("video").client,
    });
    state.apply(&remove).unwrap();
    assert_eq!(state.snapshot(), NowPlaying::default());
}
#[test]
fn position_handles_pause_live_invalid_numbers_and_long_playback() {
    let mut now = NowPlaying {
        position: Some(10.0),
        duration: Some(20.0),
        position_timestamp: Some(1000.0),
        state: PlaybackState::Playing,
        ..Default::default()
    };
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(1005)),
        Some(15.0)
    );
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(1015)),
        Some(20.0)
    );
    now.state = PlaybackState::Paused;
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(1005)),
        Some(10.0)
    );
    now.state = PlaybackState::Playing;
    assert_eq!(now.position_at(SystemTime::now()), Some(20.0));
    now.duration = None;
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(1301)),
        Some(311.0)
    );
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(4600)),
        Some(3610.0)
    );
    assert_eq!(
        now.position_at(UNIX_EPOCH + Duration::from_secs(900)),
        Some(10.0)
    );
    let mut state = state::State::default();
    let mut m = playing("a", "live");
    let meta = m.state.as_mut().unwrap().queue.as_mut().unwrap().items[0]
        .metadata
        .as_mut()
        .unwrap();
    meta.duration = Some(f64::NAN);
    meta.elapsed = Some(f64::INFINITY);
    meta.live = Some(true);
    meta.artwork_url = Some("file:///secret".into());
    state.apply(&m).unwrap();
    state.apply(&active("a")).unwrap();
    let snap = state.snapshot();
    assert_eq!(snap.duration, None);
    assert_eq!(snap.position, None);
    assert_eq!(snap.artwork_url, None);
    assert_eq!(snap.is_live, Some(true));
}
#[test]
fn bounded_framing_http_and_plist_reject_invalid_inputs() {
    assert!(messages(&[0xff; 10]).is_err());
    assert!(messages(&[0x7f, 8, 4]).is_err());
    for data in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
    ] {
        assert!(transport::take_http(&mut data.to_vec()).is_err());
    }
    let mut partial = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\na".to_vec();
    assert!(transport::take_http(&mut partial).unwrap().is_none());
    partial.extend(b"bc");
    assert_eq!(
        transport::take_http(&mut partial).unwrap().unwrap().body,
        b"abc"
    );
    let mut deep = Value::Boolean(true);
    for _ in 0..20 {
        deep = Value::Array(vec![deep]);
    }
    assert!(decode(&encode(&deep).unwrap()).is_err());
    let mut state = state::State::default();
    for n in 0..32 {
        state.apply(&active(&n.to_string())).unwrap();
    }
    assert!(state.apply(&active("overflow")).is_err());
}
// Wire fixture written independently with Python struct/plistlib, not prost.
#[test]
fn independent_airplay_binary_plist_fixture_decodes_real_field_numbers() {
    let payload = decode(include_bytes!("fixtures/now-playing.bplist")).unwrap();
    let data = payload.as_dictionary().unwrap()["params"]
        .as_dictionary()
        .unwrap()["data"]
        .as_data()
        .unwrap();
    let mut state = state::State::default();
    for m in messages(data).unwrap() {
        state.apply(&m).unwrap();
    }
    let snap = state.snapshot();
    assert_eq!(snap.title.as_deref(), Some("Fixture song"));
    assert_eq!(snap.artist.as_deref(), Some("Fixture artist"));
    assert_eq!(snap.position, Some(12.5));
    assert_eq!(snap.duration, Some(240.0));
    assert_eq!(snap.state, PlaybackState::Playing);
    assert_eq!(snap.position_timestamp, Some(978_307_300.0));
}
struct Peer {
    socket: TcpStream,
    keys: Option<([u8; 32], [u8; 32])>,
    sent: u64,
    received: u64,
    buffer: Vec<u8>,
}
impl Peer {
    fn new(socket: TcpStream) -> Self {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            socket,
            keys: None,
            sent: 0,
            received: 0,
            buffer: vec![],
        }
    }
    fn enable(&mut self, shared: &[u8], salt: &str, input: &str, output: &str) {
        self.keys = Some((
            crypto::derive(shared, salt, input).unwrap(),
            crypto::derive(shared, salt, output).unwrap(),
        ));
    }
    fn read(&mut self) {
        if let Some((key, _)) = self.keys {
            let mut size = [0; 2];
            self.socket.read_exact(&mut size).unwrap();
            let mut body = vec![0; u16::from_le_bytes(size) as usize + 16];
            self.socket.read_exact(&mut body).unwrap();
            let mut nonce = [0; 12];
            nonce[4..].copy_from_slice(&self.received.to_le_bytes());
            self.received += 1;
            self.buffer
                .extend(crypto::open(&key, &nonce, &body, &size).unwrap());
        } else {
            let mut b = [0; 4096];
            let n = self.socket.read(&mut b).unwrap();
            assert!(n > 0);
            self.buffer.extend(&b[..n]);
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        if let Some((_, key)) = self.keys {
            // Fragment records deliberately, including HTTP headers and frame lengths.
            for part in bytes.chunks(37) {
                let size = (part.len() as u16).to_le_bytes();
                let mut nonce = [0; 12];
                nonce[4..].copy_from_slice(&self.sent.to_le_bytes());
                self.sent += 1;
                let encrypted = crypto::seal(&key, &nonce, part, &size).unwrap();
                self.socket.write_all(&size).unwrap();
                self.socket.write_all(&encrypted).unwrap();
            }
        } else {
            self.socket.write_all(bytes).unwrap();
        }
    }
    fn http(&mut self) -> transport::HttpMessage {
        loop {
            if let Some(m) = transport::take_http(&mut self.buffer).unwrap() {
                return m;
            }
            self.read();
        }
    }
    fn reply(&mut self, request: &transport::HttpMessage, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 200 OK\r\nCSeq: {}\r\nContent-Length: {}\r\n\r\n",
            request.headers["cseq"],
            body.len()
        );
        self.send(&[head.as_bytes(), body].concat());
    }
    fn data(&mut self) -> Vec<proto::Envelope> {
        loop {
            while self.buffer.len() < 32 {
                self.read();
            }
            let size = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
            while self.buffer.len() < size {
                self.read();
            }
            let bytes = self.buffer.drain(..size).collect::<Vec<_>>();
            if &bytes[4..8] == b"rply" {
                continue;
            }
            let value = decode(&bytes[32..]).unwrap();
            let data = value.as_dictionary().unwrap()["params"]
                .as_dictionary()
                .unwrap()["data"]
                .as_data()
                .unwrap();
            return messages(data).unwrap();
        }
    }
    fn push(&mut self, messages: &[proto::Envelope]) {
        let bytes = messages
            .iter()
            .flat_map(Message::encode_length_delimited_to_vec)
            .collect();
        let body = encode(&dict([("params", dict([("data", Value::Data(bytes))]))])).unwrap();
        self.send(&frame(b"sync", b"comm", 42, &body).unwrap());
    }
}
#[test]
fn encrypted_airplay_peer_exercises_verify_setup_subscription_updates_and_disconnect() {
    use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
    use x25519_dalek::{PublicKey, StaticSecret};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let settings = Settings::new(
        listener.local_addr().unwrap().ip(),
        listener.local_addr().unwrap().port(),
    )
    .unwrap();
    let signing = SigningKey::from_bytes(&[77; 32]);
    let client = SigningKey::from_bytes(&[88; 32]);
    let credentials = Credentials(crate::Credentials {
        client_id: b"fixture-client".to_vec(),
        client_secret: client.to_bytes(),
        device_id: b"fixture-device".to_vec(),
        device_public: signing.verifying_key().to_bytes(),
    });
    let (sent, wait) = std::sync::mpsc::channel();
    let peer = thread::spawn(move || {
        let mut control = Peer::new(listener.accept().unwrap().0);
        let request = control.http();
        assert!(request.first.starts_with("POST /pair-verify "));
        let values = untlv(&request.body).unwrap();
        sequence(&values, 1).unwrap();
        let client_public: [u8; 32] = field(&values, 3).unwrap().try_into().unwrap();
        let secret = StaticSecret::from([33; 32]);
        let public = PublicKey::from(&secret).to_bytes();
        let shared = secret.diffie_hellman(&PublicKey::from(client_public));
        let key = crypto::derive(
            shared.as_bytes(),
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        )
        .unwrap();
        let signature = signing
            .sign(&[public.as_slice(), b"fixture-device", &client_public].concat())
            .to_bytes();
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(b"PV-Msg02");
        let encrypted = crypto::seal(
            &key,
            &nonce,
            &tlv(&[(1, b"fixture-device"), (10, &signature)]),
            &[],
        )
        .unwrap();
        control.reply(&request, &tlv(&[(6, &[2]), (3, &public), (5, &encrypted)]));
        let request = control.http();
        let values = untlv(&request.body).unwrap();
        sequence(&values, 3).unwrap();
        nonce[4..].copy_from_slice(b"PV-Msg03");
        let decrypted = crypto::open(&key, &nonce, field(&values, 5).unwrap(), &[]).unwrap();
        let values = untlv(&decrypted).unwrap();
        assert_eq!(field(&values, 1).unwrap(), b"fixture-client");
        VerifyingKey::from_bytes(&client.verifying_key().to_bytes())
            .unwrap()
            .verify_strict(
                &[client_public.as_slice(), b"fixture-client", &public].concat(),
                &Signature::from_slice(field(&values, 10).unwrap()).unwrap(),
            )
            .unwrap();
        control.reply(&request, &tlv(&[(6, &[4])]));
        control.enable(
            shared.as_bytes(),
            "Control-Salt",
            "Control-Write-Encryption-Key",
            "Control-Read-Encryption-Key",
        );
        let events = TcpListener::bind("127.0.0.1:0").unwrap();
        let data = TcpListener::bind("127.0.0.1:0").unwrap();
        let request = control.http();
        assert!(request.first.starts_with("SETUP "));
        let body = decode(&request.body).unwrap();
        assert_eq!(
            body.as_dictionary().unwrap()["isRemoteControlOnly"].as_boolean(),
            Some(true)
        );
        control.reply(
            &request,
            &encode(&dict([(
                "eventPort",
                u64::from(events.local_addr().unwrap().port()).into(),
            )]))
            .unwrap(),
        );
        let mut event = Peer::new(events.accept().unwrap().0);
        event.enable(
            shared.as_bytes(),
            "Events-Salt",
            "Events-Read-Encryption-Key",
            "Events-Write-Encryption-Key",
        );
        let request = control.http();
        assert!(request.first.starts_with("RECORD "));
        control.reply(&request, &[]);
        let request = control.http();
        let body = decode(&request.body).unwrap();
        let stream = &body.as_dictionary().unwrap()["streams"].as_array().unwrap()[0];
        let seed = stream.as_dictionary().unwrap()["seed"]
            .as_unsigned_integer()
            .unwrap();
        assert_eq!(
            stream.as_dictionary().unwrap()["type"].as_unsigned_integer(),
            Some(130)
        );
        control.reply(
            &request,
            &encode(&dict([(
                "streams",
                Value::Array(vec![dict([(
                    "dataPort",
                    u64::from(data.local_addr().unwrap().port()).into(),
                )])]),
            )]))
            .unwrap(),
        );
        let mut data = Peer::new(data.accept().unwrap().0);
        data.enable(
            shared.as_bytes(),
            &format!("DataStream-Salt{seed}"),
            "DataStream-Output-Encryption-Key",
            "DataStream-Input-Encryption-Key",
        );
        let info = data.data().remove(0);
        assert_eq!(info.kind, Some(15));
        let mut response = envelope(15);
        response.identifier = info.identifier;
        data.push(&[response]);
        let connected = data.data().remove(0);
        assert_eq!(connected.connection.unwrap().state, Some(2));
        let updates = data.data().remove(0);
        assert_eq!(updates.updates.as_ref().unwrap().now_playing, Some(true));
        let mut response = envelope(16);
        response.identifier = updates.identifier;
        data.push(&[response, playing("music", "Peer song"), active("music")]);
        event.send(b"POST /event HTTP/1.1\r\nCSeq: 17\r\nContent-Length: 0\r\n\r\n");
        let reply = event.http();
        assert!(reply.first.contains("200 OK"));
        assert_eq!(reply.headers["cseq"], "17");
        let feedback = control.http();
        assert!(feedback.first.starts_with("POST /feedback "));
        control.reply(&feedback, &[]);
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let mut client = Client::connect(&settings, &credentials).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.now_playing().title.is_none() && Instant::now() < deadline {
        client.poll().unwrap();
    }
    assert_eq!(client.now_playing().title.as_deref(), Some("Peer song"));
    client.session.as_mut().unwrap().last_feedback = Instant::now() - Duration::from_secs(3);
    client.poll().unwrap();
    sent.send(()).unwrap();
    peer.join().unwrap();
    assert!(client.poll().is_err());
    assert_eq!(client.now_playing(), NowPlaying::default());
    assert!(client.poll().is_err());
}

#[test]
fn hap_record_authentication_rejects_tampering_and_excessive_lengths() {
    for oversized in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let bytes = if oversized {
                1025u16.to_le_bytes().to_vec()
            } else {
                let mut b = 1u16.to_le_bytes().to_vec();
                b.extend([0; 17]);
                b
            };
            socket.write_all(&bytes).unwrap();
        });
        let settings = Settings::new(endpoint.ip(), endpoint.port()).unwrap();
        let mut channel = Channel::connect(&settings, endpoint.port()).unwrap();
        channel.enable(&[7; 32], "salt", "write", "read").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            match channel.read(Duration::from_millis(50)) {
                Err(e) => break e,
                _ if Instant::now() < deadline => {}
                _ => panic!("invalid record accepted"),
            }
        };
        assert_eq!(
            error,
            if oversized {
                Error::Protocol
            } else {
                Error::Authentication
            }
        );
        peer.join().unwrap();
    }
}
#[test]
fn credentials_are_redacted_and_invalid_airplay_endpoint_is_rejected() {
    assert!(Settings::new("0.0.0.0".parse().unwrap(), 7000).is_err());
    assert!(Settings::new("127.0.0.1".parse().unwrap(), 0).is_err());
    let credentials = Credentials(crate::Credentials {
        client_id: b"private-id".to_vec(),
        client_secret: [1; 32],
        device_id: vec![],
        device_public: [2; 32],
    });
    assert_eq!(format!("{credentials:?}"), "AirPlayCredentials([redacted])");
}
