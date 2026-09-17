//! Native owner-local Android image assembly. Public payloads contain no owner bytes.
use anyhow::{ensure, Context, Result};
use flate2::{bufread::GzDecoder, write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};
const LIMIT: usize = 64 * 1024 * 1024;
const PARTITION: usize = 16 * 1024 * 1024;

/// Every public input must first be admitted by the signed release manifest.
/// Owner inputs are independently checked against the compiled official OTA pins.
pub fn owner_ramdisk(neutral_gzip: &[u8], prepared: &Path) -> Result<Vec<u8>> {
    ensure!(neutral_gzip.len() <= PARTITION, "neutral ramdisk too large");
    let mut decoder = GzDecoder::new(neutral_gzip);
    let mut bytes = Vec::new();
    (&mut decoder)
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= LIMIT && decoder.get_ref().is_empty(),
        "ramdisk size or trailing data"
    );
    let mut entries = parse(&bytes)?;
    let pin: serde_json::Value =
        serde_json::from_str(include_str!("../../pins/ha100_official_runtime.json"))?;
    let ram_files: Vec<String> =
        serde_json::from_str(include_str!("../../pins/ha100_ram_runtime.json"))?;
    ensure!(ram_files.len() == 19, "invalid compiled RAM subset");
    for record in pin["files"]
        .as_array()
        .context("missing compiled inventory")?
    {
        let name = record["path"].as_str().context("missing owner path")?;
        let input = prepared.join("vendor").join(name);
        let data = bounded_regular(
            &input,
            record["size"].as_u64().context("missing size")? as usize,
        )?;
        ensure!(
            hex(&Sha256::digest(&data)) == record["sha256"],
            "owner input digest differs"
        );
        if !ram_files.iter().any(|path| path == name) {
            continue;
        }
        ensure!(
            !entries.contains_key(name),
            "owner file collides with public ramdisk"
        );
        for parent in Path::new(name)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            let parent = parent.to_str().context("invalid compiled path")?;
            if let Some(entry) = entries.get(parent) {
                ensure!(
                    entry.mode & 0o170000 == 0o040000,
                    "owner path traverses non-directory"
                );
            } else {
                entries.insert(parent.into(), Entry::new(0o040755, Vec::new()));
            }
        }
        entries.insert(name.into(), Entry::new(0o100755, data));
    }
    let cpio = encode(&entries)?;
    let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
    gzip.write_all(&cpio)?;
    let result = gzip.finish()?;
    ensure!(
        result.len() <= PARTITION,
        "assembled ramdisk exceeds partition"
    );
    Ok(result)
}
fn bounded_regular(path: &Path, expected: usize) -> Result<Vec<u8>> {
    ensure!(expected <= LIMIT, "input exceeds bound");
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() == expected as u64,
        "input type or size differs"
    );
    let mut file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() == expected as u64,
        "opened input differs"
    );
    let mut result = Vec::new();
    (&mut file)
        .take(expected as u64 + 1)
        .read_to_end(&mut result)?;
    ensure!(result.len() == expected, "input changed while reading");
    Ok(result)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
#[derive(Clone)]
struct Entry {
    mode: u32,
    rdev_major: u32,
    rdev_minor: u32,
    data: Vec<u8>,
}
impl Entry {
    fn new(mode: u32, data: Vec<u8>) -> Self {
        Self {
            mode,
            rdev_major: 0,
            rdev_minor: 0,
            data,
        }
    }
}
fn aligned(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|n| n / alignment * alignment)
        .context("image offset overflow")
}
fn parse(bytes: &[u8]) -> Result<BTreeMap<String, Entry>> {
    let mut entries = BTreeMap::new();
    let mut offset = 0;
    loop {
        let header = bytes
            .get(offset..offset + 110)
            .context("truncated cpio header")?;
        ensure!(&header[..6] == b"070701", "unsupported cpio format");
        let mut fields = [0u32; 13];
        for (i, field) in fields.iter_mut().enumerate() {
            *field = u32::from_str_radix(std::str::from_utf8(&header[6 + i * 8..14 + i * 8])?, 16)?;
        }
        ensure!(
            fields[11] > 0 && fields[11] <= 4096 && fields[12] == 0,
            "invalid cpio metadata"
        );
        // Directory link counts include dot/subdirectories; they are not hardlink
        // aliases. Other entry kinds still cannot request shared inode contents.
        ensure!(
            fields[4] <= 1 || fields[1] & 0o170000 == 0o040000,
            "cpio hardlinks are unsupported"
        );
        let name_end = offset + 110 + fields[11] as usize;
        let name = bytes
            .get(offset + 110..name_end)
            .context("truncated cpio name")?;
        ensure!(name.last() == Some(&0), "unterminated cpio name");
        let name = std::str::from_utf8(&name[..name.len() - 1])?;
        ensure!(
            !name.contains('\0')
                && !name.starts_with('/')
                && (name == "."
                    || name
                        .split('/')
                        .all(|p| !p.is_empty() && p != "." && p != "..")),
            "unsafe cpio path"
        );
        let start = aligned(name_end, 4)?;
        let end = start
            .checked_add(fields[6] as usize)
            .context("cpio overflow")?;
        let data = bytes.get(start..end).context("truncated cpio file")?;
        offset = aligned(end, 4)?;
        if name == "TRAILER!!!" {
            ensure!(
                data.is_empty()
                    && bytes
                        .get(offset..)
                        .context("truncated padding")?
                        .iter()
                        .all(|b| *b == 0),
                "cpio trailing data"
            );
            break;
        }
        ensure!(entries.len() < 4096, "too many cpio entries");
        let kind = fields[1] & 0o170000;
        ensure!(
            [0o040000, 0o100000, 0o120000, 0o020000, 0o060000].contains(&kind),
            "unsupported cpio type"
        );
        ensure!(
            entries
                .insert(
                    name.into(),
                    Entry {
                        mode: fields[1],
                        rdev_major: fields[9],
                        rdev_minor: fields[10],
                        data: data.to_vec()
                    }
                )
                .is_none(),
            "duplicate cpio path"
        );
    }
    Ok(entries)
}
fn encode(entries: &BTreeMap<String, Entry>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for (index, (name, entry)) in entries
        .iter()
        .chain(std::iter::once((
            &"TRAILER!!!".into(),
            &Entry::new(0, Vec::new()),
        )))
        .enumerate()
    {
        let fields = [
            index as u32 + 1,
            entry.mode,
            0,
            0,
            1,
            0,
            entry.data.len() as u32,
            0,
            0,
            entry.rdev_major,
            entry.rdev_minor,
            name.len() as u32 + 1,
            0,
        ];
        bytes.extend_from_slice(b"070701");
        for field in fields {
            write!(bytes, "{field:08x}")?;
        }
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.resize(aligned(bytes.len(), 4)?, 0);
        bytes.extend_from_slice(&entry.data);
        bytes.resize(aligned(bytes.len(), 4)?, 0);
        ensure!(bytes.len() <= LIMIT, "cpio exceeds bound");
    }
    bytes.resize(aligned(bytes.len(), 512)?, 0);
    Ok(bytes)
}

/// Assemble a full 16 MiB boot partition from the owner's pinned stock boot image.
/// None retains the stock kernel for rescue; Some uses the public source-built zImage.
/// DTB, addresses and header always originate from the verified owner's OTA.
pub fn boot_image(
    prepared: &Path,
    public_zimage: Option<&[u8]>,
    ramdisk: &[u8],
) -> Result<Vec<u8>> {
    let template = bounded_regular(&prepared.join("bootstrap/boot.img"), 8_030_464)?;
    ensure!(
        hex(&Sha256::digest(&template))
            == "dda78c8ebe7cb82095b08a10c2a1f779cbdbebc53464aee34c85bb3a7382cad7",
        "owner boot template differs"
    );
    repack(&template, public_zimage, ramdisk)
}
fn le(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated boot header")?
            .try_into()?,
    ))
}
fn repack(template: &[u8], public_zimage: Option<&[u8]>, ramdisk: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        template.len() >= 2048 && &template[..8] == b"ANDROID!",
        "invalid Android boot header"
    );
    let page = le(template, 36)? as usize;
    let kernel_size = le(template, 8)? as usize;
    let old_ramdisk = le(template, 16)? as usize;
    ensure!(
        [2048, 4096, 8192, 16384].contains(&page)
            && le(template, 24)? == 0
            && le(template, 40)? == 0
            && kernel_size > 0,
        "unsupported boot geometry"
    );
    let end = page.checked_add(kernel_size).context("kernel overflow")?;
    ensure!(
        aligned(end, page)?
            .checked_add(old_ramdisk)
            .is_some_and(|n| n <= template.len()),
        "truncated boot payload"
    );
    let original_kernel = template.get(page..end).context("truncated boot kernel")?;
    let kernel = if let Some(zimage) = public_zimage {
        ensure!(
            zimage.len() >= 48
                && zimage.len() <= PARTITION
                && zimage[36..40] == [0x18, 0x28, 0x6f, 0x01],
            "invalid public ARM zImage"
        );
        let dtb = dtb(original_kernel)?;
        let mut kernel = zimage.to_vec();
        kernel.extend_from_slice(dtb);
        kernel
    } else {
        original_kernel.to_vec()
    };
    ensure!(
        !ramdisk.is_empty() && ramdisk.len() <= PARTITION,
        "invalid ramdisk size"
    );
    let mut output = template
        .get(..page)
        .context("truncated header page")?
        .to_vec();
    output[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
    output[16..20].copy_from_slice(&(ramdisk.len() as u32).to_le_bytes());
    let mut digest = sha1::Sha1::new();
    for part in [&kernel[..], ramdisk, &[][..]] {
        digest.update(part);
        digest.update((part.len() as u32).to_le_bytes());
    }
    output[576..596].copy_from_slice(&digest.finalize());
    output[596..608].fill(0);
    output.extend_from_slice(&kernel);
    output.resize(aligned(output.len(), page)?, 0);
    output.extend_from_slice(ramdisk);
    ensure!(
        aligned(output.len(), page)? <= PARTITION,
        "assembled boot image exceeds 16 MiB"
    );
    output.resize(PARTITION, 0);
    Ok(output)
}
fn dtb(kernel: &[u8]) -> Result<&[u8]> {
    for offset in 0..kernel.len().saturating_sub(39) {
        if kernel[offset..offset + 4] != [0xd0, 0x0d, 0xfe, 0xed] {
            continue;
        }
        let bytes = &kernel[offset..];
        let word = |n: usize| u32::from_be_bytes(bytes[n..n + 4].try_into().unwrap()) as usize;
        let size = word(4);
        let structure = word(8);
        let strings = word(12);
        let reserve = word(16);
        let struct_size = word(36);
        let string_size = word(32);
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
            return Ok(bytes);
        }
    }
    anyhow::bail!("owner kernel has no validated appended DTB")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpio_roundtrip_preserves_devices_and_rejects_duplicate_traversal_and_tail() {
        let mut entries = BTreeMap::new();
        entries.insert(".".into(), Entry::new(0o040755, Vec::new()));
        entries.insert("init".into(), Entry::new(0o100755, b"fixture".to_vec()));
        entries.insert(
            "dev/null".into(),
            Entry {
                mode: 0o020666,
                rdev_major: 1,
                rdev_minor: 3,
                data: Vec::new(),
            },
        );
        let bytes = encode(&entries).unwrap();
        let actual = parse(&bytes).unwrap();
        assert_eq!(actual["dev/null"].rdev_minor, 3);
        assert_eq!(actual["init"].data, b"fixture");
        let mut duplicate = bytes[..112].to_vec();
        duplicate.extend_from_slice(&bytes);
        assert!(parse(&duplicate).is_err());
        let mut bad = bytes.clone();
        bad.push(1);
        assert!(parse(&bad).is_err());
        entries.insert("../escape".into(), Entry::new(0o100600, Vec::new()));
        assert!(parse(&encode(&entries).unwrap()).is_err());
        for end in [0, 109, 111, bytes.len() / 2] {
            assert!(parse(&bytes[..end]).is_err());
        }
    }
    #[test]
    fn directory_links_do_not_admit_regular_or_symlink_hardlinks() {
        for mode in [0o040755, 0o100755, 0o120777, 0o020666] {
            let mut entries = BTreeMap::new();
            entries.insert("entry".into(), Entry::new(mode, Vec::new()));
            let original = encode(&entries).unwrap();
            for count in [2u32, 7, u32::MAX] {
                let mut bytes = original.clone();
                bytes[38..46].copy_from_slice(format!("{count:08x}").as_bytes());
                assert_eq!(parse(&bytes).is_ok(), mode == 0o040755);
                if mode == 0o040755 {
                    let parsed = parse(&bytes).unwrap();
                    assert_eq!(parsed["entry"].mode, mode);
                    assert!(parse(&encode(&parsed).unwrap()).is_ok());
                }
            }
        }
    }
    #[test]
    fn boot_repack_keeps_header_and_stock_kernel_and_rejects_bad_geometry() {
        let mut template = vec![0; 6144];
        template[..8].copy_from_slice(b"ANDROID!");
        template[8..12].copy_from_slice(&48u32.to_le_bytes());
        template[16..20].copy_from_slice(&20u32.to_le_bytes());
        template[36..40].copy_from_slice(&2048u32.to_le_bytes());
        template[2048..2096].fill(7);
        let result = repack(&template, None, b"new ramdisk").unwrap();
        assert_eq!(result.len(), PARTITION);
        assert_eq!(&result[2048..2096], &template[2048..2096]);
        assert_eq!(&result[4096..4107], b"new ramdisk");
        assert_eq!(le(&result, 16).unwrap(), 11);
        template[24] = 1;
        assert!(repack(&template, None, b"test").is_err());
    }
}

/// Replace only the boot frame in an independently verified original logo backup.
/// Header extensions and every charging/other image stream are preserved byte-for-byte.
/// The caller binds the result hash into the install plan before any write.
pub fn logo_image(original: &[u8], bgra: &[u8]) -> Result<Vec<u8>> {
    const FRAME: usize = 480 * 800 * 4;
    ensure!(bgra.len() == FRAME, "expected 480x800 BGRA logo frame");
    ensure!(
        original.len() >= 4096
            && original.len() <= PARTITION
            && original.len().is_multiple_of(4096),
        "invalid logo partition size"
    );
    ensure!(
        le(original, 0)? == 0x58881688 && original.get(8..13) == Some(b"logo\0"),
        "invalid MTK logo header"
    );
    let block = le(original, 4)? as usize;
    ensure!(
        block >= 12 && block <= original.len() - 512 && le(original, 516)? as usize == block,
        "logo payload bounds mismatch"
    );
    let count = le(original, 512)? as usize;
    ensure!((1..=1024).contains(&count), "invalid logo frame count");
    let table = 8 + 4 * count;
    ensure!(table < block, "truncated logo offset table");
    let mut offsets = Vec::new();
    for index in 0..count {
        offsets.push(le(original, 520 + index * 4)? as usize);
    }
    ensure!(
        offsets[0] == table
            && offsets.windows(2).all(|p| p[0] < p[1])
            && offsets[count - 1] < block,
        "invalid logo frame offsets"
    );
    offsets.push(block);
    let first = &original[512 + offsets[0]..512 + offsets[1]];
    let mut decoder = flate2::bufread::ZlibDecoder::new(first);
    let mut raw = Vec::new();
    (&mut decoder)
        .take(FRAME as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(
        raw.len() == FRAME && decoder.get_ref().is_empty(),
        "original boot logo frame is corrupt or wrong size"
    );
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bgra)?;
    let replacement = encoder.finish()?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&(count as u32).to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    let mut next = table;
    for index in 0..count {
        payload.extend_from_slice(&(next as u32).to_le_bytes());
        next += if index == 0 {
            replacement.len()
        } else {
            offsets[index + 1] - offsets[index]
        };
    }
    ensure!(
        512 + next <= original.len(),
        "replacement logo exceeds partition"
    );
    payload[4..8].copy_from_slice(&(next as u32).to_le_bytes());
    payload.extend_from_slice(&replacement);
    payload.extend_from_slice(&original[512 + offsets[1]..512 + block]);
    let mut output = original[..512].to_vec();
    output[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    output.extend_from_slice(&payload);
    output.resize(original.len(), 0);
    Ok(output)
}

#[cfg(test)]
mod logo_tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&vec![0; 480 * 800 * 4]).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut bytes = vec![0; 512];
        bytes[..4].copy_from_slice(&0x58881688u32.to_le_bytes());
        bytes[8..13].copy_from_slice(b"logo\0");
        bytes[100..104].copy_from_slice(b"KEEP");
        let block = 16 + compressed.len() + 8;
        bytes[4..8].copy_from_slice(&(block as u32).to_le_bytes());
        for word in [2, block as u32, 16, 16 + compressed.len() as u32] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&compressed);
        bytes.extend_from_slice(b"charging");
        bytes.resize(32768, 0);
        bytes
    }
    #[test]
    fn only_boot_frame_changes_and_invalid_offsets_or_streams_are_refused() {
        let original = fixture();
        let bgra = vec![255; 480 * 800 * 4];
        let result = logo_image(&original, &bgra).unwrap();
        assert_eq!(result.len(), original.len());
        assert_eq!(&result[8..512], &original[8..512]);
        let offset = le(&result, 524).unwrap() as usize;
        assert_eq!(
            &result[512 + offset..512 + le(&result, 4).unwrap() as usize],
            b"charging"
        );
        let first = le(&result, 520).unwrap() as usize;
        let mut actual = Vec::new();
        flate2::read::ZlibDecoder::new(&result[512 + first..512 + offset])
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, bgra);
        let mut bad = original.clone();
        bad[520..524].copy_from_slice(&0u32.to_le_bytes());
        assert!(logo_image(&bad, &bgra).is_err());
        let mut bad = original.clone();
        bad[530] ^= 1;
        assert!(logo_image(&bad, &bgra).is_err());
        assert!(logo_image(&original, &bgra[..10]).is_err());
    }
}
