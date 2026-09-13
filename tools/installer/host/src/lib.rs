//! Native input preparation and supervised installer orchestration.
pub mod adapter;
pub mod android;
pub mod android_images;
pub mod android_restore;
pub mod assembly;
pub mod dependencies;
pub mod enrollment;
pub mod enrollment_sources;
pub mod frontend;
pub mod network;
pub mod orchestrator;
pub mod public_inputs;
pub mod saved_enrollment;
pub mod session;
pub mod space;
pub mod stage;
pub mod stage_files;
pub mod stage_tls;
pub mod transaction;
pub mod vendor_transfer;
pub mod wifi_debug;
pub mod wifi_debug_transition;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use zip::ZipArchive;
pub(crate) const BOOTSTRAP: [(&str, u64, &str); 4] = [
    (
        "preloader.img",
        109992,
        "0ad0d14b7203d98a6567af7a022cfe5df5b6fcbba60cb4e9b4bc2ee569cf1069",
    ),
    (
        "boot.img",
        8030464,
        "dda78c8ebe7cb82095b08a10c2a1f779cbdbebc53464aee34c85bb3a7382cad7",
    ),
    (
        "odmdtbo.img",
        37120,
        "a5cf1159f6e8c0a95bd1d3b8c1edaca3b2c9704642df50912dfe52df277f0575",
    ),
    (
        "scatter.txt",
        455,
        "531f0807ea065bed1c97a8a7d06284fa521749eadf3a6c0b149f0f2c72f8015d",
    ),
];
const BLOCK: u64 = 4096;
const MAX_IMAGE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 32 * 1024 * 1024;
const PIN: &str = include_str!("../../../release/ha100_official_runtime.json");
#[derive(Clone, Deserialize)]
struct Blob {
    size: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct RuntimeFile {
    path: String,
    size: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct Pin {
    size: u64,
    sha256: String,
    version: String,
    members: BTreeMap<String, Blob>,
    images: BTreeMap<String, Blob>,
    files: Vec<RuntimeFile>,
}

fn regular(path: &Path) -> Result<File> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "expected regular input"
    );
    Ok(File::open(path)?)
}
fn hash_file(path: &Path) -> Result<String> {
    hash_open_file(&mut regular(path)?)
}
fn hash_open_file(input: &mut File) -> Result<String> {
    input.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut data = [0; 65536];
    loop {
        let n = input.read(&mut data)?;
        if n == 0 {
            break;
        }
        hash.update(&data[..n]);
    }
    input.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", hash.finalize()))
}
fn create(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn member(
    archive: &mut ZipArchive<File>,
    name: &str,
    expected: &Blob,
    output: &Path,
) -> Result<()> {
    let mut item = archive.by_name(name)?;
    ensure!(
        item.size() == expected.size && !item.is_dir(),
        "member size/type mismatch"
    );
    ensure!(
        item.unix_mode()
            .map(|m| m & 0o170000 != 0o120000)
            .unwrap_or(true),
        "member symlink"
    );
    let mut file = create(output)?;
    let mut hash = Sha256::new();
    let mut done = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = item.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        ensure!(done <= expected.size, "member overflow");
        file.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    ensure!(
        done == expected.size && format!("{:x}", hash.finalize()) == expected.sha256,
        "member hash mismatch"
    );
    file.sync_all()?;
    Ok(())
}
fn ranges(text: &str, size: u64) -> Result<Vec<(u64, u64)>> {
    ensure!(
        text.len() <= 128 * 1024 && size > 0 && size <= MAX_IMAGE && size.is_multiple_of(BLOCK),
        "transfer size limit"
    );
    let lines: Vec<_> = text.lines().collect();
    ensure!(
        lines.len() >= 5 && lines[0] == "4" && lines[2..4] == ["0", "0"],
        "incremental transfer unsupported"
    );
    let expected: u64 = lines[1].parse()?;
    let mut total = 0u64;
    let mut all = Vec::new();
    let mut new = Vec::new();
    for line in &lines[4..] {
        let words: Vec<_> = line.split_whitespace().collect();
        ensure!(
            words.len() == 2 && ["new", "zero", "erase"].contains(&words[0]),
            "unsupported transfer command"
        );
        let values: Vec<u64> = words[1]
            .split(',')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            !values.is_empty()
                && values[0] > 0
                && values[0].is_multiple_of(2)
                && values[0] as usize == values.len() - 1,
            "range count mismatch"
        );
        for pair in values[1..].as_chunks::<2>().0 {
            let (start, end) = (pair[0], pair[1]);
            ensure!(start < end && end <= size / BLOCK, "range outside image");
            all.push((start, end));
            if words[0] != "erase" {
                total = total
                    .checked_add(end - start)
                    .context("block count overflow")?;
            }
            if words[0] == "new" {
                new.push((start * BLOCK, (end - start) * BLOCK));
            }
        }
    }
    all.sort_unstable();
    ensure!(
        all.windows(2).all(|p| p[0].1 <= p[1].0),
        "overlapping transfer ranges"
    );
    ensure!(!new.is_empty() && total == expected, "block count mismatch");
    Ok(new)
}
fn reconstruct(
    archive: &mut ZipArchive<File>,
    role: &str,
    pin: &Pin,
    scratch: &Path,
) -> Result<std::path::PathBuf> {
    let transfer = format!("{role}.transfer.list");
    let compressed = format!("{role}.new.dat.br");
    let transfer_path = scratch.join(&transfer);
    let compressed_path = scratch.join(&compressed);
    member(archive, &transfer, &pin.members[&transfer], &transfer_path)?;
    member(
        archive,
        &compressed,
        &pin.members[&compressed],
        &compressed_path,
    )?;
    let expected = &pin.images[role];
    let ranges = ranges(&fs::read_to_string(transfer_path)?, expected.size)?;
    let image = scratch.join(format!("{role}.img"));
    let mut output = create(&image)?;
    output.set_len(expected.size)?;
    let mut decoder = brotli::Decompressor::new(regular(&compressed_path)?, 65536);
    let mut buffer = [0; 65536];
    for (offset, mut size) in ranges {
        output.seek(SeekFrom::Start(offset))?;
        while size > 0 {
            let count = size.min(buffer.len() as u64) as usize;
            decoder
                .read_exact(&mut buffer[..count])
                .context("truncated OTA payload")?;
            output.write_all(&buffer[..count])?;
            size -= count as u64;
        }
    }
    ensure!(
        decoder.read(&mut [0; 1])? == 0,
        "expanded OTA payload exceeds transfer ranges"
    );
    output.sync_all()?;
    drop(output);
    ensure!(
        hash_file(&image)? == expected.sha256,
        "reconstructed filesystem hash mismatch"
    );
    Ok(image)
}

/// Uses only compile-time approved archive/member/file pins. Destination must be new.
pub fn prepare(ota: &Path, destination: &Path) -> Result<()> {
    ensure!(
        !destination.try_exists()? && fs::symlink_metadata(destination).is_err(),
        "output already exists"
    );
    let pin: Pin = serde_json::from_str(PIN)?;
    let mut input = regular(ota)?;
    ensure!(
        input.metadata()?.len() == pin.size && hash_open_file(&mut input)? == pin.sha256,
        "official archive pin mismatch"
    );
    let parent = destination.parent().context("output needs a parent")?;
    fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    ensure!(
        canonical_parent
            .ancestors()
            .all(|p| !p.join(".git").exists()),
        "keep owner vendor inputs outside Git"
    );
    let scratch = tempfile::Builder::new()
        .prefix(".couch-native-inputs-")
        .tempdir_in(parent)?;
    let staged = scratch.path().join("inputs");
    private_dir(&staged)?;
    let bootstrap = staged.join("bootstrap");
    private_dir(&bootstrap)?;
    let mut archive = ZipArchive::new(input)?;
    let names: std::collections::BTreeSet<_> = archive.file_names().map(str::to_owned).collect();
    ensure!(names.len() == archive.len(), "duplicate archive members");
    let mut bootstrap_pins = BTreeMap::new();
    for (name, size, sha) in BOOTSTRAP {
        member(
            &mut archive,
            name,
            &Blob {
                size,
                sha256: sha.into(),
            },
            &bootstrap.join(name),
        )?;
        bootstrap_pins.insert(name, json!({"size":size,"sha256":sha}));
    }
    let vendor = staged.join("vendor");
    private_dir(&vendor)?;
    let mut filesystems = BTreeMap::new();
    for role in ["system", "vendor"] {
        let image = reconstruct(&mut archive, role, &pin, scratch.path())?;
        filesystems.insert(role, ext4_view::Ext4::load_from_path(&image)?);
    }
    let mut records = Vec::new();
    for record in &pin.files {
        ensure!(
            record.size > 0 && record.size <= MAX_FILE,
            "runtime file size limit"
        );
        let (role, relative) = record
            .path
            .split_once('/')
            .context("invalid pinned runtime path")?;
        ensure!(
            !relative
                .split('/')
                .any(|s| s.is_empty() || s == ".." || s == "."),
            "invalid pinned path"
        );
        let path = format!("/{relative}");
        let fs = filesystems.get(role).context("invalid runtime role")?;
        ensure!(
            fs.metadata(path.as_str())?.len() == record.size,
            "runtime metadata size mismatch"
        );
        let data = fs.read(path.as_str())?;
        ensure!(
            data.len() as u64 == record.size
                && format!("{:x}", Sha256::digest(&data)) == record.sha256,
            "runtime file hash mismatch"
        );
        let target = vendor.join(&record.path);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut output = create(&target)?;
        output.write_all(&data)?;
        output.sync_all()?;
        records.push(json!({"path":record.path,"size":record.size,"sha256":record.sha256,"source_image":format!("{role}.img"),"source_path":path,"operation":"native Rust pinned official OTA extraction","redistribution":"unreviewed; private use only"}));
    }
    let images: BTreeMap<_, _> = pin
        .images
        .iter()
        .map(|(k, v)| (k, json!({"size":v.size,"sha256":v.sha256})))
        .collect();
    let provenance = json!({"schema":1,"kind":"couch-private-vendor-bundle","installable":false,"private_only":true,"redistribution_authorized":false,"build_attested":false,"source_images":images,"source_origin":"User-supplied pinned official OTA; native offline extraction","source_archive":{"sha256":pin.sha256,"size":pin.size,"version":pin.version},"extractor":{"name":"couch-installer-host","mode":"read-only Rust ext4 parser; no mounts or subprocesses"},"files":records,"missing":[]});
    write_json(&vendor.join("vendor-provenance.json"), &provenance)?;
    write_json(
        &staged.join("inputs.json"),
        &json!({"schema":1,"kind":"owner-official-install-inputs","source_archive_sha256":pin.sha256,"version":pin.version,"bootstrap_files":bootstrap_pins,"vendor_runtime_prepared":true,"private_only":true,"redistribution_authorized":false,"installable":false,"original_device_backup":false,"preloader_usage":"EMI input only; never a flash target"}),
    )?;
    ensure!(
        !destination.try_exists()?,
        "output appeared during preparation"
    );
    fs::rename(&staged, destination)?;
    Ok(())
}
fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut file = create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_maps_only_declared_new_data() {
        assert_eq!(
            ranges(
                "4\n3\n0\n0\nnew 2,0,2\nzero 2,2,3\nerase 2,3,4\n",
                4 * BLOCK
            )
            .unwrap(),
            vec![(0, 2 * BLOCK)]
        );
    }
    #[test]
    fn transfer_rejects_overlap_stash_bounds_and_count() {
        for text in [
            "4\n3\n0\n0\nnew 2,0,2\nzero 2,1,2",
            "4\n1\n0\n0\nstash 2,0,1",
            "4\n2\n0\n0\nnew 2,0,9",
            "4\n4\n0\n0\nnew 2,0,2",
        ] {
            assert!(ranges(text, 4 * BLOCK).is_err());
        }
    }
    #[test]
    fn public_pin_contains_exact_runtime_inventory() {
        let pin: Pin = serde_json::from_str(PIN).unwrap();
        assert_eq!(pin.files.len(), 33);
        let names: std::collections::BTreeSet<_> = pin.files.iter().map(|p| &p.path).collect();
        assert_eq!(names.len(), 33);
    }
    #[test]
    fn wrong_archive_never_creates_output() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("bad.zip");
        fs::write(&p, b"bad").unwrap();
        let out = t.path().join("out");
        assert!(prepare(&p, &out).is_err());
        assert!(!out.exists());
    }
    fn reconstruction_fixture(payload: &[u8]) -> (tempfile::TempDir, ZipArchive<File>, Pin) {
        let root = tempfile::tempdir().unwrap();
        let mut compressed = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut compressed, 4096, 3, 20);
            encoder.write_all(payload).unwrap();
        }
        let transfer = b"4\n2\n0\n0\nnew 2,0,1\nzero 2,1,2\n";
        let path = root.path().join("ota.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut members = BTreeMap::new();
        for (name, data) in [
            ("system.transfer.list", transfer.as_slice()),
            ("system.new.dat.br", compressed.as_slice()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data).unwrap();
            members.insert(
                name.into(),
                Blob {
                    size: data.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(data)),
                },
            );
        }
        zip.finish().unwrap();
        let mut expected = vec![7; BLOCK as usize];
        expected.resize((2 * BLOCK) as usize, 0);
        let pin = Pin {
            size: 0,
            sha256: String::new(),
            version: String::new(),
            members,
            images: BTreeMap::from([(
                "system".into(),
                Blob {
                    size: 2 * BLOCK,
                    sha256: format!("{:x}", Sha256::digest(&expected)),
                },
            )]),
            files: vec![],
        };
        let archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        (root, archive, pin)
    }
    #[test]
    fn bounded_brotli_reconstruction_preserves_zero_ranges() {
        let (root, mut archive, pin) = reconstruction_fixture(&vec![7; BLOCK as usize]);
        let image = reconstruct(&mut archive, "system", &pin, root.path()).unwrap();
        let data = fs::read(image).unwrap();
        assert_eq!(&data[..BLOCK as usize], &vec![7; BLOCK as usize]);
        assert!(data[BLOCK as usize..].iter().all(|b| *b == 0));
    }
    #[test]
    fn truncated_and_expanding_brotli_fail_before_input_publication() {
        for size in [BLOCK - 1, BLOCK + 1] {
            let (root, mut archive, pin) = reconstruction_fixture(&vec![7; size as usize]);
            assert!(reconstruct(&mut archive, "system", &pin, root.path()).is_err());
        }
    }
    #[test]
    fn changed_compressed_member_is_rejected_before_filesystem_parse() {
        let (root, mut archive, mut pin) = reconstruction_fixture(&vec![7; BLOCK as usize]);
        pin.members.get_mut("system.new.dat.br").unwrap().sha256 = "0".repeat(64);
        assert!(reconstruct(&mut archive, "system", &pin, root.path()).is_err());
        assert!(!root.path().join("system.img").exists());
    }
}
