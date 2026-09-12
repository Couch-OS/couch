//! Native backup-first, boot-last transaction driver for a USB-bound stage plan.
//! Admission (signed release, enrollment proof and selected-device identity) is a
//! caller prerequisite. No discovery, bootstrap writes, retries or restoration
//! are performed here. The CLI does not currently expose this entry point.
use crate::{
    session::{Phase, SessionGuard},
    stage::{Channel, CHUNK},
    stage_files::{self, VerifiedImage},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const ORDER: [&str; 5] = ["recovery", "userdata", "logo", "odmdtbo", "boot"];
const IDENTITY: [&str; 5] = ["proinfo", "nvram", "nvdata", "protect1", "protect2"];
#[derive(Clone, Copy)]
pub enum OriginalOs {
    Android,
    Couch,
}
impl OriginalOs {
    fn name(self) -> &'static str {
        match self {
            Self::Android => "Android",
            Self::Couch => "Couch",
        }
    }
}
struct Image {
    size: u64,
    sha256: String,
    chunks: Vec<String>,
}
struct Validated {
    images: BTreeMap<String, Image>,
    sizes: BTreeMap<String, u64>,
    backups: BTreeSet<String>,
    original_boot: String,
}
fn checksum(value: &Value) -> Result<String> {
    let text = value.as_str().context("missing hash")?;
    ensure!(
        text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid hash"
    );
    Ok(text.into())
}
fn size(value: &Value) -> Result<u64> {
    let value = value.as_u64().context("missing byte count")?;
    ensure!(
        value > 0 && value <= 16 * 1024 * 1024 * 1024 && value.is_multiple_of(512),
        "invalid byte count"
    );
    Ok(value)
}
fn validate(plan: &Value, paths: &BTreeMap<String, PathBuf>) -> Result<Validated> {
    ensure!(plan["schema"] == 1, "unsupported plan schema");
    ensure!(
        plan.get("reused_backups")
            .is_none_or(|v| v.as_object().is_some_and(|v| v.is_empty())),
        "native reused-backup admission is not available"
    );
    let skip = plan
        .get("skip_userdata_backup")
        .map(|v| v.as_bool().context("invalid backup policy"))
        .transpose()?
        .unwrap_or(false);
    checksum(&plan["nonce"])?;
    checksum(&plan["manifest_sha256"])?;
    checksum(&plan["stage_sha256"])?;
    let cid = plan["cid"].as_str().context("missing storage identity")?;
    ensure!(
        cid.len() == 32
            && cid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && cid != "0".repeat(32)
            && cid != "f".repeat(32),
        "invalid storage identity"
    );
    let capacity = size(&plan["capacity"])?;
    let parts = plan["partitions"]
        .as_object()
        .context("missing partition inventory")?;
    let mut sizes = BTreeMap::new();
    let mut regions = Vec::new();
    for (name, region) in parts {
        let offset = region["offset"]
            .as_u64()
            .context("invalid partition offset")?;
        let count = size(&region["size"])?;
        let end = offset.checked_add(count).context("partition overflow")?;
        ensure!(
            offset.is_multiple_of(512) && end <= capacity,
            "partition outside device"
        );
        sizes.insert(name.clone(), count);
        regions.push((offset, end));
    }
    regions.sort();
    ensure!(
        regions.windows(2).all(|v| v[0].1 <= v[1].0),
        "overlapping partitions"
    );
    let wire_images = plan["images"]
        .as_object()
        .context("missing image inventory")?;
    ensure!(
        ["boot", "recovery", "userdata"]
            .iter()
            .all(|n| wire_images.contains_key(*n))
            && wire_images.keys().all(|n| ORDER.contains(&n.as_str())),
        "invalid image targets"
    );
    ensure!(
        paths.keys().collect::<BTreeSet<_>>() == wire_images.keys().collect::<BTreeSet<_>>(),
        "image path inventory differs"
    );
    let mut images = BTreeMap::new();
    for (name, image) in wire_images {
        let count = size(&image["size"])?;
        let partition = *sizes.get(name).context("image partition missing")?;
        ensure!(
            count.is_multiple_of(4096)
                && (count == partition || (name == "userdata" && count < partition)),
            "image does not fit approved partition"
        );
        let chunks = image["chunks"]
            .as_array()
            .context("missing image chunks")?
            .iter()
            .map(checksum)
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            chunks.len() as u64 == count.div_ceil(CHUNK as u64),
            "image chunk inventory differs"
        );
        images.insert(
            name.clone(),
            Image {
                size: count,
                sha256: checksum(&image["sha256"])?,
                chunks,
            },
        );
    }
    let identity = plan["identity_sha256"]
        .as_object()
        .context("missing calibration inventory")?;
    ensure!(
        identity.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == IDENTITY.into_iter().collect(),
        "calibration inventory differs"
    );
    for name in IDENTITY {
        ensure!(sizes.contains_key(name), "missing calibration partition");
        checksum(&identity[name])?;
    }
    let mut backups: BTreeSet<String> = images
        .keys()
        .cloned()
        .chain(IDENTITY.map(String::from))
        .collect();
    if skip {
        backups.remove("userdata");
    }
    Ok(Validated {
        images,
        sizes,
        backups,
        original_boot: checksum(&plan["original_boot_sha256"])?,
    })
}
fn copy_boot(source: &Path, destination: &Path, size: u64, expected: &str) -> Result<()> {
    ensure!(
        fs::symlink_metadata(source)?.file_type().is_file(),
        "expected regular original boot"
    );
    let mut source = File::open(source)?;
    ensure!(
        source.metadata()?.len() == size,
        "original boot size differs"
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(destination)?;
    let mut bytes = vec![0; CHUNK];
    let mut hash = Sha256::new();
    let mut done = 0;
    loop {
        let n = source.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        ensure!(done <= size, "original boot grew");
        hash.update(&bytes[..n]);
        output.write_all(&bytes[..n])?;
    }
    output.sync_all()?;
    drop(output);
    ensure!(
        done == size && format!("{:x}", hash.finalize()) == expected,
        "original boot hash differs"
    );
    let mut file = File::open(destination)?;
    let mut hash = Sha256::new();
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == expected,
        "saved original boot readback differs"
    );
    Ok(())
}

/// Returns only after the stage's final identity check and a durable completion
/// journal. The caller may then explicitly choose `reboot` or `leave`; no restart
/// is issued automatically. Errors preserve originals and poison the session.
pub fn run<S: Read + Write>(
    channel: &mut Channel<S>,
    plan: &Value,
    paths: &BTreeMap<String, PathBuf>,
    original_boot: &Path,
    original_os: OriginalOs,
    session: &mut SessionGuard,
    mut progress: impl FnMut(&str, &str, u64, u64) -> Result<()>,
) -> Result<()> {
    let result = (|| {
        ensure!(
            session.phase() == Phase::StageConnected,
            "transaction requires selected authenticated stage"
        );
        let valid = validate(plan, paths)?;
        let mut images = BTreeMap::new();
        for (name, image) in &valid.images {
            images.insert(
                name.clone(),
                VerifiedImage::open(&paths[name], image.size, &image.sha256, &image.chunks)?,
            );
        }
        let backups = session.path().to_path_buf();
        copy_boot(
            original_boot,
            &backups.join("original-boot.img"),
            valid.sizes["boot"],
            &valid.original_boot,
        )?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(plan)?);
        session.checkpoint(&json!({"event":"transaction_prepared","original_os":original_os.name(),"plan_sha256":format!("{:x}",Sha256::digest(&bytes))}))?;
        channel.begin_install(plan)?;
        let mut bound = serde_json::Map::new();
        bound.insert("event".into(), json!("bound"));
        for key in ["nonce", "cid", "capacity", "partitions", "manifest_sha256"] {
            bound.insert(key.into(), plan[key].clone());
        }
        channel.expect(&Value::Object(bound))?;
        channel.expect(
            &json!({"event":"original_boot","target":"boot","sha256":valid.original_boot}),
        )?;
        session.checkpoint(
            &json!({"event":"backup_verified","target":"boot","sha256":valid.original_boot}),
        )?;
        channel.acknowledge("original_boot", "boot", &valid.original_boot)?;
        for name in valid.backups.iter().filter(|n| n.as_str() != "boot") {
            let identity = plan["identity_sha256"].get(name).and_then(Value::as_str);
            let hash = stage_files::save_backup(
                channel,
                name,
                valid.sizes[name],
                &backups.join(format!("original-{name}.img")),
                identity,
                |done, total| progress("backup", name, done, total),
            )?;
            session.checkpoint(&json!({"event":"backup_verified","target":name,"sha256":hash}))?;
            channel.acknowledge("backup_verified", name, &hash)?;
        }
        channel.expect(&json!({"event":"backups_complete"}))?;
        session.transition(Phase::BackupsVerified, &json!({"event":"backups_complete"}))?;
        channel.acknowledge("backups_complete", "none", "")?;
        session.transition(Phase::Writing, &json!({"event":"write_sequence_started"}))?;
        for name in ORDER.into_iter().filter(|n| valid.images.contains_key(*n)) {
            let image = images.get_mut(name).unwrap();
            let spec = &valid.images[name];
            channel.expect(&json!({"event":"writing","target":name}))?;
            image.reverify()?;
            session.checkpoint(&json!({"event":"writing","target":name}))?;
            channel.acknowledge("writing", name, "")?;
            image.send(channel, |done, total| progress("write", name, done, total))?;
            let mut phases = vec!["synced", "verified"];
            if name == "userdata" && spec.size < valid.sizes[name] {
                phases.extend(["expanding", "expanded"]);
            }
            for phase in phases {
                let event = if phase == "verified" {
                    channel.verification(name, "write", spec.size, |done, total| {
                        progress("verify", name, done, total)
                    })?
                } else {
                    channel.receive_json()?
                };
                ensure!(
                    event == json!({"event":phase,"target":name}),
                    "unexpected storage verification phase"
                );
                session.checkpoint(&event)?;
                channel.acknowledge(phase, name, "")?;
            }
        }
        channel.expect(&json!({"event":"complete","target":"none"}))?;
        session.transition(
            Phase::Verifying,
            &json!({"event":"all_partitions_verified"}),
        )?;
        channel.acknowledge("complete", "none", "")?;
        channel.expect(&json!({"event":"installed","verified":true}))?;
        session.finish()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = session.transition(
            Phase::Failed,
            &json!({"event":"transaction_stopped","preserve_originals":true}),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};
    struct Wire {
        incoming: Cursor<Vec<u8>>,
        session: PathBuf,
        acks: Vec<Value>,
        fail_at: Option<u64>,
    }
    impl Read for Wire {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self
                .fail_at
                .is_some_and(|at| self.incoming.position() >= at)
            {
                self.fail_at = None;
                let next = fs::read_dir(&self.session)?
                    .filter_map(|v| v.ok())
                    .filter(|v| v.file_name().to_string_lossy().starts_with("event-"))
                    .count();
                fs::write(self.session.join(format!("event-{next:05}.json")), b"{}")?;
            }
            self.incoming.read(bytes)
        }
    }
    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
                if value.get("ack").is_some() {
                    let mut events = fs::read_dir(&self.session)?
                        .filter_map(|v| v.ok())
                        .map(|v| v.path())
                        .filter(|p| {
                            p.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with("event-")
                        })
                        .collect::<Vec<_>>();
                    events.sort();
                    let event: Value =
                        serde_json::from_slice(&fs::read(events.last().unwrap())?).unwrap();
                    let ack = value["ack"].as_str().unwrap();
                    if ack == "complete" {
                        assert_eq!(event["phase"], "verifying");
                    } else if ack == "backups_complete" {
                        assert_eq!(event["phase"], "backups_verified");
                    } else {
                        let expected = if ack == "original_boot" {
                            "backup_verified"
                        } else {
                            ack
                        };
                        assert_eq!(event["evidence"]["event"], expected);
                        assert_eq!(event["evidence"]["target"], value["target"]);
                    }
                    self.acks.push(value);
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn event(bytes: &mut Vec<u8>, value: Value) {
        let data = serde_json::to_vec(&value).unwrap();
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(data);
    }
    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn exercise(
        bad_expansion: bool,
        bad_journal: bool,
        skip_userdata: bool,
        disconnected: bool,
    ) -> (bool, Phase, Vec<Value>) {
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut session = SessionGuard::create(&root.path().join("run")).unwrap();
        for phase in [
            Phase::InputsVerified,
            Phase::AndroidBound,
            Phase::OriginalsSaved,
            Phase::StageBootPending,
            Phase::StageConnected,
        ] {
            session.transition(phase, &json!({})).unwrap();
        }
        let names = IDENTITY
            .into_iter()
            .chain(["boot", "recovery", "userdata"])
            .collect::<BTreeSet<_>>();
        let mut partitions = serde_json::Map::new();
        let mut originals = BTreeMap::new();
        let mut offset = 4096;
        for name in names {
            let size = if name == "userdata" { 8192 } else { 4096 };
            partitions.insert(name.into(), json!({"offset":offset,"size":size}));
            offset += size;
            originals.insert(name, vec![name.as_bytes()[0]; size as usize]);
        }
        let mut images = serde_json::Map::new();
        let mut paths = BTreeMap::new();
        for (name, byte) in [("recovery", 1), ("userdata", 2), ("boot", 3)] {
            let data = vec![byte; 4096];
            let path = root.path().join(format!("{name}.image"));
            fs::write(&path, &data).unwrap();
            paths.insert(name.into(), path);
            images.insert(
                name.into(),
                json!({"size":4096,"sha256":hash(&data),"chunks":[hash(&data)]}),
            );
        }
        let identity = IDENTITY
            .into_iter()
            .map(|name| (name.to_string(), json!(hash(&originals[name]))))
            .collect::<serde_json::Map<_, _>>();
        let boot = root.path().join("original-boot");
        fs::write(&boot, &originals["boot"]).unwrap();
        let plan = json!({"schema":1,"skip_userdata_backup":skip_userdata,"nonce":"a".repeat(64),"manifest_sha256":"b".repeat(64),"stage_sha256":"c".repeat(64),"cid":"12".repeat(16),"capacity":offset+4096,"partitions":partitions,"images":images,"identity_sha256":identity,"original_boot_sha256":hash(&originals["boot"])});
        let mut incoming = Vec::new();
        let mut bound = json!({"event":"bound"});
        for key in ["nonce", "cid", "capacity", "partitions", "manifest_sha256"] {
            bound[key] = plan[key].clone();
        }
        event(&mut incoming, bound);
        event(
            &mut incoming,
            json!({"event":"original_boot","target":"boot","sha256":plan["original_boot_sha256"]}),
        );
        let mut fail_at = None;
        for name in validate(&plan, &paths)
            .unwrap()
            .backups
            .into_iter()
            .filter(|n| n != "boot")
        {
            let data = &originals[name.as_str()];
            event(
                &mut incoming,
                json!({"event":"backup","target":name,"size":data.len()}),
            );
            incoming.extend((data.len() as u32).to_le_bytes());
            incoming.extend((data.len() as u32).to_le_bytes());
            incoming.extend(0u32.to_le_bytes());
            incoming.extend(data);
            event(
                &mut incoming,
                json!({"event":"checking_backup","target":name}),
            );
            if bad_journal && fail_at.is_none() {
                fail_at = Some(incoming.len() as u64);
            }
            event(
                &mut incoming,
                json!({"event":"backup_verified","target":name,"sha256":hash(data)}),
            );
        }
        event(&mut incoming, json!({"event":"backups_complete"}));
        for name in ["recovery", "userdata", "boot"] {
            event(&mut incoming, json!({"event":"writing","target":name}));
            for phase in ["synced", "verified"] {
                event(&mut incoming, json!({"event":phase,"target":name}));
            }
            if name == "userdata" {
                event(&mut incoming, json!({"event":"expanding","target":name}));
                event(
                    &mut incoming,
                    if bad_expansion {
                        json!({"event":"error"})
                    } else {
                        json!({"event":"expanded","target":name})
                    },
                );
            }
        }
        event(&mut incoming, json!({"event":"complete","target":"none"}));
        event(&mut incoming, json!({"event":"installed","verified":true}));
        let mut channel = Channel::authenticated(Wire {
            incoming: Cursor::new(incoming),
            session: session.path().to_path_buf(),
            acks: Vec::new(),
            fail_at,
        });
        let ok = run(
            &mut channel,
            &plan,
            &paths,
            &boot,
            OriginalOs::Android,
            &mut session,
            |phase, target, _, _| {
                ensure!(
                    !(disconnected && phase == "write" && target == "userdata"),
                    "UI disconnected"
                );
                Ok(())
            },
        )
        .is_ok();
        // Access the fixture's output through a test-only channel accessor.
        let wire = channel.into_inner();
        (ok, session.phase(), wire.acks)
    }
    #[test]
    fn full_transaction_journals_every_ack_and_writes_boot_last() {
        let (ok, phase, acks) = exercise(false, false, false, false);
        assert!(ok);
        assert_eq!(phase, Phase::Complete);
        let writes = acks
            .iter()
            .filter(|v| v["ack"] == "writing")
            .map(|v| v["target"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(writes, ["recovery", "userdata", "boot"]);
    }
    #[test]
    fn explicit_yolo_skips_only_userdata_backup() {
        let (ok, phase, acks) = exercise(false, false, true, false);
        assert!(ok);
        assert_eq!(phase, Phase::Complete);
        assert!(!acks
            .iter()
            .any(|v| v["ack"] == "backup_verified" && v["target"] == "userdata"));
        for target in IDENTITY {
            assert!(acks
                .iter()
                .any(|v| v["ack"] == "backup_verified" && v["target"] == target));
        }
        assert!(acks
            .iter()
            .any(|v| v["ack"] == "writing" && v["target"] == "userdata"));
    }
    #[test]
    fn ui_disconnect_stops_before_userdata_transfer_and_final_boot() {
        let (ok, phase, acks) = exercise(false, false, false, true);
        assert!(!ok);
        assert_eq!(phase, Phase::Failed);
        assert!(!acks
            .iter()
            .any(|v| v["ack"] == "synced" && v["target"] == "userdata"));
        assert!(!acks
            .iter()
            .any(|v| v["ack"] == "writing" && v["target"] == "boot"));
    }
    #[test]
    fn failed_expansion_blocks_final_boot_and_preserves_failed_session() {
        let (ok, phase, acks) = exercise(true, false, false, false);
        assert!(!ok);
        assert_eq!(phase, Phase::Failed);
        assert!(!acks
            .iter()
            .any(|v| v["ack"] == "writing" && v["target"] == "boot"));
    }
    #[test]
    fn failed_journal_checkpoint_never_acknowledges_backup_or_starts_writes() {
        let (ok, _, acks) = exercise(false, true, false, false);
        assert!(!ok);
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0]["ack"], "original_boot");
    }
}
