//! Per-TV pairing material, kept outside exportable house configuration.
use crate::{endpoint, Error, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    /// `wss://IP:8002/` (token authenticated) or `ws://IP:8001/` (legacy, no token).
    pub url: String,
    /// Access token issued after the on-screen "Allow" prompt. Empty on
    /// legacy connections and on TVs that never issued one.
    #[serde(default)]
    pub token: String,
    /// DER certificate pinned while pairing over `wss`.
    #[serde(default)]
    pub certificate: Vec<u8>,
    /// Wi-Fi/Ethernet MAC reported by the TV's REST endpoint at pairing time,
    /// used for Wake-on-LAN. Absent when the TV did not report one.
    #[serde(default)]
    pub mac: Option<String>,
    /// The Frame needs a held power key to leave Art Mode for standby.
    #[serde(default)]
    pub frame_tv: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub name: String,
}
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TizenSettings")
            .field("url", &self.url)
            .field("token", &"[redacted]")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}
impl Settings {
    pub fn address(&self) -> Result<std::net::IpAddr> {
        endpoint(&self.url).map(|(_, host, _)| host)
    }
    pub fn port(&self) -> Result<u16> {
        endpoint(&self.url).map(|(_, _, port)| port)
    }
    pub fn secure(&self) -> bool {
        self.url.starts_with("wss://")
    }
    pub fn validate(&self) -> Result<()> {
        let (url, _, _) = endpoint(&self.url)?;
        if url.path() != "/"
            || url.query().is_some()
            || self.token.len() > 256
            || self.model.len() > 128
            || self.name.len() > 128
            || self.certificate.len() > 16 * 1024
        {
            return Err(Error::Configuration);
        }
        if let Some(mac) = &self.mac {
            crate::magic_packet(mac)?;
        }
        if url.scheme() == "wss" && self.certificate.is_empty() {
            return Err(Error::Certificate);
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|_| Error::Configuration)?;
        if bytes.len() > 128 * 1024 {
            return Err(Error::Configuration);
        }
        let settings: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Configuration)?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        couch_sdk::save_private(path, self).map_err(|_| Error::Configuration)
    }
}
