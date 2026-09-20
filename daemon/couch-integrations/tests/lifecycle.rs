//! Real host handshake and store transactions; extraction is a test fixture.
//! Native APK signature verification is covered by tools/integrations/smoke.sh.
use couch_integrations::Store;
use flate2::{write::GzEncoder, Compression};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("couch-lifecycle-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let apk = root.join("fixture-apk");
        fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
        fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
        Self(root)
    }
    fn package(&self, version: &str, configure_ok: bool, suffix: &str) -> PathBuf {
        let manifest = json!({"protocol_version":1,"id":"fixture","label":"Fixture",
            "version":version,"executable":"bin/plugin","capabilities":[],"settings":[]});
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":if configure_ok {json!({"type":"ok"})} else {json!({"type":"error","code":"invalid"})}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str(&format!("# {suffix}\nsleep 5\n"));
        let package = self.0.join(format!("fixture-{version}-{suffix}.apk"));
        let mut archive = tar::Builder::new(GzEncoder::new(
            fs::File::create(&package).unwrap(),
            Compression::default(),
        ));
        for (name, content, mode) in [
            (
                "manifest.json",
                serde_json::to_vec(&manifest).unwrap(),
                0o644,
            ),
            ("bin/plugin", script.into_bytes(), 0o755),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(content.len() as u64);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    format!("usr/lib/couch/integrations/fixture/{name}"),
                    content.as_slice(),
                )
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        package
    }
}

#[test]
fn upgrade_and_rollback_validate_saved_settings_and_preserve_atomic_history() {
    let fixture = Fixture::new();
    let store = Store::new(fixture.0.join("integrations")).with_apk(fixture.0.join("fixture-apk"));
    let old = fixture.package("1.0.0", false, "old");
    let new = fixture.package("2.0.0", true, "new");
    store.install(&old).unwrap();
    store.install(&new).unwrap();
    let generation = store.generation("fixture").unwrap();
    store.install(&new).unwrap();
    assert_eq!(store.generation("fixture").unwrap(), generation);
    let changed = fixture.package("2.0.0", true, "changed");
    assert!(store
        .install(&changed)
        .unwrap_err()
        .to_string()
        .contains("different contents"));
    assert_eq!(store.generation("fixture").unwrap(), generation);

    let mut config = couch_model::Config::default();
    config.connections.push(couch_model::Connection {
        id: couch_model::Id::new("connection"),
        name: "Fixture".into(),
        provider: couch_model::Provider::Plugin {
            id: "fixture".into(),
            label: "Fixture".into(),
            capabilities: vec![],
            supports_inputs: false,
            presentation: vec![],
            actions: vec![],
            children: vec![],
        },
    });
    config.validate().unwrap();
    // Admission must inspect the full envelope, not the old-runtime projection
    // whose top-level connections intentionally omit packaged integrations.
    fs::write(
        fixture.0.join("config.json"),
        serde_json::to_vec(&couch_model::StoredConfig::new(&config)).unwrap(),
    )
    .unwrap();

    let settings = fixture
        .0
        .join("connections/connection/plugin-connection.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, b"{}").unwrap();
    assert!(store
        .rollback("fixture")
        .unwrap_err()
        .to_string()
        .contains("saved connection settings"));
    assert_eq!(store.generation("fixture").unwrap(), generation);
    assert_eq!(fs::read(&settings).unwrap(), b"{}");
    assert_eq!(store.resolve("fixture").unwrap().1.version, "2.0.0");

    fs::remove_file(&settings).unwrap();
    assert_eq!(store.rollback("fixture").unwrap().version, "1.0.0");
    assert_eq!(store.rollback("fixture").unwrap().version, "2.0.0");
    assert_eq!(store.generation("fixture").unwrap(), generation);
    store.remove("fixture").unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(fixture.0.join("config.json").exists());
}
