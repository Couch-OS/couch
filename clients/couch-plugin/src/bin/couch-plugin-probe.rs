//! A package that reports what its own user may do. Test material only.
//!
//! It is a real package: a real `DeviceClient` served by [`couch_plugin::serve`]
//! through the real host, so what it reports is what an installed integration
//! would find, including the step `serve` takes to stop being dumpable.
//!
//! Its "device" is the kernel. `configure` names one question - open this path,
//! attach to this process, signal this process, describe myself - and `status`
//! answers it in `title`. The host asks the questions, because only the host
//! knows the other child's process id.
//!
//! Built only with couch-plugin's `testing` feature, which no shipped build
//! enables; `tests/isolation.rs` is its only caller.

use couch_plugin::Manifest;
use couch_sdk::{ClientSettings, DeviceClient, Selectable, Status};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

#[derive(Serialize, Deserialize)]
struct Question {
    op: String,
    arg: String,
}
impl ClientSettings for Question {
    const FILE_PREFIX: &'static str = "probe";
    fn validate(&self) -> couch_sdk::Result<()> {
        match self.op.as_str() {
            "self" | "open" | "ptrace" | "kill" => Ok(()),
            _ => Err(couch_sdk::Error::Invalid),
        }
    }
}

struct Probe(Question);
impl DeviceClient for Probe {
    type Settings = Question;
    const KIND: &'static str = "probe";
    const LABEL: &'static str = "Probe";
    fn capabilities() -> &'static [couch_sdk::Capability] {
        &[]
    }
    fn connect(settings: &Question) -> couch_sdk::Result<Self> {
        settings.validate()?;
        Ok(Self(Question {
            op: settings.op.clone(),
            arg: settings.arg.clone(),
        }))
    }
    fn execute(&mut self, _function: &couch_sdk::couch_model::commands::Function) -> couch_sdk::Result<()> {
        Err(couch_sdk::Error::Unsupported)
    }
    fn inputs(&mut self) -> couch_sdk::Result<Vec<Selectable>> {
        Ok(Vec::new())
    }
    fn status(&mut self) -> couch_sdk::Result<Status> {
        Ok(Status {
            title: Some(answer(&self.0)),
            ..Status::default()
        })
    }
}

/// `ok`, or the name of the errno the kernel gave. Never a sentence: the test
/// compares these exactly.
fn outcome(failed: bool) -> String {
    if !failed {
        return "ok".into();
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EACCES) => "EACCES".into(),
        Some(libc::EPERM) => "EPERM".into(),
        Some(libc::ESRCH) => "ESRCH".into(),
        Some(libc::ENOENT) => "ENOENT".into(),
        Some(code) => format!("errno {code}"),
        None => "errno unknown".into(),
    }
}

fn answer(question: &Question) -> String {
    match question.op.as_str() {
        "self" => describe_self(),
        "open" => {
            let Ok(path) = CString::new(question.arg.as_bytes()) else {
                return "bad path".into();
            };
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
            let result = outcome(fd < 0);
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
            result
        }
        #[cfg(target_os = "linux")]
        "ptrace" => {
            let Ok(pid) = question.arg.parse::<libc::pid_t>() else {
                return "bad pid".into();
            };
            let attached = unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0) };
            let result = outcome(attached < 0);
            if attached >= 0 {
                unsafe { libc::ptrace(libc::PTRACE_DETACH, pid, 0, 0) };
            }
            result
        }
        "kill" => {
            let Ok(pid) = question.arg.parse::<libc::pid_t>() else {
                return "bad pid".into();
            };
            outcome(unsafe { libc::kill(pid, 0) } != 0)
        }
        _ => "unknown question".into(),
    }
}

/// One line the test can parse: who this child is, what it may still gain, and
/// whether a crash of it could leave a copy of its memory on disk.
fn describe_self() -> String {
    let mut groups = vec![0 as libc::gid_t; 64];
    let found = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
    let mut groups: Vec<String> = if found < 0 {
        vec!["unreadable".into()]
    } else {
        groups[..found as usize]
            .iter()
            .map(|gid| gid.to_string())
            .collect()
    };
    groups.sort();
    let mut core = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut core) };
    let no_new_privs = {
        #[cfg(target_os = "linux")]
        {
            unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) }
        }
        #[cfg(not(target_os = "linux"))]
        {
            -1
        }
    };
    format!(
        "uid={} gid={} groups=[{}] core={},{} nnp={} status_nnp={}",
        unsafe { libc::getuid() },
        unsafe { libc::getgid() },
        groups.join(","),
        core.rlim_cur,
        core.rlim_max,
        no_new_privs,
        status_field("NoNewPrivs"),
    )
}

/// The kernel's own word for it, from the file a person would read. A child
/// may always read its own `status`, dumpable or not.
fn status_field(name: &str) -> String {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
        return "unreadable".into();
    };
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{name}:")))
        .map_or_else(|| "absent".into(), |value| value.trim().to_owned())
}

fn main() {
    // The manifest the host handed the store, read from the package directory
    // this executable sits in, so the two can never drift apart.
    let root = std::fs::canonicalize("/proc/self/exe")
        .ok()
        .or_else(|| std::env::current_exe().ok())
        .expect("the probe's own path");
    let manifest = root
        .parent()
        .and_then(|bin| bin.parent())
        .expect("package root")
        .join("manifest.json");
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(&manifest).unwrap_or_else(|e| panic!("read {}: {e}", manifest.display())),
    )
    .expect("the package's own manifest");
    let _ = couch_plugin::serve::<Probe>(manifest);
}
