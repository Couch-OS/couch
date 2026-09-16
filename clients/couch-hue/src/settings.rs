//! Credentials live separately from the exportable house configuration.
use crate::{Error, Hue, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub url: String,
    pub token: String,
    pub certificate: Vec<u8>,
}
impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path).map_err(|_| Error::Configuration)?;
        serde_json::from_slice(&data).map_err(|_| Error::Configuration)
    }
    pub fn client(&self) -> Result<Hue> {
        Hue::new(&self.url, &self.token, &self.certificate)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.client()?;
        couch_sdk::save_private(path, self).map_err(|_| Error::Configuration)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn settings_are_private_and_invalid_replacement_preserves_old_file() {
        let dir = std::env::temp_dir().join(format!("couch-hue-settings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connection.json");
        let settings = Settings {
            url: "https://127.0.0.1".into(),
            token: "test-secret".into(),
            certificate: vec![1, 2, 3],
        };
        settings.save(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let before = fs::read(&path).unwrap();
        assert!(Settings {
            url: "http://invalid".into(),
            token: "test-secret".into(),
            certificate: vec![1, 2, 3]
        }
        .save(&path)
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(Settings::load(&path).unwrap().url, settings.url);
        fs::remove_dir_all(dir).unwrap();
    }
}
