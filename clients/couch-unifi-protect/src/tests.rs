use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
};
struct Peer {
    origin: String,
    ca: Vec<u8>,
    pin: String,
    worker: Option<thread::JoinHandle<Option<String>>>,
}
impl Peer {
    fn new(status: u16, content: &str, body: Vec<u8>, headers: &str) -> Self {
        Self::with_delay(status, content, body, headers, Duration::ZERO)
    }
    fn with_delay(
        status: u16,
        content: &str,
        body: Vec<u8>,
        headers: &str,
        delay: Duration,
    ) -> Self {
        Self::with_signature(
            status,
            content,
            body,
            headers,
            delay,
            false,
            &rustls::version::TLS13,
        )
    }
    fn with_signature(
        status: u16,
        content: &str,
        body: Vec<u8>,
        headers: &str,
        delay: Duration,
        forged: bool,
        version: &'static rustls::SupportedProtocolVersion,
    ) -> Self {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let ca = certified.cert.pem().into_bytes();
        use sha2::Digest;
        let pin = format!("{:x}", sha2::Sha256::digest(certified.cert.der()));
        let key = if forged {
            rcgen::generate_simple_self_signed(vec!["localhost".into()])
                .unwrap()
                .key_pair
                .serialize_der()
        } else {
            certified.key_pair.serialize_der()
        };
        #[derive(Debug)]
        struct Resolver(Arc<rustls::sign::CertifiedKey>);
        impl rustls::server::ResolvesServerCert for Resolver {
            fn resolve(
                &self,
                _: rustls::server::ClientHello<'_>,
            ) -> Option<Arc<rustls::sign::CertifiedKey>> {
                Some(self.0.clone())
            }
        }
        // A different signing key deliberately simulates possession of the
        // public pinned certificate without possession of its private key.
        let key = rustls::crypto::ring::sign::any_supported_type(
            &rustls::pki_types::PrivatePkcs8KeyDer::from(key).into(),
        )
        .unwrap();
        let certified_key =
            rustls::sign::CertifiedKey::new(vec![certified.cert.der().clone()], key);
        let config = rustls::ServerConfig::builder_with_protocol_versions(&[version])
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Resolver(Arc::new(certified_key))));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let response=[format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",body.len()).into_bytes(),body].concat();
        let worker = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = rustls::StreamOwned::new(conn, socket);
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => request.push(byte[0]),
                    _ => return None,
                }
                assert!(request.len() < 16384);
            }
            thread::sleep(delay);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
            Some(String::from_utf8(request).unwrap())
        });
        Self {
            origin,
            ca,
            pin,
            worker: Some(worker),
        }
    }
    fn client(&self) -> Client {
        Client::new(
            &self.origin,
            ApiKey::new("fixture-secret".into()).unwrap(),
            Some(&self.ca),
            Duration::from_secs(2),
        )
        .unwrap()
    }
    fn request(mut self) -> Option<String> {
        self.worker.take().unwrap().join().unwrap()
    }
}
#[test]
fn trusted_tls_discovers_typed_cameras_with_api_key_and_exact_route() {
    let p=Peer::new(200,"application/json",br#"[{"id":"abc123","name":"Front door","type":"G4","state":"CONNECTED","hasPackageCamera":true,"futureField":1}]"#.to_vec(),"");
    let cameras = p.client().cameras().unwrap();
    assert_eq!(cameras[0].name.as_deref(), Some("Front door"));
    assert!(cameras[0].has_package_camera);
    let request = p.request().unwrap();
    assert!(request.starts_with("GET /proxy/protect/integration/v1/cameras HTTP/1.1\r\n"));
    assert!(request
        .to_lowercase()
        .contains("x-api-key: fixture-secret\r\n"));
}
#[test]
fn untrusted_tls_is_rejected_before_sending_api_key() {
    let p = Peer::new(200, "application/json", b"[]".to_vec(), "");
    let client = Client::new(
        &p.origin,
        ApiKey::new("fixture-secret".into()).unwrap(),
        None,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.cameras().unwrap_err(), Error::Transport);
    assert!(p.request().is_none());
}
#[test]
fn private_ca_does_not_disable_hostname_checks() {
    let p = Peer::new(200, "application/json", b"[]".to_vec(), "");
    let client = Client::new(
        &p.origin.replace("localhost", "127.0.0.1"),
        ApiKey::new("fixture-secret".into()).unwrap(),
        Some(&p.ca),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.cameras().unwrap_err(), Error::Transport);
    assert!(p.request().is_none());
}
#[test]
fn existing_stream_preserves_srtp_query_and_local_expiry_without_mutation() {
    let p = Peer::new(
        200,
        "application/json",
        br#"{"low":"rtsps://localhost:7441/fixture-token?enableSrtp","high":null}"#.to_vec(),
        "",
    );
    let mut view = p
        .client()
        .live_view("abc123", Quality::Low, Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        view.url().unwrap(),
        "rtsps://localhost:7441/fixture-token?enableSrtp"
    );
    assert!(!format!("{view:?}").contains("fixture-token"));
    view.deadline = Instant::now();
    assert_eq!(view.url(), Err(Error::Expired));
    view.close();
    assert!(p
        .request()
        .unwrap()
        .starts_with("GET /proxy/protect/integration/v1/cameras/abc123/rtsps-stream "));
}
#[test]
fn missing_quality_is_explicit_and_never_enables_a_stream() {
    let p = Peer::new(200, "application/json", br#"{"low":null}"#.to_vec(), "");
    assert_eq!(
        p.client()
            .live_view("abc123", Quality::Low, Duration::from_secs(1))
            .unwrap_err(),
        Error::StreamNotEnabled
    );
    assert!(p.request().unwrap().starts_with("GET "));
}
#[test]
fn redirected_api_key_is_never_forwarded_and_error_body_is_not_reported() {
    let p = Peer::new(
        302,
        "text/plain",
        b"fixture-secret".to_vec(),
        "Location: https://example.invalid/steal\r\n",
    );
    assert_eq!(p.client().cameras().unwrap_err(), Error::Status(302));
    p.request();
    for (status, error) in [
        (401, Error::Authentication),
        (403, Error::Permission),
        (404, Error::NotFound),
        (429, Error::RateLimited),
        (503, Error::Offline),
    ] {
        let p = Peer::new(status, "application/json", b"fixture-secret".to_vec(), "");
        assert_eq!(p.client().cameras().unwrap_err(), error);
        p.request();
    }
}
#[test]
fn snapshots_are_bounded_jpeg_and_request_small_main_channel() {
    let bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0xff, 0xd9];
    let p = Peer::new(200, "image/jpeg", bytes.clone(), "");
    assert_eq!(
        p.client()
            .snapshot("abc123", SnapshotChannel::Main)
            .unwrap(),
        bytes
    );
    assert!(p
        .request()
        .unwrap()
        .contains("snapshot?channel=main&highQuality=false "));
    let p = Peer::new(200, "image/jpeg", b"not jpeg".to_vec(), "");
    assert_eq!(
        p.client().snapshot("abc123", SnapshotChannel::Main),
        Err(Error::Response)
    );
    p.request();
}
#[test]
fn response_limits_and_wrong_content_are_rejected() {
    let p = Peer::new(
        200,
        "application/json",
        vec![b' '; JSON_LIMIT as usize + 1],
        "",
    );
    assert_eq!(p.client().cameras(), Err(Error::Response));
    p.request();
    let p = Peer::new(200, "text/html", b"login page".to_vec(), "");
    assert_eq!(p.client().cameras(), Err(Error::Response));
    p.request();
}
#[test]
fn rejects_injected_ids_credentials_urls_and_cross_host_streams() {
    for id in ["../secret", "a?x=1", "a/b", "a#b", ""] {
        assert!(camera_path(id).is_err());
    }
    for key in ["", "secret\r\nOther: injected", "secret value"] {
        assert!(ApiKey::new(key.into()).is_err());
    }
    assert!(
        !format!("{:?}", ApiKey::new("fixture-secret".into()).unwrap()).contains("fixture-secret")
    );
    for origin in [
        "http://localhost",
        "https://user:pass@localhost",
        "https://localhost/path",
        "https://localhost?x=1",
    ] {
        assert!(Client::new(
            origin,
            ApiKey::new("fake".into()).unwrap(),
            None,
            Duration::from_secs(1)
        )
        .is_err());
    }
    let origin = vec!["localhost".to_owned()];
    for stream in [
        "rtsp://localhost/token",
        "file:///token",
        "rtsps://other/token",
        "rtsps://user:pass@localhost/token",
        "rtsps://localhost/",
        "rtsps://localhost/token#x",
    ] {
        assert!(validate_stream(stream, &origin).is_err());
    }
}

#[test]
fn application_version_and_explicit_stream_host_authorization() {
    let p = Peer::new(
        200,
        "application/json",
        br#"{"applicationVersion":"7.3.47"}"#.to_vec(),
        "",
    );
    assert_eq!(p.client().application_version().unwrap(), "7.3.47");
    assert!(p.request().unwrap().contains("/v1/meta/info "));
    let p = Peer::new(
        200,
        "application/json",
        br#"{"low":"rtsps://192.0.2.10:7441/token?enableSrtp"}"#.to_vec(),
        "",
    );
    let client = p.client().with_stream_host("192.0.2.10").unwrap();
    assert!(client
        .live_view("abc123", Quality::Low, Duration::from_secs(1))
        .is_ok());
    p.request();
}

#[test]
fn stalled_authenticated_response_obeys_request_timeout() {
    let p = Peer::with_delay(
        200,
        "application/json",
        b"[]".to_vec(),
        "",
        Duration::from_millis(250),
    );
    let client = Client::new(
        &p.origin,
        ApiKey::new("fixture-secret".into()).unwrap(),
        Some(&p.ca),
        Duration::from_millis(50),
    )
    .unwrap();
    let start = Instant::now();
    assert_eq!(client.cameras().unwrap_err(), Error::Transport);
    assert!(start.elapsed() < Duration::from_secs(1));
    p.request();
}

#[test]
fn explicit_pin_accepts_self_signed_certificate_for_configured_alias() {
    let p = Peer::new(200, "application/json", b"[]".to_vec(), "");
    let client = Client::new_pinned(
        &p.origin.replace("localhost", "127.0.0.1"),
        ApiKey::new("fixture-secret".into()).unwrap(),
        &p.pin,
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(client.cameras().unwrap().is_empty());
    assert!(p
        .request()
        .unwrap()
        .to_lowercase()
        .contains("x-api-key: fixture-secret\r\n"));
}
#[test]
fn wrong_pin_rejects_peer_before_any_http_headers() {
    let p = Peer::new(200, "application/json", b"[]".to_vec(), "");
    let client = Client::new_pinned(
        &p.origin,
        ApiKey::new("fixture-secret".into()).unwrap(),
        &"00".repeat(32),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.cameras().unwrap_err(), Error::Transport);
    assert!(p.request().is_none());
}
#[test]
fn correct_pin_does_not_accept_forged_handshake_signature() {
    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let p = Peer::with_signature(
            200,
            "application/json",
            b"[]".to_vec(),
            "",
            Duration::ZERO,
            true,
            version,
        );
        let client = Client::new_pinned(
            &p.origin,
            ApiKey::new("fixture-secret".into()).unwrap(),
            &p.pin,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(client.cameras().unwrap_err(), Error::Transport);
        assert!(p.request().is_none());
    }
}
#[test]
fn pinned_peer_redirect_is_not_followed() {
    let p = Peer::new(
        302,
        "application/json",
        b"[]".to_vec(),
        "Location: https://127.0.0.1:1/stolen\r\n",
    );
    let client = Client::new_pinned(
        &p.origin,
        ApiKey::new("fixture-secret".into()).unwrap(),
        &p.pin,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.cameras().unwrap_err(), Error::Status(302));
    assert!(p
        .request()
        .unwrap()
        .starts_with("GET /proxy/protect/integration/v1/cameras "));
}
#[test]
fn malformed_pin_is_configuration_error() {
    for pin in ["", "abc", &"gg".repeat(32), &"0".repeat(65)] {
        assert!(matches!(
            Client::new_pinned(
                "https://localhost",
                ApiKey::new("fixture-secret".into()).unwrap(),
                pin,
                Duration::from_secs(2)
            ),
            Err(Error::Configuration)
        ));
    }
}
