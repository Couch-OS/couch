use couch_plugin::{Error, Host, Manifest};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

struct Package(PathBuf);
impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_denon_subprocess_controls_fake_avr_with_sdk_confirmation_and_capability_gate() {
    let package =
        Package(std::env::temp_dir().join(format!("couch-denon-plugin-{}", std::process::id())));
    std::fs::create_dir_all(package.0.join("bin")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_couch-plugin-denon"),
        package.0.join("bin/couch-plugin-denon"),
    )
    .unwrap();
    let manifest: Manifest = serde_json::from_str(include_str!("../plugin.json")).unwrap();
    let mut host = Host::spawn(&package.0, &manifest, Duration::from_secs(5)).unwrap();
    host.configure(json!({"host":"127.0.0.1","port":1}))
        .unwrap();
    let device = MockHost::start(
        Script::new()
            .on("MUON", Reply::line("MUON"))
            .on("MU?", Reply::line("MUON"))
            .on("ZM?", Reply::line("ZMON"))
            .on("MV?", Reply::line("MV455"))
            .on("MU?", Reply::line("MUON"))
            .on("SI?", Reply::line("SIBD"))
            .on(
                "SSFUN ?",
                Reply::Lines(vec!["SSFUNBD Blu-ray".into(), "SSFUN END".into()]),
            ),
    );
    host.configure(json!({"host":device.host(),"port":device.port()}))
        .unwrap();
    assert!(device.requests().is_empty());
    assert_eq!(host.command("play-pause"), Err(Error::Unsupported));
    assert_eq!(host.command("input:BD\rMV98"), Err(Error::Unsupported));
    host.command("mute-on").unwrap();
    let state = host.status().unwrap();
    assert_eq!(state.on, Some(true));
    assert_eq!(state.muted, Some(true));
    assert_eq!(state.volume, None);
    assert_eq!(host.inputs().unwrap()[0].id, "BD");
    assert_eq!(
        device.requests(),
        ["MUON", "MU?", "ZM?", "MV?", "MU?", "SI?", "SSFUN ?"]
    );
}
