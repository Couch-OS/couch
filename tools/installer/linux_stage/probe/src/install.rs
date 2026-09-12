//! Private installer protocol, compiled only with explicit private-install feature.
//! USB commits the plan hash; authenticated TLS carries bounded image streams.
mod block;
mod network;
#[cfg(all(test, target_os = "linux"))]
mod session_tests;
use crate::invalid;
use couch_installer_storage::{
    Hash, Identity, Image, Journal, Phase, Plan, Region, Storage, Target, Transaction, CHUNK,
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{self, Read, Write},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

static BINDING: Mutex<Option<Binding>> = Mutex::new(None);
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    plan_sha256: String,
    nonce: String,
}
pub fn bind(data: &[u8]) -> io::Result<()> {
    let value: Binding =
        serde_json::from_slice(data).map_err(|_| invalid("invalid install binding"))?;
    hash(&value.plan_sha256)?;
    hash(&value.nonce)?;
    let mut slot = BINDING.lock().map_err(|_| invalid("binding lock"))?;
    ensure(
        slot.is_none() && !ATTEMPTED.load(Ordering::Acquire),
        "install already bound",
    )?;
    *slot = Some(value);
    Ok(())
}
fn ensure(ok: bool, why: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(invalid(why))
    }
}
fn hash(value: &str) -> io::Result<Hash> {
    ensure(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid hash",
    )?;
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| invalid("invalid hex"))?;
    }
    Ok(out)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn display(phase: &str, name: &str, done: u64, total: u64) {
    let data = format!("v1 {phase} {name} {done} {total} 0 connected none\n");
    let pending = "/tmp/couch-installer-display.pending";
    if std::fs::write(pending, data).is_ok() {
        let _ = std::fs::rename(pending, "/tmp/couch-installer-display.state");
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRegion {
    offset: u64,
    size: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireImage {
    size: u64,
    sha256: String,
    chunks: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePlan {
    schema: u32,
    #[serde(default)]
    network: Option<network::Network>,
    #[serde(default)]
    skip_userdata_backup: bool,
    #[serde(default)]
    reused_backups: BTreeMap<String, String>,
    nonce: String,
    manifest_sha256: String,
    cid: String,
    capacity: u64,
    partitions: BTreeMap<String, WireRegion>,
    images: BTreeMap<String, WireImage>,
    identity_sha256: BTreeMap<String, String>,
    original_boot_sha256: String,
    stage_sha256: String,
}
const IDENTITY: [&str; 5] = ["proinfo", "nvram", "nvdata", "protect1", "protect2"];
fn target(name: &str) -> io::Result<Target> {
    match name {
        "recovery" => Ok(Target::Recovery),
        "userdata" => Ok(Target::Userdata),
        "logo" => Ok(Target::Logo),
        "odmdtbo" => Ok(Target::OdmDtbo),
        "boot" => Ok(Target::Boot),
        _ => Err(invalid("prohibited target")),
    }
}
fn read_json_bytes(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut n = [0; 4];
    stream.read_exact(&mut n)?;
    let size = u32::from_le_bytes(n) as usize;
    ensure(size > 0 && size <= 1024 * 1024, "JSON frame too large")?;
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn receive(stream: &mut impl Read) -> io::Result<serde_json::Value> {
    serde_json::from_slice(&read_json_bytes(stream)?).map_err(|_| invalid("invalid JSON frame"))
}
fn send(stream: &mut impl Write, value: serde_json::Value) -> io::Result<()> {
    let data = serde_json::to_vec(&value)?;
    ensure(data.len() <= 1024 * 1024, "JSON frame too large")?;
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(&data)?;
    stream.flush()
}
pub fn report_error(stream: &mut impl Write) -> io::Result<()> {
    send(
        stream,
        serde_json::json!({"event":"error","message":"Installer stopped; preserve the host journal and originals. No automatic retry."}),
    )
}
fn acknowledge(
    stream: &mut (impl Read + Write),
    event: &str,
    name: &str,
    digest: &str,
) -> io::Result<()> {
    let value = receive(stream)?;
    ensure(
        value == serde_json::json!({"ack":event,"target":name,"sha256":digest}),
        "host journal acknowledgment mismatch",
    )
}
fn send_chunk(stream: &mut impl Write, raw: &[u8]) -> io::Result<()> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(raw)?;
    let compressed = encoder.finish()?;
    let (kind, bytes) = if compressed.len() < raw.len() {
        (1u32, compressed.as_slice())
    } else {
        (0, raw)
    };
    for n in [raw.len() as u32, bytes.len() as u32, kind] {
        stream.write_all(&n.to_le_bytes())?;
    }
    stream.write_all(bytes)
}
fn receive_chunk(stream: &mut impl Read, expected: usize) -> io::Result<Vec<u8>> {
    let mut header = [0; 12];
    stream.read_exact(&mut header)?;
    let n = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    let wire = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let kind = u32::from_le_bytes(header[8..].try_into().unwrap());
    ensure(
        n == expected && n > 0 && n <= CHUNK && wire > 0 && wire <= CHUNK && kind <= 1,
        "invalid image chunk",
    )?;
    let mut data = vec![0; wire];
    stream.read_exact(&mut data)?;
    if kind == 0 {
        ensure(wire == n, "raw chunk length")?;
        return Ok(data);
    }
    let mut decoder = ZlibDecoder::new(data.as_slice());
    let mut raw = Vec::new();
    (&mut decoder).take(n as u64 + 1).read_to_end(&mut raw)?;
    ensure(
        raw.len() == n && decoder.total_in() == wire as u64,
        "compressed chunk length",
    )?;
    Ok(raw)
}
type Shared<'a, S> = Rc<RefCell<&'a mut S>>;
struct Source<'a, S> {
    stream: Shared<'a, S>,
    remaining: u64,
    pending: io::Cursor<Vec<u8>>,
    target: Target,
    total: u64,
}
impl<S: Read> Read for Source<'_, S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pending.position() == self.pending.get_ref().len() as u64 {
            if self.remaining == 0 {
                return Ok(0);
            }
            let n = self.remaining.min(CHUNK as u64) as usize;
            self.pending = io::Cursor::new(receive_chunk(&mut **self.stream.borrow_mut(), n)?);
            self.remaining -= n as u64;
            display(
                "write",
                self.target.name(),
                self.total - self.remaining,
                self.total,
            );
        }
        self.pending.read(out)
    }
}
fn verification_progress(
    stream: &mut impl Write,
    phase: &str,
    name: &str,
    done: u64,
    total: u64,
) -> io::Result<()> {
    display("verify", name, done, total);
    send(
        stream,
        serde_json::json!({"event":"verify_progress", "phase":phase,
        "target":name, "done":done, "total":total}),
    )
}

struct HostJournal<'a, S> {
    stream: Shared<'a, S>,
}
impl<S: Read + Write> Journal for HostJournal<'_, S> {
    fn verification_progress(&mut self, target: Target, done: u64, total: u64) -> io::Result<()> {
        verification_progress(
            &mut **self.stream.borrow_mut(),
            "write",
            target.name(),
            done,
            total,
        )
    }
    fn acknowledge(&mut self, phase: Phase) -> io::Result<()> {
        let (event, name) = match phase {
            Phase::Writing(t) => ("writing", t.name()),
            Phase::Synced(t) => ("synced", t.name()),
            Phase::Verified(t) => ("verified", t.name()),
            Phase::Expanding(t) => ("expanding", t.name()),
            Phase::Expanded(t) => ("expanded", t.name()),
            Phase::Complete => ("complete", "none"),
        };
        let mut stream = self.stream.borrow_mut();
        if event == "synced" {
            display("verify", name, 0, 0);
        }
        send(
            &mut **stream,
            serde_json::json!({"event":event,"target":name}),
        )?;
        acknowledge(&mut **stream, event, name, "")
    }
}
pub fn session(stream: &mut (impl Read + Write)) -> io::Result<()> {
    ensure(
        !ATTEMPTED.swap(true, Ordering::AcqRel),
        "installation attempt already consumed",
    )?;
    let binding = BINDING
        .lock()
        .map_err(|_| invalid("binding lock"))?
        .take()
        .ok_or_else(|| invalid("USB plan binding missing"))?;
    let bytes = zeroize::Zeroizing::new(read_json_bytes(stream)?);
    ensure(
        hex(&Sha256::digest(&bytes)) == binding.plan_sha256,
        "plan differs from USB commitment",
    )?;
    let wire: WirePlan = serde_json::from_slice(&bytes).map_err(|_| invalid("invalid plan"))?;
    ensure(
        wire.schema == 1 && wire.nonce == binding.nonce,
        "session nonce mismatch",
    )?;
    execute(stream, wire, block::Disk::new)
}
trait InstallDisk: Storage {
    fn configure_network(
        &mut self,
        network: Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> io::Result<()> {
        ensure(network.is_none(), "stage network configuration unsupported")
    }
    fn verify_identity(&mut self) -> io::Result<()>;
    fn hash_name(&self, name: &str) -> io::Result<Hash>;
    fn hash_name_progress(
        &self,
        name: &str,
        size: u64,
        progress: &mut dyn FnMut(u64, u64) -> io::Result<()>,
    ) -> io::Result<Hash> {
        progress(0, size)?;
        let hash = self.hash_name(name)?;
        progress(size, size)?;
        Ok(hash)
    }
    fn read_name(&self, name: &str) -> io::Result<std::fs::File>;
}
impl InstallDisk for block::Disk {
    fn configure_network(
        &mut self,
        network: Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> io::Result<()> {
        block::Disk::configure_network(self, network)
    }
    fn hash_name_progress(
        &self,
        name: &str,
        _size: u64,
        progress: &mut dyn FnMut(u64, u64) -> io::Result<()>,
    ) -> io::Result<Hash> {
        block::Disk::hash_name_progress(self, name, progress)
    }
    fn verify_identity(&mut self) -> io::Result<()> {
        block::Disk::verify_identity(self)
    }
    fn hash_name(&self, name: &str) -> io::Result<Hash> {
        block::Disk::hash_name(self, name)
    }
    fn read_name(&self, name: &str) -> io::Result<std::fs::File> {
        block::Disk::read_name(self, name)
    }
}
fn execute<D: InstallDisk>(
    stream: &mut (impl Read + Write),
    wire: WirePlan,
    make_disk: impl FnOnce(Identity, BTreeMap<String, String>) -> io::Result<D>,
) -> io::Result<()> {
    hash(&wire.manifest_sha256)?;
    hash(&wire.stage_sha256)?;
    hash(&wire.original_boot_sha256)?;
    ensure(wire.cid.len() == 32, "invalid CID")?;
    let cid_full = hash(&(wire.cid.clone() + &"0".repeat(32)))?;
    let identity = Identity {
        cid: cid_full[..16].try_into().unwrap(),
        capacity: wire.capacity,
        partitions: wire
            .partitions
            .iter()
            .map(|(n, r)| {
                (
                    n.clone(),
                    Region {
                        offset: r.offset,
                        size: r.size,
                    },
                )
            })
            .collect(),
    };
    ensure(
        wire.identity_sha256.len() == 5
            && IDENTITY
                .iter()
                .all(|n| wire.identity_sha256.contains_key(*n) && wire.partitions.contains_key(*n)),
        "identity inventory incomplete",
    )?;
    let mut images = BTreeMap::new();
    for (name, image) in &wire.images {
        images.insert(
            target(name)?,
            Image {
                size: image.size,
                sha256: hash(&image.sha256)?,
                chunks: image
                    .chunks
                    .iter()
                    .map(|h| hash(h))
                    .collect::<io::Result<Vec<_>>>()?,
            },
        );
    }
    ensure(
        [Target::Recovery, Target::Userdata, Target::Boot]
            .iter()
            .all(|t| images.contains_key(t)),
        "incomplete OS image set",
    )?;
    let network = wire
        .network
        .as_ref()
        .map(network::Network::encode)
        .transpose()?;
    if network.is_some() {
        ensure(
            wire.images
                .get("userdata")
                .ok_or_else(|| invalid("missing userdata image"))?
                .size
                < wire
                    .partitions
                    .get("userdata")
                    .ok_or_else(|| invalid("missing userdata partition"))?
                    .size,
            "stage network configuration requires compact userdata",
        )?;
    }
    let plan = Plan::new(identity.clone(), images)?;
    let mut disk = make_disk(identity.clone(), wire.identity_sha256.clone())?;
    disk.configure_network(network)?;
    disk.verify_identity()?;
    ensure(
        hex(&disk.hash_name("boot")?) == wire.stage_sha256,
        "running bootstrap boot differs from plan",
    )?;
    send(
        stream,
        serde_json::json!({"event":"bound","nonce":wire.nonce,"cid":wire.cid,"capacity":wire.capacity,"partitions":wire.partitions,"manifest_sha256":wire.manifest_sha256}),
    )?;
    // The original boot was backed up before bootstrap; never replace it with RAM-stage bytes.
    send(
        stream,
        serde_json::json!({"event":"original_boot","target":"boot","sha256":wire.original_boot_sha256}),
    )?;
    acknowledge(stream, "original_boot", "boot", &wire.original_boot_sha256)?;
    let mut names: Vec<String> = wire
        .images
        .keys()
        .chain(wire.identity_sha256.keys())
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    if !wire.reused_backups.is_empty() {
        ensure(
            !wire.skip_userdata_backup
                && wire.reused_backups.keys().cloned().collect::<Vec<_>>() == names,
            "reused backup inventory mismatch",
        )?;
        for checksum in wire.reused_backups.values() {
            hash(checksum)?;
        }
        ensure(
            wire.reused_backups["boot"] == wire.original_boot_sha256,
            "reused boot differs",
        )?;
        for name in IDENTITY {
            ensure(
                wire.reused_backups[name] == wire.identity_sha256[name],
                "reused calibration differs",
            )?;
        }
    }
    for name in names.iter().filter(|n| {
        wire.reused_backups.is_empty()
            && n.as_str() != "boot"
            && !(wire.skip_userdata_backup && n.as_str() == "userdata")
    }) {
        let size = identity.partitions[name].size;
        display("backup", name, 0, size);
        send(
            stream,
            serde_json::json!({"event":"backup","target":name,"size":size}),
        )?;
        let mut file = disk.read_name(name)?;
        let mut remaining = size;
        let mut buffer = vec![0; CHUNK];
        let mut digest = Sha256::new();
        while remaining > 0 {
            let n = remaining.min(CHUNK as u64) as usize;
            file.read_exact(&mut buffer[..n])?;
            digest.update(&buffer[..n]);
            send_chunk(stream, &buffer[..n])?;
            remaining -= n as u64;
            display("backup", name, size - remaining, size);
        }
        drop(file);
        stream.flush()?;
        send(
            stream,
            serde_json::json!({"event":"checking_backup","target":name}),
        )?;
        let observed = disk.hash_name_progress(name, size, &mut |done, total| {
            verification_progress(stream, "backup", name, done, total)
        })?;
        ensure(
            observed == Hash::from(digest.finalize()),
            "backup independent readback mismatch",
        )?;
        if let Some(expected) = wire.identity_sha256.get(name) {
            ensure(
                hex(&observed) == *expected,
                "identity changed during backup",
            )?;
        }
        send(
            stream,
            serde_json::json!({"event":"backup_verified","target":name,"sha256":hex(&observed)}),
        )?;
        acknowledge(stream, "backup_verified", name, &hex(&observed))?;
    }
    disk.verify_identity()?;
    send(stream, serde_json::json!({"event":"backups_complete"}))?;
    acknowledge(stream, "backups_complete", "none", "")?;
    let shared = Rc::new(RefCell::new(&mut *stream));
    let mut journal = HostJournal {
        stream: shared.clone(),
    };
    Transaction::default().run(&plan, &mut disk, &mut journal, |t| {
        Ok(Source {
            stream: shared.clone(),
            remaining: wire.images[t.name()].size,
            pending: io::Cursor::new(Vec::new()),
            target: t,
            total: wire.images[t.name()].size,
        })
    })?;
    drop(journal);
    drop(shared);
    disk.verify_identity()?;
    display("complete", "none", 0, 0);
    send(
        stream,
        serde_json::json!({"event":"installed","verified":true}),
    )?;
    let command = receive(stream)?;
    ensure(
        command == serde_json::json!({"action":"reboot"})
            || command == serde_json::json!({"action":"leave"}),
        "invalid final action",
    )?;
    if command["action"] == "reboot" {
        send(stream, serde_json::json!({"event":"rebooting"}))?;
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            unsafe {
                libc::sync();
                libc::reboot(libc::RB_AUTOBOOT);
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compression_roundtrip_and_expansion_bounds() {
        let raw = vec![0; CHUNK];
        let mut wire = Vec::new();
        send_chunk(&mut wire, &raw).unwrap();
        assert!(wire.len() < CHUNK / 10);
        assert_eq!(receive_chunk(&mut wire.as_slice(), CHUNK).unwrap(), raw);
        assert!(receive_chunk(&mut wire.as_slice(), CHUNK - 1).is_err());
        let mut damaged = wire.clone();
        damaged.pop();
        assert!(receive_chunk(&mut damaged.as_slice(), CHUNK).is_err());
    }
    #[test]
    fn bootloaders_and_identity_are_never_write_targets() {
        for name in ["lk", "lk2", "preloader_a", "nvram", "para", "../boot"] {
            assert!(target(name).is_err());
        }
    }
}
