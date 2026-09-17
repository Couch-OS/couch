//! Assembles the Android restore image set from retained, independently verified
//! originals and a fresh stock F2FS userdata image. Nothing here opens a device or
//! authorizes a write: it only re-verifies pinned inputs the caller already admitted.
//!
//! recovery, logo, odmdtbo and boot are the device's own saved Android originals
//! (pinned by the imported enrollment record and re-hashed here). userdata is a
//! full-partition F2FS image generated offline by the firmware's own make_f2fs;
//! it has no reproducible content hash, so it is verified structurally (both F2FS
//! superblocks) and against its owner-side build receipt, never a pinned image hash.
use crate::saved_enrollment::Record;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const PIN: &str = include_str!("../../pins/ha100_firmware_restore.json");
/// Images taken verbatim from the retained Android originals, in no particular order.
const ORIGINALS: [&str; 4] = ["recovery", "logo", "odmdtbo", "boot"];
/// Android sparse image magic (little-endian 0xed26ff3a); never a raw restore image.
const SPARSE_MAGIC: [u8; 4] = [0x3a, 0xff, 0x26, 0xed];
/// F2FS superblock magic, little-endian 0xf2f52010, at byte offsets 1024 and 5120.
const F2FS_MAGIC: [u8; 4] = [0x10, 0x20, 0xf5, 0xf2];
const F2FS_SUPERBLOCKS: [u64; 2] = [1024, 5120];

#[derive(Deserialize)]
struct FirmwarePin {
    archive: Blob,
    formatter: Formatter,
    userdata: UserdataPin,
}
#[derive(Deserialize)]
struct Blob {
    sha256: String,
}
#[derive(Deserialize)]
struct Formatter {
    sha256: String,
}
#[derive(Deserialize)]
struct UserdataPin {
    filesystem: String,
    partition_size: u64,
    reproducible_hash: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StockUserdataReceipt {
    schema: u32,
    kind: String,
    model: String,
    filesystem: String,
    size: u64,
    sha256: String,
    formatter_sha256: String,
    source_archive_sha256: String,
    reproducible: bool,
}

fn pin() -> Result<FirmwarePin> {
    Ok(serde_json::from_str(PIN)?)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(&hash.finalize()))
}
fn read_at(file: &mut File, offset: u64, n: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; n];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Structurally confirm a full-partition stock F2FS userdata image. No content
/// hash is pinned because make_f2fs writes fresh UUID/time fields; the caller
/// still pins the exact bytes through its own receipt (see `verify_stock_userdata`).
pub fn check_f2fs(path: &Path, partition_size: u64) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "stock userdata must be a regular file"
    );
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == partition_size,
        "stock userdata must cover the exact userdata partition (raw full write)"
    );
    let head = read_at(&mut file, 0, 4)?;
    ensure!(
        head != SPARSE_MAGIC,
        "stock userdata is an Android sparse image; expand it to a raw image first"
    );
    // ext4 superblock magic 0x53ef at offset 1024+56 would mean a Couch ext4 image.
    let ext4 = read_at(&mut file, 1024 + 56, 2)?;
    ensure!(
        ext4 != [0x53, 0xef],
        "stock userdata is an ext4 filesystem; a Couch image is not Android userdata"
    );
    for offset in F2FS_SUPERBLOCKS {
        ensure!(
            read_at(&mut file, offset, 4)? == F2FS_MAGIC,
            "stock userdata is missing an F2FS superblock"
        );
    }
    Ok(())
}

/// Verify a prepared stock userdata image against its owner-side receipt and the
/// repo firmware pin. The receipt records the exact bytes and the formatter that
/// produced them; the firmware pin fixes the formatter and source archive.
pub fn verify_stock_userdata(image: &Path, receipt: &Path) -> Result<u64> {
    let pin = pin()?;
    verify_stock_userdata_with(
        image,
        receipt,
        pin.userdata.filesystem == "f2fs" && !pin.userdata.reproducible_hash,
        pin.userdata.partition_size,
        &pin.formatter.sha256,
        &pin.archive.sha256,
    )
}
fn verify_stock_userdata_with(
    image: &Path,
    receipt: &Path,
    fresh_f2fs_pin: bool,
    partition_size: u64,
    formatter_sha256: &str,
    archive_sha256: &str,
) -> Result<u64> {
    ensure!(
        fresh_f2fs_pin,
        "firmware pin no longer describes a fresh F2FS userdata"
    );
    ensure!(
        std::fs::symlink_metadata(receipt)?.file_type().is_file()
            && std::fs::metadata(receipt)?.len() <= 65536,
        "invalid stock userdata receipt"
    );
    let receipt: StockUserdataReceipt = serde_json::from_slice(&std::fs::read(receipt)?)?;
    ensure!(
        receipt.schema == 1
            && receipt.kind == "couch-stock-userdata"
            && receipt.model == "sanytron-ha100"
            && receipt.filesystem == "f2fs"
            && !receipt.reproducible,
        "not a reviewed stock userdata receipt"
    );
    ensure!(
        receipt.size == partition_size,
        "stock userdata receipt size differs from the pinned partition size"
    );
    ensure!(
        receipt.formatter_sha256 == formatter_sha256,
        "stock userdata was not made with the pinned make_f2fs formatter"
    );
    ensure!(
        receipt.source_archive_sha256 == archive_sha256,
        "stock userdata receipt references a different firmware archive"
    );
    check_f2fs(image, partition_size)?;
    ensure!(
        digest(image)? == receipt.sha256,
        "stock userdata image bytes differ from its receipt"
    );
    Ok(partition_size)
}

/// Build the restore image paths. recovery/logo/odmdtbo/boot come from the retained
/// Android originals (re-hashed against the enrollment record); userdata is the
/// verified fresh F2FS image. Refuses anything that is not an Android enrollment.
pub fn assemble(
    directory: &Path,
    record: &Record,
    stock_userdata: &Path,
    stock_receipt: &Path,
) -> Result<BTreeMap<String, PathBuf>> {
    let pin = pin()?;
    assemble_with(
        directory,
        record,
        stock_userdata,
        stock_receipt,
        verify_stock_userdata_with(
            stock_userdata,
            stock_receipt,
            pin.userdata.filesystem == "f2fs" && !pin.userdata.reproducible_hash,
            pin.userdata.partition_size,
            &pin.formatter.sha256,
            &pin.archive.sha256,
        )?,
    )
}
fn assemble_with(
    directory: &Path,
    record: &Record,
    stock_userdata: &Path,
    _stock_receipt: &Path,
    partition_size: u64,
) -> Result<BTreeMap<String, PathBuf>> {
    ensure!(
        record.original_os == "Android",
        "refusing to present non-Android originals as an Android restore"
    );
    ensure!(
        record
            .partitions
            .get("userdata")
            .context("enrollment lacks a userdata partition")?
            .size
            == partition_size,
        "enrolled userdata partition differs from the firmware restore size"
    );
    let mut paths = BTreeMap::new();
    for name in ORIGINALS {
        let entry = record
            .originals
            .get(name)
            .with_context(|| format!("retained Android {name} original is missing"))?;
        let path = directory.join(&entry.file);
        ensure!(
            std::fs::symlink_metadata(&path)?.file_type().is_file()
                && std::fs::metadata(&path)?.len() == entry.size,
            "retained Android original changed size"
        );
        ensure!(
            digest(&path)? == entry.sha256,
            "retained Android original failed its pinned hash"
        );
        paths.insert(name.to_string(), path);
    }
    // Android boot carries the ANDROID! header; never write a Couch boot here.
    let mut boot = File::open(&paths["boot"])?;
    ensure!(
        read_at(&mut boot, 0, 8)? == *b"ANDROID!",
        "retained boot original is not an Android boot image"
    );
    paths.insert("userdata".into(), stock_userdata.to_path_buf());
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::saved_enrollment::{Original, Region};
    use serde_json::json;

    // A small stand-in partition size keeps these fixtures off a real 5.9 GB image.
    const SIZE: u64 = 16 * 1024;
    const FORMATTER: &str = "aa";
    const ARCHIVE: &str = "bb";

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }
    fn f2fs(size: u64) -> Vec<u8> {
        let mut data = vec![0u8; size as usize];
        for offset in F2FS_SUPERBLOCKS {
            data[offset as usize..offset as usize + 4].copy_from_slice(&F2FS_MAGIC);
        }
        data
    }
    fn receipt_for(data: &[u8]) -> serde_json::Value {
        json!({
            "schema":1,"kind":"couch-stock-userdata","model":"sanytron-ha100",
            "filesystem":"f2fs","size":SIZE,
            "sha256":hex(&Sha256::digest(data)),
            "formatter_sha256":FORMATTER.repeat(32),
            "source_archive_sha256":ARCHIVE.repeat(32),"reproducible":false
        })
    }
    fn verify(image: &Path, receipt: &Path) -> Result<u64> {
        verify_stock_userdata_with(
            image,
            receipt,
            true,
            SIZE,
            &FORMATTER.repeat(32),
            &ARCHIVE.repeat(32),
        )
    }

    #[test]
    fn repo_pin_describes_a_fresh_full_partition_f2fs_userdata() {
        let p = pin().unwrap();
        assert_eq!(p.userdata.filesystem, "f2fs");
        assert!(!p.userdata.reproducible_hash);
        assert_eq!(p.userdata.partition_size, 5_905_055_744);
    }

    #[test]
    fn f2fs_check_rejects_sparse_ext4_and_wrong_size() {
        let size = SIZE;
        let root = tempfile::tempdir().unwrap();
        let good = root.path().join("good");
        write(&good, &f2fs(size));
        check_f2fs(&good, size).unwrap();

        let wrong_size = root.path().join("wrong");
        write(&wrong_size, &f2fs(size - 4096));
        assert!(check_f2fs(&wrong_size, size).is_err());

        let mut sparse = f2fs(size);
        sparse[..4].copy_from_slice(&SPARSE_MAGIC);
        let sparse_path = root.path().join("sparse");
        write(&sparse_path, &sparse);
        assert!(check_f2fs(&sparse_path, size).is_err());

        let mut ext4 = vec![0u8; size as usize];
        ext4[1024 + 56..1024 + 58].copy_from_slice(&[0x53, 0xef]);
        let ext4_path = root.path().join("ext4");
        write(&ext4_path, &ext4);
        assert!(check_f2fs(&ext4_path, size).is_err());
    }

    #[test]
    fn stock_userdata_receipt_pins_formatter_archive_and_bytes() {
        let data = f2fs(SIZE);
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("userdata.img");
        write(&image, &data);
        let receipt = root.path().join("receipt.json");
        write(&receipt, receipt_for(&data).to_string().as_bytes());
        assert_eq!(verify(&image, &receipt).unwrap(), SIZE);

        let mut bad = receipt_for(&data);
        bad["formatter_sha256"] = json!("0".repeat(64));
        let bad_receipt = root.path().join("bad.json");
        write(&bad_receipt, bad.to_string().as_bytes());
        assert!(verify(&image, &bad_receipt).is_err());

        let tampered = root.path().join("tampered.img");
        let mut altered = data.clone();
        altered[10_000] ^= 1;
        write(&tampered, &altered);
        assert!(verify(&tampered, &receipt).is_err());
    }

    fn record(original_os: &str) -> (tempfile::TempDir, Record) {
        let size = SIZE;
        let root = tempfile::tempdir().unwrap();
        let mut originals = BTreeMap::new();
        let mut partitions = BTreeMap::new();
        partitions.insert("userdata".into(), Region { offset: 4096, size });
        for (name, magic) in [
            ("recovery", &b"RECOVERY"[..]),
            ("logo", &b"LOGO\0\0\0\0"[..]),
            ("odmdtbo", &b"\xd0\x0d\xfe\xed\0\0\0\0"[..]),
            ("boot", &b"ANDROID!"[..]),
        ] {
            let data = magic.to_vec();
            let file = format!("bootstrap-{name}.img");
            write(&root.path().join(&file), &data);
            originals.insert(
                name.to_string(),
                Original {
                    file,
                    size: data.len() as u64,
                    sha256: hex(&Sha256::digest(&data)),
                },
            );
            partitions.insert(
                name.into(),
                Region {
                    offset: 0,
                    size: data.len() as u64,
                },
            );
        }
        let rec = Record {
            schema: 1,
            kind: "couch-device-enrollment".into(),
            model: "sanytron-ha100".into(),
            cid: "12".repeat(16),
            capacity: size + 4096,
            partitions,
            identity_sha256: BTreeMap::new(),
            android_identity: None,
            original_os: original_os.into(),
            originals,
        };
        (root, rec)
    }

    #[test]
    fn assemble_gathers_android_originals_and_fresh_userdata() {
        let (root, rec) = record("Android");
        let image = root.path().join("userdata.img");
        write(&image, &f2fs(SIZE));
        let paths = assemble_with(root.path(), &rec, &image, Path::new("unused"), SIZE).unwrap();
        let mut names: Vec<_> = paths.keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["boot", "logo", "odmdtbo", "recovery", "userdata"]);
        assert_eq!(paths["userdata"], image);
    }

    #[test]
    fn assemble_refuses_couch_originals_as_android() {
        let (root, rec) = record("Couch");
        let image = root.path().join("userdata.img");
        write(&image, &f2fs(SIZE));
        assert!(assemble_with(root.path(), &rec, &image, Path::new("unused"), SIZE).is_err());
    }

    #[test]
    fn assemble_rejects_enrolled_userdata_size_mismatch() {
        let (root, mut rec) = record("Android");
        rec.partitions.get_mut("userdata").unwrap().size = SIZE + 4096;
        let image = root.path().join("userdata.img");
        write(&image, &f2fs(SIZE));
        assert!(assemble_with(root.path(), &rec, &image, Path::new("unused"), SIZE).is_err());
    }
}
