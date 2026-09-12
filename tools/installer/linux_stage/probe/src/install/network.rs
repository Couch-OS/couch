//! Stage-side configuration for a verified, expanded, unmounted userdata image.
use super::{ensure, invalid};
use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Network {
    pub(super) ssid_hex: String,
    pub(super) psk_hex: Option<String>,
}
impl Drop for Network {
    fn drop(&mut self) {
        self.ssid_hex.zeroize();
        if let Some(psk) = &mut self.psk_hex {
            psk.zeroize();
        }
    }
}
impl Network {
    pub(super) fn encode(&self) -> io::Result<Zeroizing<Vec<u8>>> {
        let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
        ensure(
            (2..=64).contains(&self.ssid_hex.len())
                && self.ssid_hex.len().is_multiple_of(2)
                && hex(&self.ssid_hex),
            "invalid network name encoding",
        )?;
        let mut data = Zeroizing::new(
            format!("network={{\n    ssid={}\n    scan_ssid=1\n", self.ssid_hex).into_bytes(),
        );
        if let Some(psk) = &self.psk_hex {
            ensure(psk.len() == 64 && hex(psk), "invalid network key encoding")?;
            data.extend_from_slice(b"    key_mgmt=WPA-PSK\n    proto=RSN\n    psk=");
            data.extend_from_slice(psk.as_bytes());
            data.push(b'\n');
        } else {
            data.extend_from_slice(b"    key_mgmt=NONE\n");
        }
        data.extend_from_slice(b"}\n");
        Ok(data)
    }
}

fn debugfs(args: &[&str], image: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut child = Command::new("/usr/sbin/debugfs")
        .args(args)
        .arg(image)
        .env_clear()
        .env("PATH", "/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| invalid("missing filesystem response"))?;
    let reader = thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        stdout.take(4097).read_to_end(&mut bytes)?;
        ensure(bytes.len() <= 4096, "filesystem response exceeded limit")?;
        Ok::<_, io::Error>(bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(invalid("filesystem configuration timed out"));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
        }
    };
    let output = reader
        .join()
        .map_err(|_| invalid("filesystem response reader failed"))?;
    ensure(status?.success(), "filesystem configuration command failed")?;
    output
}

/// Caller owns the fixed block node and independently checks CID/GPT/mount state.
/// No network value is placed in a command argument or interpreted as a command.
pub(super) fn configure(image: &Path, private_root: &Path, data: &[u8]) -> io::Result<()> {
    let input = private_root.join("network.conf");
    let commands = private_root.join("network.commands");
    // Paths come from a fixed private directory, not the wire protocol.
    ensure(
        input.to_str().is_some_and(|s| {
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        }),
        "invalid private network path",
    )?;
    let mut input_created = false;
    let mut commands_created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&input)?;
        input_created = true;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&commands)?;
        commands_created = true;
        writeln!(file,"rm /opt/couch/networks.conf\nwrite {} /opt/couch/networks.conf\nset_inode_field /opt/couch/networks.conf mode 0100600",input.display())?;
        file.sync_all()?;
        drop(file);
        debugfs(&["-w", "-f", commands.to_str().unwrap()], image)?;
        let actual = debugfs(&["-R", "cat /opt/couch/networks.conf"], image)?;
        ensure(
            actual.as_slice() == data,
            "network configuration readback mismatch",
        )?;
        let metadata = debugfs(&["-R", "stat /opt/couch/networks.conf"], image)?;
        let text =
            std::str::from_utf8(&metadata).map_err(|_| invalid("invalid filesystem metadata"))?;
        ensure(
            text.lines()
                .next()
                .is_some_and(|line| line.contains("Type: regular") && line.contains("Mode:  0600")),
            "network configuration permissions mismatch",
        )?;
        Ok(())
    })();
    if input_created {
        let _ = fs::remove_file(input);
    }
    if commands_created {
        let _ = fs::remove_file(commands);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regular_ext4_fixture_persists_exact_credentials_and_private_mode() {
        // Linux-only private-install suite: regular disposable image, never a block node.
        let root =
            std::env::temp_dir().join(format!("couch-network-fixture-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let image = root.join("fixture.ext4");
        let file = std::fs::File::create(&image).unwrap();
        file.set_len(16 * 1024 * 1024).unwrap();
        assert!(Command::new("/usr/sbin/mke2fs")
            .args(["-q", "-t", "ext4", "-F"])
            .arg(&image)
            .status()
            .unwrap()
            .success());
        debugfs(&["-w", "-R", "mkdir /opt"], &image).unwrap();
        debugfs(&["-w", "-R", "mkdir /opt/couch"], &image).unwrap();
        for network in [
            Network {
                ssid_hex: "00220aff".into(),
                psk_hex: Some("ab".repeat(32)),
            },
            Network {
                ssid_hex: "61".into(),
                psk_hex: None,
            },
        ] {
            let expected = network.encode().unwrap();
            configure(&image, &root, &expected).unwrap();
            assert!(!root.join("network.conf").exists());
            assert!(!root.join("network.commands").exists());
            assert_eq!(
                *debugfs(&["-R", "cat /opt/couch/networks.conf"], &image).unwrap(),
                *expected
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn temporary_secret_is_removed_when_command_creation_fails() {
        let root =
            std::env::temp_dir().join(format!("couch-network-cleanup-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("network.commands"), b"preexisting").unwrap();
        assert!(configure(&root.join("unused.ext4"), &root, b"private test secret").is_err());
        assert!(!root.join("network.conf").exists());
        assert_eq!(
            fs::read(root.join("network.commands")).unwrap(),
            b"preexisting"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn credentials_are_bounded_hex_and_never_interpreted_as_commands() {
        let network = Network {
            ssid_hex: "00220a".into(),
            psk_hex: Some("aB".repeat(32)),
        };
        assert!(network
            .encode()
            .unwrap()
            .starts_with(b"network={\n    ssid=00220a\n"));
        for value in ["", "abc", "00\n}", &"ff".repeat(33)] {
            assert!(Network {
                ssid_hex: value.into(),
                psk_hex: None
            }
            .encode()
            .is_err());
        }
        assert!(Network {
            ssid_hex: "01".into(),
            psk_hex: Some("x".repeat(64))
        }
        .encode()
        .is_err());
    }
    #[test]
    fn open_network_has_no_password_field() {
        let bytes = Network {
            ssid_hex: "61".into(),
            psk_hex: None,
        }
        .encode()
        .unwrap();
        assert_eq!(
            bytes.as_slice(),
            b"network={\n    ssid=61\n    scan_ssid=1\n    key_mgmt=NONE\n}\n"
        );
    }
}
