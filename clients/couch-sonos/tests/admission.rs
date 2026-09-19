//! Sonos's five admission cases, against a fake local Control API.
//!
//! The shared harness in `couch_plugin::testing` owns every assertion; this
//! file supplies only the fake player, the settings that address it, and the
//! HTTP dialogue each case expects. Requests are spelled as the fixture logs
//! them - method and path - which is the same role a Denon line plays in
//! `couch-denon`'s file.

mod fake;

use couch_plugin::{
    testing::{self, Adapter, Conformance, Failure, Fixture, Spike, TimeoutNoRetry},
    Request,
};
use fake::{Answer, FakeSonos, Plan, GROUP, PLAYER};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// This tree's package executable, or the one `COUCH_ADMISSION_BINARY_SONOS`
/// names. `tools/tests/old-package-wire.sh` sets it to an executable built
/// from the source the published Sonos package was built from.
fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var_os("COUCH_ADMISSION_BINARY_SONOS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_couch-plugin-sonos")))
    })
}

fn adapter() -> Adapter<'static> {
    Adapter {
        binary: binary(),
        manifest_json: include_str!("../plugin.json"),
    }
}

const INFO: &str = "GET /api/v1/players/local/info";
const GROUPS: &str = "GET /api/v1/households/local/groups";
const VOLUME: &str = "GET /api/v1/players/RINCON_TEST/playerVolume";
const FAVORITES: &str = "GET /api/v1/households/local/favorites";
const PLAYLISTS: &str = "GET /api/v1/households/local/playlists";
const TOGGLE: &str = "POST /api/v1/groups/RINCON_TEST:1/playback/togglePlayPause";

/// Four packages start at once against a player nothing answers for. The
/// package slot, the executable copy and the child's socket must not collide,
/// and startup must reach none of them.
///
/// Four rather than `couch-denon`'s sixteen. This adapter links rustls and ring,
/// so its executable is four times the size, and macOS validates the signature
/// of each fresh copy on its first exec: sixteen concurrent execs measure about
/// 2.4 seconds there on an idle machine, against the host's own three-second
/// startup deadline, and eight still misses it while the rest of the workspace
/// is running. A larger number would be a timing test, not a race test. Every
/// startup still goes through the same shared slot allocator, and `couch-denon`
/// keeps the wider sweep against a small binary.
#[test]
fn concurrent_package_startup_is_offline_and_race_free() {
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let package = testing::Package::new(adapter());
                drop(package.endpoint(
                    json!({"host": "192.0.2.10"}),
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
        Conformance {
            offline_settings: json!({"host": "192.0.2.10"}),
            // A name is not an address: this player is reached by IPv4 only.
            invalid_settings: json!({"host": "sonos.local"}),
            device: Fixture::new(|| FakeSonos::start(Plan::healthy("PLAYBACK_STATE_PLAYING"))),
            command: "play-pause",
            // Identity once, then the group read every playback write owes the
            // household, the write itself, the reading, and the two source
            // listings behind the picker.
            expected_requests: &[INFO, GROUPS, TOGGLE, GROUPS, VOLUME, FAVORITES, PLAYLISTS],
            check: |status, inputs| {
                assert_eq!(status.playing, Some(true));
                assert_eq!(status.muted, Some(true));
                assert_eq!(status.volume, Some(17));
                assert_eq!(status.on, None, "a player has no power state to report");
                assert_eq!(
                    status.input, None,
                    "a player does not say which favourite it is playing"
                );
                assert_eq!(
                    inputs
                        .iter()
                        .map(|input| input.id.as_str())
                        .collect::<Vec<_>>(),
                    ["tv", "line-in", "favorite.4", "favorite.9", "playlist.1"]
                );
                assert_eq!(inputs[2].name, "Dreaming");
            },
        },
    );
}

#[test]
fn failure() {
    // Reached only after the group read, so both failure paths log the same
    // two requests and differ only in how the player ends the second one.
    let after_identity = |second: Answer| {
        Fixture::new(move || {
            FakeSonos::start(
                Plan::new()
                    .on(INFO, Answer::ok(fake::info()))
                    .on(GROUPS, second.clone()),
            )
        })
    };
    testing::failure(
        adapter(),
        Failure {
            idle: Fixture::new(|| FakeSonos::start(Plan::new().otherwise(Answer::Silence))),
            // A Sonos player has no power state, so this is never declared and
            // the manifest gate owes the household silence.
            unknown_command: "power-off",
            request: Request::command("play"),
            malformed: after_identity(Answer::ok("not a Control API document")),
            malformed_requests: &[INFO, GROUPS],
            disconnected: after_identity(Answer::Close),
            disconnected_requests: &[INFO, GROUPS],
        },
    );
}

#[test]
fn timeout_no_retry() {
    testing::timeout_no_retry(
        adapter(),
        TimeoutNoRetry {
            // The group read never answers, so `play` cannot be known to have
            // happened; the source listings still answer, so the replacement
            // child can prove it recovered without replaying the write.
            device: Fixture::new(|| {
                FakeSonos::start(
                    Plan::new()
                        .on(INFO, Answer::ok(fake::info()))
                        .on(GROUPS, Answer::Silence)
                        .on(FAVORITES, Answer::ok(fake::favorites()))
                        .on(PLAYLISTS, Answer::ok(fake::playlists())),
                )
            }),
            command: "play",
            timed_out_requests: &[INFO, GROUPS],
            recovery_request: Request::Inputs,
            recovery_requests: &[INFO, FAVORITES, PLAYLISTS],
        },
    );
}

#[test]
fn spike() {
    testing::spike(
        adapter(),
        Spike {
            // Nothing is ever answered, so the first caller holds the endpoint
            // for its whole deadline and every later caller finds the queue.
            device: Fixture::new(|| FakeSonos::start(Plan::new().otherwise(Answer::Silence))),
            command: "play-pause",
            // The connection identifies itself before anything else; no queued
            // command may follow it.
            initial_requests: &[INFO],
        },
    );
}

/// The manifest is the package's half of the contract, and `serve` refuses to
/// start if it disagrees with the client. Assert the parts a reviewer reads:
/// the binary the catalog names, and the ids the picker will send.
#[test]
fn the_manifest_names_this_package_and_nothing_it_cannot_do() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
    assert_eq!(manifest["id"], "sonos");
    assert_eq!(manifest["executable"], "bin/couch-plugin-sonos");
    assert_eq!(manifest["supports_inputs"], true);
    assert!(
        manifest["settings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["id"] == "api_key"
                && field["kind"] == "secret"
                && field["default"].is_null()),
        "the API key must be a secret setting with no default: {manifest}"
    );
    assert_eq!(PLAYER, "RINCON_TEST");
    assert_eq!(GROUP, "RINCON_TEST:1");
}
