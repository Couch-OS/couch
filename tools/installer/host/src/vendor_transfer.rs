//! Exact compiled owner inventory transfer, before the first partition write.
use crate::{
    session::{Phase, SessionGuard},
    stage::{Channel, CHUNK},
};
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Deserialize)]
struct Inventory {
    sha256: String,
    files: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    path: String,
    size: u64,
    sha256: String,
}
struct Input {
    record: Record,
    file: File,
    chunks: Vec<[u8; 32]>,
}
pub struct VendorTransfer {
    source: String,
    files: Vec<Input>,
    attempted: bool,
}
impl VendorTransfer {
    pub fn source_sha256(&self) -> &str {
        &self.source
    }
    pub fn send<S: Read + Write>(
        &mut self,
        channel: &mut Channel<S>,
        session: &mut SessionGuard,
        mut progress: impl FnMut(&str, &str, u64, u64) -> Result<()>,
    ) -> Result<()> {
        ensure!(!self.attempted, "owner transfer already attempted");
        self.attempted = true;
        ensure!(
            session.phase() == Phase::BackupsVerified,
            "owner transfer outside backups phase"
        );
        for input in &mut self.files {
            let record = &input.record;
            channel.expect(&json!({"event":"vendor_file","path":record.path,"size":record.size,"sha256":record.sha256}))?;
            input.file.seek(SeekFrom::Start(0))?;
            progress("vendor", &record.path, 0, record.size)?;
            let mut done = 0;
            for expected in &input.chunks {
                let mut bytes = vec![0; (record.size - done).min(CHUNK as u64) as usize];
                input.file.read_exact(&mut bytes)?;
                ensure!(
                    <[u8; 32]>::from(Sha256::digest(&bytes)) == *expected,
                    "owner input changed during transfer"
                );
                channel.send_chunk(&bytes)?;
                done += bytes.len() as u64;
                progress("vendor", &record.path, done, record.size)?;
            }
            ensure!(
                done == record.size && input.file.read(&mut [0; 1])? == 0,
                "owner input length changed"
            );
            channel.expect(
                &json!({"event":"vendor_received","path":record.path,"sha256":record.sha256}),
            )?;
            session.checkpoint(
                &json!({"event":"vendor_received","target":record.path,"sha256":record.sha256}),
            )?;
            channel.acknowledge("vendor_received", &record.path, &record.sha256)?;
        }
        channel.expect(&json!({"event":"vendor_inputs_verified","sha256":self.source}))?;
        session.checkpoint(&json!({"event":"vendor_inputs_verified","sha256":self.source}))?;
        channel.acknowledge("vendor_inputs_verified", "none", &self.source)
    }
}
pub fn prepare(prepared: &Path) -> Result<VendorTransfer> {
    let mut inventory: Inventory =
        serde_json::from_str(include_str!("../../pins/ha100_official_runtime.json"))?;
    inventory.files.sort_by(|a, b| a.path.cmp(&b.path));
    ensure!(
        inventory.files.len() == 33
            && inventory.files.iter().map(|r| r.size).sum::<u64>() <= 32 * 1024 * 1024,
        "invalid compiled owner inventory"
    );
    let mut files = Vec::new();
    for record in inventory.files {
        let path = prepared.join("vendor").join(&record.path);
        ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "owner input is not a regular file"
        );
        let mut file = File::open(path)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == record.size,
            "owner input size/type changed"
        );
        let mut digest = Sha256::new();
        let mut chunks = Vec::new();
        let mut remaining = record.size;
        while remaining > 0 {
            let mut bytes = vec![0; remaining.min(CHUNK as u64) as usize];
            file.read_exact(&mut bytes)?;
            digest.update(&bytes);
            chunks.push(Sha256::digest(&bytes).into());
            remaining -= bytes.len() as u64;
        }
        ensure!(
            file.read(&mut [0; 1])? == 0 && format!("{:x}", digest.finalize()) == record.sha256,
            "owner input digest changed"
        );
        file.seek(SeekFrom::Start(0))?;
        files.push(Input {
            record,
            file,
            chunks,
        });
    }
    Ok(VendorTransfer {
        source: inventory.sha256,
        files,
        attempted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    struct Peer {
        reads: Cursor<Vec<u8>>,
        writes: Vec<u8>,
    }
    impl Read for Peer {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.reads.read(b)
        }
    }
    impl Write for Peer {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn exact_events_unaligned_bytes_and_journal_failures_are_checked() {
        for failure in 0..3 {
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
                Phase::BackupsVerified,
            ] {
                session.transition(phase, &json!({})).unwrap();
            }
            let bytes = b"fixture";
            let hash = format!("{:x}", Sha256::digest(bytes));
            let path = root.path().join("file");
            fs::write(&path, bytes).unwrap();
            let mut transfer = VendorTransfer {
                source: "source".into(),
                attempted: false,
                files: vec![Input {
                    record: Record {
                        path: "system/bin/fixture".into(),
                        size: 7,
                        sha256: hash.clone(),
                    },
                    file: File::open(path).unwrap(),
                    chunks: vec![Sha256::digest(bytes).into()],
                }],
            };
            let mut wire = Vec::new();
            for event in [
                json!({"event":"vendor_file","path":if failure==1 {"unexpected"}else{"system/bin/fixture"},"size":7,"sha256":hash}),
                json!({"event":"vendor_received","path":"system/bin/fixture","sha256":hash}),
                json!({"event":"vendor_inputs_verified","sha256":"source"}),
            ] {
                let data = serde_json::to_vec(&event).unwrap();
                wire.extend_from_slice(&(data.len() as u32).to_le_bytes());
                wire.extend_from_slice(&data);
            }
            if failure == 2 {
                fs::write(session.path().join("event-00007.json"), b"preserve").unwrap();
            }
            let mut peer = Peer {
                reads: Cursor::new(wire),
                writes: Vec::new(),
            };
            let result = transfer.send(
                &mut Channel::authenticated(&mut peer),
                &mut session,
                |_, _, _, _| Ok(()),
            );
            assert_eq!(result.is_ok(), failure == 0);
            if failure == 0 {
                assert!(session.path().join("event-00008.json").is_file());
                let mut output = Channel::authenticated(Cursor::new(peer.writes.clone()));
                assert_eq!(output.receive_chunk(7).unwrap(), bytes);
                output.expect(&json!({"ack":"vendor_received","target":"system/bin/fixture","sha256":hash})).unwrap();
                output
                    .expect(
                        &json!({"ack":"vendor_inputs_verified","target":"none","sha256":"source"}),
                    )
                    .unwrap();
            } else {
                assert!(!String::from_utf8_lossy(&peer.writes).contains("vendor_received"));
            }
            assert!(transfer
                .send(
                    &mut Channel::authenticated(&mut peer),
                    &mut session,
                    |_, _, _, _| Ok(())
                )
                .is_err());
        }
    }
}
