#![cfg(target_os = "linux")]
use super::*;
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
type Events = Rc<RefCell<Vec<String>>>;

/// Regular files ONLY. This fixture cannot be substituted as a block backend.
struct Fixture {
    directory: PathBuf,
    observed: Observation,
    writer: Option<File>,
    events: Events,
    corrupt: bool,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
impl Fixture {
    fn new(identity: Identity, events: Events) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "couch-storage-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self {
            directory,
            observed: Observation {
                identity,
                mounted: BTreeSet::new(),
            },
            writer: None,
            events,
            corrupt: false,
        }
    }
    fn path(&self, target: Target) -> PathBuf {
        self.directory.join(target.name())
    }
}
impl Storage for Fixture {
    fn observe(&mut self) -> Result<Observation> {
        Ok(self.observed.clone())
    }
    fn begin(&mut self, target: Target) -> Result<()> {
        self.events.borrow_mut().push(format!("begin:{target:?}"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(target))?;
        require(
            file.metadata()?.is_file(),
            "fixture permits only regular files",
        )?;
        self.writer = Some(file);
        Ok(())
    }
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        require(bytes.len() <= CHUNK, "unbounded chunk")?;
        self.writer.as_mut().unwrap().write_all(bytes)
    }
    fn sync_close(&mut self) -> Result<()> {
        let file = self.writer.take().unwrap();
        file.sync_all()?;
        drop(file);
        self.events.borrow_mut().push("closed".into());
        Ok(())
    }
    fn direct_hash(&mut self, target: Target, size: u64) -> Result<Hash> {
        assert!(self.writer.is_none());
        self.events.borrow_mut().push(format!("direct:{target:?}"));
        if self.corrupt {
            let mut file = OpenOptions::new().write(true).open(self.path(target))?;
            file.write_all(&[0xff])?;
            file.sync_all()?;
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT | libc::O_NOFOLLOW)
            .open(self.path(target))?;
        require(
            file.metadata()?.is_file(),
            "fixture permits only regular files",
        )?;
        direct::hash(&file, size)
    }
    fn abort(&mut self) {
        self.writer.take();
        self.events.borrow_mut().push("abort".into());
    }
}
struct HostJournal {
    events: Events,
    fail: bool,
}
impl Journal for HostJournal {
    fn acknowledge(&mut self, phase: Phase) -> Result<()> {
        if self.fail {
            return Err(io::Error::other("host journal acknowledgement failed"));
        }
        self.events.borrow_mut().push(format!("journal:{phase:?}"));
        Ok(())
    }
}
fn setup() -> (Plan, Fixture, HostJournal, Vec<u8>) {
    let data = vec![0x5a; CHUNK + ALIGNMENT as usize];
    let identity = Identity {
        cid: [3; 16],
        capacity: 16 * 1024 * 1024,
        partitions: [
            (
                "recovery".into(),
                Region {
                    offset: 4096,
                    size: data.len() as u64,
                },
            ),
            (
                "boot".into(),
                Region {
                    offset: 4 * 1024 * 1024,
                    size: data.len() as u64,
                },
            ),
        ]
        .into(),
    };
    let image = Image {
        size: data.len() as u64,
        sha256: Sha256::digest(&data).into(),
        chunks: data
            .chunks(CHUNK)
            .map(|chunk| Sha256::digest(chunk).into())
            .collect(),
    };
    let plan = Plan::new(
        identity.clone(),
        [(Target::Recovery, image.clone()), (Target::Boot, image)].into(),
    )
    .unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    (
        plan,
        Fixture::new(identity, events.clone()),
        HostJournal {
            events,
            fail: false,
        },
        data,
    )
}

#[test]
fn sequential_files_are_closed_then_directly_hashed_before_completion() {
    let (plan, mut storage, mut journal, data) = setup();
    Transaction::default()
        .run(&plan, &mut storage, &mut journal, |_| {
            Ok(Cursor::new(&data))
        })
        .unwrap();
    assert_eq!(
        *journal.events.borrow(),
        [
            "journal:Writing(Recovery)",
            "begin:Recovery",
            "closed",
            "journal:Synced(Recovery)",
            "direct:Recovery",
            "journal:Verified(Recovery)",
            "journal:Writing(Boot)",
            "begin:Boot",
            "closed",
            "journal:Synced(Boot)",
            "direct:Boot",
            "journal:Verified(Boot)",
            "journal:Complete"
        ]
    );
}
#[test]
fn mounted_or_wrong_cid_refuses_any_write() {
    for mismatch in [false, true] {
        let (plan, mut storage, mut journal, data) = setup();
        if mismatch {
            storage.observed.identity.cid[0] ^= 1;
        } else {
            storage.observed.mounted.insert(Target::Recovery);
        }
        assert!(Transaction::default()
            .run(&plan, &mut storage, &mut journal, |_| Ok(Cursor::new(
                &data
            )))
            .is_err());
        assert_eq!(*journal.events.borrow(), ["abort"]);
    }
}
#[test]
fn bad_chunk_is_rejected_before_writing_and_run_cannot_resume() {
    let (plan, mut storage, mut journal, mut data) = setup();
    data[0] ^= 1;
    let mut transaction = Transaction::default();
    assert!(transaction
        .run(&plan, &mut storage, &mut journal, |_| Ok(Cursor::new(
            &data
        )))
        .is_err());
    assert_eq!(
        fs::metadata(storage.path(Target::Recovery)).unwrap().len(),
        0
    );
    assert!(!storage.path(Target::Boot).exists());
    let events = journal.events.borrow().len();
    assert!(transaction
        .run(&plan, &mut storage, &mut journal, |_| Ok(Cursor::new(
            &data
        )))
        .is_err());
    assert_eq!(journal.events.borrow().len(), events);
}
#[test]
fn readback_detects_post_close_corruption_and_never_advances() {
    let (plan, mut storage, mut journal, data) = setup();
    storage.corrupt = true;
    let error = Transaction::default()
        .run(&plan, &mut storage, &mut journal, |_| {
            Ok(Cursor::new(&data))
        })
        .unwrap_err();
    assert!(error.to_string().contains("direct readback"));
    assert!(!storage.path(Target::Boot).exists());
    assert!(!journal
        .events
        .borrow()
        .iter()
        .any(|e| e.contains("Verified") || e.contains("Complete")));
}
#[test]
fn host_journal_failure_prevents_opening_writer() {
    let (plan, mut storage, mut journal, data) = setup();
    journal.fail = true;
    assert!(Transaction::default()
        .run(&plan, &mut storage, &mut journal, |_| Ok(Cursor::new(
            &data
        )))
        .is_err());
    assert!(!storage.path(Target::Recovery).exists());
}
#[test]
fn truncated_or_excess_stream_never_completes() {
    for excess in [false, true] {
        let (plan, mut storage, mut journal, mut data) = setup();
        if excess {
            data.push(0);
        } else {
            data.pop();
        }
        assert!(Transaction::default()
            .run(&plan, &mut storage, &mut journal, |_| Ok(Cursor::new(
                &data
            )))
            .is_err());
        assert!(!storage.path(Target::Boot).exists());
        assert!(!journal
            .events
            .borrow()
            .iter()
            .any(|e| e.contains("Complete")));
    }
}
#[test]
fn direct_reader_rejects_buffered_descriptor_instead_of_falling_back() {
    let (_, storage, _, data) = setup();
    fs::write(storage.path(Target::Recovery), &data).unwrap();
    let file = File::open(storage.path(Target::Recovery)).unwrap();
    assert!(direct::hash(&file, data.len() as u64)
        .unwrap_err()
        .to_string()
        .contains("O_DIRECT"));
}
#[test]
fn plan_rejects_overlapping_layout_and_inconsistent_chunk_inventory() {
    let (plan, _, _, _) = setup();
    let mut overlap = plan.identity.clone();
    overlap.partitions.get_mut("boot").unwrap().offset = 4096;
    assert!(Plan::new(overlap, plan.images.clone()).is_err());
    let mut images = plan.images.clone();
    images.get_mut(&Target::Recovery).unwrap().chunks.pop();
    assert!(Plan::new(plan.identity, images).is_err());
}

#[test]
fn compact_images_are_limited_to_nonempty_aligned_userdata() {
    let (plan, _, _, _) = setup();
    let image = plan.images[&Target::Boot].clone();
    let mut identity = plan.identity.clone();
    identity.partitions.insert(
        "userdata".into(),
        Region {
            offset: 16 * 1024 * 1024,
            size: 65536,
        },
    );
    identity.capacity = 32 * 1024 * 1024;
    for size in [0, 4095, 65537, 131072] {
        let mut bad = image.clone();
        bad.size = size;
        assert!(Plan::new(identity.clone(), [(Target::Userdata, bad)].into()).is_err());
    }
    let mut small = image.clone();
    small.size = 4096;
    small.chunks.truncate(1);
    assert!(Plan::new(identity.clone(), [(Target::Userdata, small.clone())].into()).is_ok());
    identity.partitions.get_mut("boot").unwrap().size += 4096;
    assert!(Plan::new(identity, [(Target::Boot, small)].into()).is_err());
}
