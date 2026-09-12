//! Signed, explicitly selected Couch runtime updates. No partition writes.
mod baseline;
mod release;
mod staging;
pub use release::{Channel, Manifest, SignedManifest};
use serde::{Deserialize, Serialize};
pub use staging::activate;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Status {
    pub installed: String,
    pub channel: Channel,
    pub available: Option<String>,
    pub notes: String,
    pub phase: String,
    pub message: String,
    pub checked_at: Option<u64>,
    pub can_install: bool,
    pub automatic_checks: bool,
}
struct State {
    status: Status,
    offer: Option<Manifest>,
    busy: bool,
}
#[derive(Clone)]
pub struct Updater {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}
impl Updater {
    pub fn new(root: PathBuf) -> Self {
        let config = std::fs::read(root.join("updates/settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .unwrap_or_default();
        let installed = std::fs::read(root.join("runtime/current/build.json"))
            .or_else(|_| std::fs::read(root.join("build.json")))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["version"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "development".into());
        Self {
            root: root.clone(),
            state: Arc::new(Mutex::new(State {
                status: Status {
                    installed,
                    channel: if config["channel"] == "alpha" {
                        Channel::Alpha
                    } else {
                        Channel::Stable
                    },
                    available: None,
                    notes: String::new(),
                    phase: if root.join("runtime/staged").exists() {
                        "ready".into()
                    } else {
                        "idle".into()
                    },
                    message: String::new(),
                    checked_at: None,
                    can_install: false,
                    automatic_checks: config["automatic_checks"].as_bool().unwrap_or(true),
                },
                offer: None,
                busy: false,
            })),
        }
    }
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status.clone()
    }
    pub fn settings(&self, channel: Channel, automatic_checks: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.busy || state.status.phase == "ready" {
            return Err("An update operation is already running".into());
        }
        staging::atomic(
            &self.root.join("updates/settings.json"),
            &serde_json::to_vec(
                &serde_json::json!({"channel":channel,"automatic_checks":automatic_checks}),
            )
            .unwrap(),
        )?;
        state.status.channel = channel;
        state.status.automatic_checks = automatic_checks;
        state.status.available = None;
        state.status.can_install = false;
        state.offer = None;
        state.status.checked_at = None;
        Ok(())
    }
    pub fn check(&self, automatic: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if automatic
            && (!state.status.automatic_checks
                || state
                    .status
                    .checked_at
                    .is_some_and(|last| now.saturating_sub(last) < 6 * 3600))
        {
            return Ok(());
        }
        if state.busy || state.status.phase == "ready" {
            return Err("An update operation is already running".into());
        }
        state.busy = true;
        state.status.phase = "checking".into();
        state.status.message.clear();
        let channel = state.status.channel;
        let installed = state.status.installed.clone();
        drop(state);
        let this = self.clone();
        std::thread::spawn(move || {
            let result = release::discover(channel, &installed, &this.root.join("update-key.pub"));
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            state.status.checked_at = Some(now);
            state.status.phase = "idle".into();
            state.status.can_install = false;
            match result {
                Ok(Some(offer)) => {
                    state.status.available = Some(offer.version.clone());
                    state.status.notes = offer.notes.clone();
                    state.status.can_install = offer.kind == "runtime" && offer.installable;
                    state.status.message = if state.status.can_install {
                        "An update is available.".into()
                    } else {
                        "This build cannot be installed by this updater.".into()
                    };
                    state.offer = Some(offer);
                }
                Ok(None) => {
                    state.status.available = None;
                    state.offer = None;
                    state.status.message =
                        "No newer signed build is available on this channel.".into();
                }
                Err(error) => {
                    state.status.phase = "error".into();
                    state.status.message = error;
                    state.offer = None;
                    state.status.available = None;
                }
            }
        });
        Ok(())
    }
    pub fn install(&self, version: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.busy || state.status.phase == "ready" {
            return Err("An update operation is already running".into());
        }
        let offer = state
            .offer
            .clone()
            .filter(|o| o.version == version && state.status.can_install)
            .ok_or("Check for and select a signed compatible update first")?;
        state.busy = true;
        state.status.phase = "downloading".into();
        state.status.message = "Downloading and verifying the selected build.".into();
        state.status.can_install = false;
        drop(state);
        let this = self.clone();
        std::thread::spawn(move || {
            let result = staging::stage(&this.root, &offer, |phase| {
                this.state.lock().unwrap().status.phase = phase.into();
            });
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            match result {
                Ok(()) => {
                    state.status.phase = "ready".into();
                    state.status.message =
                        "Update verified and staged. Restart to apply it.".into();
                }
                Err(error) => {
                    state.status.phase = "error".into();
                    state.status.message = error;
                }
            }
        });
        Ok(())
    }
    pub fn ready(&self) -> bool {
        self.state.lock().unwrap().status.phase == "ready"
    }
}

/// Release publisher tool; runtime application never accepts a caller-supplied key.
pub fn bundle(
    source: &std::path::Path,
    version: &str,
    seed: &[u8; 32],
    output: &std::path::Path,
) -> Result<String> {
    use ed25519_dalek::Signer;
    use sha2::{Digest, Sha256};
    if !version.starts_with('v') || semver::Version::parse(&version[1..]).is_err() {
        return Err("Use a versioned vMAJOR.MINOR.PATCH tag".into());
    }
    if output.exists() {
        return Err("Bundle output must be new".into());
    }
    let required_os_baseline = Some(baseline::installed(source)?);
    let mut names = staging::required_names();
    for directory in ["www", "licenses"] {
        let mut pending = vec![source.join(directory)];
        while let Some(path) = pending.pop() {
            if !path.exists() {
                continue;
            }
            let meta =
                std::fs::symlink_metadata(&path).map_err(|_| "Could not inspect release input")?;
            if meta.is_dir() {
                for entry in
                    std::fs::read_dir(&path).map_err(|_| "Could not inspect runtime directory")?
                {
                    pending.push(entry.map_err(|_| "Could not inspect runtime entry")?.path());
                }
            } else if meta.is_file() {
                names.push(
                    path.strip_prefix(source)
                        .map_err(|_| "Invalid runtime input")?
                        .to_str()
                        .ok_or("Invalid runtime name")?
                        .to_owned(),
                );
            } else {
                return Err("Runtime inputs must be regular files".into());
            }
        }
    }
    names.sort();
    names.dedup();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut archive = tar::Builder::new(encoder);
    let mut files = Vec::new();
    for name in names {
        let data = if name == "build.json" {
            serde_json::to_vec(&serde_json::json!({"version":version})).unwrap()
        } else {
            let path = source.join(&name);
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                return Err("Missing regular runtime input".into());
            }
            std::fs::read(path).map_err(|_| "Could not read runtime input")?
        };
        let mode = if name.ends_with(".sh")
            || name.starts_with("couch-")
            || name == "fbcon"
            || name.starts_with("www/cgi-bin/")
        {
            0o755
        } else {
            0o644
        };
        let mut header = tar::Header::new_ustar();
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        archive
            .append_data(&mut header, &name, &data[..])
            .map_err(|_| "Could not archive runtime input")?;
        files.push(release::File {
            path: name,
            size: data.len() as u64,
            sha256: release::hex(&Sha256::digest(&data)),
            mode,
        });
    }
    let data = archive
        .into_inner()
        .map_err(|_| "Could not finish runtime archive")?
        .finish()
        .map_err(|_| "Could not compress runtime archive")?;
    let manifest=Manifest{schema:1,model:"sanytron-ha100".into(),version:version.into(),kind:"runtime".into(),installable:true,notes:format!("Couch apps and services {version}"),url:format!("https://github.com/dangerouslaser/couch/releases/download/{version}/couch-{version}-ha100-runtime.tar.gz"),size:data.len() as u64,sha256:release::digest(&data),files,required_os_baseline};
    staging::validate_inventory(&manifest)?;
    let key = ed25519_dalek::SigningKey::from_bytes(seed);
    let signature = release::hex(&key.sign(&serde_json::to_vec(&manifest).unwrap()).to_bytes());
    std::fs::create_dir(output).map_err(|_| "Could not create release output")?;
    std::fs::write(
        output.join(format!("couch-{version}-ha100-runtime.tar.gz")),
        data,
    )
    .map_err(|_| "Could not save update archive")?;
    std::fs::write(
        output.join(format!("couch-{version}-ha100-update.json")),
        serde_json::to_vec_pretty(&SignedManifest {
            signed: manifest,
            signature,
        })
        .unwrap(),
    )
    .map_err(|_| "Could not save signed manifest")?;
    Ok(release::hex(key.verifying_key().as_bytes()))
}
