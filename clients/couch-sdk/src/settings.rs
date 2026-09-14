//! Private per-connection credentials, written the way the daemon expects.
//!
//! Every existing client stores its credentials in
//! `connections/<connection-id>/<prefix>-connection.json` beside `config.json`,
//! mode 0600, and writes it through a temporary file so a crashed save cannot
//! leave a truncated credential behind. That code was copied by hand into nine
//! places, eight of them naming the temporary file after the process alone, so
//! two saves in one process collided and surfaced as an invalid connection.
//! Every one of them now calls this, and a new client copies nothing.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{de::DeserializeOwned, Serialize};

use crate::{Error, Result};

/// Read a JSON credential file.
///
/// Typed `io::Result` rather than [`Error`] on purpose. This is a file
/// primitive, and an existing client that already distinguishes "the file is
/// not there" from "the file is not usable" must be able to keep doing so -
/// `couch-denon` does exactly that. A parse failure arrives as
/// [`std::io::ErrorKind::InvalidData`].
///
/// [`ClientSettings::load`] is the opinionated wrapper: it calls this and
/// reports either outcome as [`Error::Invalid`], because to the UI both mean
/// "this connection has not been set up yet".
pub fn load_private<T: DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write a JSON credential file atomically, mode 0600.
///
/// Each temporary file has a process-local sequence number and a clock value,
/// so a save does not collide with another thread or with a stale temporary
/// file left by an interrupted earlier process. Both the file and its
/// directory are synced before the call returns -
/// this device loses power by having its battery run out, and a half-written
/// pairing key is indistinguishable from a revoked one. A failed write leaves
/// the previous file untouched.
/// Typed `io::Result` for the same reason as [`load_private`]: the caller
/// decides what a storage failure means to its user.
pub fn save_private<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    save_private_bytes(path, &serde_json::to_vec(value)?)
}

/// The same write for a caller that has already produced the bytes, because it
/// serialises with its own formatting. `couch-matter` writes pretty JSON a
/// person is expected to read.
pub fn save_private_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent directory")
    })?;
    // A bare relative filename has an empty parent, which is this directory.
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let (temporary, mut file) = open_temporary(path)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    // The temporary file never survives a failure, whichever step failed.
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The path the daemon and the GUI both compute for a connection's credentials.
///
/// `home` is the directory holding `config.json`. An empty `connection_id` is
/// the pre-connections singleton layout, which both readers still honour; new
/// code should always pass a real ID.
pub fn connection_file(home: &Path, connection_id: &str, prefix: &str) -> Result<PathBuf> {
    if connection_id.is_empty() {
        Ok(home.join(format!("{prefix}-connection.json")))
    } else {
        let components = Path::new(connection_id).components().collect::<Vec<_>>();
        let valid = components.len() == 1
            && matches!(components[0], std::path::Component::Normal(_))
            && !connection_id
                .chars()
                .any(|character| character.is_control() || character == '\\');
        if valid {
            Ok(home
                .join("connections")
                .join(connection_id)
                .join(format!("{prefix}-connection.json")))
        } else {
            Err(Error::Invalid)
        }
    }
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_path(path: &Path) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_extension(format!("{}.{}.{}.new", std::process::id(), nanos, sequence))
}

/// Open a name owned by this invocation. If an unlikely clock/sequence clash
/// finds an old file, leave that file alone and try another name.
fn open_temporary(path: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let temporary = temporary_path(path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Owner-only from the first byte. The installer builds this crate for
        // Windows hosts too, where there is no mode to set; the device is Linux.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary credential file",
    ))
}

/// What a client needs to reach one device, and where that is kept.
///
/// Implement this on a plain serde struct. The default `load`/`save` give you
/// the private-file behaviour above plus validation on both sides, so an
/// invalid value can neither be written nor silently used after someone edits
/// the file by hand.
pub trait ClientSettings: Serialize + DeserializeOwned + Sized {
    /// The filename stem, matching the provider slug the daemon routes on:
    /// `"denon"` gives `denon-connection.json`.
    const FILE_PREFIX: &'static str;

    /// Reject anything that cannot address a device. Called before every save
    /// and after every load. Do not perform I/O here.
    fn validate(&self) -> Result<()>;

    /// Unreadable and unparseable are both [`Error::Invalid`] here: to the
    /// person setting up a connection they mean the same thing.
    fn load(path: &Path) -> Result<Self> {
        let settings: Self = load_private(path).map_err(|_| Error::Invalid)?;
        settings.validate()?;
        Ok(settings)
    }

    /// A failed write is a storage problem, and saying "the device could not be
    /// reached" would send whoever reads the message to the wrong place.
    fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save_private(path, self)
            .map_err(|_| Error::Remote("Could not save the connection settings".into()))
    }

    /// `<home>/connections/<connection-id>/<prefix>-connection.json`.
    ///
    /// Rejects path separators, `.` and `..` in `connection_id`; it becomes a
    /// directory name owned by the daemon, not a path fragment supplied by a
    /// browser request.
    fn path_in(home: &Path, connection_id: &str) -> Result<PathBuf> {
        connection_file(home, connection_id, Self::FILE_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Example {
        host: String,
        token: String,
    }
    impl ClientSettings for Example {
        const FILE_PREFIX: &'static str = "example";
        fn validate(&self) -> Result<()> {
            if self.host.is_empty() || self.token.is_empty() {
                return Err(Error::Invalid);
            }
            Ok(())
        }
    }

    #[test]
    fn saved_credentials_are_private_and_a_rejected_save_keeps_the_old_file() {
        let dir = std::env::temp_dir().join(format!("couch-sdk-settings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = Example::path_in(&dir, "").unwrap();
        let good = Example {
            host: "10.0.0.5".into(),
            token: "secret".into(),
        };
        good.save(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let before = fs::read(&path).unwrap();
        let bad = Example {
            host: String::new(),
            token: "secret".into(),
        };
        assert_eq!(bad.save(&path), Err(Error::Invalid));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(Example::load(&path).unwrap(), good);

        // A file edited by hand into an unusable state is a setup problem, not
        // a device problem, and must not be handed to a transport.
        fs::write(&path, br#"{"host":"","token":"secret"}"#).unwrap();
        assert_eq!(Example::load(&path), Err(Error::Invalid));
        fs::write(&path, b"not json at all").unwrap();
        assert_eq!(Example::load(&path), Err(Error::Invalid));
        assert_eq!(Example::load(&dir.join("absent.json")), Err(Error::Invalid));
        let occupied = dir.join("occupied");
        fs::create_dir(&occupied).unwrap();
        assert_eq!(
            good.save(&occupied),
            Err(Error::Remote(
                "Could not save the connection settings".into()
            ))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_primitives_keep_the_io_error_so_a_caller_can_tell_absent_from_corrupt() {
        // couch-denon reports an unreadable file and an unparseable one
        // differently, and did so before this helper existed. The primitive
        // has to let it keep that distinction; only the trait default flattens.
        let dir = std::env::temp_dir().join(format!("couch-sdk-io-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("example-connection.json");
        assert_eq!(
            load_private::<Example>(&path).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        fs::write(&path, b"{").unwrap();
        assert_eq!(
            load_private::<Example>(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );

        // Every failing step removes the temporary file it created. A target
        // that is a directory fails at the rename, which is the only step that
        // can fail after the temporary exists and has been written.
        let occupied = dir.join("occupied.json");
        fs::create_dir(&occupied).unwrap();
        let value = Example {
            host: "10.0.0.5".into(),
            token: "secret".into(),
        };
        assert!(save_private(&occupied, &value).is_err());
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".new"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn connection_paths_match_the_layout_the_daemon_and_gui_compute() {
        let home = Path::new("/home/couch");
        assert_eq!(
            Example::path_in(home, "abc123").unwrap(),
            home.join("connections/abc123/example-connection.json")
        );
        assert_eq!(
            Example::path_in(home, "").unwrap(),
            home.join("example-connection.json")
        );
        for bad in [
            ".",
            "..",
            "../outside",
            "nested/id",
            "/absolute",
            r"nested\id",
        ] {
            assert_eq!(Example::path_in(home, bad), Err(Error::Invalid), "{bad}");
        }
    }

    #[test]
    fn stale_temporary_files_do_not_block_a_later_save() {
        let dir = std::env::temp_dir().join(format!("couch-sdk-stale-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("example-connection.json");
        fs::write(
            path.with_extension(format!("{}.old.new", std::process::id())),
            b"partial",
        )
        .unwrap();
        let value = Example {
            host: "10.0.0.5".into(),
            token: "secret".into(),
        };
        save_private(&path, &value).unwrap();
        assert_eq!(load_private::<Example>(&path).unwrap(), value);
        fs::remove_dir_all(&dir).unwrap();
    }
}
