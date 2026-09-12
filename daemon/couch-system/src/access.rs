use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
const ROOT: &str = "/mnt/alpine";
pub fn available() -> bool {
    fs::read_to_string(format!("{ROOT}/root/.ssh/authorized_keys")).is_ok_and(|s| {
        s.lines()
            .any(|l| !l.trim().is_empty() && !l.starts_with('#'))
    }) || fs::read_to_string(format!("{ROOT}/etc/shadow")).is_ok_and(|s| {
        s.lines().any(|l| {
            l.starts_with("root:") && l.split(':').nth(1).is_some_and(|h| h.starts_with('$'))
        })
    })
}
pub fn ssh(enabled: bool) -> Result<(), String> {
    if !enabled {
        let _ = crate::service::alpine("/bin/busybox")
            .args(["killall", "sshd"])
            .status();
        return Ok(());
    }
    if !available() {
        return Err("Enroll a key or password on the remote first".into());
    }
    if !Path::new(&format!("{ROOT}/etc/ssh/ssh_host_ed25519_key")).exists()
        && !crate::service::alpine("/usr/bin/ssh-keygen")
            .arg("-A")
            .status()
            .map_err(|_| "Host-key generation failed")?
            .success()
    {
        return Err("Host-key generation failed".into());
    }
    if crate::service::alpine("/bin/busybox")
        .args(["pidof", "sshd"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return Ok(());
    }
    if crate::service::alpine("/usr/sbin/sshd")
        .status()
        .map_err(|_| "SSH could not start")?
        .success()
    {
        Ok(())
    } else {
        Err("SSH could not start".into())
    }
}
pub fn ssh_auto() -> Result<(), String> {
    if fs::read_to_string(format!("{ROOT}/opt/couch/settings.conf"))
        .is_ok_and(|s| s.lines().any(|l| l == "ssh=0"))
    {
        return Ok(());
    }
    if available() {
        ssh(true)
    } else {
        Ok(())
    }
}
fn atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!("system-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|_| "Could not persist access configuration".into())
}
fn pressed(kind: u16, code: u16, value: i32) -> bool {
    // HA100 matrix keys are EV_KEY below BTN_MISC (0x100). SYN, key release,
    // repeat and touchscreen BTN_TOUCH must never approve permanent SSH access.
    kind == 1 && code > 0 && code < 0x100 && value == 1
}
fn approve() -> Result<(), String> {
    #[repr(C)]
    struct InputEvent {
        // evdev uses two kernel unsigned longs, including on 32-bit musl
        // with 64-bit time_t. libc::timeval would mis-size HA100 events.
        seconds: usize,
        microseconds: usize,
        kind: u16,
        code: u16,
        value: i32,
    }
    let mut devices: Vec<_> = (0..3)
        .filter_map(|n| {
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(format!("/dev/input/event{n}"))
                .ok()
        })
        .collect();
    if devices.is_empty() {
        return Err("Physical keypad is unavailable".into());
    }
    // Drain events queued before the request; approval must be a new key-down.
    let mut bytes = vec![0u8; std::mem::size_of::<InputEvent>()];
    for file in &mut devices {
        while file.read(&mut bytes).is_ok_and(|n| n > 0) {}
    }
    let _ = fs::remove_file("/tmp/press.result");
    atomic(Path::new("/tmp/press.request"), b"ssh")?;
    let start = Instant::now();
    let mut accepted = false;
    while start.elapsed() < Duration::from_secs(25) && !accepted {
        for file in &mut devices {
            if file.read(&mut bytes).is_ok_and(|n| n == bytes.len()) {
                // read_unaligned handles the byte-buffer alignment on ARMv7.
                let event =
                    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const InputEvent) };
                accepted |= pressed(event.kind, event.code, event.value);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = fs::remove_file("/tmp/press.request");
    let _ = atomic(
        Path::new("/tmp/press.result"),
        if accepted { b"ok" } else { b"timeout" },
    );
    if accepted {
        Ok(())
    } else {
        Err("No button press on the remote; access was not granted".into())
    }
}
fn valid_key(key: &str) -> bool {
    let fields: Vec<_> = key.split_whitespace().collect();
    key.len() <= 8192
        && !key.chars().any(char::is_control)
        && fields.len() >= 2
        && matches!(
            fields[0],
            "ssh-ed25519"
                | "ssh-rsa"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
        )
        && fields[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
}
pub fn enroll(key: &str) -> Result<(), String> {
    if !valid_key(key) {
        return Err("Enter one SSH public key without options or control characters".into());
    }
    let pending = Path::new("/mnt/alpine/tmp/couch-system-key");
    atomic(pending, format!("{key}\n").as_bytes())?;
    let checked = crate::service::alpine("/usr/bin/ssh-keygen")
        .args(["-l", "-f", "/tmp/couch-system-key"])
        .status();
    let _ = fs::remove_file(pending);
    if !checked.is_ok_and(|s| s.success()) {
        return Err("Invalid SSH public key".into());
    }
    approve()?;
    let dir = Path::new("/mnt/alpine/root/.ssh");
    fs::create_dir_all(dir).map_err(|_| "Could not create SSH directory")?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Could not secure SSH directory")?;
    let path = dir.join("authorized_keys");
    let mut old = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("Could not read SSH keys".into()),
    };
    if !old.lines().any(|line| line == key) {
        if !old.is_empty() && !old.ends_with('\n') {
            old.push('\n');
        }
        old.push_str(key);
        old.push('\n');
        atomic(&path, old.as_bytes())?;
    }
    Ok(())
}
fn valid_password(password: &str) -> bool {
    (8..=256).contains(&password.len())
        && password
            .bytes()
            .all(|b| (32..=126).contains(&b) && b != b':')
}
pub fn password(password: &str) -> Result<(), String> {
    if !valid_password(password) {
        return Err("Use 8–256 printable ASCII characters without a colon".into());
    }
    approve()?;
    let mut child = crate::service::alpine("/usr/sbin/chpasswd")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| "Password helper could not start")?;
    let written = writeln!(
        child
            .stdin
            .take()
            .ok_or("Password helper input unavailable")?,
        "root:{password}"
    );
    let status = child.wait().map_err(|_| "Password helper failed")?;
    if written.is_err() || !status.success() {
        return Err("Could not set password".into());
    }
    atomic(
        Path::new("/mnt/alpine/etc/ssh/sshd_config.d/couch.conf"),
        b"PermitRootLogin yes\nPasswordAuthentication yes\nPubkeyAuthentication yes\nUseDNS no\n",
    )?;
    ssh(false)?;
    for _ in 0..50 {
        if !crate::service::alpine("/bin/busybox")
            .args(["pidof", "sshd"])
            .status()
            .is_ok_and(|s| s.success())
        {
            return ssh(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Password saved, but SSH did not stop for restart".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_new_physical_key_down_approves() {
        assert!(pressed(1, 28, 1));
        for (kind, code, value) in [(0, 0, 0), (1, 28, 0), (1, 28, 2), (1, 330, 1)] {
            assert!(!pressed(kind, code, value));
        }
    }
    #[test]
    fn access_input_cannot_inject_another_record() {
        assert!(!valid_key("ssh-ed25519 AAAA\nssh-rsa AAAA"));
        assert!(!valid_key("command=evil ssh-ed25519 AAAA"));
        assert!(!valid_password("password\nroot:another"));
        assert!(!valid_password("password:bad"));
    }
}
