use crate::{Error, Result};
use std::{
    io::Read,
    net::IpAddr,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
const LIMIT: u64 = 65536;
/// Requires an OpenSSH-compatible `ssh` on PATH. No ambient SSH config, agent,
/// password prompt, proxy, forwarding or unknown-host acceptance is permitted.
#[derive(Clone, Debug)]
pub struct SshConfig {
    address: IpAddr,
    port: u16,
    user: String,
    identity_file: PathBuf,
    known_hosts: PathBuf,
}
impl SshConfig {
    pub fn new(
        address: IpAddr,
        port: u16,
        user: String,
        identity_file: PathBuf,
        known_hosts: PathBuf,
    ) -> Result<Self> {
        if port == 0
            || user.is_empty()
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::InvalidConfig("invalid SSH user or port"));
        }
        for p in [&identity_file, &known_hosts] {
            if !p.is_absolute()
                || !p.is_file()
                || p.to_str()
                    .is_none_or(|s| s.contains(['\n', '\r', '%', '$', '"', '\\']))
            {
                return Err(Error::InvalidConfig("SSH key and known_hosts must be existing absolute paths without SSH expansion characters"));
            }
        }
        Ok(Self {
            address,
            port,
            user,
            identity_file,
            known_hosts,
        })
    }
    fn command(&self, remote: &str) -> Command {
        let mut c = Command::new("ssh");
        c.args([
            "-F",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-T",
            "-n",
        ]);
        for opt in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "PreferredAuthentications=publickey",
            "ConnectTimeout=5",
            "ConnectionAttempts=1",
            "ServerAliveInterval=2",
            "ServerAliveCountMax=2",
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "PermitLocalCommand=no",
            "LogLevel=ERROR",
        ] {
            c.args(["-o", opt]);
        }
        c.arg("-o").arg(format!(
            "UserKnownHostsFile=\"{}\"",
            self.known_hosts.display()
        ));
        c.args(["-o", "GlobalKnownHostsFile=none"]);
        c.arg("-i")
            .arg(&self.identity_file)
            .arg("-p")
            .arg(self.port.to_string());
        c.arg("-l")
            .arg(&self.user)
            .arg(self.address.to_string())
            .arg(remote);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    pub(crate) fn run(&self, remote: &str) -> Result<String> {
        run_bounded(self.command(remote), Duration::from_secs(12))
    }
}
fn read_bounded(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(LIMIT + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn run_bounded(mut command: Command, timeout: Duration) -> Result<String> {
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || read_bounded(stdout));
    let err = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Error::Timeout);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Error::Io(e));
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| Error::Protocol("SSH reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| Error::Protocol("SSH reader failed"))??;
    if stdout.len() > LIMIT as usize || stderr.len() > LIMIT as usize {
        return Err(Error::OutputLimit);
    }
    let status = status?;
    if status.code() == Some(42) {
        return Err(Error::NotCoreElec);
    }
    if !status.success() {
        return Err(Error::SshFailed(status.code()));
    }
    String::from_utf8(stdout).map_err(|_| Error::Protocol("SSH response is not UTF-8"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_reader_rejects_unlimited_output() {
        assert_eq!(
            read_bounded(std::io::repeat(1)).unwrap().len(),
            LIMIT as usize + 1
        );
    }
    #[test]
    fn ssh_arguments_pin_host_key_and_disable_ambient_authentication() {
        let path = std::env::temp_dir().join(format!("couch-coreelec-ssh-{}", std::process::id()));
        std::fs::write(&path, "fixture").unwrap();
        let config = SshConfig::new(
            "192.0.2.20".parse().unwrap(),
            2222,
            "root".into(),
            path.clone(),
            path.clone(),
        )
        .unwrap();
        let command = config.command("cat /etc/os-release");
        let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.contains(&"StrictHostKeyChecking=yes"));
        assert!(args.contains(&"IdentityAgent=none"));
        assert!(args.contains(&"BatchMode=yes"));
        assert!(args.contains(&"GlobalKnownHostsFile=none"));
        assert!(args.contains(&"ClearAllForwardings=yes"));
        assert_eq!(
            &args[args.len() - 2..],
            &["192.0.2.20", "cat /etc/os-release"]
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_user_cannot_become_an_option_or_shell_input() {
        for user in ["-oProxyCommand=evil", "root;reboot", "root\n"] {
            assert!(SshConfig::new(
                "127.0.0.1".parse().unwrap(),
                22,
                user.into(),
                "/missing".into(),
                "/missing".into()
            )
            .is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn timeout_kills_and_reaps_child() {
        let mut c = Command::new("sleep");
        c.arg("2").stdout(Stdio::piped()).stderr(Stdio::piped());
        assert!(matches!(
            run_bounded(c, Duration::from_millis(40)),
            Err(Error::Timeout)
        ));
    }
    #[cfg(unix)]
    #[test]
    fn wrong_os_exit_is_distinct_from_transport_failure() {
        let mut c = Command::new("sh");
        c.args(["-c", "exit 42"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert!(matches!(
            run_bounded(c, Duration::from_secs(1)),
            Err(Error::NotCoreElec)
        ));
    }
}
