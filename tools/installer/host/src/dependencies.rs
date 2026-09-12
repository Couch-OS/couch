//! Native owner-local dependency preparation. No Python, Git, shell, or USB is used.
use crate::session::{Phase, SessionGuard};
use anyhow::{bail, ensure, Context, Result};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MTK_PINS: &[u8] = include_bytes!("../../../release/mtk_dependencies.json");
const ADB_PINS: &[u8] = include_bytes!("../../../release/host_dependencies.json");
const SOURCE_INVENTORY: &[u8] = include_bytes!("../../mtk_source_inventory.json");
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MEMBERS: usize = 50_000;
const DA_MEMBER: &str = "mtkclient/Loader/MTK_DA_V5.bin";
const DA_SIZE: u64 = 22_483_280;
const DA_SHA256: &str = "aef234190ccb8145d2e3b8459741e9adb70f2caa8481aa216c1b25152afaca1f";

type Progress<'a> = dyn FnMut(&str, u64, u64) -> Result<()> + 'a;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Blob {
    size: u64,
    sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Download {
    url: String,
    #[serde(flatten)]
    blob: Blob,
}
#[derive(Deserialize)]
struct RuntimePin {
    python: Download,
    wheels: Vec<Download>,
    python_executable: String,
    site_packages: String,
}
#[derive(Deserialize)]
struct MtkSource {
    #[serde(flatten)]
    download: Download,
    inventory_sha256: String,
    revision: String,
}
#[derive(Deserialize)]
struct MtkPins {
    platforms: BTreeMap<String, RuntimePin>,
    mtk_source: MtkSource,
}
#[derive(Deserialize)]
struct AdbPin {
    #[serde(flatten)]
    download: Download,
    adb: String,
    files: BTreeMap<String, Blob>,
}
#[derive(Deserialize)]
struct AdbPins {
    platforms: BTreeMap<String, AdbPin>,
}
#[derive(Deserialize)]
struct SourceInventory {
    revision: String,
    files: BTreeMap<String, Blob>,
    loader_included: bool,
    prebuilt_payloads_included: bool,
}

/// Paths returned only after the pinned downloads, source inventory and installed
/// files are verified. Retain the SessionGuard and reverify before execution.
#[derive(Debug)]
pub struct PreparedDependencies {
    pub platform: String,
    pub runtime_root: PathBuf,
    pub runtime_receipt_sha256: String,
    pub python: PathBuf,
    pub mtk_root: PathBuf,
    pub libusb: PathBuf,
    pub adb_root: PathBuf,
    pub adb_receipt_sha256: String,
    pub adb: PathBuf,
    pub owner_da_root: PathBuf,
    pub owner_da_receipt_sha256: String,
    pub owner_da: PathBuf,
    pub owner_da_sha256: String,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn check_bytes(bytes: &[u8], expected: &Blob) -> Result<()> {
    ensure!(expected.size <= MAX_FILE, "invalid dependency size pin");
    ensure!(
        bytes.len() as u64 == expected.size && digest(bytes) == expected.sha256,
        "dependency size/hash differs from reviewed pin"
    );
    Ok(())
}
fn safe_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(['\\', ':']) && !name.chars().any(char::is_control),
        "unsafe dependency path"
    );
    for part in name.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != ".." && !part.ends_with([' ', '.']),
            "unsafe dependency path component"
        );
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && matches!(base.as_bytes()[3], b'1'..=b'9')),
            "reserved Windows dependency path"
        );
    }
    Ok(())
}
fn create(root: &Path, name: &str, bytes: &[u8], executable: bool) -> Result<()> {
    safe_name(name)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE,
        "dependency member exceeds bound"
    );
    let target = root.join(name);
    fs::create_dir_all(target.parent().context("dependency parent missing")?)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(if executable { 0o700 } else { 0o600 })
            .custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = options.open(&target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn json_receipt(root: &Path, name: &str, value: &Value) -> Result<String> {
    let data = serde_json::to_vec_pretty(value)?;
    create(root, name, &data, false)?;
    Ok(digest(&data))
}
fn describe_file(path: &Path) -> Result<Blob> {
    let file = crate::regular(path)?;
    let size = file.metadata()?.len();
    ensure!(size <= MAX_FILE, "dependency file exceeds bound");
    let mut reader = file.take(size + 1);
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        hash.update(&buffer[..n]);
    }
    ensure!(count == size, "dependency file changed while verifying");
    Ok(Blob {
        size,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn verified_receipt(path: &Path, expected: &str) -> Result<Value> {
    let mut bytes = Vec::new();
    crate::regular(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024 && digest(&bytes) == expected,
        "dependency receipt changed"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn file_inventory(root: &Path) -> Result<BTreeMap<String, Blob>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, Blob>,
        total: &mut u64,
    ) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "dependency symlink refused");
            if kind.is_dir() {
                visit(root, &entry.path(), files, total)?;
            } else {
                ensure!(kind.is_file(), "dependency special file refused");
                let blob = describe_file(&entry.path())?;
                *total += blob.size;
                ensure!(
                    *total <= MAX_TOTAL && files.len() < MAX_MEMBERS,
                    "dependency tree exceeds bound"
                );
                let name = entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 dependency path")?
                    .replace('\\', "/");
                safe_name(&name)?;
                files.insert(name, blob);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files, &mut 0)?;
    Ok(files)
}
fn verified_download(
    client: &Client,
    pin: &Download,
    cache: &Path,
    label: &str,
    progress: &mut Progress<'_>,
) -> Result<Vec<u8>> {
    ensure!(
        pin.blob.sha256.len() == 64
            && pin
                .blob
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid dependency hash pin"
    );
    ensure!(
        pin.blob.size > 0 && pin.blob.size <= MAX_FILE,
        "dependency download exceeds bound"
    );
    let url = Url::parse(&pin.url)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "dependency requires public HTTPS"
    );
    progress(label, 0, pin.blob.size)?;
    let path = cache.join(&pin.blob.sha256);
    let bytes = if path.exists() {
        ensure!(
            fs::symlink_metadata(&path)?.file_type().is_file(),
            "cache must be a regular file"
        );
        let mut bytes = Vec::new();
        File::open(&path)?
            .take(pin.blob.size + 1)
            .read_to_end(&mut bytes)?;
        bytes
    } else {
        let mut response = client.get(url).send()?.error_for_status()?;
        ensure!(
            response.url().scheme() == "https",
            "dependency redirected away from HTTPS"
        );
        if let Some(size) = response.content_length() {
            ensure!(size == pin.blob.size, "dependency response size differs");
        }
        let mut bytes = Vec::with_capacity(pin.blob.size as usize);
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = response.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            ensure!(
                bytes.len() as u64 + n as u64 <= pin.blob.size,
                "dependency download overflow"
            );
            bytes.extend_from_slice(&buffer[..n]);
            progress(label, bytes.len() as u64, pin.blob.size)?;
        }
        check_bytes(&bytes, &pin.blob)?;
        create(cache, &pin.blob.sha256, &bytes, false)?;
        bytes
    };
    check_bytes(&bytes, &pin.blob)?;
    progress(label, pin.blob.size, pin.blob.size)?;
    // The returned snapshot, rather than the cache path, is the only parse input.
    Ok(bytes)
}

struct TarMember {
    data: Vec<u8>,
    executable: bool,
    alias: Option<(String, bool)>,
}
fn tar_members(bytes: &[u8], prefix: &str) -> Result<BTreeMap<String, TarMember>> {
    let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(bytes)));
    let mut result = BTreeMap::new();
    let mut total = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let name = std::str::from_utf8(&entry.path_bytes())?.to_string();
        safe_name(&name)?;
        ensure!(
            name.starts_with(prefix) && !result.contains_key(&name),
            "unexpected or duplicate tar member"
        );
        ensure!(
            kind.is_file() || kind.is_symlink() || kind.is_hard_link(),
            "unsupported tar member type"
        );
        let size = entry.size();
        ensure!(size <= MAX_FILE, "tar member exceeds bound");
        total += size;
        ensure!(
            total <= MAX_TOTAL && result.len() < MAX_MEMBERS,
            "tar source exceeds bound"
        );
        let alias = if kind.is_symlink() || kind.is_hard_link() {
            Some((
                std::str::from_utf8(
                    &entry
                        .link_name_bytes()
                        .context("tar alias target missing")?,
                )?
                .to_string(),
                kind.is_symlink(),
            ))
        } else {
            None
        };
        let executable = entry.header().mode()? & 0o111 != 0;
        let mut data = Vec::new();
        entry.take(size + 1).read_to_end(&mut data)?;
        ensure!(data.len() as u64 == size, "truncated tar member");
        result.insert(
            name,
            TarMember {
                data,
                executable,
                alias,
            },
        );
    }
    Ok(result)
}
fn alias_target(name: &str, link: &str, relative: bool) -> Result<String> {
    ensure!(
        !link.starts_with('/')
            && !link.contains(['\\', ':'])
            && !link.chars().any(char::is_control),
        "unsafe tar alias"
    );
    let mut parts: Vec<&str> = if relative {
        name.rsplit_once('/')
            .map(|(parent, _)| parent.split('/').collect())
            .unwrap_or_default()
    } else {
        vec![]
    };
    for part in link.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                ensure!(parts.pop().is_some(), "tar alias escapes root");
            }
            part => parts.push(part),
        }
    }
    let target = parts.join("/");
    safe_name(&target)?;
    Ok(target)
}
fn unpack_python(bytes: &[u8], root: &Path, progress: &mut Progress<'_>) -> Result<()> {
    let members = tar_members(bytes, "python/")?;
    let mut total = 0;
    for (index, (name, _)) in members.iter().enumerate() {
        progress("Preparing Python", index as u64, members.len() as u64)?;
        let mut current = name.clone();
        let mut seen = BTreeSet::new();
        let member = loop {
            ensure!(
                seen.insert(current.clone()) && seen.len() <= 16,
                "cyclic tar alias"
            );
            let member = members
                .get(&current)
                .context("tar alias escapes admitted files")?;
            if let Some((target, relative)) = &member.alias {
                current = alias_target(&current, target, *relative)?;
            } else {
                break member;
            }
        };
        total += member.data.len() as u64;
        ensure!(total <= MAX_TOTAL, "expanded Python aliases exceed bound");
        create(root, name, &member.data, member.executable)?;
    }
    progress(
        "Preparing Python",
        members.len() as u64,
        members.len() as u64,
    )
}
fn zip_files(bytes: &[u8], mut consume: impl FnMut(&str, &[u8], bool) -> Result<()>) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(archive.len() <= MAX_MEMBERS, "too many zip members");
    let mut names = BTreeSet::new();
    let mut total = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = std::str::from_utf8(entry.name_raw())?.to_string();
        safe_name(&name)?;
        ensure!(names.insert(name.clone()), "duplicate zip member");
        ensure!(
            entry
                .unix_mode()
                .map(|mode| mode & 0o170000 != 0o120000)
                .unwrap_or(true),
            "zip symlink refused"
        );
        let size = entry.size();
        ensure!(size <= MAX_FILE, "zip member exceeds bound");
        total += size;
        ensure!(
            size <= MAX_FILE && total <= MAX_TOTAL,
            "zip member exceeds bound"
        );
        let executable = entry.unix_mode().unwrap_or(0) & 0o111 != 0;
        let mut data = Vec::new();
        entry.take(size + 1).read_to_end(&mut data)?;
        ensure!(data.len() as u64 == size, "truncated zip member");
        consume(&name, &data, executable)?;
    }
    Ok(())
}
fn unpack_wheel(
    bytes: &[u8],
    root: &Path,
    site_packages: &str,
    progress: &mut Progress<'_>,
) -> Result<()> {
    safe_name(site_packages)?;
    let mut count = 0;
    zip_files(bytes, |name, data, executable| {
        ensure!(
            !name.split('/').any(|part| part.ends_with(".data")),
            "wheel requires unreviewed install scheme"
        );
        progress("Preparing Python packages", count, 0)?;
        create(root, &format!("{site_packages}/{name}"), data, executable)?;
        count += 1;
        Ok(())
    })
}
fn unpack_mtk(
    bytes: &[u8],
    root: &Path,
    source: &SourceInventory,
    progress: &mut Progress<'_>,
) -> Result<()> {
    ensure!(
        !source.loader_included && !source.prebuilt_payloads_included,
        "source inventory admits MTK payloads"
    );
    let prefix = format!("mtkclient-{}/", source.revision);
    let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(bytes)));
    let mut selected = BTreeSet::new();
    let mut expanded = 0;
    for (index, entry) in archive.entries()?.enumerate() {
        let entry = entry?;
        ensure!(entry.size() <= MAX_FILE, "MTK archive member exceeds bound");
        expanded += entry.size();
        ensure!(
            entry.size() <= MAX_FILE && expanded <= MAX_TOTAL && index < MAX_MEMBERS,
            "MTK archive exceeds bound"
        );
        let full_name = std::str::from_utf8(&entry.path_bytes())?.to_string();
        let Some(name) = full_name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(expected) = source.files.get(name) else {
            continue;
        };
        safe_name(name)?;
        ensure!(
            entry.header().entry_type().is_file() && selected.insert(name.to_string()),
            "unexpected MTK source type or duplicate"
        );
        ensure!(
            name.ends_with(".py")
                || matches!(
                    name,
                    "LICENSE" | "README.md" | "pyproject.toml" | "requirements.txt"
                ),
            "non-source MTK payload in runtime"
        );
        ensure!(entry.size() == expected.size, "MTK source size mismatch");
        let mut data = Vec::new();
        entry.take(expected.size + 1).read_to_end(&mut data)?;
        check_bytes(&data, expected)?;
        progress(
            "Preparing reviewed MTK source",
            selected.len() as u64,
            source.files.len() as u64,
        )?;
        create(root, &format!("mtk/{name}"), &data, false)?;
    }
    ensure!(
        selected.len() == source.files.len(),
        "MTK source archive is incomplete"
    );
    Ok(())
}
fn unpack_adb(bytes: &[u8], root: &Path, pin: &AdbPin, progress: &mut Progress<'_>) -> Result<()> {
    let mut selected = BTreeSet::new();
    zip_files(bytes, |name, data, _| {
        if let Some(expected) = pin.files.get(name) {
            check_bytes(data, expected)?;
            progress(
                "Preparing ADB",
                selected.len() as u64,
                pin.files.len() as u64,
            )?;
            create(root, name, data, name == pin.adb)?;
            selected.insert(name.to_string());
        }
        Ok(())
    })?;
    ensure!(
        selected.len() == pin.files.len(),
        "ADB archive lacks pinned files"
    );
    Ok(())
}
fn unpack_owner_da(
    bytes: &[u8],
    root: &Path,
    revision: &str,
    progress: &mut Progress<'_>,
) -> Result<()> {
    let wanted = format!("mtkclient-{revision}/{DA_MEMBER}");
    let expected = Blob {
        size: DA_SIZE,
        sha256: DA_SHA256.into(),
    };
    let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(bytes)));
    let mut selected = false;
    let mut expanded = 0;
    for (index, entry) in archive.entries()?.enumerate() {
        let entry = entry?;
        ensure!(entry.size() <= MAX_FILE, "MTK archive member exceeds bound");
        expanded += entry.size();
        ensure!(
            entry.size() <= MAX_FILE && expanded <= MAX_TOTAL && index < MAX_MEMBERS,
            "owner DA archive exceeds bound"
        );
        if entry.path_bytes().as_ref() != wanted.as_bytes() {
            continue;
        }
        ensure!(
            !selected && entry.header().entry_type().is_file() && entry.size() == DA_SIZE,
            "unexpected owner DA member"
        );
        let mut data = Vec::new();
        entry.take(DA_SIZE + 1).read_to_end(&mut data)?;
        check_bytes(&data, &expected)?;
        progress("Preparing owner-local download agent", DA_SIZE, DA_SIZE)?;
        create(root, "loader.bin", &data, false)?;
        selected = true;
    }
    ensure!(selected, "owner DA absent from pinned source archive");
    Ok(())
}

/// Build all dependencies in a fresh child of an already private, leased session.
/// Callback errors cancel before further downloads/extraction; nothing is executed.
pub fn prepare(
    session: &SessionGuard,
    platform: &str,
    mut progress: impl FnMut(&str, u64, u64) -> Result<()>,
) -> Result<PreparedDependencies> {
    ensure!(
        session.phase() == Phase::Created,
        "dependencies must be prepared before installation starts"
    );
    let pins: MtkPins = serde_json::from_slice(MTK_PINS)?;
    let pin = pins
        .platforms
        .get(platform)
        .context("unsupported dependency platform")?;
    let adb_pins: AdbPins = serde_json::from_slice(ADB_PINS)?;
    let adb_platform = if platform.starts_with("macos-") {
        "macos-universal"
    } else {
        platform
    };
    let adb_pin = adb_pins
        .platforms
        .get(adb_platform)
        .context("unsupported ADB platform")?;
    ensure!(
        digest(SOURCE_INVENTORY) == pins.mtk_source.inventory_sha256,
        "compiled MTK inventory differs from pin"
    );
    let inventory: SourceInventory = serde_json::from_slice(SOURCE_INVENTORY)?;
    ensure!(
        inventory.revision == pins.mtk_source.revision,
        "MTK source revision mismatch"
    );
    progress("Preparing dependencies", 0, 0)?;
    let base = session.path().join("dependencies");
    crate::private_dir(&base).context("dependency output must be new")?;
    let cache = base.join("cache");
    crate::private_dir(&cache)?;
    let client = Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(180))
        .redirect(Policy::limited(5))
        .no_proxy()
        .user_agent("Couch-installer-native/1")
        .build()?;
    let python = verified_download(
        &client,
        &pin.python,
        &cache,
        "Downloading Python",
        &mut progress,
    )?;
    let wheels = pin
        .wheels
        .iter()
        .map(|wheel| {
            verified_download(
                &client,
                wheel,
                &cache,
                "Downloading Python packages",
                &mut progress,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mtk = verified_download(
        &client,
        &pins.mtk_source.download,
        &cache,
        "Downloading reviewed MTK source",
        &mut progress,
    )?;
    let adb = verified_download(
        &client,
        &adb_pin.download,
        &cache,
        "Downloading ADB",
        &mut progress,
    )?;
    let runtime_root = base.join("runtime");
    crate::private_dir(&runtime_root)?;
    unpack_python(&python, &runtime_root, &mut progress)?;
    for wheel in &wheels {
        unpack_wheel(wheel, &runtime_root, &pin.site_packages, &mut progress)?;
    }
    unpack_mtk(&mtk, &runtime_root, &inventory, &mut progress)?;
    let files = file_inventory(&runtime_root)?;
    let libraries: Vec<_> = files
        .keys()
        .filter(|name| {
            name.starts_with(&format!("{}/libusb_package/", pin.site_packages))
                && name.contains("libusb")
                && [".so", ".dll", ".dylib"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        })
        .cloned()
        .collect();
    ensure!(
        libraries.len() == 1 && files.contains_key(&pin.python_executable),
        "runtime lacks pinned Python/libusb entrypoints"
    );
    let libusb = &libraries[0];
    let runtime_receipt_sha256 = json_receipt(
        &runtime_root,
        "runtime.json",
        &json!({
            "schema":1,"kind":"couch-owner-mtk-runtime","platform":platform,"files":files,
            "executables":{"python":pin.python_executable},"site_packages":pin.site_packages,
            "mtk":{"root":"mtk","revision":inventory.revision,"source_inventory_sha256":digest(SOURCE_INVENTORY)},
            "native_libraries":{"libusb":libusb},"pins_sha256":digest(MTK_PINS),
            "complete":true,"loader_included":false,"prebuilt_mtk_payloads_included":false,"physical_usb_validated":false
        }),
    )?;
    let adb_root = base.join("adb");
    crate::private_dir(&adb_root)?;
    unpack_adb(&adb, &adb_root, adb_pin, &mut progress)?;
    let adb_receipt_sha256 = json_receipt(
        &adb_root,
        "receipt.json",
        &json!({
            "schema":1,"kind":"couch-owner-adb","platform":adb_platform,"files":file_inventory(&adb_root)?,
            "executables":{"adb":adb_pin.adb},"complete":true,"usb_opened":false
        }),
    )?;
    let owner_da_root = base.join("owner-da");
    crate::private_dir(&owner_da_root)?;
    unpack_owner_da(&mtk, &owner_da_root, &inventory.revision, &mut progress)?;
    let owner_da_receipt_sha256 = json_receipt(
        &owner_da_root,
        "receipt.json",
        &json!({
            "schema":1,"kind":"couch-owner-mtk-da","files":file_inventory(&owner_da_root)?,
            "source_archive_sha256":pins.mtk_source.download.blob.sha256,"source_revision":inventory.revision,
            "member":DA_MEMBER,"loader":"loader.bin","complete":true,"redistributable":false
        }),
    )?;
    let prepared = PreparedDependencies {
        platform: platform.into(),
        python: runtime_root.join(&pin.python_executable),
        mtk_root: runtime_root.join("mtk"),
        libusb: runtime_root.join(libusb),
        runtime_root,
        runtime_receipt_sha256,
        adb: adb_root.join(&adb_pin.adb),
        adb_root,
        adb_receipt_sha256,
        owner_da: owner_da_root.join("loader.bin"),
        owner_da_sha256: DA_SHA256.into(),
        owner_da_root,
        owner_da_receipt_sha256,
    };
    prepared.verify()?;
    progress("Dependencies verified", 1, 1)?;
    Ok(prepared)
}
impl PreparedDependencies {
    /// Reverify the remembered receipt hashes and every regular file before a child
    /// process may use these paths. Do not reconstruct trust from an untrusted receipt.
    pub fn verify(&self) -> Result<()> {
        let mut receipts = BTreeMap::new();
        for (root, receipt, expected, kind) in [
            (
                &self.runtime_root,
                "runtime.json",
                &self.runtime_receipt_sha256,
                "couch-owner-mtk-runtime",
            ),
            (
                &self.adb_root,
                "receipt.json",
                &self.adb_receipt_sha256,
                "couch-owner-adb",
            ),
            (
                &self.owner_da_root,
                "receipt.json",
                &self.owner_da_receipt_sha256,
                "couch-owner-mtk-da",
            ),
        ] {
            let path = root.join(receipt);
            let value = verified_receipt(&path, expected)?;
            ensure!(
                value["schema"] == 1 && value["kind"] == kind && value["complete"] == true,
                "invalid dependency receipt"
            );
            let expected_files: BTreeMap<String, Blob> =
                serde_json::from_value(value["files"].clone())?;
            let mut actual = file_inventory(root)?;
            actual.remove(receipt);
            ensure!(
                actual == expected_files,
                "prepared dependency files changed"
            );
            receipts.insert(kind, value);
        }
        let runtime = &receipts["couch-owner-mtk-runtime"];
        let adb = &receipts["couch-owner-adb"];
        ensure!(
            runtime["platform"] == self.platform
                && self.python
                    == self.runtime_root.join(
                        runtime["executables"]["python"]
                            .as_str()
                            .context("missing Python entrypoint")?
                    )
                && self.libusb
                    == self.runtime_root.join(
                        runtime["native_libraries"]["libusb"]
                            .as_str()
                            .context("missing libusb entrypoint")?
                    )
                && self.mtk_root == self.runtime_root.join("mtk")
                && self.adb
                    == self.adb_root.join(
                        adb["executables"]["adb"]
                            .as_str()
                            .context("missing ADB entrypoint")?
                    )
                && self.owner_da == self.owner_da_root.join("loader.bin"),
            "dependency paths differ from admitted receipts"
        );
        ensure!(
            self.owner_da_sha256 == DA_SHA256 && describe_file(&self.owner_da)?.sha256 == DA_SHA256,
            "owner DA differs from approved pin"
        );
        Ok(())
    }
}

/// Stable runtime platform identifiers, independent of optional system tools.
pub fn host_platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("macos", "aarch64") => Ok("macos-aarch64"),
        ("macos", "x86_64") => Ok("macos-x86_64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        _ => bail!("unsupported installer dependency platform"),
    }
}

/// Use ordinary absolute Windows paths at Python's import boundary. PyCryptodome
/// joins package paths with `..`, which Windows verbatim paths do not normalize.
/// The underlying prepared files and receipt verification retain their original paths.
pub fn python_path(path: &Path) -> Result<String> {
    let text = path.to_str().context("non-UTF8 Python path")?;
    if cfg!(windows) {
        return ordinary_windows_path(text);
    }
    Ok(text.into())
}

fn ordinary_windows_path(text: &str) -> Result<String> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Ok(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        ensure!(
            rest.as_bytes().get(1) == Some(&b':') && rest.as_bytes().get(2) == Some(&b'\\'),
            "unsupported verbatim Python path"
        );
        return Ok(rest.into());
    }
    Ok(text.into())
}

/// Read-only executable/import check. Every invocation is preceded by full receipt
/// verification; discovery is prohibited and the environment contains no tool PATH.
pub fn smoke(prepared: &PreparedDependencies) -> Result<Value> {
    use std::process::{Command, Stdio};
    use std::{thread, time::Instant};
    fn run(executable: &Path, args: &[String]) -> Result<String> {
        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir(
                executable
                    .parent()
                    .context("smoke executable parent absent")?,
            )
            .env_clear()
            .env("PATH", "")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().context("smoke stdout absent")?;
        let stderr = child.stderr.take().context("smoke stderr absent")?;
        let read = |stream: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                stream.take(65537).read_to_end(&mut bytes).map(|_| bytes)
            })
        };
        let out = read(Box::new(stdout));
        let err = read(Box::new(stderr));
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                bail!("dependency smoke deadline exceeded");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let output = out
            .join()
            .map_err(|_| anyhow::anyhow!("smoke reader panicked"))??;
        let errors = err
            .join()
            .map_err(|_| anyhow::anyhow!("smoke reader panicked"))??;
        ensure!(
            output.len() <= 65536 && errors.len() <= 65536,
            "dependency smoke output exceeds bound"
        );
        ensure!(
            status.success(),
            "dependency smoke failed: {}",
            String::from_utf8_lossy(&errors)
        );
        Ok(String::from_utf8(output)?)
    }
    ensure!(
        prepared.platform == host_platform()?,
        "native smoke requires matching platform"
    );
    prepared.verify()?;
    let version = run(
        &prepared.python,
        &["-I".into(), "-B".into(), "--version".into()],
    )?;
    ensure!(
        version.trim() == "Python 3.12.14",
        "unexpected Python version"
    );
    prepared.verify()?;
    let adb = run(&prepared.adb, &["version".into()])?;
    ensure!(
        adb.contains("Android Debug Bridge version") && adb.contains("37.0.1"),
        "unexpected ADB version"
    );
    let pins: MtkPins = serde_json::from_slice(MTK_PINS)?;
    let pin = pins
        .platforms
        .get(&prepared.platform)
        .context("unsupported runtime platform")?;
    let site = prepared.runtime_root.join(&pin.site_packages);
    let code = r#"import ctypes,json,pathlib,sys
sys.path.insert(0,sys.argv[1]);sys.path.insert(0,sys.argv[2])
import usb.core,serial,colorama,Crypto.Cipher.AES,Cryptodome.Cipher.AES,libusb_package
library=pathlib.Path(libusb_package.get_library_path()).resolve()
assert library==pathlib.Path(sys.argv[3]).resolve()
ctypes.CDLL(str(library))
def no_usb(*a,**k): raise RuntimeError("USB forbidden during dependency smoke")
usb.core.find=no_usb
from mtkclient.Library.mtk_class import Mtk
from mtkclient.Library.Connection.usblib import UsbClass
from mtkclient.config.mtk_config import MtkConfig
config=MtkConfig();config.interface=0;config.stock=True;config.loader=None
Mtk(config,preinit=False)
print(json.dumps({"imports":"passed","libusb_load":"passed","usb_opened":False}))
"#;
    prepared.verify()?;
    let result = run(
        &prepared.python,
        &[
            "-I".into(),
            "-B".into(),
            "-S".into(),
            "-c".into(),
            code.into(),
            python_path(&site)?,
            python_path(&prepared.mtk_root)?,
            python_path(&prepared.libusb)?,
        ],
    )?;
    let imports: Value =
        serde_json::from_str(result.lines().last().context("smoke result missing")?)?;
    prepared.verify()?;
    Ok(
        json!({"platform":prepared.platform,"python":version.trim(),"adb":adb.lines().next(),"imports":imports,"usb_opened":false,"runtime_receipt_sha256":prepared.runtime_receipt_sha256,"owner_da_sha256":DA_SHA256}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    fn tar_fixture(alias: Option<&str>) -> Vec<u8> {
        let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "python/bin/python3.12", &b"fixture"[..])
            .unwrap();
        if let Some(target) = alias {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name(target).unwrap();
            header.set_cksum();
            archive
                .append_data(&mut header, "python/bin/python3", &b""[..])
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }
    fn no_progress(_: &str, _: u64, _: u64) -> Result<()> {
        Ok(())
    }
    #[test]
    fn python_windows_paths_allow_package_parent_joins_without_verbatim_semantics() {
        assert_eq!(
            ordinary_windows_path(r"\\?\C:\private\runtime").unwrap(),
            r"C:\private\runtime"
        );
        assert_eq!(
            ordinary_windows_path(r"\\?\UNC\server\share\runtime").unwrap(),
            r"\\server\share\runtime"
        );
        assert_eq!(
            ordinary_windows_path(r"C:\private\runtime").unwrap(),
            r"C:\private\runtime"
        );
        assert!(ordinary_windows_path(r"\\?\GLOBALROOT\Device").is_err());
    }
    #[test]
    fn receipt_parser_uses_only_authenticated_snapshot_and_file_reads_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("receipt.json");
        let bytes = br#"{"fixture":"original"}"#;
        fs::write(&path, bytes).unwrap();
        let admitted = verified_receipt(&path, &digest(bytes)).unwrap();
        fs::write(&path, br#"{"fixture":"swapped"}"#).unwrap();
        assert_eq!(admitted["fixture"], "original");
        assert!(verified_receipt(&path, &digest(bytes)).is_err());
        let large = root.path().join("too-large");
        File::create(&large).unwrap().set_len(MAX_FILE + 1).unwrap();
        assert!(describe_file(&large).is_err());
    }
    #[test]
    fn compiled_pins_cover_all_hosts_and_exact_source_inventory() {
        let pins: MtkPins = serde_json::from_slice(MTK_PINS).unwrap();
        assert_eq!(pins.platforms.len(), 4);
        assert_eq!(pins.mtk_source.inventory_sha256, digest(SOURCE_INVENTORY));
        let source: SourceInventory = serde_json::from_slice(SOURCE_INVENTORY).unwrap();
        assert_eq!(source.revision, pins.mtk_source.revision);
        assert_eq!(source.files.len(), 100);
        assert!(!source.loader_included && !source.prebuilt_payloads_included);
        for pin in pins.platforms.values() {
            assert_eq!(pin.wheels.len(), 7);
            assert!(pin.python.url.starts_with("https://"));
        }
    }
    #[test]
    fn dangerous_paths_and_aliases_fail_before_publication() {
        for name in [
            "../escape",
            "/absolute",
            "a\\b",
            "a/./b",
            "c:drive",
            "AUX.txt",
            "foo\0tail",
            "a/ending.",
        ] {
            assert!(safe_name(name).is_err(), "{name:?}");
        }
        let root = tempfile::tempdir().unwrap();
        for (i, target) in ["../../../escape", "/absolute", "python3"]
            .iter()
            .enumerate()
        {
            assert!(unpack_python(
                &tar_fixture(Some(target)),
                &root.path().join(i.to_string()),
                &mut no_progress
            )
            .is_err());
        }
        assert!(!root.path().join("escape").exists());
    }
    #[test]
    fn python_aliases_are_regular_executable_copies_and_snapshot_bytes_are_immutable() {
        let root = tempfile::tempdir().unwrap();
        let bytes = tar_fixture(Some("python3.12"));
        unpack_python(&bytes, root.path(), &mut no_progress).unwrap();
        let alias = root.path().join("python/bin/python3");
        assert!(!fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(alias).unwrap(), b"fixture");
        let expected = Blob {
            size: bytes.len() as u64,
            sha256: digest(&bytes),
        };
        let mut swapped = bytes.clone();
        swapped[0] ^= 1;
        assert!(check_bytes(&swapped, &expected).is_err());
        check_bytes(&bytes, &expected).unwrap();
    }
    #[test]
    fn cancellation_stops_extraction_and_cache_admission_before_network() {
        let root = tempfile::tempdir().unwrap();
        let mut cancel = |_: &str, _: u64, _: u64| -> Result<()> { bail!("UI disconnected") };
        assert!(unpack_python(&tar_fixture(None), root.path(), &mut cancel).is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let pin = Download {
            url: "https://example.invalid/source".into(),
            blob: Blob {
                size: 7,
                sha256: digest(b"fixture"),
            },
        };
        assert!(
            verified_download(&Client::new(), &pin, root.path(), "fixture", &mut cancel)
                .unwrap_err()
                .to_string()
                .contains("UI disconnected")
        );
    }
    #[test]
    fn cache_corruption_and_insecure_urls_are_rejected_without_download() {
        let root = tempfile::tempdir().unwrap();
        let mut pin = Download {
            url: "https://example.invalid/source".into(),
            blob: Blob {
                size: 7,
                sha256: digest(b"fixture"),
            },
        };
        fs::write(root.path().join(&pin.blob.sha256), b"corrupt").unwrap();
        assert!(verified_download(
            &Client::new(),
            &pin,
            root.path(),
            "fixture",
            &mut no_progress
        )
        .is_err());
        pin.url = "http://example.invalid/source".into();
        assert!(verified_download(
            &Client::new(),
            &pin,
            root.path(),
            "fixture",
            &mut no_progress
        )
        .is_err());
        pin.url = "https://user:password@example.invalid/source".into();
        assert!(verified_download(
            &Client::new(),
            &pin,
            root.path(),
            "fixture",
            &mut no_progress
        )
        .is_err());
    }
    #[test]
    fn source_only_mtk_extraction_omits_da_and_handles_empty_modules() {
        let root = tempfile::tempdir().unwrap();
        let revision = "a".repeat(40);
        let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (name, data) in [
            ("mtkclient/__init__.py", &b""[..]),
            (DA_MEMBER, &b"must not enter runtime"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("mtkclient-{revision}/{name}"), data)
                .unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let source = SourceInventory {
            revision,
            files: BTreeMap::from([(
                "mtkclient/__init__.py".into(),
                Blob {
                    size: 0,
                    sha256: digest(b""),
                },
            )]),
            loader_included: false,
            prebuilt_payloads_included: false,
        };
        unpack_mtk(&bytes, root.path(), &source, &mut no_progress).unwrap();
        assert_eq!(file_inventory(root.path()).unwrap().len(), 1);
        assert!(!root.path().join("mtk").join(DA_MEMBER).exists());
        assert!(unpack_owner_da(&bytes, root.path(), &source.revision, &mut no_progress).is_err());
        assert!(!root.path().join("loader.bin").exists());
    }
    #[test]
    fn zip_raw_names_and_unreviewed_wheel_schemes_are_refused() {
        for name in ["../escape", "bad\\name", "x.data/scripts/hook"] {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"fixture").unwrap();
            let bytes = archive.finish().unwrap().into_inner();
            let root = tempfile::tempdir().unwrap();
            assert!(unpack_wheel(&bytes, root.path(), "site", &mut no_progress).is_err());
        }
    }
}
