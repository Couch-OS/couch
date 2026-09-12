//! Read-only local UniFi Protect Integration API client.
//!
//! Discovers existing RTSPS streams; it never creates/deletes globally shared
//! camera streams. Descriptors are not video players. See docs/unifi-protect.md.
#[cfg(feature = "media")]
pub mod media;
#[cfg(feature = "media")]
mod media_io;
mod pinning;
#[cfg(feature = "media")]
pub mod player;
pub mod settings;
use serde::Deserialize;
use std::{
    fmt,
    time::{Duration, Instant},
};
use url::Url;
use zeroize::Zeroizing;

const JSON_LIMIT: u64 = 2 * 1024 * 1024;
const SNAPSHOT_LIMIT: u64 = 8 * 1024 * 1024;
const API: &str = "/proxy/protect/integration/v1";
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Transport,
    Authentication,
    Permission,
    NotFound,
    Offline,
    RateLimited,
    Status(u16),
    Response,
    StreamNotEnabled,
    Expired,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UniFi Protect: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Secret is deliberately neither serializable nor printable.
pub struct ApiKey(Zeroizing<String>);
impl ApiKey {
    pub fn new(value: String) -> Result<Self> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Configuration);
        }
        Ok(Self(value))
    }
}
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub model: String,
    /// Kept as a string to tolerate new Protect device states.
    pub state: String,
    pub has_package_camera: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    Package,
}
impl Quality {
    fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Package => "package",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum SnapshotChannel {
    Main,
    Package,
}

/// Holds a bearer-like stream URL. Debug never exposes the URL/path token.
/// Expiry is a LOCAL handoff deadline, not server-side revocation or a lease.
/// A player must stop its own sockets when the view closes/expires.
pub struct LiveView {
    camera_id: String,
    quality: Quality,
    url: Zeroizing<String>,
    deadline: Instant,
}
impl fmt::Debug for LiveView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveView")
            .field("quality", &self.quality)
            .field("url", &"[redacted]")
            .finish()
    }
}
impl LiveView {
    pub fn camera_id(&self) -> &str {
        &self.camera_id
    }
    pub fn quality(&self) -> Quality {
        self.quality
    }
    pub fn expires_at(&self) -> Instant {
        self.deadline
    }
    pub fn url(&self) -> Result<&str> {
        if Instant::now() >= self.deadline {
            Err(Error::Expired)
        } else {
            Ok(&self.url)
        }
    }
    /// Clear this local descriptor. Does not disable shared RTSPS configuration.
    pub fn close(self) {}
}

pub struct Client {
    origin: Url,
    key: ApiKey,
    agent: ureq::Agent,
    stream_hosts: Vec<String>,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProtectClient([redacted])")
    }
}
impl Client {
    /// Local console HTTPS origin, e.g. https://protect.example.test (no path).
    /// If supplied, private_ca_pem replaces public roots for THIS client only;
    /// certificate chain AND hostname verification remain enabled. No bypass.
    pub fn new(
        origin: &str,
        key: ApiKey,
        private_ca_pem: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Self> {
        let origin = Url::parse(origin).map_err(|_| Error::Configuration)?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || origin.username() != ""
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return Err(Error::Configuration);
        }
        let mut tls = ureq::tls::TlsConfig::builder();
        if let Some(pem) = private_ca_pem {
            if pem.len() > 65536 {
                return Err(Error::Configuration);
            }
            let cert = ureq::tls::Certificate::from_pem(pem).map_err(|_| Error::Configuration)?;
            tls = tls.root_certs(vec![cert].into());
        }
        let agent = ureq::Agent::config_builder()
            .tls_config(tls.build())
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Ok(Self {
            stream_hosts: vec![origin.host_str().unwrap().to_owned()],
            origin,
            key,
            agent,
        })
    }
    /// Explicit alternative trust: bind this HTTPS origin to the SHA-256 of the
    /// exact leaf DER certificate approved by the operator. Replaces CA/name/time
    /// certificate validation, but retains TLS handshake-signature verification.
    /// Never populate this pin automatically from an unauthenticated observation.
    pub fn new_pinned(
        origin: &str,
        key: ApiKey,
        certificate_sha256: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let mut client = Self::new(origin, key, None, timeout)?;
        client.agent = pinning::agent(
            client.agent.config().clone(),
            &client.origin,
            certificate_sha256,
        )?;
        Ok(client)
    }
    /// Explicitly authorize an additional stream host (for consoles whose API
    /// hostname differs from returned LAN stream IP). Does not change API routing,
    /// send keys there, rewrite URLs or bypass the player's TLS verification.
    pub fn with_stream_host(mut self, host: &str) -> Result<Self> {
        let parsed = Url::parse(&format!("rtsps://{host}")).map_err(|_| Error::Configuration)?;
        if parsed.host_str().is_none()
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || self.stream_hosts.len() >= 8
        {
            return Err(Error::Configuration);
        }
        self.stream_hosts
            .push(parsed.host_str().unwrap().to_owned());
        Ok(self)
    }
    fn get(&self, path: &str, accept: &str, limit: u64) -> Result<Vec<u8>> {
        let target = self
            .origin
            .join(&format!("{API}/{path}"))
            .map_err(|_| Error::Configuration)?;
        let mut response = self
            .agent
            .get(target.as_str())
            .header("X-API-Key", self.key.0.as_str())
            .header("Accept", accept)
            .call()
            .map_err(|_| Error::Transport)?;
        match response.status().as_u16() {
            200 => {}
            401 => return Err(Error::Authentication),
            403 => return Err(Error::Permission),
            404 => return Err(Error::NotFound),
            429 => return Err(Error::RateLimited),
            503 => return Err(Error::Offline),
            code => return Err(Error::Status(code)),
        }
        let content = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or("");
        if !content.eq_ignore_ascii_case(accept) {
            return Err(Error::Response);
        }
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|_| Error::Response)
    }
    pub fn application_version(&self) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Info {
            application_version: String,
        }
        let info: Info =
            serde_json::from_slice(&self.get("meta/info", "application/json", JSON_LIMIT)?)
                .map_err(|_| Error::Response)?;
        if info.application_version.is_empty() || info.application_version.len() > 128 {
            return Err(Error::Response);
        }
        Ok(info.application_version)
    }
    pub fn cameras(&self) -> Result<Vec<Camera>> {
        let cameras: Vec<Camera> =
            serde_json::from_slice(&self.get("cameras", "application/json", JSON_LIMIT)?)
                .map_err(|_| Error::Response)?;
        if cameras.len() > 512 {
            return Err(Error::Response);
        }
        for camera in &cameras {
            camera_path(&camera.id).map_err(|_| Error::Response)?;
            if camera.name.as_ref().is_some_and(|n| n.len() > 1024)
                || camera.model.len() > 256
                || camera.state.len() > 128
            {
                return Err(Error::Response);
            }
        }
        Ok(cameras)
    }
    /// Uses an existing shared stream. No implicit quality fallback or mutations.
    /// Prefer Low initially on HA100; actual codec/decode support is not negotiated here.
    pub fn live_view(
        &self,
        camera_id: &str,
        quality: Quality,
        lifetime: Duration,
    ) -> Result<LiveView> {
        if lifetime.is_zero() || lifetime > Duration::from_secs(300) {
            return Err(Error::Configuration);
        }
        let path = format!("{}/rtsps-stream", camera_path(camera_id)?);
        let streams: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&self.get(&path, "application/json", JSON_LIMIT)?)
                .map_err(|_| Error::Response)?;
        let raw = match streams.get(quality.key()) {
            None | Some(serde_json::Value::Null) => return Err(Error::StreamNotEnabled),
            Some(serde_json::Value::String(value)) => value,
            _ => return Err(Error::Response),
        };
        validate_stream(raw, &self.stream_hosts)?;
        Ok(LiveView {
            camera_id: camera_id.into(),
            quality,
            url: Zeroizing::new(raw.clone()),
            deadline: Instant::now() + lifetime,
        })
    }
    /// Returns bounded JPEG bytes; dimensions must still be bounded by the decoder.
    pub fn snapshot(&self, camera_id: &str, channel: SnapshotChannel) -> Result<Vec<u8>> {
        let channel = match channel {
            SnapshotChannel::Main => "main",
            SnapshotChannel::Package => "package",
        };
        let path = format!(
            "{}/snapshot?channel={channel}&highQuality=false",
            camera_path(camera_id)?
        );
        let bytes = self.get(&path, "image/jpeg", SNAPSHOT_LIMIT)?;
        if !bytes.starts_with(&[0xff, 0xd8, 0xff]) || !bytes.ends_with(&[0xff, 0xd9]) {
            return Err(Error::Response);
        }
        Ok(bytes)
    }
}
fn camera_path(id: &str) -> Result<String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Configuration);
    }
    Ok(format!("cameras/{id}"))
}
fn validate_stream(raw: &str, hosts: &[String]) -> Result<()> {
    if raw.len() > 4096
        || raw
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(Error::Response);
    }
    let url = Url::parse(raw).map_err(|_| Error::Response)?;
    // Fail closed on an unexpected host; do not hand untrusted response URLs to a
    // player or silently rewrite hosts (which changes TLS identity/routing).
    if url.scheme() != "rtsps"
        || !url
            .host_str()
            .is_some_and(|host| hosts.iter().any(|allowed| allowed == host))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() == "/"
        || url.path().is_empty()
        || url.port() == Some(0)
    {
        return Err(Error::Response);
    }
    Ok(())
}
#[cfg(test)]
mod tests;
