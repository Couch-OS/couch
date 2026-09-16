use couch_integrations::run_cli;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "couch-integration-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn args(&self, args: &[&str]) -> Vec<String> {
        let mut result = vec!["--root".into(), self.0.to_string_lossy().into_owned()];
        result.extend(args.iter().map(|s| (*s).to_owned()));
        result
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn trailing_arguments_are_rejected_before_any_filesystem_action() {
    for args in [
        vec!["list", "extra"],
        vec!["remove", "echo", "extra"],
        vec!["rollback", "echo", "extra"],
        vec!["install-sideload", "missing.apk", "extra"],
        vec![
            "install-repository",
            "couch-integration-echo",
            "--repository",
            "https://example.invalid",
            "extra",
        ],
    ] {
        let scratch = Scratch::new();
        let error = run_cli(scratch.args(&args)).unwrap_err();
        assert!(error.to_string().contains("unexpected"), "{error}");
        assert!(
            !scratch.0.exists(),
            "invalid command created the store: {args:?}"
        );
    }
}

#[test]
fn invalid_remove_does_not_delete_an_existing_slot() {
    let scratch = Scratch::new();
    let slot = scratch.0.join("slots/echo/1");
    fs::create_dir_all(&slot).unwrap();
    let payload = slot.join("sentinel");
    fs::write(&payload, b"keep").unwrap();
    assert!(run_cli(scratch.args(&["remove", "echo", "extra"])).is_err());
    assert_eq!(fs::read(&payload).unwrap(), b"keep");
}

#[test]
fn missing_required_arguments_do_not_create_the_store() {
    for args in [
        vec![],
        vec!["--keys-dir"],
        vec!["--apk"],
        vec!["install-sideload"],
        vec!["install-repository", "echo"],
        vec!["install-repository", "echo", "--repository"],
        vec!["rollback"],
        vec!["remove"],
        vec!["unknown"],
    ] {
        let scratch = Scratch::new();
        assert!(run_cli(scratch.args(&args)).is_err());
        assert!(!scratch.0.exists());
    }
}
