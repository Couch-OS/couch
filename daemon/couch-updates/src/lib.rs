//! Signed, explicitly selected Couch updates: runtime slots, and boot payloads
//! that rewrite the boot partition's kernel and ramdisk after a backup.
mod baseline;
pub mod boot;
mod release;
mod staging;
pub use release::{Channel, Manifest, SignedManifest};
use serde::{Deserialize, Serialize};
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
    /// What `available` (or the staged update) is: `runtime` for application
    /// slots, `boot` for a kernel and boot ramdisk written to the boot partition.
    #[serde(default)]
    pub kind: String,
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
    boot_device: PathBuf,
    state: Arc<Mutex<State>>,
}
impl Updater {
    pub fn new(root: PathBuf) -> Self {
        Self::with_boot_device(root, PathBuf::from(boot::DEVICE))
    }
    pub fn with_boot_device(root: PathBuf, boot_device: PathBuf) -> Self {
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
            boot_device,
            state: Arc::new(Mutex::new(State {
                status: Status {
                    installed,
                    channel: match config["channel"].as_str() {
                        Some("alpha") => Channel::Alpha,
                        Some("dev") => Channel::Dev,
                        _ => Channel::Stable,
                    },
                    available: None,
                    kind: if root.join("boot/staged").exists() {
                        "boot".into()
                    } else if root.join("runtime/staged").exists() {
                        "runtime".into()
                    } else {
                        String::new()
                    },
                    notes: String::new(),
                    phase: if root.join("runtime/staged").exists()
                        || root.join("boot/staged").exists()
                    {
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
        state.status.kind.clear();
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
            let result = release::discover(channel, &installed, &this.root.join("update-key.pub"))
                .and_then(|offers| this.select(offers));
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            state.status.checked_at = Some(now);
            state.status.phase = "idle".into();
            state.status.can_install = false;
            state.status.kind.clear();
            match result {
                Ok(Some(offer)) => {
                    state.status.available = Some(offer.version.clone());
                    state.status.kind = offer.kind.clone();
                    state.status.notes = offer.notes.clone();
                    state.status.can_install =
                        matches!(offer.kind.as_str(), "runtime" | "boot") && offer.installable;
                    state.status.message = if !state.status.can_install {
                        "This build cannot be installed by this updater.".into()
                    } else if offer.kind == "boot" {
                        "A boot image (kernel and boot ramdisk) is available for the installed build. It is written to the boot partition; the previous image is kept.".into()
                    } else {
                        "An update is available.".into()
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
    /// A newer runtime wins; otherwise the installed release's boot payload is
    /// offered when the boot partition does not already carry it.
    fn select(&self, offers: release::Offers) -> Result<Option<Manifest>> {
        if offers.runtime.is_some() {
            return Ok(offers.runtime);
        }
        let Some(offer) = offers.boot else {
            return Ok(None);
        };
        if boot::installed(&self.boot_device, &offer)? {
            return Ok(None);
        }
        Ok(Some(offer))
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
            let progress = |phase: &str| {
                this.state.lock().unwrap().status.phase = phase.into();
            };
            let result = if offer.kind == "boot" {
                boot::stage(&this.root, &offer, progress)
            } else {
                staging::stage(&this.root, &offer, progress)
            };
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            match result {
                Ok(()) => {
                    state.status.phase = "ready".into();
                    state.status.message = if offer.kind == "boot" {
                        "Boot image verified and staged. Restart to write it to the boot partition."
                            .into()
                    } else {
                        "Update verified and staged. Restart to apply it.".into()
                    };
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
/// Apply whatever is staged: a boot payload is written to the boot partition, a
/// runtime slot becomes the next boot's selection. The caller reboots on Ok.
pub fn activate(root: &std::path::Path) -> Result<()> {
    activate_with(root, std::path::Path::new(boot::DEVICE))
}
pub fn activate_with(root: &std::path::Path, boot_device: &std::path::Path) -> Result<()> {
    if root.join("boot/staged").exists() {
        boot::activate(root, boot_device)
    } else {
        staging::activate(root)
    }
}
fn seal(
    version: &str,
    seed: &[u8; 32],
    output: &std::path::Path,
    manifest: Manifest,
    archive: &[u8],
) -> Result<String> {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(seed);
    let signature = release::hex(&key.sign(&serde_json::to_vec(&manifest).unwrap()).to_bytes());
    let (payload, json) = if manifest.kind == "boot" {
        (
            format!("couch-{version}-ha100-boot.tar.gz"),
            format!("couch-{version}-ha100-boot.json"),
        )
    } else {
        (
            format!("couch-{version}-ha100-runtime.tar.gz"),
            format!("couch-{version}-ha100-update.json"),
        )
    };
    std::fs::create_dir(output).map_err(|_| "Could not create release output")?;
    std::fs::write(output.join(payload), archive).map_err(|_| "Could not save update archive")?;
    std::fs::write(
        output.join(json),
        serde_json::to_vec_pretty(&SignedManifest {
            signed: manifest,
            signature,
        })
        .unwrap(),
    )
    .map_err(|_| "Could not save signed manifest")?;
    Ok(release::hex(key.verifying_key().as_bytes()))
}
/// Publisher for boot payloads: `source` holds the owner-neutral `zImage` and
/// `boot.cpio.gz` (as `tools/release/prepare_public_boot.py` exports them, with
/// its `boot.json` alongside for the kernel commit); `runtime` is the clean
/// runtime of the same version, for the OS baseline the payload is bound to.
pub fn bundle_boot(
    source: &std::path::Path,
    runtime: &std::path::Path,
    version: &str,
    seed: &[u8; 32],
    output: &std::path::Path,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    if !version.starts_with('v') || semver::Version::parse(&version[1..]).is_err() {
        return Err("Use a versioned vMAJOR.MINOR.PATCH tag".into());
    }
    if output.exists() {
        return Err("Bundle output must be new".into());
    }
    let required_os_baseline = Some(baseline::installed(runtime)?);
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = source.join(name);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            return Err("Missing regular boot payload input".into());
        }
        std::fs::read(path).map_err(|_| "Could not read boot payload input".into())
    };
    let zimage = read("zImage")?;
    let ramdisk = read("boot.cpio.gz")?;
    boot::check_payload(&zimage, &ramdisk)?;
    let commit = std::fs::read(source.join("boot.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v["source_kernel_commit"]
                .as_str()
                .map(|s| s[..s.len().min(12)].to_owned())
        });
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut archive = tar::Builder::new(encoder);
    let mut files = Vec::new();
    for (name, data) in [("boot.cpio.gz", &ramdisk), ("zImage", &zimage)] {
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        archive
            .append_data(&mut header, name, &data[..])
            .map_err(|_| "Could not archive boot payload input")?;
        files.push(release::File {
            path: name.into(),
            size: data.len() as u64,
            sha256: release::hex(&Sha256::digest(data)),
            mode: 0o644,
        });
    }
    let data = archive
        .into_inner()
        .map_err(|_| "Could not finish boot archive")?
        .finish()
        .map_err(|_| "Could not compress boot archive")?;
    let notes = match commit {
        Some(commit) => format!("Couch boot image {version}: kernel {commit} and boot ramdisk"),
        None => format!("Couch boot image {version}: kernel and boot ramdisk"),
    };
    let manifest = Manifest {
        schema: 1,
        model: "sanytron-ha100".into(),
        version: version.into(),
        kind: "boot".into(),
        installable: true,
        notes,
        url: format!(
            "{}{version}/couch-{version}-ha100-boot.tar.gz",
            release::PREFIX
        ),
        size: data.len() as u64,
        sha256: release::digest(&data),
        files,
        required_os_baseline,
    };
    boot::inventory(&manifest)?;
    seal(version, seed, output, manifest, &data)
}

/// Release publisher tool; runtime application never accepts a caller-supplied key.
pub fn bundle(
    source: &std::path::Path,
    version: &str,
    seed: &[u8; 32],
    output: &std::path::Path,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    if !version.starts_with('v') || semver::Version::parse(&version[1..]).is_err() {
        return Err("Use a versioned vMAJOR.MINOR.PATCH tag".into());
    }
    if output.exists() {
        return Err("Bundle output must be new".into());
    }
    let required_os_baseline = Some(baseline::installed(source)?);
    let mut names = staging::required_names();
    // Further top-level Couch binaries present in the clean tree ride along.
    for entry in std::fs::read_dir(source).map_err(|_| "Could not inspect release input")? {
        let entry = entry.map_err(|_| "Could not inspect runtime entry")?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("Invalid runtime name")?;
        if name.starts_with("couch-")
            && entry.file_type().is_ok_and(|t| t.is_file())
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_owned());
        }
    }
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
    seal(version, seed, output, manifest, &data)
}
