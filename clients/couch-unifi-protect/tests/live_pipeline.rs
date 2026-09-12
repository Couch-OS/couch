#![cfg(all(feature = "media", unix))]
use base64::{engine::general_purpose::STANDARD, Engine};
use couch_unifi_protect::{
    player::{Player, Status},
    settings::Settings,
};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::Digest;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use webrtc_srtp::{context::Context, protection_profile::ProtectionProfile};
fn accept(listener: TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        match listener.accept() {
            Ok((s, _)) => {
                s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                s.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                return s;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < end, "fixture connection timed out");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}
fn request(stream: &mut impl Read) -> String {
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        data.push(byte[0]);
        assert!(data.len() < 8192);
    }
    String::from_utf8(data).unwrap()
}
fn nals(data: &[u8]) -> Vec<&[u8]> {
    let mut boundaries = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let length = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            0
        };
        if length > 0 {
            boundaries.push((i, i + length));
            i += length;
        } else {
            i += 1;
        }
    }
    boundaries
        .iter()
        .enumerate()
        .map(|(i, (_, start))| {
            &data[*start..boundaries.get(i + 1).map(|v| v.0).unwrap_or(data.len())]
        })
        .filter(|n| !n.is_empty())
        .collect()
}
#[test]
#[ignore = "requires the packaged Alpine FFmpeg dependency footprint; CI runs explicitly"]
fn pinned_srtps_to_bounded_rgb_frames_and_cancellation() {
    if !std::path::Path::new("/usr/bin/ffmpeg").is_file() {
        eprintln!("SKIP: /usr/bin/ffmpeg is required for the decoder fixture");
        return;
    }
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pin = format!("{:x}", sha2::Sha256::digest(certificate.cert.der()));
    let tls = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der())
                    .into(),
            )
            .unwrap(),
    );
    let api = TcpListener::bind("127.0.0.1:0").unwrap();
    let media = TcpListener::bind("127.0.0.1:0").unwrap();
    let api_origin = format!("https://localhost:{}", api.local_addr().unwrap().port());
    let media_origin = format!("rtsps://localhost:{}/", media.local_addr().unwrap().port());
    let media_url = format!("{}fixture-private-stream?enableSrtp", media_origin);
    let api_tls = tls.clone();
    let api_worker = thread::spawn(move || {
        let mut stream = StreamOwned::new(ServerConnection::new(api_tls).unwrap(), accept(api));
        let input = request(&mut stream);
        assert!(input.starts_with("GET /proxy/protect/integration/v1/cameras/fixture/rtsps-stream"));
        assert!(input.to_lowercase().contains("x-api-key: fixture-api-key"));
        let body = serde_json::json!({"low":media_url}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        stream.flush().unwrap();
    });
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let media_worker = thread::spawn(move || {
        let mut stream = StreamOwned::new(ServerConnection::new(tls).unwrap(), accept(media));
        let key = [8; 30];
        let encoded = STANDARD.encode(key);
        for sequence in 1..=3 {
            let input = request(&mut stream);
            assert!(!input.contains("fixture-api-key"));
            assert!(!input.to_lowercase().contains("x-api-key"));
            let response = match sequence {
                1 => {
                    assert!(input.starts_with("DESCRIBE "));
                    let body=format!("v=0\r\nm=video 0 RTP/SAVP 96\r\na=rtpmap:96 H264/90000\r\na=control:track0\r\na=fmtp:96 packetization-mode=1\r\na=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:{encoded}\r\n");
                    format!(
                        "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                }
                2 => {
                    assert!(input.contains("Transport: RTP/SAVP/TCP;unicast;interleaved=0-1"));
                    "RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: test-session\r\nTransport: RTP/SAVP/TCP;unicast;interleaved=0-1\r\n\r\n".into()
                }
                _ => "RTSP/1.0 200 OK\r\nCSeq: 3\r\n\r\n".into(),
            };
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        let mut crypto = Context::new(
            &key[..16],
            &key[16..],
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .unwrap();
        let mut sequence = 0u16;
        let mut timestamp = 0u32;
        let fixture = nals(include_bytes!("fixtures/test-pattern.h264"));
        for _ in 0..16 {
            for nal in &fixture {
                if done.load(Ordering::Acquire) {
                    return;
                }
                let chunks = if nal.len() > 1024 {
                    nal[1..]
                        .chunks(1024)
                        .enumerate()
                        .map(|(i, data)| {
                            let mut value = vec![
                                (nal[0] & 0xe0) | 28,
                                (nal[0] & 31)
                                    | if i == 0 { 0x80 } else { 0 }
                                    | if (i + 1) * 1024 >= nal.len() - 1 {
                                        0x40
                                    } else {
                                        0
                                    },
                            ];
                            value.extend(data);
                            value
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![nal.to_vec()]
                };
                for payload in chunks {
                    let mut packet = vec![0x80, 96];
                    packet.extend(sequence.to_be_bytes());
                    packet.extend(timestamp.to_be_bytes());
                    packet.extend(7u32.to_be_bytes());
                    packet.extend(payload);
                    sequence = sequence.wrapping_add(1);
                    let encrypted = crypto.encrypt_rtp(&packet).unwrap();
                    let mut header = vec![b'$', 0];
                    header.extend((encrypted.len() as u16).to_be_bytes());
                    if stream
                        .write_all(&header)
                        .and_then(|_| stream.write_all(&encrypted))
                        .and_then(|_| stream.flush())
                        .is_err()
                    {
                        return;
                    }
                }
                if matches!(nal[0] & 31, 1 | 5) {
                    timestamp = timestamp.wrapping_add(11250);
                    thread::sleep(Duration::from_millis(125));
                }
            }
        }
    });
    let settings:Settings=serde_json::from_value(serde_json::json!({"origin":api_origin,"api_key":"fixture-api-key","certificate_sha256":pin,"media_certificate_sha256":pin,"media_origin":media_origin})).unwrap();
    let player = Player::start(settings, "fixture".into());
    let until = Instant::now() + Duration::from_secs(8);
    let mut first = None;
    let mut changed = false;
    while Instant::now() < until {
        if let Some(frame) = player.take_frame() {
            assert_eq!(frame.len(), 480 * 270 * 3);
            if let Some(before) = &first {
                if *before != frame {
                    changed = true;
                    break;
                }
            } else {
                first = Some(frame);
            }
        }
        if player.status() == Status::Unavailable {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let final_status = player.status();
    let close = Instant::now();
    drop(player);
    stop.store(true, Ordering::Release);
    api_worker.join().unwrap();
    media_worker.join().unwrap();
    assert!(close.elapsed() < Duration::from_secs(1));
    assert!(changed, "moving frames missing, state {final_status:?}");
}
