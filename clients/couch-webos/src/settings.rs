//! Per-TV pairing credentials, kept outside exportable house configuration.
use crate::{endpoint, Error, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub url: String,
    pub client_key: String,
    #[serde(default)]
    pub certificate: Vec<u8>,
}
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebOsSettings")
            .field("url", &self.url)
            .field("client_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}
impl Settings {
    pub fn address(&self) -> Result<std::net::IpAddr> {
        endpoint(&self.url).map(|(_, host, _)| host)
    }

    pub fn validate(&self) -> Result<()> {
        let (url, _, _) = endpoint(&self.url)?;
        if url.path() != "/"
            || url.query().is_some()
            || self.client_key.is_empty()
            || self.client_key.len() > 4096
        {
            return Err(Error::Configuration);
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
