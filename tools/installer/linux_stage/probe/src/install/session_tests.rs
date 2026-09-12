use super::*;
use couch_installer_storage::{direct, Observation};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    identity: Identity,
    writer: Option<File>,
    writes: Arc<Mutex<Vec<String>>>,
    bad_readback: bool,
    bad_expand: bool,
    network: bool,
    bad_network: bool,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl InstallDisk for Fixture {
    fn configure_network(
        &mut self,
        network: Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> io::Result<()> {
        self.network = network.is_some();
        Ok(())
    }
    fn verify_identity(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn hash_name(&self, name: &str) -> io::Result<Hash> {
        if self.bad_readback
            && name == "userdata"
            && self.writes.lock().unwrap().iter().any(|n| n == name)
        {
            return Ok([0; 32]);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(self.root.join(name))?;
        direct::hash(&file, self.identity.partitions[name].size)
    }
    fn read_name(&self, name: &str) -> io::Result<File> {
        File::open(self.root.join(name))
    }
}
impl Storage for Fixture {
    fn observe(&mut self) -> io::Result<Observation> {
        Ok(Observation {
            identity: self.identity.clone(),
            mounted: BTreeSet::new(),
        })
    }
    fn begin(&mut self, t: Target) -> io::Result<()> {
        self.writes.lock().unwrap().push(t.name().to_string());
        self.writer = Some(
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(self.root.join(t.name()))?,
        );
        Ok(())
    }
    fn append(&mut self, b: &[u8]) -> io::Result<()> {
        self.writer.as_mut().unwrap().write_all(b)
    }
    fn sync_close(&mut self) -> io::Result<()> {
        let file = self.writer.take().unwrap();
        file.sync_all()?;
        drop(file);
        Ok(())
    }
    fn direct_hash(&mut self, t: Target, n: u64) -> io::Result<Hash> {
        assert!(self.writer.is_none());
        if self.bad_readback && t == Target::Userdata {
            return Ok([0; 32]);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(self.root.join(t.name()))?;
        direct::hash(&file, n)
    }
    fn expand_userdata(&mut self, image_size: u64, capacity: u64) -> io::Result<()> {
        assert!(self.writer.is_none() && image_size < capacity);
        if self.bad_expand {
            return Err(invalid("fixture expansion failed"));
        }
        self.writes.lock().unwrap().push("expand_userdata".into());
        if self.network {
            if self.bad_network {
                return Err(invalid("fixture network readback failed"));
            }
            self.writes
                .lock()
                .unwrap()
                .push("network_configured".into());
        }
        Ok(())
    }
    fn abort(&mut self) {
        self.writer.take();
    }
}
fn exercise(corrupt: bool, bad_readback: bool, drop_before_write: bool) -> (bool, Vec<String>) {
    exercise_options(
        corrupt,
        bad_readback,
        drop_before_write,
        false,
        false,
        false,
        false,
    )
}
fn exercise_options(
    corrupt: bool,
    bad_readback: bool,
    drop_before_write: bool,
    yolo: bool,
    compact: bool,
    bad_expand: bool,
    reuse: bool,
) -> (bool, Vec<String>) {
    exercise_network(
        corrupt,
        bad_readback,
        drop_before_write,
        yolo,
        compact,
        bad_expand,
        reuse,
        false,
        false,
    )
}
#[allow(clippy::too_many_arguments)]
fn exercise_network(
    corrupt: bool,
    bad_readback: bool,
    drop_before_write: bool,
    yolo: bool,
    compact: bool,
    bad_expand: bool,
    reuse: bool,
    network: bool,
    bad_network: bool,
) -> (bool, Vec<String>) {
    let root = std::env::temp_dir().join(format!(
        "couch-wifi-session-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let names = [
        "boot", "recovery", "userdata", "nvram", "nvdata", "proinfo", "protect1", "protect2",
    ];
    let mut partitions = BTreeMap::new();
    let mut images = BTreeMap::new();
    let mut originals = BTreeMap::new();
    let mut image_data = BTreeMap::new();
    let mut offset = 4096;
    for name in names {
        let size = if name == "userdata" {
            CHUNK + 4096
        } else {
            4096
        };
        let old = vec![b'O'; size];
        fs::write(root.join(name), &old).unwrap();
        originals.insert(name.to_string(), hex(&Sha256::digest(&old)));
        partitions.insert(
            name.to_string(),
            WireRegion {
                offset,
                size: size as u64,
            },
        );
        offset += size as u64;
        if target(name).is_ok() {
            let image_size = if compact && name == "userdata" {
                4096
            } else {
                size
            };
            let raw = vec![b'N'; image_size];
            images.insert(
                name.to_string(),
                WireImage {
                    size: image_size as u64,
                    sha256: hex(&Sha256::digest(&raw)),
                    chunks: raw.chunks(CHUNK).map(|c| hex(&Sha256::digest(c))).collect(),
                },
            );
            image_data.insert(name.to_string(), raw);
        }
    }
    let wire = WirePlan {
        network: network.then(|| network::Network {
            ssid_hex: "61".into(),
            psk_hex: None,
        }),
        schema: 1,
        skip_userdata_backup: yolo,
        reused_backups: if reuse {
            originals.clone()
        } else {
            BTreeMap::new()
        },
        nonce: "a".repeat(64),
        manifest_sha256: "b".repeat(64),
        cid: "12".repeat(16),
        capacity: offset + 4096,
        partitions,
        images,
        identity_sha256: IDENTITY
            .iter()
            .map(|n| (n.to_string(), originals[*n].clone()))
            .collect(),
        original_boot_sha256: originals["boot"].clone(),
        stage_sha256: originals["boot"].clone(),
    };
    let writes = Arc::new(Mutex::new(Vec::new()));
    let observed = writes.clone();
    let (mut host, mut server) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        execute(&mut server, wire, |identity, _| {
            Ok(Fixture {
                root,
                identity,
                writer: None,
                writes: observed,
                bad_readback,
                bad_expand,
                network: false,
                bad_network,
            })
        })
        .is_ok()
    });
    let host_result = (|| -> io::Result<()> {
        ensure(receive(&mut host)?["event"] == "bound", "bound")?;
        let event = receive(&mut host)?;
        send(
            &mut host,
            serde_json::json!({"ack":"original_boot","target":"boot","sha256":event["sha256"]}),
        )?;
        loop {
            let event = receive(&mut host)?;
            let kind = event["event"].as_str().unwrap();
            if kind == "backups_complete" {
                if drop_before_write {
                    return Ok(());
                }
                send(
                    &mut host,
                    serde_json::json!({"ack":kind,"target":"none","sha256":""}),
                )?;
                break;
            }
            ensure(!reuse && kind == "backup", "backup order")?;
            let name = event["target"].as_str().unwrap();
            ensure(
                !(yolo && name == "userdata"),
                "YOLO must omit userdata backup",
            )?;
            let mut left = event["size"].as_u64().unwrap();
            while left > 0 {
                let n = left.min(CHUNK as u64) as usize;
                let raw = receive_chunk(&mut host, n)?;
                ensure(raw == vec![b'O'; n], "backup content")?;
                left -= n as u64;
            }
            ensure(
                receive(&mut host)?["event"] == "checking_backup",
                "checking backup",
            )?;
            for done in [0, event["size"].as_u64().unwrap()] {
                ensure(
                    receive(&mut host)?
                        == serde_json::json!({"event":"verify_progress",
                    "phase":"backup", "target":name, "done":done, "total":event["size"]}),
                    "backup progress",
                )?;
            }
            let checked = receive(&mut host)?;
            ensure(checked["sha256"] == originals[name], "backup hash")?;
            send(
                &mut host,
                serde_json::json!({"ack":"backup_verified","target":name,"sha256":checked["sha256"]}),
            )?;
        }
        for name in ["recovery", "userdata", "boot"] {
            ensure(
                receive(&mut host)? == serde_json::json!({"event":"writing","target":name}),
                "write order",
            )?;
            send(
                &mut host,
                serde_json::json!({"ack":"writing","target":name,"sha256":""}),
            )?;
            for chunk in image_data[name].chunks(CHUNK) {
                let mut raw = chunk.to_vec();
                if corrupt {
                    raw[0] ^= 1;
                }
                send_chunk(&mut host, &raw)?;
            }
            host.flush()?;
            let mut phases = vec!["synced", "verified"];
            if compact && name == "userdata" {
                phases.extend(["expanding", "expanded"]);
            }
            for phase in phases {
                if phase == "verified" {
                    let total = image_data[name].len() as u64;
                    for done in [0, total] {
                        ensure(
                            receive(&mut host)?
                                == serde_json::json!({"event":"verify_progress",
                            "phase":"write", "target":name, "done":done, "total":total}),
                            "write verification progress",
                        )?;
                    }
                }
                ensure(
                    receive(&mut host)? == serde_json::json!({"event":phase,"target":name}),
                    "phase",
                )?;
                send(
                    &mut host,
                    serde_json::json!({"ack":phase,"target":name,"sha256":""}),
                )?;
            }
        }
        ensure(receive(&mut host)?["event"] == "complete", "complete")?;
        send(
            &mut host,
            serde_json::json!({"ack":"complete","target":"none","sha256":""}),
        )?;
        ensure(receive(&mut host)?["event"] == "installed", "installed")?;
        send(&mut host, serde_json::json!({"action":"leave"}))
    })();
    drop(host);
    let success = worker.join().unwrap();
    if success {
        assert!(host_result.is_ok());
    }
    let result = writes.lock().unwrap().clone();
    (success, result)
}
#[test]
fn full_device_protocol_closes_writers_and_verifies_before_boot_last() {
    let (ok, writes) = exercise(false, false, false);
    assert!(ok);
    assert_eq!(writes, ["recovery", "userdata", "boot"]);
}
#[test]
fn corrupt_incoming_chunk_never_reaches_final_boot() {
    let (ok, writes) = exercise(true, false, false);
    assert!(!ok);
    assert!(!writes.iter().any(|n| n == "boot"));
}
#[test]
fn failed_independent_readback_never_reaches_final_boot() {
    let (ok, writes) = exercise(false, true, false);
    assert!(!ok);
    assert_eq!(writes, ["recovery", "userdata"]);
}
#[test]
fn missing_durable_backup_ack_prevents_all_writes() {
    let (ok, writes) = exercise(false, false, true);
    assert!(!ok);
    assert!(writes.is_empty());
}

#[test]
fn compact_userdata_is_verified_and_expanded_before_boot() {
    let (ok, writes) = exercise_options(false, false, false, false, true, false, false);
    assert!(ok);
    assert_eq!(writes, ["recovery", "userdata", "expand_userdata", "boot"]);
}
#[test]
fn failed_expansion_prevents_boot_write() {
    let (ok, writes) = exercise_options(false, false, false, false, true, true, false);
    assert!(!ok);
    assert_eq!(writes, ["recovery", "userdata"]);
}
#[test]
fn yolo_skips_userdata_backup_but_keeps_write_verification_and_expansion() {
    let (ok, writes) = exercise_options(false, false, false, true, true, false, false);
    assert!(ok);
    assert_eq!(writes, ["recovery", "userdata", "expand_userdata", "boot"]);
}

#[test]
fn restart_reuses_originals_but_still_verifies_compact_install() {
    let (ok, writes) = exercise_options(false, false, false, false, true, false, true);
    assert!(ok);
    assert_eq!(writes, ["recovery", "userdata", "expand_userdata", "boot"]);
}

#[test]
fn network_readback_is_required_before_final_boot() {
    let (ok, writes) =
        exercise_network(false, false, false, false, true, false, false, true, false);
    assert!(ok);
    let configured = writes
        .iter()
        .position(|s| s == "network_configured")
        .unwrap();
    let boot = writes.iter().position(|s| s == "boot").unwrap();
    assert!(configured < boot);
    let (ok, writes) = exercise_network(false, false, false, false, true, false, false, true, true);
    assert!(!ok);
    assert!(!writes.iter().any(|s| s == "boot"));
}
#[test]
fn network_customization_rejects_full_restore_images_before_writes() {
    let (ok, writes) =
        exercise_network(false, false, false, false, false, false, false, true, false);
    assert!(!ok);
    assert!(writes.is_empty());
}
