//! Kodi control plus opt-in, host-key-verified CoreELEC OS management.
//! No command executes until a method is called. OS commands have no user shell text.
mod discovery;
mod ssh;
pub use couch_kodi;
pub use discovery::{discover, Candidate};
use serde::Serialize;
pub use ssh::SshConfig;
use std::{collections::BTreeMap, fmt};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    InvalidConfig(&'static str),
    Protocol(&'static str),
    NotCoreElec,
    OsDisabled,
    Timeout,
    OutputLimit,
    SshFailed(Option<i32>),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::InvalidConfig(s) | Self::Protocol(s) => f.write_str(s),
            Self::NotCoreElec => f.write_str("device did not identify as CoreELEC"),
            Self::OsDisabled => f.write_str("OS access requires explicit SSH configuration"),
            Self::Timeout => f.write_str("SSH command timed out; outcome may be unknown; not retried"),
            Self::OutputLimit => f.write_str("SSH output exceeded 64 KiB"),
            Self::SshFailed(code) => write!(f, "SSH failed ({code:?}); verify host key, key authentication and service availability; no retry"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub struct Client {
    pub kodi: couch_kodi::Kodi,
    ssh: Option<SshConfig>,
}
impl Client {
    pub fn new(kodi: couch_kodi::Kodi) -> Self {
        Self { kodi, ssh: None }
    }
    pub fn with_ssh(mut self, config: SshConfig) -> Self {
        self.ssh = Some(config);
        self
    }
    /// Authenticated OS identity. Reading the file never sources/evaluates it.
    pub fn identity(&self) -> Result<Identity> {
        Identity::parse(&self.ssh()?.run("cat /etc/os-release")?)
    }
    pub fn kodi_service(&self) -> Result<ServiceStatus> {
        let text = self.ssh()?.run(&guarded("systemctl show kodi.service --property=LoadState --property=ActiveState --property=SubState"))?;
        let fields = fields(&text)?;
        Ok(ServiceStatus {
            load: fields
                .get("LoadState")
                .cloned()
                .ok_or(Error::Protocol("missing LoadState"))?,
            active: fields
                .get("ActiveState")
                .cloned()
                .ok_or(Error::Protocol("missing ActiveState"))?,
            sub: fields
                .get("SubState")
                .cloned()
                .ok_or(Error::Protocol("missing SubState"))?,
        })
    }
    /// Explicit disruptive action. Never automatically retried after disconnect.
    pub fn action(&self, action: OsAction) -> Result<()> {
        self.ssh()?.run(&guarded(action.command())).map(|_| ())
    }
    fn ssh(&self) -> Result<&SshConfig> {
        self.ssh.as_ref().ok_or(Error::OsDisabled)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum OsAction {
    RestartKodi,
    Reboot,
    PowerOff,
}
impl OsAction {
    fn command(self) -> &'static str {
        match self {
            Self::RestartKodi => "systemctl --no-block restart kodi.service",
            Self::Reboot => "systemctl --no-block reboot",
            Self::PowerOff => "systemctl --no-block poweroff",
        }
    }
}
fn guarded(command: &str) -> String {
    // CoreELEC's scripts/image writes exactly ID="coreelec". Never source remote text.
    format!("(test \"$(grep -c '^ID=' /etc/os-release)\" = 1 && (grep -qx 'ID=\"coreelec\"' /etc/os-release || grep -qx 'ID=coreelec' /etc/os-release)) || exit 42; {command}")
}
#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub version: String,
    pub architecture: Option<String>,
    pub project: Option<String>,
    pub device: Option<String>,
    pub build: Option<String>,
}
impl Identity {
    pub fn parse(text: &str) -> Result<Self> {
        let f = fields(text)?;
        if f.get("ID").map(String::as_str) != Some("coreelec") {
            return Err(Error::NotCoreElec);
        }
        Ok(Self {
            version: f
                .get("VERSION")
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or(Error::Protocol("missing CoreELEC version"))?,
            architecture: f.get("DISTRO_ARCH").cloned(),
            project: f.get("DISTRO_PROJECT").cloned(),
            device: f.get("DISTRO_DEVICE").cloned(),
            build: f.get("BUILD_ID").cloned(),
        })
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub load: String,
    pub active: String,
    pub sub: String,
}
fn fields(text: &str) -> Result<BTreeMap<String, String>> {
    if text.len() > 65536 {
        return Err(Error::OutputLimit);
    }
    let mut out = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (k, v) = line
            .split_once('=')
            .ok_or(Error::Protocol("malformed key/value response"))?;
        let v = if v.starts_with('"') {
            v.strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or(Error::Protocol("unbalanced quote"))?
        } else {
            v
        };
        if out.insert(k.into(), v.into()).is_some() {
            return Err(Error::Protocol("duplicate key"));
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coreelec_identity_and_optional_hardware_fields() {
        let i = Identity::parse("ID=\"coreelec\"\nVERSION=\"21.2-Omega\"\nDISTRO_ARCH=\"Amlogic-ng.arm\"\nDISTRO_PROJECT=\"Amlogic-ng\"\n").unwrap();
        assert_eq!(i.version, "21.2-Omega");
        assert_eq!(i.architecture.as_deref(), Some("Amlogic-ng.arm"));
        assert!(i.device.is_none());
    }
    #[test]
    fn refuses_other_distros_and_ambiguous_identity() {
        assert!(matches!(
            Identity::parse("ID=libreelec\nVERSION=12"),
            Err(Error::NotCoreElec)
        ));
        assert!(Identity::parse("ID=coreelec\nID=coreelec\nVERSION=21").is_err());
        assert!(Identity::parse("ID=coreelec\nVERSION=\"21").is_err());
    }
    #[test]
    fn os_access_is_opt_in() {
        assert!(matches!(
            Client::new(couch_kodi::Kodi::tcp("127.0.0.1", 9090)).identity(),
            Err(Error::OsDisabled)
        ));
    }
    #[cfg(unix)]
    #[test]
    fn os_action_guard_rejects_duplicate_or_conflicting_identities() {
        let path =
            std::env::temp_dir().join(format!("couch-coreelec-release-{}", std::process::id()));
        let script =
            guarded("printf accepted").replace("/etc/os-release", "\"$COUCH_TEST_OS_RELEASE\"");
        for (text, allowed) in [
            ("ID=coreelec\n", true),
            ("ID=\"coreelec\"\n", true),
            ("ID=other\n", false),
            ("VERSION=21\n", false),
            ("ID=other\nID=coreelec\n", false),
            ("ID=coreelec\nID=coreelec\n", false),
        ] {
            std::fs::write(&path, text).unwrap();
            let output = std::process::Command::new("sh")
                .args(["-c", &script])
                .env("COUCH_TEST_OS_RELEASE", &path)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                allowed,
                "identity fixture: {text:?}"
            );
            assert_eq!(
                output.stdout,
                if allowed {
                    b"accepted".as_slice()
                } else {
                    b"".as_slice()
                }
            );
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn every_mutation_is_guarded() {
        for a in [OsAction::RestartKodi, OsAction::Reboot, OsAction::PowerOff] {
            let s = guarded(a.command());
            assert!(s.contains("|| exit 42; systemctl --no-block"));
        }
    }
}
