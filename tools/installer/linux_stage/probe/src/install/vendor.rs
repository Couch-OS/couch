//! Owner-supplied bytes admitted only against the compiled official OTA inventory.
use super::{acknowledge, ensure, hash, hex, receive_chunk, send, CHUNK};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
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
fn inventory() -> io::Result<Inventory> {
    let mut value: Inventory =
        serde_json::from_str(include_str!("../../../../pins/ha100_official_runtime.json"))?;
    value.files.sort_by(|a, b| a.path.cmp(&b.path));
    ensure(
        value.files.len() == 33
            && value.files.iter().map(|f| f.size).sum::<u64>() <= 32 * 1024 * 1024,
        "invalid compiled vendor inventory",
    )?;
    Ok(value)
}
pub(super) fn validate(source: Option<&str>) -> io::Result<()> {
    if let Some(source) = source {
        ensure(
            source == inventory()?.sha256,
            "unsupported owner vendor source",
        )?;
    }
    Ok(())
}
/// Owns only a newly-created directory; dropping never removes preexisting state.
pub(super) struct Inputs {
    root: PathBuf,
    records: Vec<Record>,
}
impl Drop for Inputs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
pub(super) fn receive(stream: &mut (impl Read + Write), parent: &Path) -> io::Result<Inputs> {
    receive_inventory(stream, parent, inventory()?)
}
fn receive_inventory(
    stream: &mut (impl Read + Write),
    parent: &Path,
    inventory: Inventory,
) -> io::Result<Inputs> {
    let root = parent.join("owner-vendor");
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let inputs = Inputs {
        root,
        records: inventory.files,
    };
    for (index, record) in inputs.records.iter().enumerate() {
        send(
            stream,
            serde_json::json!({"event":"vendor_file","path":record.path,"size":record.size,"sha256":record.sha256}),
        )?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(inputs.root.join(index.to_string()))?;
        let mut remaining = record.size;
        let mut digest = Sha256::new();
        while remaining > 0 {
            let size = remaining.min(CHUNK as u64) as usize;
            let bytes = receive_chunk(stream, size)?;
            file.write_all(&bytes)?;
            digest.update(&bytes);
            remaining -= size as u64;
        }
        ensure(
            hex(&digest.finalize()) == record.sha256,
            "owner vendor file hash mismatch",
        )?;
        file.sync_all()?;
        send(
            stream,
            serde_json::json!({"event":"vendor_received","path":record.path,"sha256":record.sha256}),
        )?;
        acknowledge(stream, "vendor_received", &record.path, &record.sha256)?;
    }
    File::open(&inputs.root)?.sync_all()?;
    send(
        stream,
        serde_json::json!({"event":"vendor_inputs_verified","sha256":inventory.sha256}),
    )?;
    acknowledge(stream, "vendor_inputs_verified", "none", &inventory.sha256)?;
    Ok(inputs)
}
use std::os::unix::fs::DirBuilderExt;
impl Inputs {
    /// The base image has already passed its complete digest and ext4 checks.
    pub(super) fn configure(&self, image: &Path) -> io::Result<()> {
        let safe = self.root.to_str().is_some_and(|s| {
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        });
        ensure(safe, "unsafe private vendor path")?;
        let mut directories = std::collections::BTreeSet::new();
        for record in &self.records {
            let target = PathBuf::from("/opt/couch").join(&record.path);
            for parent in target.ancestors().skip(1) {
                if parent != Path::new("/") {
                    directories.insert(parent.to_path_buf());
                }
            }
        }
        for directory in directories {
            let path = directory.to_str().unwrap();
            let metadata = super::network::debugfs(&["-R", &format!("stat {path}")], image)?;
            if metadata.is_empty() {
                super::network::debugfs(&["-w", "-R", &format!("mkdir {path}")], image)?;
            }
            let metadata = super::network::debugfs(&["-R", &format!("stat {path}")], image)?;
            ensure(
                std::str::from_utf8(&metadata)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .is_some_and(|s| s.contains("Type: directory")),
                "vendor directory missing or not a directory",
            )?;
        }
        for (index, record) in self.records.iter().enumerate() {
            let input = self.root.join(index.to_string());
            // Recheck private input immediately before inserting it.
            verify(&input, record)?;
            let target = format!("/opt/couch/{}", record.path);
            let metadata = super::network::debugfs(&["-R", &format!("stat {target}")], image)?;
            ensure(
                metadata.is_empty(),
                "vendor overlay collides with base image",
            )?;
            super::network::debugfs(
                &["-w", "-R", &format!("write {} {target}", input.display())],
                image,
            )?;
            let mode = if record.path.contains("/bin/") {
                "0755"
            } else {
                "0644"
            };
            super::network::debugfs(
                &[
                    "-w",
                    "-R",
                    &format!("set_inode_field {target} mode 010{mode}"),
                ],
                image,
            )?;
            let metadata = super::network::debugfs(&["-R", &format!("stat {target}")], image)?;
            ensure(
                std::str::from_utf8(&metadata)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .is_some_and(|s| {
                        s.contains("Type: regular") && s.contains(&format!("Mode:  {mode}"))
                    }),
                "vendor mode mismatch",
            )?;
            let readback = self.root.join("readback");
            ensure(!readback.try_exists()?, "vendor readback collision")?;
            super::network::debugfs(
                &["-R", &format!("dump {target} {}", readback.display())],
                image,
            )?;
            verify(&readback, record)?;
            fs::remove_file(readback)?;
        }
        Ok(())
    }
}
fn verify(path: &Path, record: &Record) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure(
        metadata.is_file() && metadata.len() == record.size,
        "vendor file size/type mismatch",
    )?;
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    let mut remaining = record.size;
    while remaining > 0 {
        let size = remaining.min(bytes.len() as u64) as usize;
        file.read_exact(&mut bytes[..size])?;
        digest.update(&bytes[..size]);
        remaining -= size as u64;
    }
    ensure(
        digest.finalize().as_slice() == hash(&record.sha256)?,
        "vendor readback hash mismatch",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_inventory_is_bounded_sorted_and_unique() {
        let value = inventory().unwrap();
        assert!(value.files.windows(2).all(|p| p[0].path < p[1].path));
        assert!(value
            .files
            .iter()
            .all(|r| r.path.starts_with("system/") || r.path.starts_with("vendor/")));
        validate(Some(&value.sha256)).unwrap();
        assert!(validate(Some(&"0".repeat(64))).is_err());
        validate(None).unwrap();
    }
    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("couch-vendor-{name}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        root
    }
    fn record(path: &str, bytes: &[u8]) -> Record {
        Record {
            path: path.into(),
            size: bytes.len() as u64,
            sha256: hex(&Sha256::digest(bytes)),
        }
    }
    struct Peer {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Peer {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }
    impl Write for Peer {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn transfer_requires_hash_and_durable_ack_and_cleans_owned_files() {
        for failure in 0..3 {
            let root = fixture(&format!("transfer-{failure}"));
            let bytes = b"synthetic owner fixture";
            let record = record("system/bin/fixture", bytes);
            let digest = if failure == 2 {
                "0".repeat(64)
            } else {
                record.sha256.clone()
            };
            let mut wire = Vec::new();
            let mut sent = bytes.to_vec();
            if failure == 1 {
                sent[0] ^= 1;
            }
            super::super::send_chunk(&mut wire, &sent).unwrap();
            send(
                &mut wire,
                serde_json::json!({"ack":"vendor_received","target":record.path,"sha256":digest}),
            )
            .unwrap();
            send(&mut wire,serde_json::json!({"ack":"vendor_inputs_verified","target":"none","sha256":"fixture"})).unwrap();
            let mut peer = Peer {
                input: io::Cursor::new(wire),
                output: Vec::new(),
            };
            let result = receive_inventory(
                &mut peer,
                &root,
                Inventory {
                    sha256: "fixture".into(),
                    files: vec![record],
                },
            );
            if failure != 0 {
                assert!(result.is_err());
            } else {
                let inputs = result.unwrap();
                assert_eq!(fs::read(inputs.root.join("0")).unwrap(), bytes);
                drop(inputs);
            }
            assert!(!root.join("owner-vendor").exists());
            fs::remove_dir(root).unwrap();
        }
    }
    #[test]
    fn existing_input_directory_is_never_removed() {
        let root = fixture("collision");
        fs::create_dir(root.join("owner-vendor")).unwrap();
        fs::write(root.join("owner-vendor/keep"), b"keep").unwrap();
        let mut peer = Peer {
            input: io::Cursor::new(Vec::new()),
            output: Vec::new(),
        };
        assert!(receive_inventory(
            &mut peer,
            &root,
            Inventory {
                sha256: "fixture".into(),
                files: Vec::new()
            }
        )
        .is_err());
        assert_eq!(fs::read(root.join("owner-vendor/keep")).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn actual_ext4_overlay_readback_modes_and_collision_rejection() {
        let root = fixture("ext4");
        let image = root.join("image.ext4");
        File::create(&image)
            .unwrap()
            .set_len(16 * 1024 * 1024)
            .unwrap();
        assert!(std::process::Command::new("/usr/sbin/mke2fs")
            .args(["-q", "-t", "ext4", "-F"])
            .arg(&image)
            .status()
            .unwrap()
            .success());
        let input = root.join("owner-vendor");
        fs::create_dir(&input).unwrap();
        let bytes = b"synthetic ELF substitute";
        fs::write(input.join("0"), bytes).unwrap();
        let inputs = Inputs {
            root: input,
            records: vec![record("system/bin/fixture", bytes)],
        };
        inputs.configure(&image).unwrap();
        assert!(inputs.configure(&image).is_err());
        drop(inputs);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires explicit local owner inputs and a regular neutral ext4 fixture; never a device"]
    fn full_pinned_inventory_overlays_neutral_regular_ext4() {
        let source = PathBuf::from(std::env::var_os("COUCH_OWNER_VENDOR_FIXTURE").unwrap());
        let neutral = PathBuf::from(std::env::var_os("COUCH_NEUTRAL_IMAGE_FIXTURE").unwrap());
        assert!(fs::symlink_metadata(&neutral).unwrap().is_file());
        let root = fixture("full-inventory");
        let image = root.join("image.ext4");
        fs::copy(neutral, &image).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&image)
            .unwrap()
            .set_len(512 * 1024 * 1024)
            .unwrap();
        assert!(std::process::Command::new("/usr/sbin/resize2fs")
            .arg(&image)
            .output()
            .unwrap()
            .status
            .success());
        let input = root.join("owner-vendor");
        fs::DirBuilder::new().mode(0o700).create(&input).unwrap();
        let records = inventory().unwrap().files;
        for (index, record) in records.iter().enumerate() {
            let from = source.join(&record.path);
            verify(&from, record).unwrap();
            fs::copy(from, input.join(index.to_string())).unwrap();
        }
        let inputs = Inputs {
            root: input,
            records,
        };
        inputs.configure(&image).unwrap();
        assert!(std::process::Command::new("/usr/sbin/e2fsck")
            .args(["-fn"])
            .arg(&image)
            .output()
            .unwrap()
            .status
            .success());
        drop(inputs);
        fs::remove_dir_all(root).unwrap();
    }
}
