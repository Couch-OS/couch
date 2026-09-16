use couch_plugin::{
    testing::{self, Adapter, ConformanceCase, FailureCase, SpikeCase, TimeoutCase},
    Request,
};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::json;
use std::path::Path;

fn adapter() -> Adapter<'static> {
    Adapter {
        binary: Path::new(env!("CARGO_BIN_EXE_couch-plugin-denon")),
        manifest_json: include_str!("../plugin.json"),
    }
}

fn settings(device: &MockHost) -> serde_json::Value {
    json!({"host":device.host(),"port":device.port()})
}

#[test]
fn concurrent_package_startup_is_offline_and_race_free() {
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                let package = testing::Package::new(adapter());
                drop(package.endpoint(
                    json!({"host":"127.0.0.1","port":1}),
                    std::time::Duration::from_secs(3),
                ));
            });
        }
    });
}

#[test]
fn conformance() {
    testing::conformance(
        adapter(),
        ConformanceCase {
            offline_settings: json!({"host":"127.0.0.1","port":1}),
            invalid_settings: json!({"host":"127.0.0.1","port":0}),
            device_settings: settings,
            script: Script::new()
                .terminator(b'\r')
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
            command: "mute-on",
            expected_requests: &["MUON", "MU?", "ZM?", "MV?", "MU?", "SI?", "SSFUN ?"],
            check: |status, inputs| {
                assert_eq!(status.on, Some(true));
                assert_eq!(status.muted, Some(true));
                assert_eq!(status.input.as_deref(), Some("BD"));
                assert_eq!(status.volume, None);
                assert_eq!(inputs[0].id, "BD");
            },
        },
    );
}

#[test]
fn failure() {
    testing::failure(
        adapter(),
        FailureCase {
            device_settings: settings,
            unknown_command: "play-pause",
            request: Request::Command {
                function: "mute-on".into(),
            },
            malformed_requests: &["MUON", "MU?"],
            disconnected_requests: &["MUON"],
            malformed: Script::new()
                .terminator(b'\r')
                .on("MUON", Reply::Silence)
                .on("MU?", Reply::line("X".repeat(1025))),
            disconnected: Script::new().terminator(b'\r').on("MUON", Reply::Close),
        },
    );
}

#[test]
fn timeout_no_retry() {
    let script = Script::new()
        .terminator(b'\r')
        .on("MVUP", Reply::Silence)
        .on("MV?", Reply::Silence)
        .on(
            "SSFUN ?",
            Reply::Lines(vec!["SSFUNBD Blu-ray".into(), "SSFUN END".into()]),
        );
    testing::timeout_no_retry(
        adapter(),
        TimeoutCase {
            device_settings: settings,
            script,
            command: "volume-up",
            timed_out_requests: &["MVUP", "MV?"],
            recovery_request: Request::Inputs,
            recovery_requests: &["SSFUN ?"],
        },
    );
}

#[test]
fn spike() {
    testing::spike(
        adapter(),
        SpikeCase {
            device_settings: settings,
            command: "volume-up",
            initial_requests: &["MVUP", "MV?"],
            terminator: b'\r',
        },
    );
}
