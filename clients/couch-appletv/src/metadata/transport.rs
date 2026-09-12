//! Bounded HTTP/RTSP and HAP record transport. Each encrypted stream has its
//! own counter and keys; authentication failure makes the stream unusable.
use super::{Error, Result, Settings};
use crate::crypto;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
pub(super) const LIMIT: usize = 2 * 1024 * 1024;
struct Cipher {
    output: [u8; 32],
    input: [u8; 32],
    sent: u64,
    received: u64,
}
impl Drop for Cipher {
    fn drop(&mut self) {
        self.output.fill(0);
        self.input.fill(0);
    }
}
pub(super) struct Channel {
    stream: TcpStream,
    cipher: Option<Cipher>,
    encrypted: Vec<u8>,
    pub buffer: Vec<u8>,
}
fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}
impl Channel {
    pub fn connect(settings: &Settings, port: u16) -> Result<Self> {
        Settings::new(settings.address, port)?;
        let stream = TcpStream::connect_timeout(
            &SocketAddr::new(settings.address, port),
            Duration::from_secs(5),
        )
        .map_err(|_| Error::Transport)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| Error::Transport)?;
        stream.set_nodelay(true).map_err(|_| Error::Transport)?;
        Ok(Self {
            stream,
            cipher: None,
            encrypted: vec![],
            buffer: vec![],
        })
    }
    pub fn local_ip(&self) -> Result<std::net::IpAddr> {
        self.stream
            .local_addr()
            .map(|a| a.ip())
            .map_err(|_| Error::Transport)
    }
    pub fn enable(&mut self, shared: &[u8], salt: &str, output: &str, input: &str) -> Result<()> {
        if !self.buffer.is_empty() || !self.encrypted.is_empty() || self.cipher.is_some() {
            return Err(Error::Protocol);
        }
        self.cipher = Some(Cipher {
            output: crypto::derive(shared, salt, output)?,
            input: crypto::derive(shared, salt, input)?,
            sent: 0,
            received: 0,
        });
        Ok(())
    }
    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > LIMIT {
            return Err(Error::Protocol);
        }
        let mut wire = Vec::new();
        if let Some(c) = &mut self.cipher {
            for part in bytes.chunks(1024) {
                let size = (part.len() as u16).to_le_bytes();
                let encrypted = crypto::seal(&c.output, &nonce(c.sent), part, &size)?;
                c.sent = c.sent.checked_add(1).ok_or(Error::Protocol)?;
                wire.extend(size);
                wire.extend(encrypted);
            }
        } else {
            wire.extend(bytes);
        }
        self.stream.write_all(&wire).map_err(|_| Error::Transport)
    }
    pub fn read(&mut self, timeout: Duration) -> Result<bool> {
        self.stream
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(|_| Error::Transport)?;
        let mut bytes = [0; 8192];
        let count = match self.stream.read(&mut bytes) {
            Ok(0) => return Err(Error::Transport),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(false)
            }
            Err(_) => return Err(Error::Transport),
        };
        if let Some(c) = &mut self.cipher {
            self.encrypted.extend(&bytes[..count]);
            while self.encrypted.len() >= 2 {
                let size = u16::from_le_bytes(self.encrypted[..2].try_into().unwrap()) as usize;
                if size == 0 || size > 1024 {
                    return Err(Error::Protocol);
                }
                if self.encrypted.len() < size + 18 {
                    break;
                }
                let plain = crypto::open(
                    &c.input,
                    &nonce(c.received),
                    &self.encrypted[2..size + 18],
                    &self.encrypted[..2],
                )?;
                c.received = c.received.checked_add(1).ok_or(Error::Protocol)?;
                self.buffer.extend(plain);
                self.encrypted.drain(..size + 18);
            }
        } else {
            self.buffer.extend(&bytes[..count]);
        }
        if self.buffer.len() > LIMIT {
            return Err(Error::Protocol);
        }
        Ok(true)
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

pub(super) struct HttpMessage {
    pub first: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub(super) fn take_http(buffer: &mut Vec<u8>) -> Result<Option<HttpMessage>> {
    let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buffer.len() > 16384 {
            return Err(Error::Protocol);
        }
        return Ok(None);
    };
    if end > 16384 {
        return Err(Error::Protocol);
    }
    let head = std::str::from_utf8(&buffer[..end]).map_err(|_| Error::Protocol)?;
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or(Error::Protocol)?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        let (key, value) = line.split_once(':').ok_or(Error::Protocol)?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if key.is_empty()
            || value.chars().any(char::is_control)
            || headers.insert(key, value).is_some()
        {
            return Err(Error::Protocol);
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err(Error::Protocol);
    }
    let size = headers
        .get("content-length")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| Error::Protocol)?
        .unwrap_or(0);
    if size > LIMIT - 16388 {
        return Err(Error::Protocol);
    }
    let total = end + 4 + size;
    if buffer.len() < total {
        return Ok(None);
    }
    let body = buffer[end + 4..total].to_vec();
    buffer.drain(..total);
    Ok(Some(HttpMessage {
        first,
        headers,
        body,
    }))
}
pub(super) struct Http {
    pub channel: Channel,
    cseq: u32,
    pub uri: String,
    id: String,
}
impl Http {
    pub fn connect(settings: &Settings) -> Result<Self> {
        let channel = Channel::connect(settings, settings.airplay_port)?;
        let uri = format!("rtsp://{}/{}", channel.local_ip()?, rand::random::<u32>());
        Ok(Self {
            channel,
            cseq: 0,
            uri,
            id: format!("{:X}", rand::random::<u64>()),
        })
    }
    pub fn request(
        &mut self,
        method: &str,
        uri: &str,
        body: &[u8],
        pairing: bool,
    ) -> Result<Vec<u8>> {
        let sequence = self.cseq;
        self.cseq = self.cseq.checked_add(1).ok_or(Error::Protocol)?;
        let (protocol, content) = if pairing {
            ("HTTP/1.1", "application/octet-stream")
        } else {
            ("RTSP/1.0", "application/x-apple-binary-plist")
        };
        let mut request=format!("{method} {uri} {protocol}\r\nCSeq: {sequence}\r\nUser-Agent: AirPlay/550.10\r\nConnection: keep-alive\r\nDACP-ID: {}\r\nClient-Instance: {}\r\nActive-Remote: 1\r\nContent-Type: {content}\r\nContent-Length: {}\r\n",self.id,self.id,body.len());
        if pairing {
            request.push_str("X-Apple-HKP: 3\r\n");
        }
        request.push_str("\r\n");
        self.channel.send(&[request.as_bytes(), body].concat())?;
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(response) = take_http(&mut self.channel.buffer)? {
                let mut parts = response.first.split_whitespace();
                if !matches!(parts.next(), Some("HTTP/1.1" | "RTSP/1.0")) {
                    return Err(Error::Protocol);
                }
                match parts.next() {
                    Some("200") => {}
                    Some("401" | "403" | "470") => return Err(Error::Authentication),
                    _ => return Err(Error::Rejected),
                }
                if (!pairing || response.headers.contains_key("cseq"))
                    && response.headers.get("cseq") != Some(&sequence.to_string())
                {
                    return Err(Error::Protocol);
                }
                return Ok(response.body);
            }
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            self.channel
                .read(remaining.min(Duration::from_millis(200)))?;
        }
    }
}
