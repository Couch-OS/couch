//! Verified RTSPS/TCP ingest. No URLs, SDES keys or session identifiers are logged.
//! Supports H264 packetization mode 0/1 and SDES AES_CM_128_HMAC_SHA1_80.
use crate::{settings::Settings, Error, LiveView, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, ServerName},
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;
use webrtc_srtp::{
    context::Context, option::srtp_replay_protection, protection_profile::ProtectionProfile,
};
use zeroize::Zeroizing;
const MAX_NAL: usize = 2 * 1024 * 1024;
pub use crate::media_io::Cancellation;
type Tls = StreamOwned<ClientConnection, crate::media_io::Socket>;
fn bad<T>() -> Result<T> {
    Err(Error::Response)
}

pub struct Session {
    tls: Tls,
    url: Url,
    cseq: u32,
    session: Zeroizing<String>,
    deadline: Instant,
    keepalive: Instant,
    payload_type: u8,
    srtp: Option<Context>,
    depacketizer: H264,
    initialization: Zeroizing<Vec<u8>>,
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectMedia([redacted])")
    }
}
impl Session {
    pub fn connect(view: &LiveView, settings: &Settings) -> Result<Self> {
        Self::connect_cancellable(view, settings, Arc::new(Cancellation::default()))
    }
    pub fn connect_cancellable(
        view: &LiveView,
        settings: &Settings,
        cancel: Arc<Cancellation>,
    ) -> Result<Self> {
        settings.client()?; // Validate explicit trust scope even for direct Session callers.
        let url = Url::parse(view.url()?).map_err(|_| Error::Configuration)?;
        if url.scheme() != "rtsps" {
            return Err(Error::Configuration);
        }
        if let Some(approved) = &settings.media_origin {
            let approved = Url::parse(approved).map_err(|_| Error::Configuration)?;
            if (url.host_str(), url.port().unwrap_or(322))
                != (approved.host_str(), approved.port().unwrap_or(322))
            {
                return Err(Error::Configuration);
            }
        }
        let host = url
            .host_str()
            .ok_or(Error::Configuration)?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = url.port().unwrap_or(322);
        let config = media_tls(settings)?;
        let name = ServerName::try_from(host.to_owned()).map_err(|_| Error::Configuration)?;
        let mut socket = None;
        for address in crate::media_io::resolve(host, port, view.expires_at(), &cancel)
            .map_err(|_| Error::Transport)?
        {
            if cancel.is_cancelled() || Instant::now() >= view.expires_at() {
                return Err(Error::Expired);
            }
            if let Ok(s) = TcpStream::connect_timeout(
                &address,
                view.expires_at()
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            ) {
                socket = Some(s);
                break;
            }
        }
        let socket = socket.ok_or(Error::Transport)?;
        let socket = crate::media_io::Socket::new(socket, view.expires_at(), cancel)
            .map_err(|_| Error::Transport)?;
        let mut tls = StreamOwned::new(
            ClientConnection::new(Arc::new(config), name).map_err(|_| Error::Transport)?,
            socket,
        );
        while tls.conn.is_handshaking() {
            tls.conn
                .complete_io(&mut tls.sock)
                .map_err(|_| Error::Transport)?;
        }
        let mut s = Self {
            tls,
            url,
            cseq: 0,
            session: Zeroizing::new(String::new()),
            deadline: view.expires_at(),
            keepalive: Instant::now(),
            payload_type: 0,
            srtp: None,
            depacketizer: H264::default(),
            initialization: Zeroizing::new(Vec::new()),
        };
        let describe = s.request("DESCRIBE", s.url.clone(), "Accept: application/sdp\r\n")?;
        let description = Sdp::parse(&describe.body)?;
        let base = match describe.headers.get("content-base") {
            Some(v) => checked_control(&s.url, v)?,
            None => s.url.clone(),
        };
        let track = checked_control(&base, &description.control)?;
        s.payload_type = description.payload_type;
        if s.url.query_pairs().any(|(key, _)| key == "enableSrtp") && description.srtp.is_none() {
            return bad();
        }
        s.srtp = description.srtp;
        s.initialization = description.initialization;
        let transport = if s.srtp.is_some() {
            "RTP/SAVP/TCP"
        } else {
            "RTP/AVP/TCP"
        };
        let setup = s.request(
            "SETUP",
            track,
            &format!("Transport: {transport};unicast;interleaved=0-1\r\n"),
        )?;
        let negotiated = setup.headers.get("transport").ok_or(Error::Response)?;
        if !negotiated.starts_with(transport)
            || !negotiated.split(';').any(|s| s.trim() == "interleaved=0-1")
        {
            return bad();
        }
        let session = setup
            .headers
            .get("session")
            .and_then(|s| s.split(';').next())
            .ok_or(Error::Response)?;
        if session.is_empty()
            || session.len() > 128
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return bad();
        }
        s.session = Zeroizing::new(session.into());
        s.request("PLAY", s.url.clone(), "Range: npt=0.000-\r\n")?;
        s.keepalive = Instant::now();
        Ok(s)
    }
    /// Returns one complete Annex-B NAL group; incomplete/lost fragments are discarded.
    pub fn next_h264(&mut self) -> Result<Vec<u8>> {
        if !self.initialization.is_empty() {
            return Ok(std::mem::take(&mut *self.initialization));
        }
        loop {
            if Instant::now() >= self.deadline {
                return Err(Error::Expired);
            }
            if self.keepalive.elapsed() >= Duration::from_secs(15) {
                self.request("OPTIONS", self.url.clone(), "")?;
                self.keepalive = Instant::now();
            }
            let packet = self.packet()?;
            let plaintext = match &mut self.srtp {
                Some(context) => context
                    .decrypt_rtp(&packet)
                    .map_err(|_| Error::Response)?
                    .to_vec(),
                None => packet,
            };
            if let Some(nal) = self.depacketizer.push(&plaintext, self.payload_type)? {
                return Ok(nal);
            }
        }
    }
    fn packet(&mut self) -> Result<Vec<u8>> {
        loop {
            let mut header = [0; 4];
            self.tls
                .read_exact(&mut header)
                .map_err(|_| Error::Transport)?;
            if header[0] != b'$' || header[1] > 1 {
                return bad();
            }
            let len = u16::from_be_bytes([header[2], header[3]]) as usize;
            if len < 4 {
                return bad();
            }
            let mut packet = vec![0; len];
            self.tls
                .read_exact(&mut packet)
                .map_err(|_| Error::Transport)?;
            if Instant::now() >= self.deadline {
                return Err(Error::Expired);
            }
            if header[1] == 0 {
                return Ok(packet);
            }
        }
    }
    fn request(&mut self, method: &str, url: Url, extra: &str) -> Result<Response> {
        if Instant::now() >= self.deadline {
            return Err(Error::Expired);
        }
        self.cseq = self.cseq.checked_add(1).ok_or(Error::Response)?;
        let session = if self.session.is_empty() {
            String::new()
        } else {
            format!("Session: {}\r\n", *self.session)
        };
        let request = Zeroizing::new(format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\n{session}{extra}\r\n",
            self.cseq
        ));
        self.tls
            .write_all(request.as_bytes())
            .map_err(|_| Error::Transport)?;
        // In-flight interleaved media may precede an OPTIONS/TEARDOWN reply.
        let result = read_response(&mut self.tls, self.deadline)?;
        if result
            .headers
            .get("cseq")
            .and_then(|v| v.parse::<u32>().ok())
            != Some(self.cseq)
        {
            return bad();
        }
        Ok(result)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Best effort only; close the TLS socket even when the peer is stalled.
        if !self.session.is_empty() {
            let message = Zeroizing::new(format!(
                "TEARDOWN {} RTSP/1.0\r\nCSeq: {}\r\nSession: {}\r\n\r\n",
                self.url,
                self.cseq.saturating_add(1),
                *self.session
            ));
            self.tls
                .sock
                .shorten(Instant::now() + Duration::from_millis(100));
            let _ = self.tls.write_all(message.as_bytes());
        }
        let _ = self.tls.sock.stream.shutdown(std::net::Shutdown::Both);
    }
}
fn media_tls(settings: &Settings) -> Result<ClientConfig> {
    if let Some(pin) = &settings.media_certificate_sha256 {
        return crate::pinning::tls_config(pin);
    }
    let mut roots = RootCertStore::empty();
    if let Some(pem) = &settings.private_ca_pem {
        for certificate in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            roots
                .add(certificate.map_err(|_| Error::Configuration)?)
                .map_err(|_| Error::Configuration)?;
        }
        if roots.is_empty() {
            return Err(Error::Configuration);
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    Ok(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| Error::Configuration)?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}
struct Response {
    headers: BTreeMap<String, String>,
    body: Zeroizing<Vec<u8>>,
}
fn read_response(stream: &mut impl Read, deadline: Instant) -> Result<Response> {
    let mut header = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Expired);
        }
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|_| Error::Transport)?;
        if header.is_empty() && byte[0] == b'$' {
            let mut rest = [0; 3];
            stream.read_exact(&mut rest).map_err(|_| Error::Transport)?;
            let len = u16::from_be_bytes([rest[1], rest[2]]) as usize;
            let mut discard = vec![0; len];
            stream
                .read_exact(&mut discard)
                .map_err(|_| Error::Transport)?;
            continue;
        }
        header.push(byte[0]);
        if header.len() > 8192 {
            return bad();
        }
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&header).map_err(|_| Error::Response)?;
    let mut lines = header.split("\r\n");
    if !lines.next().is_some_and(|s| s.starts_with("RTSP/1.0 200 ")) {
        return bad();
    }
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (key, value) = line.split_once(':').ok_or(Error::Response)?;
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().into())
            .is_some()
        {
            return bad();
        }
    }
    let size = match headers.get("content-length") {
        Some(v) => v.parse::<usize>().map_err(|_| Error::Response)?,
        None => 0,
    };
    if size > 65536 {
        return bad();
    }
    let mut body = Zeroizing::new(vec![0; size]);
    stream.read_exact(&mut body).map_err(|_| Error::Transport)?;
    Ok(Response { headers, body })
}
fn checked_control(base: &Url, value: &str) -> Result<Url> {
    if value.chars().any(char::is_control) {
        return bad();
    }
    let mut target = base.join(value).map_err(|_| Error::Response)?;
    if target.scheme() == "rtsp" {
        target.set_scheme("rtsps").map_err(|_| Error::Response)?;
    }
    if (
        target.scheme(),
        target.host_str(),
        target.port().unwrap_or(322),
    ) != (base.scheme(), base.host_str(), base.port().unwrap_or(322))
        || target.username() != ""
        || target.password().is_some()
        || target.fragment().is_some()
    {
        return bad();
    }
    Ok(target)
}
struct Sdp {
    control: String,
    payload_type: u8,
    srtp: Option<Context>,
    initialization: Zeroizing<Vec<u8>>,
}
impl Sdp {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Response)?;
        let section = text
            .split("m=")
            .skip(1)
            .find(|s| s.starts_with("video "))
            .ok_or(Error::Response)?;
        let mut lines = section.lines();
        let media = lines.next().ok_or(Error::Response)?;
        let fields = media.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || !matches!(fields[2], "RTP/AVP" | "RTP/SAVP") {
            return bad();
        }
        let payload_type = fields[3].parse::<u8>().map_err(|_| Error::Response)?;
        if payload_type > 127 {
            return bad();
        }
        let lines = lines.collect::<Vec<_>>();
        let prefix = format!("a=rtpmap:{payload_type} ");
        if !lines.iter().any(|l| {
            l.strip_prefix(&prefix)
                .is_some_and(|v| v.eq_ignore_ascii_case("H264/90000"))
        }) {
            return bad();
        }
        let control = lines
            .iter()
            .find_map(|s| s.strip_prefix("a=control:"))
            .ok_or(Error::Response)?
            .to_owned();
        let mut initialization = Zeroizing::new(Vec::new());
        if let Some(fmtp) = lines
            .iter()
            .find_map(|s| s.strip_prefix(&format!("a=fmtp:{payload_type} ")))
        {
            for parameter in fmtp.split(';').map(str::trim) {
                if let Some(mode) = parameter.strip_prefix("packetization-mode=") {
                    if mode != "0" && mode != "1" {
                        return bad();
                    }
                }
                if let Some(parameters) = parameter.strip_prefix("sprop-parameter-sets=") {
                    for value in parameters.split(',') {
                        let nal = STANDARD.decode(value).map_err(|_| Error::Response)?;
                        if nal.is_empty() || nal.len() > 4096 || !matches!(nal[0] & 31, 7 | 8) {
                            return bad();
                        }
                        initialization.extend_from_slice(&[0, 0, 0, 1]);
                        initialization.extend(nal);
                    }
                }
            }
        }
        let srtp = if let Some(crypto) = lines.iter().find_map(|s| s.strip_prefix("a=crypto:")) {
            let fields = crypto.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 || fields[1] != "AES_CM_128_HMAC_SHA1_80" {
                return bad();
            }
            let key = Zeroizing::new(
                STANDARD
                    .decode(fields[2].strip_prefix("inline:").ok_or(Error::Response)?)
                    .map_err(|_| Error::Response)?,
            );
            if key.len() != 30 {
                return bad();
            }
            Some(
                Context::new(
                    &key[..16],
                    &key[16..],
                    ProtectionProfile::Aes128CmHmacSha1_80,
                    Some(srtp_replay_protection(128)),
                    None,
                )
                .map_err(|_| Error::Response)?,
            )
        } else {
            if fields[2] == "RTP/SAVP" {
                return bad();
            }
            None
        };
        Ok(Self {
            control,
            payload_type,
            srtp,
            initialization,
        })
    }
}
#[derive(Default)]
struct H264 {
    fragment: Vec<u8>,
    sequence: Option<u16>,
    ssrc: Option<u32>,
    timestamp: u32,
}
impl H264 {
    fn push(&mut self, packet: &[u8], payload_type: u8) -> Result<Option<Vec<u8>>> {
        if packet.len() < 12 || packet[0] >> 6 != 2 || packet[1] & 127 != payload_type {
            return bad();
        }
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let timestamp = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        let ssrc = u32::from_be_bytes(packet[8..12].try_into().unwrap());
        if self.ssrc.is_some_and(|s| s != ssrc) {
            return bad();
        }
        self.ssrc = Some(ssrc);
        if self.sequence.is_some_and(|s| s.wrapping_add(1) != sequence)
            || self.timestamp != timestamp
        {
            self.fragment.clear();
        }
        self.sequence = Some(sequence);
        self.timestamp = timestamp;
        let mut offset = 12 + 4 * (packet[0] & 15) as usize;
        if packet[0] & 16 != 0 {
            if packet.len() < offset + 4 {
                return bad();
            }
            offset += 4 + 4 * u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        }
        let padding = if packet[0] & 32 != 0 {
            let n = *packet.last().unwrap() as usize;
            if n == 0 {
                return bad();
            }
            n
        } else {
            0
        };
        if offset + padding >= packet.len() {
            return bad();
        }
        let payload = &packet[offset..packet.len() - padding];
        match payload[0] & 31 {
            1..=23 => {
                self.fragment.clear();
                let mut out = vec![0, 0, 0, 1];
                out.extend_from_slice(payload);
                Ok(Some(out))
            }
            24 => {
                self.fragment.clear();
                let mut out = Vec::new();
                let mut pos = 1;
                while pos < payload.len() {
                    if pos + 2 > payload.len() {
                        return bad();
                    }
                    let n = u16::from_be_bytes([payload[pos], payload[pos + 1]]) as usize;
                    pos += 2;
                    if n == 0 || pos + n > payload.len() || !matches!(payload[pos] & 31, 1..=23) {
                        return bad();
                    }
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(&payload[pos..pos + n]);
                    pos += n;
                }
                Ok(Some(out))
            }
            28 => {
                if payload.len() < 3 {
                    return bad();
                }
                let flags = payload[1];
                if flags & 0x20 != 0 || flags & 0xc0 == 0xc0 || !matches!(flags & 31, 1..=23) {
                    return bad();
                }
                if flags & 0x80 != 0 {
                    self.fragment = vec![0, 0, 0, 1, (payload[0] & 0xe0) | (flags & 31)];
                } else if self.fragment.is_empty() {
                    return Ok(None);
                }
                if self.fragment[4] != (payload[0] & 0xe0) | (flags & 31) {
                    return bad();
                }
                if self.fragment.len() + payload.len() - 2 > MAX_NAL {
                    return bad();
                }
                self.fragment.extend_from_slice(&payload[2..]);
                if flags & 0x40 != 0 {
                    Ok(Some(std::mem::take(&mut self.fragment)))
                } else {
                    Ok(None)
                }
            }
            _ => bad(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rtp(sequence: u16, timestamp: u32, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 96];
        packet.extend(sequence.to_be_bytes());
        packet.extend(timestamp.to_be_bytes());
        packet.extend(7u32.to_be_bytes());
        packet.extend(payload);
        packet
    }
    fn sdp(extra: &str) -> Vec<u8> {
        format!("v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:trackID=0\r\na=fmtp:96 packetization-mode=1\r\n{extra}").into_bytes()
    }
    #[test]
    fn rtp_fragment_loss_never_splices_a_corrupt_nal() {
        let mut h = H264::default();
        assert!(h
            .push(&rtp(65535, 42, &[0x7c, 0x85, 1, 2]), 96)
            .unwrap()
            .is_none());
        assert_eq!(
            h.push(&rtp(0, 42, &[0x7c, 0x45, 3, 4]), 96).unwrap(),
            Some(vec![0, 0, 0, 1, 0x65, 1, 2, 3, 4])
        );
        assert!(h.push(&rtp(1, 43, &[0x7c, 0x85, 1]), 96).unwrap().is_none());
        assert!(h.push(&rtp(3, 43, &[0x7c, 0x45, 2]), 96).unwrap().is_none());
        assert!(h.push(&rtp(4, 44, &[0x7c, 0xc5, 1]), 96).is_err());
    }
    #[test]
    fn fu_continuations_must_preserve_type_and_nri() {
        for tail in [[0x5c, 0x45, 2], [0x7c, 0x44, 2]] {
            let mut h = H264::default();
            h.push(&rtp(1, 5, &[0x7c, 0x85, 1]), 96).unwrap();
            assert!(h.push(&rtp(2, 5, &tail), 96).is_err());
        }
    }
    #[test]
    fn aggregation_padding_and_extension_lengths_are_checked() {
        let mut h = H264::default();
        assert_eq!(
            h.push(&rtp(1, 1, &[0x78, 0, 2, 0x67, 1, 0, 2, 0x68, 2]), 96)
                .unwrap(),
            Some(vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2])
        );
        assert!(h.push(&rtp(2, 1, &[0x78, 0, 9, 0x67]), 96).is_err());
        let mut p = rtp(3, 1, &[0x65, 1]);
        p[0] |= 16;
        assert!(h.push(&p, 96).is_err());
        p[0] = 0xa0;
        *p.last_mut().unwrap() = 255;
        assert!(h.push(&p, 96).is_err());
    }
    #[test]
    fn sdp_refuses_unsupported_codec_crypto_and_packetization() {
        assert!(Sdp::parse(&sdp("")).is_ok());
        assert!(Sdp::parse(
            &String::from_utf8(sdp(""))
                .unwrap()
                .replace("H264/90000", "H265/90000")
                .into_bytes()
        )
        .is_err());
        assert!(Sdp::parse(
            &String::from_utf8(sdp(""))
                .unwrap()
                .replace("packetization-mode=1", "packetization-mode=2")
                .into_bytes()
        )
        .is_err());
        assert!(Sdp::parse(&sdp("a=crypto:1 INVALID inline:AA==\r\n")).is_err());
        assert!(Sdp::parse(&sdp("a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:AA==\r\n")).is_err());
    }
    #[test]
    fn sdes_srtp_authenticates_and_rejects_replay_before_depacketizing() {
        let key = [9; 30];
        let encoded = STANDARD.encode(key);
        let mut description = Sdp::parse(&sdp(&format!(
            "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:{encoded}\r\n"
        )))
        .unwrap();
        let mut sender = Context::new(
            &key[..16],
            &key[16..],
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .unwrap();
        let clear = rtp(1, 5, &[0x65, 8]);
        let encrypted = sender.encrypt_rtp(&clear).unwrap();
        let receiver = description.srtp.as_mut().unwrap();
        assert_eq!(receiver.decrypt_rtp(&encrypted).unwrap().as_ref(), clear);
        assert!(receiver.decrypt_rtp(&encrypted).is_err());
        let mut damaged = sender.encrypt_rtp(&rtp(2, 5, &[0x65, 9])).unwrap().to_vec();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(receiver.decrypt_rtp(&damaged).is_err());
    }
    #[test]
    fn control_urls_cannot_change_media_origin_or_inject_headers() {
        let base = Url::parse("rtsps://console.test:7441/private/").unwrap();
        assert_eq!(
            checked_control(&base, "trackID=0").unwrap().as_str(),
            "rtsps://console.test:7441/private/trackID=0"
        );
        assert!(checked_control(&base, "rtsp://console.test:7441/private/trackID=0").is_ok());
        for bad in [
            "rtsps://other.test:7441/private",
            "rtsps://console.test:8443/private",
            "track\r\nInjected:yes",
            "https://console.test:7441/",
        ] {
            assert!(checked_control(&base, bad).is_err());
        }
    }
    #[test]
    fn rtsp_response_bounds_status_and_duplicate_headers_are_enforced() {
        for bytes in [
            b"RTSP/1.0 302 Redirect\r\n\r\n".as_slice(),
            b"RTSP/1.0 200 OK\r\nContent-Length: 65537\r\n\r\n",
            b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nCSeq: 2\r\n\r\n",
        ] {
            assert!(read_response(
                &mut std::io::Cursor::new(bytes),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err());
        }
        let response = read_response(
            &mut std::io::Cursor::new(
                b"$\x01\x00\x04ABCDRTSP/1.0 200 OK\r\nCSeq: 9\r\nContent-Length: 3\r\n\r\nsdp",
            ),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(response.headers["cseq"], "9");
        assert_eq!(&*response.body, b"sdp");
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    use sha2::Digest;
    use std::{net::TcpListener, thread};
    fn request(stream: &mut impl Read) -> String {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 8192);
        }
        String::from_utf8(bytes).unwrap()
    }
    #[test]
    fn real_tls_peer_negotiates_track_and_closes_without_api_key() {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let pin = format!("{:x}", sha2::Sha256::digest(certified.cert.der()));
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certified.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der())
                    .into(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream = StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(tls)).unwrap(),
                socket,
            );
            let mut seen = Vec::new();
            for cseq in 1..=3 {
                let received = request(&mut stream);
                seen.push(received.clone());
                let response = match cseq {
                    1 => {
                        assert!(received.starts_with("DESCRIBE rtsps://localhost:"));
                        let sdp="v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:track0\r\n";
                        format!("RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Base: rtsps://localhost:{port}/private/\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len())
                    }
                    2 => {
                        assert!(received.starts_with(&format!(
                            "SETUP rtsps://localhost:{port}/private/track0 "
                        )));
                        "RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: fixture-session;timeout=30\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n".into()
                    }
                    _ => {
                        assert!(received.starts_with("PLAY "));
                        assert!(received.contains("Session: fixture-session"));
                        "RTSP/1.0 200 OK\r\nCSeq: 3\r\n\r\n".into()
                    }
                };
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            stream
                .write_all(b"$\x00\x00\x0e\x80\x60\x00\x01\x00\x00\x00\x01\x00\x00\x00\x07\x65\x99")
                .unwrap();
            stream.flush().unwrap();
            seen.push(request(&mut stream));
            assert!(seen.last().unwrap().starts_with("TEARDOWN "));
            assert!(seen
                .iter()
                .all(|r| !r.to_lowercase().contains("x-api-key")
                    && !r.contains("fixture-api-secret")));
        });
        let settings:Settings=serde_json::from_value(serde_json::json!({"origin":"https://localhost","api_key":"fixture-api-secret","media_certificate_sha256":pin,"media_origin":format!("rtsps://localhost:{port}/")})).unwrap();
        let view = LiveView {
            camera_id: "fixture".into(),
            quality: crate::Quality::Low,
            url: Zeroizing::new(format!("rtsps://localhost:{port}/private")),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let mut session = Session::connect(&view, &settings).unwrap();
        assert_eq!(session.next_h264().unwrap(), [0, 0, 0, 1, 0x65, 0x99]);
        drop(session);
        worker.join().unwrap();
    }
}
