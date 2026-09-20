//! What one integration package's user may do to another's, asked of the
//! kernel rather than of the code.
//!
//! Every case here needs real user ids, so every case is `#[ignore]`d and
//! returns without asserting anything unless it is run as root. CI runs it as
//! a separate step; on a developer's machine `cargo test` walks past it.
//!
//! The children are real packages started by the real host, so what they
//! report is what the Denon, Sonos and Kodi packages will find on the remote.
#![cfg(target_os = "linux")]

use couch_plugin::{is_non_dumpable, Host, HostPolicy, Manifest};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// The two users a case gives its children. Nothing on disk belongs to either.
const FIRST: u32 = 60001;
const SECOND: u32 = 60002;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Ubuntu ships `kernel.yama.ptrace_scope = 1`, which stops a process reading
/// a sibling's memory whatever the two users are. The HA100's kernel has no
/// Yama at all. Where it is on, the one assertion that needs a read to
/// *succeed* cannot be made, and says so rather than passing quietly.
fn yama_restricts_siblings() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .map(|value| value.trim() != "0")
        .unwrap_or(false)
}

struct Package {
    root: PathBuf,
    manifest: Manifest,
}
impl Package {
    fn new(id: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "couch-plugin-isolation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for directory in [&root, &root.join("bin")] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let manifest: Manifest = serde_json::from_value(json!({
            "protocol_version": 1,
            "id": id,
            "label": "Probe",
            "version": "1.0.0",
            "executable": "bin/plugin",
            "capabilities": [],
            "settings": [
                {"id": "op", "label": "Question", "kind": "text", "required": true},
                {"id": "arg", "label": "Argument", "kind": "text", "default": ""}
            ]
        }))
        .unwrap();
        let described = root.join("manifest.json");
        std::fs::write(&described, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::set_permissions(described, std::fs::Permissions::from_mode(0o644)).unwrap();
        Self { root, manifest }
    }

    /// The real package: `serve`, and so the real loss of dumpability.
    fn probe() -> Self {
        let package = Package::new("probe");
        package.install(std::fs::read(env!("CARGO_BIN_EXE_couch-plugin-probe")).unwrap());
        package
    }

    /// A package built before this SDK: it answers the handshake and never
    /// makes itself undumpable, which is what the control needs.
    fn older(body: &str) -> Self {
        let package = Package::new("older");
        let hello = frame(&json!({"id":1,"body":{"type":"hello","manifest":package.manifest}}));
        package.install(format!("#!/bin/sh\n{hello}{body}").into_bytes());
        package
    }

    fn install(&self, bytes: Vec<u8>) {
        let executable = self.root.join("bin/plugin");
        std::fs::write(&executable, bytes).unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn start(&self, uid: u32) -> Host {
        Host::spawn_with_policy(
            &self.root,
            &self.manifest,
            Duration::from_secs(5),
            HostPolicy::for_package(uid, uid),
        )
        .expect("package handshake")
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One question to a probe child, and its one-word answer.
fn ask(host: &mut Host, op: &str, arg: &str) -> String {
    host.configure(json!({"op": op, "arg": arg})).unwrap();
    host.status().unwrap().title.unwrap()
}

/// A shell line that prints one protocol frame, as the other host tests do.
fn frame(value: &serde_json::Value) -> String {
    let mut bytes = Vec::new();
    couch_plugin::write_frame(&mut bytes, value).unwrap();
    let escaped: String = bytes.iter().map(|byte| format!("\\{byte:03o}")).collect();
    format!("printf '{escaped}'\n")
}

/// The same frame with its one interesting value computed while the script
/// runs. Fixed test text, never anything a person typed.
fn dynamic_status(value: &str) -> String {
    format!(
        r#"printf '\000\000\000'
body="{{\"id\":2,\"body\":{{\"type\":\"status\",\"status\":{{\"title\":\"{value}\"}}}}}}"
octal=$(printf '%03o' "${{#body}}")
printf "\\$octal"
printf '%s' "$body"
"#
    )
}

/// A root-owned file only root may read, standing in for a stored key.
fn private_file() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "couch-plugin-isolation-secret-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, b"a pairing key").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
#[ignore = "needs root: it starts children as two different users"]
fn two_packages_cannot_reach_into_one_another() {
    if !root() {
        eprintln!("not root: nothing was asserted");
        return;
    }
    let package = Package::probe();
    let mut second = package.start(SECOND);
    let target = second.pid();
    assert_eq!(ask(&mut second, "self", ""), describe(SECOND));

    let mut first = package.start(FIRST);
    assert_eq!(ask(&mut first, "self", ""), describe(FIRST));
    assert_eq!(
        ask(&mut first, "open", &format!("/proc/{target}/mem")),
        "EACCES"
    );
    assert_eq!(
        ask(&mut first, "open", &format!("/proc/{target}/environ")),
        "EACCES"
    );
    assert_eq!(ask(&mut first, "ptrace", &target.to_string()), "EPERM");
    assert_eq!(ask(&mut first, "kill", &target.to_string()), "EPERM");

    let secret = private_file();
    assert_eq!(ask(&mut first, "open", secret.to_str().unwrap()), "EACCES");
    let _ = std::fs::remove_file(secret);
}

/// The users alone would not do it: two children of one package share a user,
/// and before `serve` gave up being dumpable that was enough to read each
/// other. This is both halves of that, one user throughout.
#[test]
#[ignore = "needs root: it starts children as a user of their own"]
fn a_different_user_is_not_what_closes_proc() {
    if !root() {
        eprintln!("not root: nothing was asserted");
        return;
    }
    let older = Package::older("exec /bin/sleep 30\n");
    let dumpable = older.start(FIRST);
    let target = dumpable.pid();
    assert_eq!(is_non_dumpable(target), Some(false));

    if yama_restricts_siblings() {
        eprintln!(
            "kernel.yama.ptrace_scope is on: skipping the read that must succeed. \
             The HA100's kernel has no Yama."
        );
    } else {
        let reader = Package::older(&format!(
            "if : < /proc/{target}/environ 2>/dev/null; then answer=open; else answer=denied; fi\n{}exec /bin/sleep 30\n",
            dynamic_status("$answer")
        ));
        let mut reader = reader.start(FIRST);
        assert_eq!(
            reader.status().unwrap().title.as_deref(),
            Some("open"),
            "one user is all two older packages ever had between them"
        );
    }

    // The same user, the same two files, a package built with this SDK.
    let package = Package::probe();
    let mut hidden = package.start(FIRST);
    let target = hidden.pid();
    assert_eq!(ask(&mut hidden, "self", ""), describe(FIRST));
    assert_eq!(is_non_dumpable(target), Some(true));
    let mut other = package.start(FIRST);
    assert_eq!(
        ask(&mut other, "open", &format!("/proc/{target}/environ")),
        "EACCES"
    );
    assert_eq!(
        ask(&mut other, "open", &format!("/proc/{target}/mem")),
        "EACCES"
    );
}

/// A policy naming root is a bug in whatever built it - a store that somehow
/// answered zero, a caller passing a default it should not have. The spawn
/// fails rather than starting a package as root, and the check lives in
/// `pre_exec`, so it holds however the policy was arrived at.
#[test]
#[ignore = "needs root: there is nothing to drop from otherwise"]
fn a_package_is_never_started_as_root() {
    if !root() {
        eprintln!("not root: nothing was asserted");
        return;
    }
    let package = Package::probe();
    for (uid, gid) in [(0, 0), (0, FIRST), (FIRST, 0)] {
        assert!(
            Host::spawn_with_policy(
                &package.root,
                &package.manifest,
                Duration::from_secs(5),
                HostPolicy::for_package(uid, gid),
            )
            .is_err(),
            "a package was started as uid {uid} gid {gid}"
        );
    }
    // The same package under a real user still starts, so what was refused
    // above is the policy and not the package.
    assert_eq!(ask(&mut package.start(FIRST), "self", ""), describe(FIRST));
}

#[test]
fn a_child_is_only_called_undumpable_when_that_can_be_told_apart() {
    if root() {
        return;
    }
    // Unprivileged, a child shares this process's user, so who owns its
    // /proc entries says nothing. The host must not read an answer into that.
    assert_eq!(is_non_dumpable(std::process::id()), None);
}

/// What a probe child says about itself when the host has done its part.
fn describe(uid: u32) -> String {
    let mut groups: Vec<String> = HostPolicy::for_package(uid, uid)
        .supplementary_gids()
        .iter()
        .map(|gid| gid.to_string())
        .collect();
    groups.sort();
    format!(
        "uid={uid} gid={uid} groups=[{}] core=0,0 nnp=1 status_nnp=1",
        groups.join(",")
    )
}
