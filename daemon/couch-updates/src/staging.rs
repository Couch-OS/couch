use crate::{
    release::{self, File, Manifest},
    Result,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt},
    path::{Component, Path},
};
const MAX_TOTAL: u64 = 128 * 1024 * 1024;
const REQUIRED: &[&str] = &[
    "couch-gui",
    "couch-confd",
    "couch-system",
    "couch-sonos",
    "couch-coreelec",
    "couch-wmt-properties.so",
    "stage2.sh",
    "hardware-init.sh",
    "gui-start.sh",
    "system.sh",
    "confd.sh",
    "setup-mode.sh",
    "portal.sh",
    "station.sh",
    "wifi-conf.sh",
    "build.json",
];
pub(crate) fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("Invalid state path")?;
    fs::create_dir_all(parent).map_err(|_| "Could not create update state")?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Could not secure update state")?;
    let temp = path.with_extension(format!("new-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|_| "Could not persist update state".into())
}
fn id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn allowed(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 180
        || !Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        || name.contains('\\')
    {
        return false;
    }
    REQUIRED.contains(&name)
        || name == "fbcon"
        || name.strip_prefix("www/").is_some_and(|rest| {
            !rest.starts_with('.')
                && (rest.starts_with("cgi-bin/")
                    && matches!(
                        rest,
                        "cgi-bin/save" | "cgi-bin/scan" | "cgi-bin/enroll" | "cgi-bin/setpw"
                    )
                    || [".html", ".css", ".js", ".svg", ".png", ".woff2"]
                        .iter()
                        .any(|ext| rest.ends_with(ext)))
        })
        || name.starts_with("licenses/") && name.ends_with(".txt")
}
fn inventory(m: &Manifest) -> Result<BTreeMap<String, &File>> {
    if m.kind != "runtime" || !m.installable || m.files.len() > 256 {
        return Err("Unsupported update type".into());
    }
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for f in &m.files {
        if !allowed(&f.path)
            || !matches!(f.mode, 0o644 | 0o755)
            || f.size > 64 * 1024 * 1024
            || !id(&f.sha256)
            || files.insert(f.path.clone(), f).is_some()
        {
            return Err("Invalid or duplicate runtime update path".into());
        }
        if (f.path.ends_with(".sh")
            || f.path.starts_with("couch-")
            || f.path.starts_with("www/cgi-bin/"))
            && f.mode != 0o755
        {
            return Err("Runtime executable is not executable".into());
        }
        total = total.checked_add(f.size).ok_or("Update too large")?;
    }
    if total > MAX_TOTAL || REQUIRED.iter().any(|p| !files.contains_key(*p)) {
        return Err("Incomplete or oversized runtime update".into());
    }
    Ok(files)
}
pub(crate) fn stage(root: &Path, m: &Manifest, phase: impl Fn(&str)) -> Result<()> {
    crate::baseline::check(root, m)?;
    inventory(m)?;
    let bytes = release::fetch(&m.url, m.size)?;
    if bytes.len() as u64 != m.size || release::digest(&bytes) != m.sha256 {
        return Err("Update download digest mismatch".into());
    }
    phase("verifying");
    unpack(root, m, &bytes)?;
    atomic(&root.join("runtime/staged"), m.sha256.as_bytes())
}
fn unpack(root: &Path, m: &Manifest, bytes: &[u8]) -> Result<()> {
    crate::baseline::check(root, m)?;
    let files = inventory(m)?;
    let slots = root.join("runtime/slots");
    fs::create_dir_all(&slots).map_err(|_| "Could not create runtime slots")?;
    fs::set_permissions(&slots, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Could not secure runtime slots")?;
    let target = slots.join(&m.sha256);
    if target.exists() {
        return verify_files(&target, m);
    }
    let work = slots.join(format!(".staging-{}", std::process::id()));
    // The service serializes staging. Only this private, fixed temporary path is
    // reclaimed; completed slots and the active runtime are never removed.
    if work.exists() {
        fs::remove_dir_all(&work).map_err(|_| "Could not clean interrupted staging")?;
    }
    fs::create_dir(&work).map_err(|_| "Could not create staging directory")?;
    let result = (|| -> Result<()> {
        let decoder = flate2::read::GzDecoder::new(bytes).take(MAX_TOTAL + 1024 * 1024);
        let mut archive = tar::Archive::new(decoder);
        let mut seen = BTreeSet::new();
        for entry in archive.entries().map_err(|_| "Invalid update archive")? {
            let mut entry = entry.map_err(|_| "Invalid update archive entry")?;
            // Bundle format contains regular files only, with explicit parent creation.
            if !entry.header().entry_type().is_file() {
                return Err("Links and special files are forbidden in updates".into());
            }
            let name = entry
                .path()
                .map_err(|_| "Invalid update path")?
                .to_str()
                .ok_or("Invalid update path encoding")?
                .to_owned();
            let item = files.get(&name).ok_or("Unlisted update file")?;
            if !seen.insert(name.clone()) || entry.size() != item.size {
                return Err("Duplicate file or wrong update file size".into());
            }
            let path = work.join(&name);
            let parent = path.parent().unwrap();
            fs::create_dir_all(parent).map_err(|_| "Could not stage update directory")?;
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(item.mode)
                .open(&path)
                .map_err(|_| "Could not stage update file")?;
            let mut hash = Sha256::new();
            let mut count = 0u64;
            let mut buf = [0u8; 65536];
            loop {
                let n = entry.read(&mut buf).map_err(|_| "Truncated update")?;
                if n == 0 {
                    break;
                }
                output
                    .write_all(&buf[..n])
                    .map_err(|_| "Not enough space to stage update")?;
                hash.update(&buf[..n]);
                count += n as u64;
            }
            if count != item.size || release::hex(&hash.finalize()) != item.sha256 {
                return Err("Runtime file digest mismatch".into());
            }
            output
                .sync_all()
                .map_err(|_| "Could not flush update file")?;
            fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|_| "Could not flush update directory")?;
        }
        if seen.len() != files.len() {
            return Err("Update archive is incomplete".into());
        }
        let build: serde_json::Value = serde_json::from_slice(
            &fs::read(work.join("build.json")).map_err(|_| "Missing build identity")?,
        )
        .map_err(|_| "Invalid build identity")?;
        if build["version"] != m.version {
            return Err("Build identity does not match the selected update".into());
        }
        atomic(
            &work.join(".manifest.json"),
            &serde_json::to_vec(m).map_err(|_| "Invalid update manifest")?,
        )?;
        fs::rename(&work, &target).map_err(|_| "Could not finalize runtime slot")?;
        fs::File::open(&slots)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Could not flush runtime slots")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&work);
    }
    result
}
fn verify_files(slot: &Path, m: &Manifest) -> Result<()> {
    for f in inventory(m)?.values() {
        let path = slot.join(&f.path);
        let meta = fs::symlink_metadata(&path).map_err(|_| "Staged update file missing")?;
        if !meta.is_file() || meta.len() != f.size || meta.permissions().mode() & 0o777 != f.mode {
            return Err("Staged update file changed".into());
        }
        let mut input = fs::File::open(path).map_err(|_| "Could not read staged file")?;
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = input
                .read(&mut buf)
                .map_err(|_| "Could not verify staged file")?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        if release::hex(&hash.finalize()) != f.sha256 {
            return Err("Staged update digest mismatch".into());
        }
    }
    Ok(())
}
pub fn activate(root: &Path) -> Result<()> {
    let runtime = root.join("runtime");
    if runtime.join("pending").exists() {
        return Err("An update already awaits boot confirmation".into());
    }
    let selected = fs::read_to_string(runtime.join("staged")).map_err(|_| "No staged update")?;
    if !id(&selected) {
        return Err("Invalid staged update identifier".into());
    }
    let slot = runtime.join("slots").join(&selected);
    let m: Manifest = serde_json::from_slice(
        &fs::read(slot.join(".manifest.json")).map_err(|_| "Staged manifest missing")?,
    )
    .map_err(|_| "Invalid staged manifest")?;
    if m.sha256 != selected {
        return Err("Staged manifest changed".into());
    }
    crate::baseline::check(root, &m)?;
    verify_files(&slot, &m)?;
    let previous = match fs::read_link(runtime.join("current")) {
        Ok(path) => {
            let s = path
                .to_str()
                .and_then(|s| s.strip_prefix("slots/"))
                .filter(|s| id(s))
                .ok_or("Invalid active runtime pointer")?;
            s.to_owned()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "base".into(),
        Err(_) => return Err("Could not inspect current runtime".into()),
    };
    // A power loss before rename can leave the private next pointer behind.
    // Reclaim only that symlink; never remove a directory or completed slot.
    let next = runtime.join("next");
    match fs::symlink_metadata(&next) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::remove_file(&next).map_err(|_| "Could not clear interrupted runtime switch")?;
        }
        Ok(_) => return Err("Unexpected runtime switch path".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not inspect runtime switch path".into()),
    }
    atomic(
        &runtime.join("pending"),
        format!("{previous} {selected}\n").as_bytes(),
    )?;
    symlink(format!("slots/{selected}"), &next).map_err(|_| "Could not prepare runtime switch")?;
    fs::rename(&next, runtime.join("current")).map_err(|_| "Could not switch runtime")?;
    fs::File::open(&runtime)
        .and_then(|f| f.sync_all())
        .map_err(|_| "Could not flush runtime switch")?;
    fs::remove_file(runtime.join("staged")).map_err(|_| "Could not finalize activation")?;
    Ok(())
}

pub(crate) fn required_names() -> Vec<String> {
    REQUIRED.iter().map(|s| s.to_string()).collect()
}
pub(crate) fn validate_inventory(manifest: &Manifest) -> Result<()> {
    inventory(manifest).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (std::path::PathBuf, Manifest, Vec<u8>) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "couch-update-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        let mut files = Vec::new();
        for name in REQUIRED {
            let bytes = if *name == "build.json" {
                b"{\"version\":\"v1.2.3\"}".as_slice()
            } else {
                b"fixture runtime".as_slice()
            };
            let mode = if *name == "build.json" { 0o644 } else { 0o755 };
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
            files.push(File {
                path: name.to_string(),
                size: bytes.len() as u64,
                sha256: release::digest(bytes),
                mode,
            });
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let manifest=Manifest{schema:1,model:"sanytron-ha100".into(),version:"v1.2.3".into(),kind:"runtime".into(),installable:true,notes:String::new(),url:"https://github.com/dangerouslaser/couch/releases/download/v1.2.3/couch-v1.2.3-ha100-runtime.tar.gz".into(),size:bytes.len() as u64,sha256:release::digest(&bytes),files,required_os_baseline:None};
        (root, manifest, bytes)
    }
    fn marker(root: &Path, id: &str) {
        fs::write(
            root.join("os-baseline.json"),
            serde_json::to_vec(&serde_json::json!({"schema":1,"model":"sanytron-ha100","id":id}))
                .unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn publisher_requires_baseline_and_signs_it_without_shipping_marker() {
        let (root, m, bytes) = fixture();
        unpack(&root, &m, &bytes).unwrap();
        let source = root.join("runtime/slots").join(&m.sha256);
        let output = root.join("published");
        assert!(crate::bundle(&source, "v1.2.4", &[42; 32], &output)
            .unwrap_err()
            .contains("compatibility marker missing"));
        assert!(!output.exists());
        marker(&source, "baseline-a");
        crate::bundle(&source, "v1.2.4", &[42; 32], &output).unwrap();
        let signed: crate::release::SignedManifest = serde_json::from_slice(
            &fs::read(output.join("couch-v1.2.4-ha100-update.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            signed.signed.required_os_baseline.as_deref(),
            Some("baseline-a")
        );
        assert!(!signed
            .signed
            .files
            .iter()
            .any(|file| file.path == "os-baseline.json"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn baseline_is_checked_before_download_or_staging_and_again_on_activation() {
        let (root, mut m, bytes) = fixture();
        m.required_os_baseline = Some("baseline-a".into());
        // This URL cannot be downloaded. Baseline failure must happen first.
        m.url = "https://invalid.example/never".into();
        let error = stage(&root, &m, |_| panic!("must not stage")).unwrap_err();
        assert!(error.contains("compatibility marker missing"));
        assert!(!root.join("runtime").exists());
        marker(&root, "baseline-b");
        assert!(stage(&root, &m, |_| panic!("must not stage"))
            .unwrap_err()
            .contains("different full OS"));
        assert!(unpack(&root, &m, &bytes).is_err());
        assert!(!root.join("runtime").exists());
        marker(&root, "baseline-a");
        unpack(&root, &m, &bytes).unwrap();
        atomic(&root.join("runtime/staged"), m.sha256.as_bytes()).unwrap();
        marker(&root, "baseline-b");
        assert!(activate(&root).unwrap_err().contains("different full OS"));
        assert!(!root.join("runtime/current").exists());
        assert!(!root.join("runtime/pending").exists());
        fs::remove_file(root.join("os-baseline.json")).unwrap();
        assert!(activate(&root).is_err());
        marker(&root, "baseline-a");
        activate(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_symlink_and_oversized_baseline_markers_are_refused() {
        let (root, mut m, _) = fixture();
        m.required_os_baseline = Some("baseline-a".into());
        for bytes in [
            b"{}".to_vec(),
            vec![b' '; 1025],
            br#"{"schema":1,"model":"other","id":"baseline-a"}"#.to_vec(),
        ] {
            fs::write(root.join("os-baseline.json"), bytes).unwrap();
            assert!(crate::baseline::check(&root, &m).is_err());
        }
        fs::remove_file(root.join("os-baseline.json")).unwrap();
        fs::write(
            root.join("outside"),
            br#"{"schema":1,"model":"sanytron-ha100","id":"baseline-a"}"#,
        )
        .unwrap();
        symlink("outside", root.join("os-baseline.json")).unwrap();
        assert!(crate::baseline::check(&root, &m).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn staged_update_does_not_replace_current_or_configuration() {
        let (root, m, bytes) = fixture();
        fs::write(root.join("networks.conf"), b"existing settings").unwrap();
        unpack(&root, &m, &bytes).unwrap();
        assert!(!root.join("runtime/current").exists());
        atomic(&root.join("runtime/staged"), m.sha256.as_bytes()).unwrap();
        activate(&root).unwrap();
        assert_eq!(
            fs::read_link(root.join("runtime/current")).unwrap(),
            Path::new("slots").join(&m.sha256)
        );
        assert_eq!(
            fs::read_to_string(root.join("runtime/pending")).unwrap(),
            format!("base {}\n", m.sha256)
        );
        assert_eq!(
            fs::read(root.join("networks.conf")).unwrap(),
            b"existing settings"
        );
        assert!(activate(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_pointer_preparation_can_be_retried() {
        let (root, m, bytes) = fixture();
        unpack(&root, &m, &bytes).unwrap();
        atomic(&root.join("runtime/staged"), m.sha256.as_bytes()).unwrap();
        symlink("slots/abandoned", root.join("runtime/next")).unwrap();
        activate(&root).unwrap();
        assert_eq!(
            fs::read_link(root.join("runtime/current")).unwrap(),
            Path::new("slots").join(&m.sha256)
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unexpected_pointer_directory_is_preserved_without_pending_activation() {
        let (root, m, bytes) = fixture();
        unpack(&root, &m, &bytes).unwrap();
        atomic(&root.join("runtime/staged"), m.sha256.as_bytes()).unwrap();
        fs::create_dir(root.join("runtime/next")).unwrap();
        assert!(activate(&root).is_err());
        assert!(root.join("runtime/next").is_dir());
        assert!(!root.join("runtime/pending").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn modified_staged_bytes_cannot_be_activated() {
        let (root, m, bytes) = fixture();
        unpack(&root, &m, &bytes).unwrap();
        atomic(&root.join("runtime/staged"), m.sha256.as_bytes()).unwrap();
        fs::write(
            root.join("runtime/slots").join(&m.sha256).join("couch-gui"),
            b"tampered runtime",
        )
        .unwrap();
        assert!(activate(&root).is_err());
        assert!(!root.join("runtime/current").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn configuration_paths_and_incomplete_bundles_are_refused() {
        let (root, mut m, _) = fixture();
        for path in [
            "../boot.img",
            "/etc/shadow",
            "networks.conf",
            "config.json",
            "runtime/current",
            "os-baseline.json",
            "www/../../etc/shadow",
        ] {
            assert!(!allowed(path), "{path}");
        }
        m.files.pop();
        assert!(inventory(&m).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn archive_links_are_not_followed() {
        let (root, m, _) = fixture();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_link_name("/etc/shadow").unwrap();
        header.set_cksum();
        archive
            .append_data(&mut header, "couch-gui", &[][..])
            .unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        assert!(unpack(&root, &m, &bytes).is_err());
        assert!(!root.join("runtime/current").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
