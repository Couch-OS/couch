//! Signed boot payloads: the source-built kernel and boot ramdisk, spliced into
//! the remote's own Android boot image and written back to the boot partition.
//!
//! The payload carries no Android header and no device tree. Both come from the
//! image already on the partition, the way the installer assembles a boot image
//! from the owner's original on the host: the header page keeps lk's load
//! addresses and command line, the appended DTB stays the device's own, and only
//! the zImage and ramdisk change. The previous partition contents are saved
//! under `boot/previous.img` before the write, and the write is read back from
//! the medium before the update counts as applied.
use crate::{
    release::{self, File, Manifest},
    staging, Result,
};
use sha1::Sha1;
use sha2::Digest;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

/// The HA100 boot partition (`boot`, reference node p8).
pub const DEVICE: &str = "/dev/mmcblk0p8";
/// The partition is 16 MiB; the whole of it is read, rewritten and read back.
pub(crate) const PARTITION: usize = 16 * 1024 * 1024;
const ZIMAGE: &str = "zImage";
const RAMDISK: &str = "boot.cpio.gz";
const ZIMAGE_LIMIT: u64 = 12 * 1024 * 1024;
const RAMDISK_LIMIT: u64 = 4 * 1024 * 1024;
const ZIMAGE_MAGIC: [u8; 4] = [0x18, 0x28, 0x6f, 0x01];
const FDT_MAGIC: [u8; 4] = [0xd0, 0x0d, 0xfe, 0xed];

pub(crate) fn inventory(m: &Manifest) -> Result<BTreeMap<String, &File>> {
    if m.kind != "boot" || !m.installable || m.files.len() != 2 {
        return Err("Unsupported update type".into());
    }
    let mut files = BTreeMap::new();
    for f in &m.files {
        let limit = match f.path.as_str() {
            ZIMAGE => ZIMAGE_LIMIT,
            RAMDISK => RAMDISK_LIMIT,
            _ => return Err("Unexpected boot payload file".into()),
        };
        if f.mode != 0o644
            || f.size == 0
            || f.size > limit
            || !staging::id(&f.sha256)
            || files.insert(f.path.clone(), f).is_some()
        {
            return Err("Invalid or duplicate boot payload file".into());
        }
    }
    if files.len() != 2 {
        return Err("Incomplete boot payload".into());
    }
    Ok(files)
}

/// Download, verify and unpack a boot payload under `boot/slots/<sha256>`.
pub(crate) fn stage(root: &Path, m: &Manifest, phase: impl Fn(&str)) -> Result<()> {
    crate::baseline::check(root, m)?;
    inventory(m)?;
    let bytes = release::fetch(&m.url, m.size)?;
    stage_bytes(root, m, &bytes, phase)
}

fn stage_bytes(root: &Path, m: &Manifest, bytes: &[u8], phase: impl Fn(&str)) -> Result<()> {
    crate::baseline::check(root, m)?;
    let files = inventory(m)?;
    if bytes.len() as u64 != m.size || release::digest(bytes) != m.sha256 {
        return Err("Update download digest mismatch".into());
    }
    phase("verifying");
    let slots = root.join("boot/slots");
    fs::create_dir_all(&slots).map_err(|_| "Could not create boot slots")?;
    fs::set_permissions(&slots, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Could not secure boot slots")?;
    let target = slots.join(&m.sha256);
    if target.exists() {
        staging::verify_files(&target, &files)?;
    } else {
        let work = slots.join(format!(".staging-{}", std::process::id()));
        if work.exists() {
            fs::remove_dir_all(&work).map_err(|_| "Could not clean interrupted staging")?;
        }
        fs::create_dir(&work).map_err(|_| "Could not create staging directory")?;
        let result = (|| -> Result<()> {
            staging::extract(&work, &files, bytes)?;
            let zimage = fs::read(work.join(ZIMAGE)).map_err(|_| "Could not read staged kernel")?;
            let ramdisk =
                fs::read(work.join(RAMDISK)).map_err(|_| "Could not read staged ramdisk")?;
            check_payload(&zimage, &ramdisk)?;
            staging::atomic(
                &work.join(".manifest.json"),
                &serde_json::to_vec(m).map_err(|_| "Invalid update manifest")?,
            )?;
            fs::rename(&work, &target).map_err(|_| "Could not finalize boot slot")?;
            fs::File::open(&slots)
                .and_then(|f| f.sync_all())
                .map_err(|_| "Could not flush boot slots".into())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&work);
        }
        result?;
    }
    // Identical payload bytes may be published under several release versions.
    // Refresh the signed selection metadata even when the slot is reused.
    staging::atomic(
        &target.join(".manifest.json"),
        &serde_json::to_vec(m).unwrap(),
    )?;
    staging::atomic(&root.join("boot/staged"), m.sha256.as_bytes())
}

pub(crate) fn check_payload(zimage: &[u8], ramdisk: &[u8]) -> Result<()> {
    if zimage.len() < 48 || zimage[36..40] != ZIMAGE_MAGIC {
        return Err("Boot payload kernel is not an ARM zImage".into());
    }
    if ramdisk.len() < 18 || ramdisk[..2] != [0x1f, 0x8b] {
        return Err("Boot payload ramdisk is not gzip".into());
    }
    Ok(())
}

struct Image<'a> {
    page: usize,
    kernel: &'a [u8],
    ramdisk: &'a [u8],
}
fn aligned(value: usize, page: usize) -> Result<usize> {
    value
        .checked_add(page - 1)
        .map(|n| n / page * page)
        .ok_or_else(|| "Boot image offset overflow".into())
}
fn parse(image: &[u8]) -> Result<Image<'_>> {
    if image.len() < 2048 || &image[..8] != b"ANDROID!" {
        return Err("The boot partition does not hold an Android boot image".into());
    }
    let le = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as usize;
    let (kernel_size, ramdisk_size, second, page, version) =
        (le(8), le(16), le(24), le(36), le(40));
    if ![2048, 4096, 8192, 16384].contains(&page) || second != 0 || version != 0 || kernel_size == 0
    {
        return Err("Unsupported boot image layout".into());
    }
    let kernel_end = page
        .checked_add(kernel_size)
        .ok_or("Boot image offset overflow")?;
    let ramdisk_at = aligned(kernel_end, page)?;
    let ramdisk_end = ramdisk_at
        .checked_add(ramdisk_size)
        .ok_or("Boot image offset overflow")?;
    if ramdisk_end > image.len() || ramdisk_size == 0 {
        return Err("Truncated boot image".into());
    }
    Ok(Image {
        page,
        kernel: &image[page..kernel_end],
        ramdisk: &image[ramdisk_at..ramdisk_end],
    })
}
/// Where the validated appended device tree starts inside the kernel blob. The
/// same checks the installer applies to the owner's original: the flattened
/// tree must end exactly at the kernel boundary and close with FDT_END.
fn dtb_offset(kernel: &[u8]) -> Result<usize> {
    let mut from = 0;
    while let Some(found) = kernel
        .get(from..)
        .and_then(|rest| rest.windows(4).position(|w| w == FDT_MAGIC))
    {
        let offset = from + found;
        from = offset + 1;
        let bytes = &kernel[offset..];
        if bytes.len() < 40 {
            break;
        }
        let word = |n: usize| u32::from_be_bytes(bytes[n..n + 4].try_into().unwrap()) as usize;
        let (size, structure, strings, reserve) = (word(4), word(8), word(12), word(16));
        let (string_size, struct_size) = (word(32), word(36));
        if size == bytes.len()
            && word(20) >= 17
            && word(24) <= 17
            && reserve >= 40
            && reserve < size
            && structure >= 40
            && strings >= 40
            && struct_size >= 4
            && structure
                .checked_add(struct_size)
                .is_some_and(|n| n <= size)
            && strings.checked_add(string_size).is_some_and(|n| n <= size)
            && bytes[structure + struct_size - 4..structure + struct_size] == [0, 0, 0, 9]
        {
            return Ok(offset);
        }
    }
    Err("The installed kernel has no validated appended device tree".into())
}
fn split(kernel: &[u8]) -> Result<(&[u8], &[u8])> {
    Ok(kernel.split_at(dtb_offset(kernel)?))
}
/// Whether the partition already carries exactly this payload's kernel and ramdisk.
pub(crate) fn installed(device: &Path, m: &Manifest) -> Result<bool> {
    let files = inventory(m)?;
    let image = read_partition(device)?;
    let parsed = parse(&image)?;
    let (zimage, _) = split(parsed.kernel)?;
    Ok(release::digest(zimage) == files[ZIMAGE].sha256
        && release::digest(parsed.ramdisk) == files[RAMDISK].sha256)
}
fn read_partition(device: &Path) -> Result<Vec<u8>> {
    let mut input = fs::File::open(device).map_err(|_| "Could not open the boot partition")?;
    let mut image = Vec::new();
    Read::by_ref(&mut input)
        .take(PARTITION as u64)
        .read_to_end(&mut image)
        .map_err(|_| "Could not read the boot partition")?;
    if image.len() != PARTITION {
        return Err("The boot partition is not the expected size".into());
    }
    Ok(image)
}
/// A new image with this kernel and ramdisk in the current image's header and
/// with its device tree, padded to the whole partition.
pub(crate) fn repack(current: &[u8], zimage: &[u8], ramdisk: &[u8]) -> Result<Vec<u8>> {
    check_payload(zimage, ramdisk)?;
    let parsed = parse(current)?;
    let page = parsed.page;
    let (_, dtb) = split(parsed.kernel)?;
    let mut kernel = zimage.to_vec();
    kernel.extend_from_slice(dtb);
    let mut output = current[..page].to_vec();
    output[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
    output[16..20].copy_from_slice(&(ramdisk.len() as u32).to_le_bytes());
    let mut digest = Sha1::new();
    for part in [&kernel[..], ramdisk, &[][..]] {
        digest.update(part);
        digest.update((part.len() as u32).to_le_bytes());
    }
    output[576..596].copy_from_slice(&digest.finalize());
    output[596..608].fill(0);
    output.extend_from_slice(&kernel);
    output.resize(aligned(output.len(), page)?, 0);
    output.extend_from_slice(ramdisk);
    if aligned(output.len(), page)? > PARTITION {
        return Err("The assembled boot image exceeds the boot partition".into());
    }
    output.resize(PARTITION, 0);
    Ok(output)
}
/// Write the staged payload to the boot partition. The caller reboots on Ok.
pub fn activate(root: &Path, device: &Path) -> Result<()> {
    activate_staged(root, device, true)
}

pub(crate) fn validate_staged(root: &Path) -> Result<Manifest> {
    let selected = fs::read_to_string(root.join("boot/staged")).map_err(|_| "No staged update")?;
    if !staging::id(&selected) {
        return Err("Invalid staged update identifier".into());
    }
    let slot = root.join("boot/slots").join(&selected);
    let m: Manifest = serde_json::from_slice(
        &fs::read(slot.join(".manifest.json")).map_err(|_| "Staged manifest missing")?,
    )
    .map_err(|_| "Invalid staged manifest")?;
    if m.sha256 != selected {
        return Err("Staged manifest changed".into());
    }
    crate::baseline::check(root, &m)?;
    let files = inventory(&m)?;
    staging::verify_files(&slot, &files)?;
    let zimage = fs::read(slot.join(ZIMAGE)).map_err(|_| "Could not read staged kernel")?;
    let ramdisk = fs::read(slot.join(RAMDISK)).map_err(|_| "Could not read staged ramdisk")?;
    check_payload(&zimage, &ramdisk)?;
    Ok(m)
}

pub(crate) fn activate_staged(root: &Path, device: &Path, consume: bool) -> Result<()> {
    let m = validate_staged(root)?;
    let files = inventory(&m)?;
    let slot = root.join("boot/slots").join(&m.sha256);
    let zimage = fs::read(slot.join(ZIMAGE)).map_err(|_| "Could not read staged kernel")?;
    let ramdisk = fs::read(slot.join(RAMDISK)).map_err(|_| "Could not read staged ramdisk")?;
    let current = read_partition(device)?;
    let next = repack(&current, &zimage, &ramdisk)?;
    let parsed = parse(&current)?;
    let (old_zimage, _) = split(parsed.kernel)?;
    if old_zimage != zimage || parsed.ramdisk != ramdisk {
        staging::atomic(&root.join("boot/previous.img"), &current)?;
        staging::atomic(
            &root.join("boot/previous.json"),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1,
                "replaced_zimage_sha256": release::digest(old_zimage),
                "replaced_ramdisk_sha256": release::digest(parsed.ramdisk),
                "written_version": m.version,
                "written_zimage_sha256": files[ZIMAGE].sha256,
            }))
            .unwrap(),
        )?;
        write_partition(device, &next).map_err(|error| {
            // Put the saved image back so the next boot is the one that worked.
            match write_partition(device, &current) {
                Ok(()) => format!("{error}; the previous boot image was restored"),
                Err(_) => format!(
                    "{error}; restoring the previous boot image also failed. Do not power off; \
                     copy boot/previous.img back to the boot partition from the recovery shell"
                ),
            }
        })?;
    }
    record_installed(root, &m)?;
    if consume {
        fs::remove_file(root.join("boot/staged")).map_err(|_| "Could not finalize activation")?;
    }
    Ok(())
}

/// Called only after checking the partition itself against a signed manifest.
pub(crate) fn record_installed(root: &Path, m: &Manifest) -> Result<()> {
    let files = inventory(m)?;
    staging::atomic(
        &root.join("boot/installed.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1,
            "version": m.version,
            "kernel": kernel_commit(&m.notes).unwrap_or_default(),
            "zimage_sha256": files[ZIMAGE].sha256,
            "ramdisk_sha256": files[RAMDISK].sha256,
        }))
        .unwrap(),
    )
}
/// The kernel commit the publisher wrote into the payload's notes ("Couch boot
/// image VERSION: kernel COMMIT and boot ramdisk"). The manifest has no field
/// for it and its bytes are signed, so the notes are where it travels.
fn kernel_commit(notes: &str) -> Option<&str> {
    notes
        .split_once(" kernel ")
        .map(|(_, rest)| rest.split_whitespace().next().unwrap_or_default())
        .filter(|commit| commit.len() >= 7 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
}
/// What the update status says about the boot partition: the version this
/// updater last wrote, the kernel commit its notes named, and whether a saved
/// previous image is there to go back to. Nothing read here is trusted for a
/// write; `restore` verifies the image itself.
pub(crate) fn record(root: &Path) -> (String, String, bool) {
    let installed = fs::read(root.join("boot/installed.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default();
    let field = |key: &str| installed[key].as_str().unwrap_or_default().to_owned();
    (
        field("version"),
        field("kernel"),
        root.join("boot/previous.json").is_file() && root.join("boot/previous.img").is_file(),
    )
}
/// Write the saved previous boot image back to the partition, verified against
/// its own record and read back from the medium the way an install is. The
/// caller restarts on Ok.
///
/// The saved image is consumed: once it is back on the partition the image it
/// replaced is not installed any more, so keeping a 16 MiB file offering to
/// write the kernel that was just undone would be worse than keeping nothing.
/// The next check offers that boot payload again, as the partition no longer
/// carries it.
pub fn restore(root: &Path, device: &Path) -> Result<()> {
    let boot = root.join("boot");
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(boot.join("previous.json")).map_err(|_| "No previous boot image is saved")?,
    )
    .map_err(|_| "The saved boot image record is unreadable")?;
    let saved =
        fs::read(boot.join("previous.img")).map_err(|_| "No previous boot image is saved")?;
    if saved.len() != PARTITION {
        return Err("The saved boot image is not the size of the boot partition".into());
    }
    let parsed = parse(&saved)?;
    let (zimage, _) = split(parsed.kernel)?;
    let (zimage_sha256, ramdisk_sha256) =
        (release::digest(zimage), release::digest(parsed.ramdisk));
    if record["replaced_zimage_sha256"].as_str() != Some(zimage_sha256.as_str())
        || record["replaced_ramdisk_sha256"].as_str() != Some(ramdisk_sha256.as_str())
    {
        return Err("The saved boot image does not match its record".into());
    }
    if read_partition(device)? != saved {
        write_partition(device, &saved)?;
    }
    staging::atomic(
        &boot.join("installed.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1,
            // The saved image predates any boot payload this updater wrote.
            "version": "",
            "kernel": "",
            "zimage_sha256": zimage_sha256,
            "ramdisk_sha256": ramdisk_sha256,
            "restored_from": record["written_version"],
        }))
        .unwrap(),
    )?;
    // Image first: a crash between the two leaves a record with no image, which
    // `restore` refuses, rather than a record promising an image that is gone.
    let _ = fs::remove_file(boot.join("previous.img"));
    let _ = fs::remove_file(boot.join("previous.json"));
    Ok(())
}
fn write_partition(device: &Path, image: &[u8]) -> Result<()> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .read(true)
        .open(device)
        .map_err(|_| "Could not open the boot partition for writing")?;
    let length = output
        .seek(SeekFrom::End(0))
        .map_err(|_| "Could not measure the boot partition")?;
    if length < image.len() as u64 {
        return Err("The boot partition is smaller than the image".into());
    }
    output
        .seek(SeekFrom::Start(0))
        .and_then(|_| output.write_all(image))
        .and_then(|_| output.sync_all())
        .map_err(|_| "Could not write the boot partition")?;
    drop_cache(&output);
    let mut readback = Vec::with_capacity(image.len());
    output
        .seek(SeekFrom::Start(0))
        .and_then(|_| {
            Read::by_ref(&mut output)
                .take(image.len() as u64)
                .read_to_end(&mut readback)
        })
        .map_err(|_| "Could not read the boot partition back")?;
    if readback != image {
        return Err("The boot partition read back differently from what was written".into());
    }
    Ok(())
}
/// Ask the kernel to forget the cached pages so the readback comes from the medium.
#[cfg(target_os = "linux")]
fn drop_cache(file: &fs::File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
}
#[cfg(not(target_os = "linux"))]
fn drop_cache(_file: &fs::File) {}

use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    /// The smallest flattened device tree the splitter accepts: a header whose
    /// structure block is one FDT_END token.
    pub(crate) fn dtb() -> Vec<u8> {
        let mut out = FDT_MAGIC.to_vec();
        for word in [44u32, 40, 44, 40, 17, 17, 0, 0, 4] {
            out.extend_from_slice(&word.to_be_bytes());
        }
        out.extend_from_slice(&[0, 0, 0, 9]);
        assert_eq!(out.len(), 44);
        out
    }
    pub(crate) fn zimage(fill: u8, len: usize) -> Vec<u8> {
        let mut z = vec![fill; len];
        z[36..40].copy_from_slice(&ZIMAGE_MAGIC);
        z
    }
    pub(crate) fn ramdisk(fill: u8, len: usize) -> Vec<u8> {
        let mut r = vec![fill; len];
        r[..2].copy_from_slice(&[0x1f, 0x8b]);
        r
    }
    /// A header-v0 image the way lk expects it, with a made-up command line.
    pub(crate) fn image(zimage: &[u8], ramdisk: &[u8]) -> Vec<u8> {
        let page = 2048;
        let mut header = vec![0u8; page];
        header[..8].copy_from_slice(b"ANDROID!");
        let mut kernel = zimage.to_vec();
        kernel.extend_from_slice(&dtb());
        header[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
        header[12..16].copy_from_slice(&0x8000_8000u32.to_le_bytes());
        header[16..20].copy_from_slice(&(ramdisk.len() as u32).to_le_bytes());
        header[20..24].copy_from_slice(&0x8400_0000u32.to_le_bytes());
        header[36..40].copy_from_slice(&(page as u32).to_le_bytes());
        header[64..76].copy_from_slice(b"bootopt=64S3");
        let mut out = header;
        out.extend_from_slice(&kernel);
        out.resize(aligned(out.len(), page).unwrap(), 0);
        out.extend_from_slice(ramdisk);
        out.resize(PARTITION, 0);
        out
    }
    pub(crate) fn manifest(zimage: &[u8], ramdisk: &[u8]) -> Manifest {
        Manifest {
            schema: 1,
            model: "sanytron-ha100".into(),
            version: "v1.2.3".into(),
            kind: "boot".into(),
            installable: true,
            notes: String::new(),
            url: format!("{}v1.2.3/couch-v1.2.3-ha100-boot.tar.gz", release::PREFIX),
            size: 1,
            sha256: "b".repeat(64),
            files: vec![
                File {
                    path: ZIMAGE.into(),
                    size: zimage.len() as u64,
                    sha256: release::digest(zimage),
                    mode: 0o644,
                },
                File {
                    path: RAMDISK.into(),
                    size: ramdisk.len() as u64,
                    sha256: release::digest(ramdisk),
                    mode: 0o644,
                },
            ],
            required_os_baseline: None,
        }
    }
    pub(crate) fn stage_fixture(root: &Path, m: &Manifest, zimage: &[u8], ramdisk: &[u8]) {
        let slot = root.join("boot/slots").join(&m.sha256);
        fs::create_dir_all(&slot).unwrap();
        fs::write(slot.join(ZIMAGE), zimage).unwrap();
        fs::write(slot.join(RAMDISK), ramdisk).unwrap();
        fs::set_permissions(slot.join(ZIMAGE), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(slot.join(RAMDISK), fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(slot.join(".manifest.json"), serde_json::to_vec(m).unwrap()).unwrap();
        staging::atomic(&root.join("boot/staged"), m.sha256.as_bytes()).unwrap();
    }
    fn root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("couch-boot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn unchanged_payload_preserves_the_partition_and_existing_backup() {
        let root = root("unchanged");
        let device = root.join("boot-device");
        let z = zimage(1, 100);
        let r = ramdisk(2, 100);
        let original = image(&z, &r);
        fs::write(&device, &original).unwrap();
        let m = manifest(&z, &r);
        stage_fixture(&root, &m, &z, &r);
        staging::atomic(&root.join("boot/previous.img"), b"existing backup").unwrap();
        activate(&root, &device).unwrap();
        assert_eq!(fs::read(device).unwrap(), original);
        assert_eq!(
            fs::read(root.join("boot/previous.img")).unwrap(),
            b"existing backup"
        );
        assert_eq!(record(&root).0, m.version);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn repack_keeps_header_and_device_tree_and_replaces_kernel_and_ramdisk() {
        let current = image(&zimage(1, 5000), &ramdisk(2, 3000));
        let (new_z, new_r) = (zimage(3, 7000), ramdisk(4, 100));
        let next = repack(&current, &new_z, &new_r).unwrap();
        assert_eq!(next.len(), PARTITION);
        assert_eq!(&next[64..76], b"bootopt=64S3");
        assert_eq!(&next[12..16], &current[12..16]);
        let parsed = parse(&next).unwrap();
        let (z, d) = split(parsed.kernel).unwrap();
        assert_eq!(z, &new_z[..]);
        assert_eq!(d, &dtb()[..]);
        assert_eq!(parsed.ramdisk, &new_r[..]);
        assert_ne!(&next[576..596], &current[576..596]);
        assert!(repack(&next, &new_z, &new_r).unwrap() == next);
    }
    #[test]
    fn repack_rejects_foreign_partitions_bad_payloads_and_oversize() {
        let current = image(&zimage(1, 5000), &ramdisk(2, 3000));
        assert!(repack(&vec![0; PARTITION], &zimage(3, 100), &ramdisk(4, 100)).is_err());
        assert!(repack(&current, &[3; 100], &ramdisk(4, 100)).is_err());
        assert!(repack(&current, &zimage(3, 100), &[4; 100]).is_err());
        assert!(repack(&current, &zimage(3, PARTITION - 4000), &ramdisk(4, 4096)).is_err());
        let mut no_dtb = current.clone();
        no_dtb[2048 + 5000..2048 + 5004].copy_from_slice(&[0; 4]);
        assert!(repack(&no_dtb, &zimage(3, 100), &ramdisk(4, 100)).is_err());
    }
    #[test]
    fn inventory_accepts_exactly_the_two_payload_files() {
        let m = manifest(&zimage(1, 100), &ramdisk(2, 100));
        assert!(inventory(&m).is_ok());
        let mut extra = m.clone();
        extra.files.push(File {
            path: "recovery.cpio.gz".into(),
            size: 1,
            sha256: "a".repeat(64),
            mode: 0o644,
        });
        assert!(inventory(&extra).is_err());
        let mut runtime = m.clone();
        runtime.kind = "runtime".into();
        assert!(inventory(&runtime).is_err());
        let mut executable = m.clone();
        executable.files[0].mode = 0o755;
        assert!(inventory(&executable).is_err());
        let mut huge = m.clone();
        huge.files[0].size = ZIMAGE_LIMIT + 1;
        assert!(inventory(&huge).is_err());
        let mut one = m;
        one.files.pop();
        assert!(inventory(&one).is_err());
    }
    #[test]
    fn activation_backs_up_writes_verifies_and_reports_installed() {
        let root = root("activate");
        let device = root.join("mmcblk0p8");
        let (old_z, old_r) = (zimage(1, 5000), ramdisk(2, 3000));
        fs::write(&device, image(&old_z, &old_r)).unwrap();
        let (new_z, new_r) = (zimage(3, 7000), ramdisk(4, 100));
        let m = manifest(&new_z, &new_r);
        assert!(!installed(&device, &m).unwrap());
        assert!(installed(&device, &manifest(&old_z, &old_r)).unwrap());
        stage_fixture(&root, &m, &new_z, &new_r);
        activate(&root, &device).unwrap();
        assert!(installed(&device, &m).unwrap());
        assert!(!root.join("boot/staged").exists());
        assert_eq!(
            fs::read(root.join("boot/previous.img")).unwrap(),
            image(&old_z, &old_r)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("boot/installed.json")).unwrap()).unwrap();
        assert_eq!(record["zimage_sha256"], release::digest(&new_z));
        let previous: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("boot/previous.json")).unwrap()).unwrap();
        assert_eq!(previous["replaced_zimage_sha256"], release::digest(&old_z));
        // Activating again with nothing staged is refused; the partition is untouched.
        let written = fs::read(&device).unwrap();
        assert!(activate(&root, &device).is_err());
        assert_eq!(fs::read(&device).unwrap(), written);
        let _ = fs::remove_dir_all(root);
    }
    /// The publisher's archive, signed and verified like a download, unpacks
    /// into a slot the activation accepts, and a runtime manifest cannot be
    /// passed off as a boot payload or the other way round.
    #[test]
    fn published_boot_payload_round_trips_through_verification_and_activation() {
        let root = root("publish");
        let payload = root.join("payload");
        let runtime = root.join("runtime");
        fs::create_dir_all(&payload).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let (new_z, new_r) = (zimage(3, 7000), ramdisk(4, 100));
        fs::write(payload.join(ZIMAGE), &new_z).unwrap();
        fs::write(payload.join(RAMDISK), &new_r).unwrap();
        fs::write(
            payload.join("boot.json"),
            br#"{"source_kernel_commit":"ea122a39f434d962158dc07c5ff2ba2a27de1673"}"#,
        )
        .unwrap();
        let marker = br#"{"schema":1,"model":"sanytron-ha100","id":"baseline-a"}"#;
        fs::write(runtime.join("os-baseline.json"), marker).unwrap();
        fs::write(root.join("os-baseline.json"), marker).unwrap();
        let seed = [7u8; 32];
        let out = root.join("out");
        let key = crate::bundle_boot(&payload, &runtime, "v1.2.3", &seed, &out).unwrap();
        assert!(crate::bundle_boot(&payload, &runtime, "v1.2.3", &seed, &out).is_err());
        let key: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&key[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        let signed = fs::read(out.join("couch-v1.2.3-ha100-boot.json")).unwrap();
        let m = release::verify(&signed, &key, "v1.2.3").unwrap();
        assert_eq!(m.kind, "boot");
        assert_eq!(m.required_os_baseline.as_deref(), Some("baseline-a"));
        assert!(m.notes.contains("ea122a39f434"));
        assert!(release::verify(&signed, &key, "v1.2.4").is_err());
        let archive = fs::read(out.join("couch-v1.2.3-ha100-boot.tar.gz")).unwrap();
        assert_eq!(archive.len() as u64, m.size);
        assert_eq!(release::digest(&archive), m.sha256);
        let files = inventory(&m).unwrap();
        assert!(staging::inventory(&m).is_err());
        let slot = root.join("boot/slots").join(&m.sha256);
        fs::create_dir_all(&slot).unwrap();
        staging::extract(&slot, &files, &archive).unwrap();
        staging::verify_files(&slot, &files).unwrap();
        fs::write(slot.join(".manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        staging::atomic(&root.join("boot/staged"), m.sha256.as_bytes()).unwrap();
        let device = root.join("mmcblk0p8");
        fs::write(&device, image(&zimage(1, 5000), &ramdisk(2, 3000))).unwrap();
        crate::activate_with(&root, &device).unwrap();
        assert!(installed(&device, &m).unwrap());
        // A later release can reuse the same archive/slot. Its selection must
        // retain the NEW manifest, or paired activation would reject it.
        let mut republished = m.clone();
        republished.version = "v1.2.4".into();
        stage_bytes(&root, &republished, &archive, |_| {}).unwrap();
        assert_eq!(validate_staged(&root).unwrap().version, "v1.2.4");
        // A payload bound to another OS baseline is refused before anything is written.
        staging::atomic(&root.join("boot/staged"), m.sha256.as_bytes()).unwrap();
        fs::write(
            root.join("os-baseline.json"),
            br#"{"schema":1,"model":"sanytron-ha100","id":"baseline-b"}"#,
        )
        .unwrap();
        assert!(crate::activate_with(&root, &device).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn rollback_writes_the_saved_image_back_and_refuses_one_that_does_not_verify() {
        let root = root("rollback");
        let device = root.join("mmcblk0p8");
        let (old_z, old_r) = (zimage(1, 5000), ramdisk(2, 3000));
        let original = image(&old_z, &old_r);
        fs::write(&device, &original).unwrap();
        // Nothing has been written, so there is nothing to go back to.
        assert!(restore(&root, &device).is_err());
        assert_eq!(record(&root), (String::new(), String::new(), false));
        let (new_z, new_r) = (zimage(3, 7000), ramdisk(4, 100));
        let mut m = manifest(&new_z, &new_r);
        m.notes = "Couch boot image v1.2.3: kernel ea122a39f434 and boot ramdisk".into();
        stage_fixture(&root, &m, &new_z, &new_r);
        activate(&root, &device).unwrap();
        assert_eq!(
            record(&root),
            ("v1.2.3".into(), "ea122a39f434".into(), true)
        );
        let saved = root.join("boot/previous.json");
        let kept: serde_json::Value = serde_json::from_slice(&fs::read(&saved).unwrap()).unwrap();
        let mut wrong = kept.clone();
        wrong["replaced_zimage_sha256"] = serde_json::json!("f".repeat(64));
        fs::write(&saved, serde_json::to_vec(&wrong).unwrap()).unwrap();
        let written = fs::read(&device).unwrap();
        assert!(restore(&root, &device).is_err());
        assert_eq!(fs::read(&device).unwrap(), written);
        fs::write(&saved, serde_json::to_vec(&kept).unwrap()).unwrap();
        restore(&root, &device).unwrap();
        assert_eq!(fs::read(&device).unwrap(), original);
        assert!(installed(&device, &manifest(&old_z, &old_r)).unwrap());
        assert!(!root.join("boot/previous.img").exists());
        assert!(!saved.exists());
        assert_eq!(record(&root), (String::new(), String::new(), false));
        let now: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("boot/installed.json")).unwrap()).unwrap();
        assert_eq!(now["restored_from"], "v1.2.3");
        assert_eq!(now["zimage_sha256"], release::digest(&old_z));
        // The save is consumed: a second rollback has nothing to put back.
        assert!(restore(&root, &device).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn only_a_commit_shaped_token_is_read_out_of_the_payload_notes() {
        assert_eq!(
            kernel_commit("Couch boot image v1.2.3: kernel ea122a39f434 and boot ramdisk"),
            Some("ea122a39f434")
        );
        assert_eq!(
            kernel_commit("Couch boot image v1.2.3: kernel and boot"),
            None
        );
        assert_eq!(kernel_commit("Couch apps and services v1.2.3"), None);
    }
    #[test]
    fn activation_refuses_a_tampered_slot_and_a_partition_that_is_not_a_boot_image() {
        let root = root("tamper");
        let device = root.join("mmcblk0p8");
        fs::write(&device, image(&zimage(1, 5000), &ramdisk(2, 3000))).unwrap();
        let (new_z, new_r) = (zimage(3, 7000), ramdisk(4, 100));
        let m = manifest(&new_z, &new_r);
        stage_fixture(&root, &m, &new_z, &new_r);
        fs::write(
            root.join("boot/slots").join(&m.sha256).join(ZIMAGE),
            zimage(9, 7000),
        )
        .unwrap();
        let before = fs::read(&device).unwrap();
        assert!(activate(&root, &device).is_err());
        assert_eq!(fs::read(&device).unwrap(), before);
        assert!(root.join("boot/staged").exists());
        stage_fixture(&root, &m, &new_z, &new_r);
        fs::write(&device, vec![0u8; PARTITION]).unwrap();
        assert!(activate(&root, &device).is_err());
        assert!(!root.join("boot/previous.img").exists());
        let _ = fs::remove_dir_all(root);
    }
}
