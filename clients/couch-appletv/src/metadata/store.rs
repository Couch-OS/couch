//! Private AirPlay pairing, deliberately separate from Companion credentials.
use super::{Credentials, Error, Result, Settings};
#[cfg(unix)]
use std::io::Write;
use std::{io::Read, path::Path};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredConnection {
    pub settings: Settings,
    pub credentials: Credentials,
}
impl std::fmt::Debug for StoredConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AirPlayConnection([redacted])")
    }
}
impl StoredConnection {
    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = vec![];
        std::fs::File::open(path)
            .map_err(|_| Error::Configuration)?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Configuration)?;
        if bytes.len() > 65536 {
            return Err(Error::Configuration);
        }
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Configuration)?;
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        Settings::new(self.settings.address, self.settings.airplay_port)?;
        let c = &self.credentials.0;
        if c.client_id.is_empty()
            || c.client_id.len() > 128
            || c.device_id.is_empty()
            || c.device_id.len() > 128
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
    #[cfg(unix)]
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        self.validate()?;
        let parent = path.parent().ok_or(Error::Configuration)?;
        std::fs::create_dir_all(parent).map_err(|_| Error::Configuration)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Configuration)?;
        let tmp = path.with_extension(format!("{}.new", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|_| Error::Configuration)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(self).map_err(|_| Error::Configuration)?)
                .map_err(|_| Error::Configuration)?;
            file.sync_all().map_err(|_| Error::Configuration)?;
            std::fs::rename(&tmp, path).map_err(|_| Error::Configuration)?;
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|_| Error::Configuration)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn private_store_is_bounded_atomic_and_separate_from_companion() {
        let dir =
            std::env::temp_dir().join(format!("couch-airplay-store-{}", uuid::Uuid::new_v4()));
        let path = dir.join("appletv-metadata-connection.json");
        let value = StoredConnection {
            settings: Settings::new("192.0.2.1".parse().unwrap(), 7000).unwrap(),
            credentials: Credentials(crate::Credentials {
                client_id: b"fixture-client".to_vec(),
                device_id: b"fixture-device".to_vec(),
                client_secret: [0; 32],
                device_public: [1; 32],
            }),
        };
        value.save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            StoredConnection::load(&path).unwrap().settings.airplay_port,
            7000
        );
        assert!(!format!("{value:?}").contains("fixture-client"));
        assert!(!dir.join("appletv-connection.json").exists());
        std::fs::write(&path, vec![b' '; 65537]).unwrap();
        assert!(StoredConnection::load(&path).is_err());
        std::fs::write(&path, b"{}").unwrap();
        assert!(StoredConnection::load(&path).is_err());
        value.save(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
