//! Host file verification for the authenticated stage protocol. Saving a backup
//! deliberately does not acknowledge it: orchestration must durably journal the
//! returned hash before permitting the device to continue.
use crate::stage::{Channel, CHUNK};
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = vec![0; CHUNK];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", hash.finalize()))
}
pub struct VerifiedImage {
    file: File,
    size: u64,
    chunks: Vec<String>,
    hash: String,
}
impl VerifiedImage {
    pub fn open(path: &Path, size: u64, expected_hash: &str, chunks: &[String]) -> Result<Self> {
        ensure!(
            size > 0
                && size <= 16 * 1024 * 1024 * 1024
                && valid_hash(expected_hash)
                && chunks.len() as u64 == size.div_ceil(CHUNK as u64)
                && chunks.iter().all(|h| valid_hash(h)),
            "invalid image inventory"
        );
        ensure!(
            fs::symlink_metadata(path)?.file_type().is_file(),
            "expected regular image file"
        );
        let mut file = File::open(path)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == size,
            "image size/type changed"
        );
        ensure!(
            digest(&mut file)? == expected_hash,
            "image hash differs from approved plan"
        );
        Ok(Self {
            file,
            size,
            chunks: chunks.to_vec(),
            hash: expected_hash.into(),
        })
    }
    pub fn reverify(&mut self) -> Result<()> {
        ensure!(
            self.file.metadata()?.len() == self.size && digest(&mut self.file)? == self.hash,
            "image changed before writing"
        );
        Ok(())
    }
    /// Call only after the writing phase is durably journaled and acknowledged.
    /// Every chunk is rechecked against the approved plan immediately before send.
    pub fn send<S: Read + Write>(
        &mut self,
        channel: &mut Channel<S>,
        mut progress: impl FnMut(u64, u64) -> Result<()>,
    ) -> Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut done = 0;
        for checksum in &self.chunks {
            let count = (self.size - done).min(CHUNK as u64) as usize;
            let mut bytes = vec![0; count];
            self.file.read_exact(&mut bytes)?;
            ensure!(
                format!("{:x}", Sha256::digest(&bytes)) == *checksum,
                "image changed during transfer"
            );
            progress(done, self.size)?;
            channel.send_chunk(&bytes)?;
            done += count as u64;
            progress(done, self.size)?;
        }
        ensure!(
            done == self.size && self.file.read(&mut [0; 1])? == 0,
            "image length changed"
        );
        Ok(())
    }
}

pub fn save_backup<S: Read + Write>(
    channel: &mut Channel<S>,
    target: &str,
    size: u64,
    destination: &Path,
    identity_hash: Option<&str>,
    mut progress: impl FnMut(u64, u64) -> Result<()>,
) -> Result<String> {
    ensure!(
        [
            "recovery", "userdata", "logo", "odmdtbo", "proinfo", "nvram", "nvdata", "protect1",
            "protect2"
        ]
        .contains(&target)
            && size > 0
            && size <= 16 * 1024 * 1024 * 1024,
        "invalid backup inventory"
    );
    ensure!(
        identity_hash.is_none_or(valid_hash),
        "invalid identity hash"
    );
    channel.expect(&serde_json::json!({"event":"backup","target":target,"size":size}))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(destination)?;
    let mut hash = Sha256::new();
    let mut done = 0;
    while done < size {
        let bytes = channel.receive_chunk((size - done).min(CHUNK as u64) as usize)?;
        hash.update(&bytes);
        if bytes.iter().all(|b| *b == 0) {
            file.seek(SeekFrom::Current(bytes.len() as i64))?;
        } else {
            file.write_all(&bytes)?;
        }
        done += bytes.len() as u64;
        progress(done, size)?;
    }
    file.set_len(size)?;
    file.sync_all()?;
    channel.expect(&serde_json::json!({"event":"checking_backup","target":target}))?;
    let checksum = format!("{:x}", hash.finalize());
    ensure!(
        channel.verification(target, "backup", size, &mut progress)?
            == serde_json::json!({"event":"backup_verified","target":target,"sha256":checksum}),
        "independent device backup readback differs"
    );
    ensure!(
        file.metadata()?.len() == size && digest(&mut file)? == checksum,
        "host backup readback differs"
    );
    ensure!(
        identity_hash.is_none_or(|expected| expected == checksum),
        "device identity changed during backup"
    );
    Ok(checksum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn backup_stream(data: &[u8], checksum: &str) -> Channel<Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        fn json(bytes: &mut Vec<u8>, value: serde_json::Value) {
            let value = serde_json::to_vec(&value).unwrap();
            bytes.extend((value.len() as u32).to_le_bytes());
            bytes.extend(value);
        }
        json(
            &mut bytes,
            serde_json::json!({"event":"backup","target":"nvram","size":data.len()}),
        );
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(data);
        json(
            &mut bytes,
            serde_json::json!({"event":"checking_backup","target":"nvram"}),
        );
        json(
            &mut bytes,
            serde_json::json!({"event":"backup_verified","target":"nvram","sha256":checksum}),
        );
        Channel::authenticated(Cursor::new(bytes))
    }
    #[test]
    fn backup_requires_independent_device_hash_and_identity_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let data = vec![0; 4096];
        let hash = format!("{:x}", Sha256::digest(&data));
        let path = root.path().join("good");
        assert_eq!(
            save_backup(
                &mut backup_stream(&data, &hash),
                "nvram",
                4096,
                &path,
                Some(&hash),
                |_, _| Ok(())
            )
            .unwrap(),
            hash
        );
        assert_eq!(fs::read(path).unwrap(), data);
        assert!(save_backup(
            &mut backup_stream(&data, &"0".repeat(64)),
            "nvram",
            4096,
            &root.path().join("bad-readback"),
            Some(&hash),
            |_, _| Ok(())
        )
        .is_err());
        assert!(save_backup(
            &mut backup_stream(&data, &hash),
            "nvram",
            4096,
            &root.path().join("bad-identity"),
            Some(&"0".repeat(64)),
            |_, _| Ok(())
        )
        .is_err());
    }
    #[test]
    fn changed_image_chunk_is_rejected_before_send() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("image");
        fs::write(&path, b"approved").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"approved"));
        let mut image = VerifiedImage::open(&path, 8, &hash, std::slice::from_ref(&hash)).unwrap();
        fs::write(&path, b"modified").unwrap();
        assert!(image
            .send(
                &mut Channel::authenticated(Cursor::new(Vec::new())),
                |_, _| Ok(())
            )
            .is_err());
    }
    #[test]
    fn cancelled_progress_sends_no_image_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("image");
        fs::write(&path, b"approved").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"approved"));
        let mut image = VerifiedImage::open(&path, 8, &hash, std::slice::from_ref(&hash)).unwrap();
        let mut channel = Channel::authenticated(Cursor::new(Vec::new()));
        assert!(image
            .send(&mut channel, |_, _| anyhow::bail!("UI disconnected"))
            .is_err());
        assert!(channel.into_inner().into_inner().is_empty());
    }
    #[test]
    fn malformed_inventory_and_size_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("image");
        fs::write(&path, b"approved").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"approved"));
        assert!(VerifiedImage::open(&path, 9, &hash, std::slice::from_ref(&hash)).is_err());
        assert!(VerifiedImage::open(&path, 8, &hash, &[]).is_err());
    }
}
