//! Signed, explicitly selected Couch updates: runtime slots, and boot payloads
//! that rewrite the boot partition's kernel and ramdisk after a backup.
mod baseline;
pub mod boot;
mod release;
mod staging;
mod transaction;
pub use release::{Channel, Manifest, SignedManifest};
use serde::{Deserialize, Serialize};
pub use staging::collect;
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
    /// `runtime`, `boot` (legacy completion), or `combined` for both payloads.
    #[serde(default)]
    pub kind: String,
    pub notes: String,
    pub phase: String,
    pub message: String,
    pub checked_at: Option<u64>,
    pub can_install: bool,
    pub automatic_checks: bool,
    /// The boot image on the partition, from `boot/installed.json`: the version
    /// this updater wrote and the kernel commit its notes named, both empty
    /// when no boot payload has been installed. `boot_previous` says whether
    /// the image it replaced is still saved, so a rollback is possible.
    #[serde(default)]
    pub boot_version: String,
    #[serde(default)]
    pub boot_kernel: String,
    #[serde(default)]
    pub boot_previous: bool,
    /// The release whose kernel and boot ramdisk the partition carries: the
    /// boot payload this updater wrote, or, when it has written none, the
    /// build the full OS image shipped, which is what the partition still
    /// holds. Empty when neither is known.
    #[serde(default)]
    pub boot_release: String,
    /// That release is older than the installed software.
    #[serde(default)]
    pub boot_behind: bool,
    /// A legacy update still needs its boot payload checked or installed.
    #[serde(default)]
    pub boot_pending: bool,
    /// Number of install/restart actions: one for any offer, zero without one.
    #[serde(default)]
    pub steps: u8,
    /// Shared guidance for the remote and web UI.
    #[serde(default)]
    pub guidance: String,
}
/// A legacy updater left this note for a boot payload still to be checked.
const PENDING: &str = "updates/pending-boot.json";

/// A prerelease tag rendered for one line of a small screen: `.165` says
/// enough beside the full build name on the row above, a dev build keeps the
/// identifier that distinguishes it from the alpha it follows, and a finished
/// version keeps its own name.
pub fn short_version(version: &str) -> String {
    let (core, dev) = match version.strip_suffix(".dev") {
        Some(core) => (core, ".dev"),
        None => (version, ""),
    };
    match core.rsplit_once('.') {
        Some((head, build)) if head.contains("alpha") && !build.is_empty() => {
            format!(".{build}{dev}")
        }
        _ => version.to_owned(),
    }
}
/// The build whose kernel and boot ramdisk the partition carries: a boot
/// payload this updater wrote, and otherwise the build the full OS image
/// shipped. That image's `build.json` sits at the root of `/opt/couch`, beside
/// the runtime slots rather than inside one, so runtime updates never replace
/// it and it still names the image the boot partition was written from.
fn boot_release(root: &std::path::Path, boot_version: &str) -> String {
    if !boot_version.is_empty() {
        return boot_version.to_owned();
    }
    std::fs::read(root.join("build.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["version"].as_str().map(str::to_owned))
        .unwrap_or_default()
}
/// Is the boot partition's release older than the installed software? Both
/// tags sort in one order whatever channel they came from; anything that does
/// not parse (a development tree with no `build.json`) is not behind.
fn behind(boot_release: &str, installed: &str) -> bool {
    match (release::version(boot_release), release::version(installed)) {
        (Some(boot), Some(software)) => boot < software,
        _ => false,
    }
}
/// Did step 1 of a two-step update and not step 2. Step 1 writes the note when
/// it stages a runtime whose release also publishes a boot payload, so the
/// remote can say the update is unfinished with no network at all. A stale
/// note - a different build installed since, or the kernel caught up - is
/// removed rather than believed.
fn expects_boot(root: &std::path::Path, installed: &str, boot_release: &str) -> bool {
    let path = root.join(PENDING);
    let Some(noted) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["version"].as_str().map(str::to_owned))
    else {
        return false;
    };
    if noted == installed && boot_release != installed {
        return true;
    }
    let _ = std::fs::remove_file(&path);
    false
}
/// Recompute everything the two UIs say from what the updater knows: how many
/// steps the offer on the table belongs to, where the boot partition stands
/// against the installed software, and the sentence that names the journey.
fn explain(status: &mut Status, pair: bool, expects_boot: bool) {
    status.steps = u8::from(status.available.is_some());
    if pair && status.kind == "runtime" {
        status.kind = "combined".into();
    }
    status.boot_behind = behind(&status.boot_release, &status.installed);
    status.boot_pending = expects_boot || (status.kind == "boot" && status.available.is_some());
    status.guidance = guidance(status);
}
/// The one sentence both UIs show above the buttons. Short enough for the
/// remote's message area, plain enough for someone who has never heard of a
/// boot ramdisk, and silent when there is nothing to explain: a release that
/// ships software alone installs in one step and needs no story.
fn guidance(status: &Status) -> String {
    let Some(available) = status.available.as_deref() else {
        return if status.boot_pending {
            "A previous update still needs its boot image checked. Check for updates to finish it."
                .into()
        } else {
            String::new()
        };
    };
    let target = short_version(available);
    match status.kind.as_str() {
        "combined" => format!("Update to {target}: Couch software, kernel and boot image install together with one restart."),
        "boot" => format!("Finish updating to {target}: install the kernel and boot image, then restart."),
        _ => String::new(),
    }
}
struct State {
    status: Status,
    offer: Option<transaction::Plan>,
    /// The selection includes both runtime and boot.
    pair: bool,
    /// A note from step 1 says this build's boot payload is still to install.
    expects_boot: bool,
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
        let (boot_version, boot_kernel, boot_previous) = boot::record(&root);
        let release = boot_release(&root, &boot_version);
        let expects_boot = expects_boot(&root, &installed, &release);
        let saved = transaction::Plan::recover(&root);
        let this = Self {
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
                    boot_version,
                    boot_kernel,
                    boot_previous,
                    boot_release: release,
                    boot_behind: false,
                    boot_pending: false,
                    steps: 0,
                    guidance: String::new(),
                },
                offer: None,
                pair: false,
                expects_boot,
                busy: false,
            })),
        };
        {
            let mut state = this.state.lock().unwrap();
            match saved {
                Ok(Some(plan)) => {
                    state.status.available = Some(plan.primary().version.clone());
                    state.status.kind = plan.kind().into();
                    state.status.notes = plan.primary().notes.clone();
                    let ready = plan.can_resume(&root);
                    state.status.phase = if ready { "ready" } else { "error" }.into();
                    state.status.message = if ready {
                        "Update verified and staged. Install and restart to apply it."
                    } else {
                        "Update was interrupted or rolled back; check and download again."
                    }
                    .into();
                    state.pair = plan.kind() == "combined";
                    state.offer = Some(plan);
                }
                Err(error) => {
                    state.status.phase = "error".into();
                    state.status.message = error;
                }
                Ok(None) => {}
            }
            let (pair, expects) = (state.pair, state.expects_boot);
            explain(&mut state.status, pair, expects);
        }
        this
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
        state.pair = false;
        state.status.checked_at = None;
        let (pair, expects) = (state.pair, state.expects_boot);
        explain(&mut state.status, pair, expects);
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
                    state.pair = offer.kind() == "combined";
                    state.status.available = Some(offer.primary().version.clone());
                    state.status.kind = offer.kind().into();
                    state.status.notes = offer.primary().notes.clone();
                    state.status.can_install = [&offer.runtime, &offer.boot]
                        .into_iter()
                        .flatten()
                        .all(|m| m.installable);
                    state.status.message = if !state.status.can_install {
                        "This build cannot be installed by this updater.".into()
                    } else if offer.kind() == "boot" {
                        "The kernel and boot ramdisk for the installed build are ready to download. They are written to the boot partition; the image they replace is kept.".into()
                    } else {
                        "An update is available.".into()
                    };
                    state.offer = Some(offer);
                }
                Ok(None) => {
                    state.status.available = None;
                    state.offer = None;
                    state.pair = false;
                    state.status.message =
                        "No newer signed build is available on this channel.".into();
                }
                Err(error) => {
                    state.status.phase = "error".into();
                    state.status.message = error;
                    state.offer = None;
                    state.pair = false;
                    state.status.available = None;
                }
            }
            let (version, kernel, previous) = boot::record(&this.root);
            state.status.boot_release = boot_release(&this.root, &version);
            state.status.boot_version = version;
            state.status.boot_kernel = kernel;
            state.status.boot_previous = previous;
            state.expects_boot = expects_boot(
                &this.root,
                &state.status.installed,
                &state.status.boot_release,
            );
            let (pair, expects) = (state.pair, state.expects_boot);
            explain(&mut state.status, pair, expects);
        });
        Ok(())
    }
    /// Keep a release's signed pair together. For legacy half-updates, compare
    /// actual partition bytes and reconcile the version even when no write is needed.
    fn select(&self, offers: release::Offers) -> Result<Option<transaction::Plan>> {
        if offers.runtime.is_some() {
            return transaction::Plan::new(offers.runtime, offers.boot).map(Some);
        }
        let Some(offer) = offers.boot else {
            return Ok(None);
        };
        if boot::installed(&self.boot_device, &offer)? {
            boot::record_installed(&self.root, &offer)?;
            return Ok(None);
        }
        transaction::Plan::new(None, Some(offer)).map(Some)
    }
    pub fn install(&self, version: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.busy || state.status.phase == "ready" {
            return Err("An update operation is already running".into());
        }
        let mut offer = state
            .offer
            .clone()
            .filter(|o| o.primary().version == version && state.status.can_install)
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
            let result = offer.stage(&this.root, progress);
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            match result {
                Ok(()) => {
                    state.status.phase = "ready".into();
                    state.status.message = if offer.kind() == "combined" {
                        "Software and boot image verified. Install both with one restart."
                    } else {
                        "Update verified and staged. Restart to apply it."
                    }
                    .into();
                }
                Err(error) => {
                    state.status.phase = "error".into();
                    state.status.message = error;
                }
            }
        });
        Ok(())
    }
    /// Put the saved previous boot image back on the partition, from the
    /// Updates page: the counterpart of an install that the device otherwise
    /// only has through a recovery serial shell. The caller restarts.
    pub fn boot_rollback(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.busy || state.status.phase == "ready" {
            return Err("An update operation is already running".into());
        }
        state.busy = true;
        drop(state);
        let result = boot::restore(&self.root, &self.boot_device);
        let mut state = self.state.lock().unwrap();
        state.busy = false;
        let (version, kernel, previous) = boot::record(&self.root);
        state.status.boot_version = version.clone();
        state.status.boot_kernel = kernel;
        state.status.boot_previous = previous;
        state.status.boot_release = boot_release(&self.root, &version);
        let (pair, expects) = (state.pair, state.expects_boot);
        explain(&mut state.status, pair, expects);
        if result.is_ok() {
            state.status.message =
                "The previous boot image is back on the boot partition. Restart to run it.".into();
        }
        result
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
    if let Some(plan) = transaction::Plan::load(root)? {
        plan.activate(root, boot_device)
    } else if root.join("boot/staged").exists() && root.join("runtime/staged").exists() {
        Err("Ambiguous legacy staged updates; download the release again".into())
    } else if root.join("boot/staged").exists() {
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
    let manifest = Manifest {
        schema: 1,
        model: "sanytron-ha100".into(),
        version: version.into(),
        kind: "runtime".into(),
        installable: true,
        notes: format!("Couch apps and services {version}"),
        url: format!(
            "{}{version}/couch-{version}-ha100-runtime.tar.gz",
            release::PREFIX
        ),
        size: data.len() as u64,
        sha256: release::digest(&data),
        files,
        required_os_baseline,
    };
    staging::validate_inventory(&manifest)?;
    // Installable by this updater is not enough: it has to be installable by
    // the oldest one in the field, or the release strands every remote on it.
    staging::check_floor(&manifest)?;
    seal(version, seed, output, manifest, &data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "couch-updates-journey-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn status(installed: &str, boot: &str) -> Status {
        Status {
            installed: installed.into(),
            channel: Channel::Alpha,
            available: None,
            kind: String::new(),
            notes: String::new(),
            phase: "idle".into(),
            message: String::new(),
            checked_at: None,
            can_install: false,
            automatic_checks: true,
            boot_version: String::new(),
            boot_kernel: String::new(),
            boot_previous: false,
            boot_release: boot.into(),
            boot_behind: false,
            boot_pending: false,
            steps: 0,
            guidance: String::new(),
        }
    }

    #[test]
    fn a_prerelease_tag_shortens_to_its_build_number_and_keeps_a_dev_identifier() {
        assert_eq!(short_version("v0.1.0-alpha.20260913.165"), ".165");
        assert_eq!(short_version("v0.1.0-alpha.20260913.165.dev"), ".165.dev");
        assert_eq!(short_version("v0.2.0"), "v0.2.0");
        assert_eq!(short_version(""), "");
    }

    #[test]
    fn the_kernel_release_falls_back_to_the_build_the_os_image_shipped() {
        let root = root();
        // Nothing written by this updater and no image identity: unknown.
        assert_eq!(boot_release(&root, ""), "");
        std::fs::write(
            root.join("build.json"),
            br#"{"version":"v0.1.0-alpha.20260913.124"}"#,
        )
        .unwrap();
        // The partition still carries the kernel that image was built with.
        assert_eq!(boot_release(&root, ""), "v0.1.0-alpha.20260913.124");
        // Once a boot payload has been written, that is the answer instead.
        assert_eq!(
            boot_release(&root, "v0.1.0-alpha.20260913.165"),
            "v0.1.0-alpha.20260913.165"
        );
    }

    #[test]
    fn a_kernel_older_than_the_software_is_detected_across_channels() {
        let old = "v0.1.0-alpha.20260913.124";
        let new = "v0.1.0-alpha.20260913.165";
        assert!(behind(old, new));
        assert!(!behind(new, old));
        assert!(!behind(new, new));
        // A dev build sorts above the alpha whose core it keeps.
        assert!(behind(new, "v0.1.0-alpha.20260913.165.dev"));
        // A development tree names no build: nothing to compare, nothing said.
        assert!(!behind("", new));
        assert!(!behind(old, "development"));
    }

    #[test]
    fn step_one_leaves_a_note_that_step_two_reads_and_a_finished_update_clears() {
        let root = root();
        let installed = "v0.1.0-alpha.20260913.165";
        assert!(!expects_boot(&root, installed, "v0.1.0-alpha.20260913.124"));
        staging::atomic(
            &root.join(PENDING),
            br#"{"version":"v0.1.0-alpha.20260913.165"}"#,
        )
        .unwrap();
        // The runtime of that release is installed and the kernel is not.
        assert!(expects_boot(&root, installed, "v0.1.0-alpha.20260913.124"));
        assert!(root.join(PENDING).is_file());
        // Step 2 done: the note is stale and is removed, not believed.
        assert!(!expects_boot(&root, installed, installed));
        assert!(!root.join(PENDING).is_file());
        // A note naming some other build is stale too.
        staging::atomic(&root.join(PENDING), br#"{"version":"v0.9.9"}"#).unwrap();
        assert!(!expects_boot(&root, installed, "v0.1.0-alpha.20260913.124"));
        assert!(!root.join(PENDING).is_file());
    }

    #[test]
    fn a_two_payload_release_installs_with_one_restart() {
        let mut s = status("v1.2.2", "v1.2.2");
        s.available = Some("v1.2.3".into());
        s.kind = "combined".into();
        explain(&mut s, true, false);
        assert_eq!(s.steps, 1);
        assert!(!s.boot_pending);
        assert!(s.guidance.contains("one restart"));
        s.kind = "runtime".into();
        explain(&mut s, false, false);
        assert_eq!(s.steps, 1);
        assert!(s.guidance.is_empty());
    }

    #[test]
    fn a_legacy_boot_offer_can_finish_an_old_update() {
        let mut s = status("v1.2.3", "v1.2.2");
        s.available = Some("v1.2.3".into());
        s.kind = "boot".into();
        explain(&mut s, false, true);
        assert_eq!(s.steps, 1);
        assert!(s.boot_pending);
        assert!(s.guidance.contains("Finish updating"));
    }

    #[test]
    fn a_remote_left_half_updated_can_request_a_check() {
        let mut s = status("v1.2.3", "v1.2.2");
        explain(&mut s, false, true);
        assert_eq!(s.steps, 0);
        assert!(s.boot_pending);
        assert!(s.guidance.contains("Check for updates"));
    }

    #[test]
    fn identical_boot_bytes_reconcile_a_new_release_without_flashing() {
        let root = root();
        let device = root.join("boot-device");
        let z = boot::tests::zimage(1, 100);
        let r = boot::tests::ramdisk(2, 100);
        let m = boot::tests::manifest(&z, &r);
        let image = boot::tests::image(&z, &r);
        std::fs::write(&device, &image).unwrap();
        std::fs::write(root.join("build.json"), br#"{"version":"v1.2.3"}"#).unwrap();
        staging::atomic(&root.join(PENDING), br#"{"version":"v1.2.3"}"#).unwrap();
        let updater = Updater::with_boot_device(root.clone(), device.clone());
        assert!(updater
            .select(release::Offers {
                runtime: None,
                boot: Some(m)
            })
            .unwrap()
            .is_none());
        let status = Updater::with_boot_device(root.clone(), device.clone()).status();
        assert_eq!(status.boot_release, "v1.2.3");
        assert!(!status.boot_pending);
        assert!(!root.join(PENDING).exists());
        assert!(!root.join("boot/previous.img").exists());
        assert_eq!(std::fs::read(device).unwrap(), image);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_older_kernel_with_no_second_step_outstanding_raises_no_alarm() {
        // Most releases publish software alone: the kernel stays where the
        // last boot payload left it, which is behind but not unfinished.
        let mut s = status("v0.1.0-alpha.20260913.170", "v0.1.0-alpha.20260913.165");
        explain(&mut s, false, false);
        assert!(s.boot_behind);
        assert!(!s.boot_pending);
        assert_eq!(s.guidance, "");
    }

    #[test]
    fn a_fresh_updater_reports_the_partition_against_the_installed_software() {
        let root = root();
        std::fs::write(
            root.join("build.json"),
            br#"{"version":"v0.1.0-alpha.20260913.124"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("runtime/current")).unwrap();
        std::fs::write(
            root.join("runtime/current/build.json"),
            br#"{"version":"v0.1.0-alpha.20260913.165"}"#,
        )
        .unwrap();
        staging::atomic(
            &root.join(PENDING),
            br#"{"version":"v0.1.0-alpha.20260913.165"}"#,
        )
        .unwrap();
        let status = Updater::new(root.clone()).status();
        assert_eq!(status.installed, "v0.1.0-alpha.20260913.165");
        assert_eq!(status.boot_release, "v0.1.0-alpha.20260913.124");
        assert!(status.boot_behind);
        assert!(status.boot_pending);
        assert!(status.guidance.contains("Check for updates"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
