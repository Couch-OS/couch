//! Linux-only, fixed eMMC backend. GPT and sysfs must independently agree.
use super::{ensure, hash, hex, invalid, IDENTITY};
use couch_installer_storage::{direct, Hash, Identity, Observation, Region, Storage, Target};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

fn number(path: impl AsRef<Path>) -> io::Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| invalid("invalid sysfs number"))
}
fn le32(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn le64(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}
fn read_at(file: &mut File, offset: u64, n: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut b = vec![0; n];
    file.read_exact(&mut b)?;
    Ok(b)
}
#[derive(PartialEq, Debug)]
struct Header {
    guid: Vec<u8>,
    first: u64,
    last: u64,
    count: usize,
    stride: usize,
    table: u64,
    crc: u32,
}
fn header(b: &[u8], lba: u64, capacity: u64) -> io::Result<Header> {
    ensure(b.len() == 512 && &b[..8] == b"EFI PART", "missing GPT")?;
    let n = le32(b, 12) as usize;
    ensure(
        le32(b, 8) == 0x10000 && (92..=512).contains(&n) && le32(b, 20) == 0,
        "GPT version",
    )?;
    let mut checked = b[..n].to_vec();
    checked[16..20].fill(0);
    ensure(crc32fast::hash(&checked) == le32(b, 16), "GPT header CRC")?;
    let sectors = capacity / 512;
    ensure(
        le64(b, 24) == lba && le64(b, 32) == if lba == 1 { sectors - 1 } else { 1 },
        "GPT location",
    )?;
    let first = le64(b, 40);
    let last = le64(b, 48);
    let table = le64(b, 72);
    let count = le32(b, 80) as usize;
    let stride = le32(b, 84) as usize;
    ensure(
        first > 1
            && first <= last
            && last < sectors - 1
            && count > 0
            && count <= 4096
            && stride >= 128
            && stride % 128 == 0,
        "GPT bounds",
    )?;
    let bytes = count
        .checked_mul(stride)
        .ok_or_else(|| invalid("GPT overflow"))?;
    ensure(bytes <= 1024 * 1024, "GPT array bound")?;
    let end = table
        .checked_add((bytes as u64).div_ceil(512))
        .ok_or_else(|| invalid("GPT overflow"))?;
    ensure(
        if lba == 1 {
            table > 1 && end <= first
        } else {
            table > last && end <= lba
        },
        "GPT array location",
    )?;
    Ok(Header {
        guid: b[56..72].to_vec(),
        first,
        last,
        count,
        stride,
        table,
        crc: le32(b, 88),
    })
}
fn layout(file: &mut File, capacity: u64) -> io::Result<BTreeMap<String, (Region, u32)>> {
    let mut copies = Vec::new();
    for lba in [1, capacity / 512 - 1] {
        let h = header(&read_at(file, lba * 512, 512)?, lba, capacity)?;
        let data = read_at(file, h.table * 512, h.count * h.stride)?;
        ensure(crc32fast::hash(&data) == h.crc, "GPT entries CRC")?;
        copies.push((h, data));
    }
    let (a, raw) = &copies[0];
    let (b, other) = &copies[1];
    ensure(
        a.guid == b.guid
            && a.first == b.first
            && a.last == b.last
            && a.count == b.count
            && a.stride == b.stride
            && raw == other,
        "GPT copies differ",
    )?;
    let mut map = BTreeMap::new();
    let mut bounds = Vec::new();
    for (i, entry) in raw.chunks_exact(a.stride).enumerate() {
        if entry[..16] == [0; 16] {
            continue;
        }
        let first = le64(entry, 32);
        let last = le64(entry, 40);
        ensure(
            first >= a.first && last >= first && last <= a.last,
            "GPT partition bounds",
        )?;
        let units: Vec<u16> = entry[56..128]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|u| *u != 0)
            .collect();
        let name = String::from_utf16(&units).map_err(|_| invalid("GPT name encoding"))?;
        ensure(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "GPT name",
        )?;
        ensure(
            map.insert(
                name,
                (
                    Region {
                        offset: first * 512,
                        size: (last - first + 1) * 512,
                    },
                    i as u32 + 1,
                ),
            )
            .is_none(),
            "duplicate GPT name",
        )?;
        bounds.push((first, last));
    }
    bounds.sort();
    ensure(
        bounds.windows(2).all(|p| p[0].1 < p[1].0),
        "overlapping GPT partitions",
    )?;
    Ok(map)
}
fn node(path: &Path, minor: u32) -> io::Result<()> {
    if !path.exists() {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| invalid("node path"))?;
        let result = unsafe {
            libc::mknod(
                name.as_ptr(),
                libc::S_IFBLK | 0o600,
                libc::makedev(179, minor),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure(
        metadata.file_type().is_block_device()
            && metadata.rdev() == libc::makedev(179, minor) as u64,
        "wrong block node",
    )
}
fn open(path: &Path, minor: u32, write: bool, direct: bool) -> io::Result<File> {
    node(path, minor)?;
    let flags = libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | if write { libc::O_EXCL } else { 0 }
        | if direct { libc::O_DIRECT } else { 0 };
    let file = OpenOptions::new()
        .read(!write)
        .write(write)
        .custom_flags(flags)
        .open(path)?;
    let m = file.metadata()?;
    ensure(
        m.file_type().is_block_device() && m.rdev() == libc::makedev(179, minor) as u64,
        "block descriptor changed",
    )?;
    Ok(file)
}
fn no_mounts_or_holders() -> io::Result<()> {
    // This RAM root has no reason to mount ANY eMMC partition. Reject aliases,
    // bind mounts, other namespaces and swap, including targets outside the plan.
    for entry in fs::read_dir("/proc")? {
        let path = entry?.path();
        if !path
            .file_name()
            .unwrap()
            .as_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let data = match fs::read_to_string(path.join("mountinfo")) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for line in data.lines() {
            let dev = line
                .split_whitespace()
                .nth(2)
                .ok_or_else(|| invalid("mountinfo"))?;
            ensure(!dev.starts_with("179:"), "eMMC is mounted")?;
        }
    }
    for line in fs::read_to_string("/proc/swaps")?.lines().skip(1) {
        let name = line
            .split_whitespace()
            .next()
            .ok_or_else(|| invalid("swap entry"))?;
        let m = fs::metadata(name)?;
        ensure(
            !m.file_type().is_block_device() || libc::major(m.rdev()) != 179,
            "eMMC swap active",
        )?;
    }
    for entry in fs::read_dir("/sys/class/block")? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name == "mmcblk0" || name.starts_with("mmcblk0p") {
            ensure(
                fs::read_dir(path.join("holders"))?.next().is_none(),
                "eMMC has holders",
            )?;
        }
    }
    Ok(())
}
pub struct Disk {
    network: Option<zeroize::Zeroizing<Vec<u8>>>,
    expected: Identity,
    identity_hashes: BTreeMap<String, String>,
    nodes: BTreeMap<String, u32>,
    writer: Option<File>,
    root: PathBuf,
}
impl Disk {
    pub fn new(expected: Identity, identity_hashes: BTreeMap<String, String>) -> io::Result<Self> {
        let root = PathBuf::from("/dev/couch-installer");
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let mut disk = Self {
            network: None,
            expected,
            identity_hashes,
            nodes: BTreeMap::new(),
            writer: None,
            root,
        };
        disk.refresh()?;
        Ok(disk)
    }
    pub(super) fn configure_network(
        &mut self,
        network: Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> io::Result<()> {
        ensure(self.network.is_none(), "network configuration already set")?;
        self.network = network;
        Ok(())
    }
    fn refresh(&mut self) -> io::Result<()> {
        no_mounts_or_holders()?;
        let capacity = number("/sys/class/block/mmcblk0/size")?
            .checked_mul(512)
            .ok_or_else(|| invalid("capacity overflow"))?;
        ensure(
            capacity == self.expected.capacity,
            "storage capacity changed",
        )?;
        ensure(
            fs::read_to_string("/sys/class/block/mmcblk0/device/cid")?.trim()
                == hex(&self.expected.cid),
            "CID changed",
        )?;
        let mut file = open(&self.root.join("disk"), 0, false, false)?;
        // BLKGETSIZE64 encodes sizeof(size_t), hence differs on ARMv7 vs host.
        let request = 0x8000_0000u64 | ((std::mem::size_of::<usize>() as u64) << 16) | 0x1272;
        let mut bytes = 0u64;
        if unsafe { libc::ioctl(file.as_raw_fd(), request as _, &mut bytes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        ensure(bytes == capacity, "block capacity differs from sysfs")?;
        let observed = layout(&mut file, capacity)?;
        ensure(
            observed.len() == self.expected.partitions.len(),
            "partition inventory differs",
        )?;
        for (name, (region, minor)) in observed {
            ensure(
                self.expected.partitions.get(&name) == Some(&region),
                "observed GPT differs from plan",
            )?;
            let sys = PathBuf::from(format!("/sys/class/block/mmcblk0p{minor}"));
            ensure(
                number(sys.join("start"))? * 512 == region.offset
                    && number(sys.join("size"))? * 512 == region.size,
                "kernel partition differs from GPT",
            )?;
            ensure(
                fs::read_to_string(sys.join("dev"))?.trim() == format!("179:{minor}"),
                "kernel device number differs",
            )?;
            self.nodes.insert(name, minor);
        }
        Ok(())
    }
    fn allowed(name: &str) -> bool {
        IDENTITY.contains(&name) || super::target(name).is_ok()
    }
    fn file(&self, name: &str, write: bool, direct: bool) -> io::Result<File> {
        ensure(
            Self::allowed(name) && (!write || super::target(name).is_ok()),
            "prohibited storage access",
        )?;
        let minor = *self
            .nodes
            .get(name)
            .ok_or_else(|| invalid("missing partition"))?;
        open(&self.root.join(name), minor, write, direct)
    }
    pub fn read_name(&self, name: &str) -> io::Result<File> {
        self.file(name, false, false)
    }
    pub fn hash_name(&self, name: &str) -> io::Result<Hash> {
        direct::hash(
            &self.file(name, false, true)?,
            self.expected.partitions[name].size,
        )
    }
    pub fn hash_name_progress(
        &self,
        name: &str,
        progress: &mut dyn FnMut(u64, u64) -> io::Result<()>,
    ) -> io::Result<Hash> {
        direct::hash_with_progress(
            &self.file(name, false, true)?,
            self.expected.partitions[name].size,
            progress,
        )
    }
    pub fn verify_identity(&mut self) -> io::Result<()> {
        self.refresh()?;
        for (name, expected) in &self.identity_hashes {
            ensure(
                self.hash_name(name)? == hash(expected)?,
                "calibration changed",
            )?;
        }
        Ok(())
    }
}
impl Storage for Disk {
    fn observe(&mut self) -> io::Result<Observation> {
        self.refresh()?;
        Ok(Observation {
            identity: self.expected.clone(),
            mounted: BTreeSet::new(),
        })
    }
    fn begin(&mut self, target: Target) -> io::Result<()> {
        ensure(self.writer.is_none(), "writer already open")?;
        self.refresh()?;
        self.writer = Some(self.file(target.name(), true, false)?);
        Ok(())
    }
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer
            .as_mut()
            .ok_or_else(|| invalid("writer missing"))?
            .write_all(bytes)
    }
    fn sync_close(&mut self) -> io::Result<()> {
        let file = self
            .writer
            .take()
            .ok_or_else(|| invalid("writer missing"))?;
        file.sync_all()?;
        drop(file);
        Ok(())
    }
    fn direct_hash(&mut self, target: Target, size: u64) -> io::Result<Hash> {
        ensure(
            self.writer.is_none()
                && size > 0
                && (size == self.expected.partitions[target.name()].size
                    || (target == Target::Userdata
                        && size < self.expected.partitions[target.name()].size)),
            "readback scope",
        )?;
        direct::hash(&self.file(target.name(), false, true)?, size)
    }
    fn expand_userdata(&mut self, image_size: u64, partition_size: u64) -> io::Result<()> {
        ensure(
            self.writer.is_none()
                && image_size < partition_size
                && partition_size == self.expected.partitions["userdata"].size,
            "expansion scope",
        )?;
        self.refresh()?;
        // Hand exclusive ownership to resize2fs: retaining our O_EXCL descriptor
        // makes its own exclusive open fail with EBUSY. The fixed node lives in
        // our root-only directory; this RAM stage has no automounter. Revalidate
        // identity/mount state across the handoff; no caller paths or discard.
        let mut guard = self.file("userdata", false, false)?;
        check_ext4(&mut guard, image_size)?;
        drop(guard);
        let path = self.root.join("userdata");
        fs_tool("/sbin/e2fsck", &["-fn"], &path, None)?;
        self.refresh()?;
        fs_tool(
            "/usr/sbin/resize2fs",
            &[],
            &path,
            Some(&(partition_size / 4096).to_string()),
        )?;
        self.refresh()?;
        let mut guard = self.file("userdata", false, false)?;
        guard.sync_all()?;
        check_ext4(&mut guard, partition_size)?;
        drop(guard);
        fs_tool("/sbin/e2fsck", &["-fn"], &path, None)?;
        self.refresh()?;
        if let Some(network) = self.network.take() {
            super::network::configure(&path, &self.root, &network)?;
            self.refresh()?;
            let guard = self.file("userdata", false, false)?;
            guard.sync_all()?;
            drop(guard);
            fs_tool("/sbin/e2fsck", &["-fn"], &path, None)?;
        }
        self.refresh()
    }
    fn abort(&mut self) {
        self.network.take();
        self.writer.take();
    }
    fn direct_hash_progress(
        &mut self,
        target: Target,
        size: u64,
        progress: &mut dyn FnMut(u64, u64) -> io::Result<()>,
    ) -> io::Result<Hash> {
        ensure(
            self.writer.is_none()
                && size > 0
                && (size == self.expected.partitions[target.name()].size
                    || (target == Target::Userdata
                        && size < self.expected.partitions[target.name()].size)),
            "readback scope",
        )?;
        direct::hash_with_progress(&self.file(target.name(), false, true)?, size, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_gpt_headers_and_metadata_overlap_are_rejected() {
        let mut b = vec![0; 512];
        b[..8].copy_from_slice(b"EFI PART");
        b[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        b[12..16].copy_from_slice(&92u32.to_le_bytes());
        b[24..32].copy_from_slice(&1u64.to_le_bytes());
        b[32..40].copy_from_slice(&2047u64.to_le_bytes());
        b[40..48].copy_from_slice(&34u64.to_le_bytes());
        b[48..56].copy_from_slice(&2014u64.to_le_bytes());
        b[72..80].copy_from_slice(&2u64.to_le_bytes());
        b[80..84].copy_from_slice(&128u32.to_le_bytes());
        b[84..88].copy_from_slice(&128u32.to_le_bytes());
        let crc = crc32fast::hash(&b[..92]);
        b[16..20].copy_from_slice(&crc.to_le_bytes());
        assert!(header(&b, 1, 1048576).is_ok());
        b[40] ^= 1;
        assert!(header(&b, 1, 1048576).is_err());
    }
}

// The release uses 4 KiB legacy ext4 with no 64-bit blocks or metadata checksums.
fn check_ext4(file: &mut File, expected_size: u64) -> io::Result<()> {
    let sb = read_at(file, 1024, 1024)?;
    ensure(
        sb[56..58] == [0x53, 0xef]
            && le32(&sb, 24) == 2
            && u64::from(le32(&sb, 4)) * 4096 == expected_size
            && le32(&sb, 92) == 0x2c
            && le32(&sb, 96) == 0x42
            && le32(&sb, 100) == 0x73,
        "unexpected ext4 geometry or features",
    )
}
fn fs_tool(tool: &str, args: &[&str], path: &Path, size: Option<&str>) -> io::Result<()> {
    let mut command = std::process::Command::new(tool);
    command.args(args).arg(path);
    if let Some(size) = size {
        command.arg(size);
    }
    // Tool output contains filesystem statistics only; never credentials.
    let status = command.env_clear().env("PATH", "/sbin:/bin").status()?;
    ensure(status.success(), "filesystem check or expansion failed")
}
