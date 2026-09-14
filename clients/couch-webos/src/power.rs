//! Per-pairing power transport. Missing settings select IR, never implicit WoL.
use couch_ir::{
    codeset::Codeset,
    proto::Message,
    tx::{self, Irtx},
};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    #[default]
    Ir,
    Network,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerSettings {
    #[serde(default)]
    pub method: Method,
    /// Existing couch-ir codeset syntax; no codes are guessed or pre-populated.
    #[serde(default)]
    pub codeset: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    url: String,
    settings: PowerSettings,
}
impl PowerSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.codeset.len() > 4096 {
            return Err("Power codeset is too large".into());
        }
        let codes = Codeset::parse("TV power", &self.codeset).map_err(|e| e.to_string())?;
        let mut names = std::collections::HashSet::new();
        for name in codes.buttons() {
            let name = name.to_ascii_lowercase();
            if !["power", "power-on", "power-off"].contains(&name.as_str()) || !names.insert(name) {
                return Err("Use unique power, power-on and power-off entries".into());
            }
        }
        Ok(())
    }
    pub fn load(credentials: &Path, url: &str) -> Result<Self, String> {
        let mut file = match fs::File::open(credentials.with_file_name("webos-power.json")) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("Cannot read TV power settings".into()),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read TV power settings")?;
        if bytes.len() > 8192 {
            return Err("TV power settings are too large".into());
        }
        let saved: Saved =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid TV power settings")?;
        if saved.url != url {
            return Ok(Self::default());
        }
        saved.settings.validate()?;
        Ok(saved.settings)
    }
    pub fn save(&self, credentials: &Path, url: &str) -> Result<(), String> {
        self.validate()?;
        let saved = Saved {
            url: url.into(),
            settings: self.clone(),
        };
        couch_sdk::save_private(&credentials.with_file_name("webos-power.json"), &saved)
            .map_err(|_| "Cannot save TV power settings".into())
    }
    pub fn message(&self, button: &str) -> Result<Message, String> {
        self.validate()?;
        if self.method != Method::Ir {
            return Err("IR power is not selected".into());
        }
        let codes = Codeset::parse("TV power", &self.codeset).map_err(|e| e.to_string())?;
        let entry = codes.get(button).ok_or_else(|| {
            format!("Configure the verified {button} IR code in this TV's Connections settings")
        })?;
        entry.encode(false).map_err(|e| e.to_string())
    }
    pub fn transmit(&self, button: &str) -> Result<(), String> {
        let message = self.message(button)?;
        let mut blaster = Irtx::open("/dev/irtx").map_err(|_| "The remote's IR blaster is unavailable; enable its driver or explicitly select network power in Connections")?;
        // Exactly one frame; no toggle fallback, retries or network fallback.
        tx::transmit(&mut blaster, &message, 0)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_ir_never_guesses_codes_or_substitutes_toggle_for_discrete_power() {
        assert_eq!(PowerSettings::default().method, Method::Ir);
        assert!(PowerSettings::default().message("power").is_err());
        let p = PowerSettings {
            method: Method::Ir,
            codeset: "power nec 0x04 0x08".into(),
        };
        assert!(p.message("power").is_ok());
        assert!(p.message("power-on").is_err());
        assert!(p.message("power-off").is_err());
        for text in [
            "power nec 4 8\npower nec 4 9",
            "volume nec 4 2",
            "power mystery 1 2",
        ] {
            assert!(PowerSettings {
                method: Method::Ir,
                codeset: text.into()
            }
            .validate()
            .is_err());
        }
    }
    #[test]
    fn power_preferences_are_bound_to_pairing_endpoint_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "couch-power-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("webos-connection.json");
        assert_eq!(
            PowerSettings::load(&path, "ws://one").unwrap().method,
            Method::Ir
        );
        PowerSettings {
            method: Method::Network,
            codeset: String::new(),
        }
        .save(&path, "ws://one")
        .unwrap();
        assert_eq!(
            PowerSettings::load(&path, "ws://one").unwrap().method,
            Method::Network
        );
        assert_eq!(
            PowerSettings::load(&path, "ws://two").unwrap().method,
            Method::Ir
        );
        assert_eq!(
            fs::metadata(root.join("webos-power.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::write(root.join("webos-power.json"), b"bad").unwrap();
        assert!(PowerSettings::load(&path, "ws://one").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
