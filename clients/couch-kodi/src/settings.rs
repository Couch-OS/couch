//! Private per-connection web credentials. Never included in house exports.
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub host: String,
    pub web_port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub http_control: bool,
}
impl Settings {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    pub fn client(&self) -> crate::Kodi {
        crate::Kodi::http(&self.host, self.web_port).with_auth(&self.username, &self.password)
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        couch_sdk::save_private(path, self)
    }
}
