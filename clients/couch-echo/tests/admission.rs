use couch_plugin::{
    testing::{self, Adapter, ConformanceCase, FailureCase, SpikeCase, TimeoutCase},
    Request,
};
use couch_sdk::testing::{MockHost, Reply, Script};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// This tree's package executable, or the one `COUCH_ADMISSION_BINARY_ECHO`
/// names. `tools/tests/old-package-wire.sh` sets it to an executable built
/// from the source the published packages were built from, so that this
/// tree's host is proved against a child that refuses every unknown field.
fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var_os("COUCH_ADMISSION_BINARY_ECHO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_couch-plugin-echo")))
    })
}

fn adapter() -> Adapter<'static> {
    Adapter {
        binary: binary(),
        manifest_json: include_str!("../plugin.json"),
    }
}

fn settings(device: &MockHost) -> serde_json::Value {
    json!({"host":device.host(),"port":device.port()})
}

#[test]
fn conformance() {
    testing::conformance(
        adapter(),
        ConformanceCase {
            offline_settings: json!({"host":"127.0.0.1","port":1,"token":"private"}),
            invalid_settings: json!({"host":"127.0.0.1","port":0}),
            device_settings: settings,
            script: Script::new()
                .terminator(b'\n')
                .on("CMD volume-up", Reply::line("OK"))
                .on(
                    "GET STATUS",
                    Reply::line("STATUS power=on;mute=off;volume=31;input=hdmi1"),
                )
                .on(
                    "LIST INPUTS",
                    Reply::Lines(vec!["INPUT hdmi1 Console".into(), "END".into()]),
                ),
            command: "volume-up",
            expected_requests: &["CMD volume-up", "GET STATUS", "LIST INPUTS"],
            check: |status, inputs| {
                assert_eq!(status.on, Some(true));
                assert_eq!(status.volume, Some(31));
                assert_eq!(status.input.as_deref(), Some("hdmi1"));
                assert_eq!(inputs[0].id, "hdmi1");
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
            unknown_command: "mute-on",
            request: Request::command("volume-up"),
            malformed_requests: &["CMD volume-up"],
            disconnected_requests: &["CMD volume-up"],
            malformed: Script::new()
                .terminator(b'\n')
                .on("CMD volume-up", Reply::line("not-a-valid-reply")),
            disconnected: Script::new()
                .terminator(b'\n')
                .on("CMD volume-up", Reply::Close),
        },
    );
}

#[test]
fn timeout_no_retry() {
    testing::timeout_no_retry(
        adapter(),
        TimeoutCase {
            device_settings: settings,
            script: Script::new()
                .terminator(b'\n')
                .on("CMD volume-up", Reply::Silence)
                .on("GET STATUS", Reply::line("STATUS power=on")),
            command: "volume-up",
            timed_out_requests: &["CMD volume-up"],
            recovery_request: Request::status(),
            recovery_requests: &["GET STATUS"],
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
            initial_requests: &["CMD volume-up"],
            terminator: b'\n',
        },
    );
}
