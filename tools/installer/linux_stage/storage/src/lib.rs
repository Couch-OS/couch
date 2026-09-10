//! Offline transaction policy. No production block writer or network handler.
//! A real backend must independently obtain CID/GPT/mount state; caller assertions
//! are insufficient. The RAM probe does not depend on this crate.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};

pub const CHUNK: usize = 1024 * 1024;
pub const ALIGNMENT: u64 = 4096;
pub type Hash = [u8; 32];
pub type Result<T> = io::Result<T>;

fn require(ok: bool, reason: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, reason))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Target {
    Recovery,
    Userdata,
    Logo,
    OdmDtbo,
    Boot,
}
impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Self::Recovery => "recovery",
            Self::Userdata => "userdata",
            Self::Logo => "logo",
            Self::OdmDtbo => "odmdtbo",
            Self::Boot => "boot",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Region {
    pub offset: u64,
    pub size: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub cid: [u8; 16],
    pub capacity: u64,
    pub partitions: BTreeMap<String, Region>,
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub identity: Identity,
    /// Includes mounts in every relevant namespace and holders/whole-device aliases.
    pub mounted: BTreeSet<Target>,
}
#[derive(Clone, Debug)]
pub struct Image {
    pub size: u64,
    pub sha256: Hash,
    /// Digests of exact CHUNK slices (last slice may be shorter), pinned as part
    /// of the reviewed plan. A bad incoming slice is rejected before its write.
    pub chunks: Vec<Hash>,
}
pub struct Plan {
    identity: Identity,
    images: BTreeMap<Target, Image>,
}
impl Plan {
    pub fn new(identity: Identity, images: BTreeMap<Target, Image>) -> Result<Self> {
        require(
            identity.cid != [0; 16] && identity.cid != [255; 16],
            "invalid CID",
        )?;
        require(
            identity.capacity > 0 && identity.capacity % 512 == 0,
            "invalid capacity",
        )?;
        require(
            !identity.partitions.is_empty() && !images.is_empty(),
            "empty layout or image plan",
        )?;
        let mut regions = Vec::new();
        for (name, region) in &identity.partitions {
            require(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "invalid partition name",
            )?;
            let end = region
                .offset
                .checked_add(region.size)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "partition overflow"))?;
            require(
                region.offset >= 512
                    && region.offset % 512 == 0
                    && region.size > 0
                    && region.size % 512 == 0
                    && end <= identity.capacity,
                "invalid partition bounds",
            )?;
            regions.push((region.offset, end));
        }
        regions.sort_unstable();
        require(
            regions.windows(2).all(|p| p[0].1 <= p[1].0),
            "overlapping partitions",
        )?;
        for (target, image) in &images {
            let region = identity
                .partitions
                .get(target.name())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing target"))?;
            require(
                image.size == region.size
                    && image.size <= 8 * 1024 * 1024 * 1024
                    && image.size % ALIGNMENT == 0,
                "image size or direct I/O alignment mismatch",
            )?;
            require(
                image.chunks.len() as u64 == image.size.div_ceil(CHUNK as u64),
                "invalid chunk inventory",
            )?;
        }
        Ok(Self { identity, images })
    }
    fn check(&self, observation: Observation) -> Result<()> {
        require(
            observation.identity == self.identity,
            "observed CID or full layout changed",
        )?;
        require(
            self.images.keys().all(|t| !observation.mounted.contains(t)),
            "target is mounted or held",
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Writing(Target),
    Synced(Target),
    Verified(Target),
    Complete,
}
/// Return success only after the computer has durably recorded and acknowledged
/// this phase. A device RAM record alone is not an installation journal.
pub trait Journal {
    fn acknowledge(&mut self, phase: Phase) -> Result<()>;
}
/// No paths or caller offsets appear at the transport boundary. Integration must
/// enforce an exclusive unmounted device, validate descriptor major/minor and GPT,
/// and independently preserve calibration. No production implementation exists.
pub trait Storage {
    fn observe(&mut self) -> Result<Observation>;
    fn begin(&mut self, target: Target) -> Result<()>;
    fn append(&mut self, bytes: &[u8]) -> Result<()>;
    /// fsync the writer and CLOSE it before returning; no buffered readback reuse.
    fn sync_close(&mut self) -> Result<()>;
    /// Open a new O_RDONLY|O_DIRECT descriptor, hash exact full target, no fallback.
    fn direct_hash(&mut self, target: Target, size: u64) -> Result<Hash>;
    /// Close any open writer after failure; never retry or claim completion.
    fn abort(&mut self);
}

pub struct Transaction {
    attempted: bool,
}
impl Default for Transaction {
    fn default() -> Self {
        Self { attempted: false }
    }
}
impl Transaction {
    /// The source factory supplies the exact framed image stream, in write order.
    /// It never supplies a destination path or offset. Any error poisons this run.
    pub fn run<S, J, R, F>(
        &mut self,
        plan: &Plan,
        storage: &mut S,
        journal: &mut J,
        mut source: F,
    ) -> Result<()>
    where
        S: Storage,
        J: Journal,
        R: Read,
        F: FnMut(Target) -> Result<R>,
    {
        require(!self.attempted, "transaction cannot be retried")?;
        self.attempted = true;
        let result = (|| {
            plan.check(storage.observe()?)?;
            let mut buffer = vec![0u8; CHUNK];
            for (&target, image) in &plan.images {
                plan.check(storage.observe()?)?;
                let mut input = source(target)?;
                journal.acknowledge(Phase::Writing(target))?;
                storage.begin(target)?;
                let mut incoming = Sha256::new();
                let mut remaining = image.size;
                for expected in &image.chunks {
                    let count = remaining.min(CHUNK as u64) as usize;
                    let bytes = &mut buffer[..count];
                    input.read_exact(bytes)?;
                    require(
                        Hash::from(Sha256::digest(&*bytes)) == *expected,
                        "incoming chunk hash mismatch",
                    )?;
                    incoming.update(&*bytes);
                    storage.append(bytes)?;
                    remaining -= count as u64;
                }
                require(
                    remaining == 0 && Hash::from(incoming.finalize()) == image.sha256,
                    "incoming image SHA mismatch",
                )?;
                let mut extra = [0u8];
                require(
                    input.read(&mut extra)? == 0,
                    "image stream exceeds declared size",
                )?;
                storage.sync_close()?;
                journal.acknowledge(Phase::Synced(target))?;
                plan.check(storage.observe()?)?;
                require(
                    storage.direct_hash(target, image.size)? == image.sha256,
                    "independent direct readback mismatch",
                )?;
                journal.acknowledge(Phase::Verified(target))?;
            }
            plan.check(storage.observe()?)?;
            journal.acknowledge(Phase::Complete)
        })();
        if result.is_err() {
            storage.abort();
        }
        result
    }
}

#[cfg(target_os = "linux")]
pub mod direct;

#[cfg(test)]
mod tests;
