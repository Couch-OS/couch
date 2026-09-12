//! Per-connection enrollment. API keys never belong in the public Couch config.
use crate::{ApiKey, Client, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub origin: String,
    api_key: String,
    #[serde(default)]
    pub certificate_sha256: Option<String>,
    #[serde(default)]
    pub private_ca_pem: Option<String>,
    #[serde(default)]
    pub stream_host: Option<String>,
    #[serde(default)]
    pub media_certificate_sha256: Option<String>,
    #[serde(default)]
    pub media_origin: Option<String>,
}
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectSettings([redacted])")
    }
}
impl Drop for Settings {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}
impl Settings {
    pub fn client(&self) -> Result<Client> {
        if self
            .private_ca_pem
            .as_ref()
            .is_some_and(|p| p.len() > 65536)
            || (self.private_ca_pem.is_some() && self.certificate_sha256.is_some())
        {
            return Err(Error::Configuration);
        }
        if self.media_certificate_sha256.is_some() != self.media_origin.is_some() {
            return Err(Error::Configuration);
        }
        if let Some(pin) = &self.media_certificate_sha256 {
            crate::pinning::tls_config(pin)?;
            let origin = url::Url::parse(self.media_origin.as_deref().ok_or(Error::Configuration)?)
                .map_err(|_| Error::Configuration)?;
            if origin.scheme() != "rtsps"
                || origin.host_str().is_none()
                || !matches!(origin.path(), "" | "/")
                || origin.query().is_some()
                || origin.fragment().is_some()
                || origin.username() != ""
                || origin.password().is_some()
            {
                return Err(Error::Configuration);
            }
        }
        let key = ApiKey::new(self.api_key.clone())?;
        let mut client = match &self.certificate_sha256 {
            Some(pin) => Client::new_pinned(&self.origin, key, pin, Duration::from_secs(5))?,
            None => Client::new(
                &self.origin,
                key,
                self.private_ca_pem.as_deref().map(str::as_bytes),
                Duration::from_secs(5),
            )?,
        };
        if let Some(host) = &self.stream_host {
            client = client.with_stream_host(host)?;
        }
        Ok(client)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|_| Error::Configuration)?;
        if !metadata.file_type().is_file() || metadata.len() > 98304 {
            return Err(Error::Configuration);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Configuration);
            }
        }
        let mut data = Zeroizing::new(Vec::new());
        fs::File::open(path)
            .map_err(|_| Error::Configuration)?
            .take(98305)
            .read_to_end(&mut data)
            .map_err(|_| Error::Configuration)?;
        if data.len() > 98304 {
            return Err(Error::Configuration);
        }
        let value: Self = serde_json::from_slice(&data).map_err(|_| Error::Configuration)?;
        value.client()?;
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.client()?; // Validate trust and credentials before replacing enrollment.
        let data = Zeroizing::new(serde_json::to_vec(self).map_err(|_| Error::Configuration)?);
        if data.len() > 98304 {
            return Err(Error::Configuration);
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = path.with_extension(format!("new-{}-{next}", std::process::id()));
        persist(&data, path, &temporary)
    }
}

fn persist(data: &[u8], path: &Path, temporary: &Path) -> Result<()> {
    let mut created = false;
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary)?;
        created = true;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|_| Error::Configuration)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Settings {
        serde_json::from_value(
            serde_json::json!({"origin":"https://console.test", "api_key":"fixture-private-key"}),
        )
        .unwrap()
    }
    #[test]
    fn trust_is_validated_without_network_and_debug_redacts_keys() {
        let mut s = fixture();
        assert!(s.client().is_ok());
        assert!(!format!("{s:?}").contains("fixture-private-key"));
        s.certificate_sha256 = Some("00".repeat(32));
        assert!(s.client().is_ok());
        s.private_ca_pem = Some("invalid".into());
        assert!(s.client().is_err());
    }
    #[test]
    #[cfg(unix)]
    fn private_atomic_roundtrip_rejects_public_mode_and_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("protect-settings-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("connection.json");
        fixture().save(&file).unwrap();
        assert!(Settings::load(&file).is_ok());
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let collision = dir.join("preexisting");
        fs::write(&collision, b"preserve").unwrap();
        assert!(persist(b"new", &file, &collision).is_err());
        assert_eq!(fs::read(collision).unwrap(), b"preserve");
        assert!(Settings::load(&file).is_ok());
        symlink(&file, dir.join("link")).unwrap();
        assert!(Settings::load(&dir.join("link")).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Settings::load(&file).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
