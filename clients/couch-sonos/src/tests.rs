//! Loopback JSON fixtures, not speaker commands: every test drives the real
//! request path against a scripted server on 127.0.0.1.
use super::*;
use std::net::SocketAddr;

pub(crate) const PLAYER: &str = "RINCON_TEST";
const OTHER: &str = "RINCON_OTHER";
pub(crate) const KEY: &str = "fixture-key";

pub(crate) fn info() -> String {
    serde_json::json!({
        "_objectType": "discoveryInfo",
        "playerId": PLAYER,
        "householdId": "Sonos_test",
        "groupId": "RINCON_TEST:1",
        "restUrl": "https://192.0.2.10:1443/api",
        "device": {
            "_objectType": "deviceInfo",
            "id": PLAYER,
            "name": "Living room & kitchen",
            "model": "S19",
            "modelDisplayName": "Arc",
            "capabilities": ["PLAYBACK", "CLOUD"],
        },
    })
    .to_string()
}
/// The stock fixture with extra capabilities, for the source picker's gates.
pub(crate) fn info_with(capabilities: &[&str]) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&info()).unwrap();
    value["device"]["capabilities"] = serde_json::json!(capabilities);
    value.to_string()
}
pub(crate) fn favorites() -> String {
    serde_json::json!({
        "version": "RINCON_TEST:14",
        "items": [
            {"id": "4", "name": "Dreaming", "description": "By Marshmello", "service": {"name": "Apple Music"}},
            {"id": "6", "name": "Made for Spatial Audio", "description": "Apple Music Playlist", "service": {"name": "Apple Music"}},
            {"id": "9", "name": "Hotel Lobby", "description": "Apple Music", "service": {"name": "Apple Music"}},
            {"id": "7", "name": "Radio", "description": "", "service": {"name": ""}},
            {"id": "", "name": "Broken", "description": "no id", "service": {"name": "x"}},
        ],
    })
    .to_string()
}
pub(crate) fn playlists() -> String {
    serde_json::json!({
        "version": "RINCON_TEST:6",
        "playlists": [
            {"id": "1", "name": "All Songs", "type": "playlist", "trackCount": 2760},
            {"id": "0", "name": "One", "type": "playlist", "trackCount": 1},
            {"id": "2", "name": "Unknown size", "type": "playlist"},
        ],
    })
    .to_string()
}
fn playback(state: &str, position: u64) -> String {
    serde_json::json!({
        "_objectType": "playbackStatus",
        "playbackState": state,
        "positionMillis": position,
        "itemId": "7",
        "playModes": {"repeat": false, "repeatOne": true, "shuffle": true, "crossfade": false},
        "availablePlaybackActions": {"canSeek": true, "canSkip": true, "canSkipBack": false, "canPause": true},
    })
    .to_string()
}
fn metadata() -> String {
    serde_json::json!({
        "_objectType": "metadataStatus",
        "container": {"name": "Made for Spatial Audio", "type": "playlist", "id": {"serviceId": "65435", "objectId": "0"}},
        "currentItem": {"track": {"name": "Sun Ain't Even Gone Down Yet", "artist": {"name": "Brothers Osborne"}, "album": {"name": "Brothers Osborne"},
            "imageUrl": "http://192.0.2.10:1400/getaa?s=1&u=x", "durationMillis": 182000, "service": {"name": "Apple Music"}}},
        "nextItem": {"track": {"name": "Now And Then", "artist": {"name": "The Beatles"}, "durationMillis": 248000}},
    })
    .to_string()
}
pub(crate) fn groups(coordinator: &str, state: &str) -> String {
    serde_json::json!({
        "_objectType": "groups",
        "groups": [{
            "_objectType": "group",
            "id": "RINCON_TEST:1",
            "name": "Living room",
            "coordinatorId": coordinator,
            "playbackState": state,
            "playerIds": [PLAYER, OTHER],
        }],
        "players": [
            {"_objectType": "player", "id": PLAYER, "name": "Living room & kitchen"},
            {"_objectType": "player", "id": OTHER, "name": "Sonos Arc"},
        ],
    })
    .to_string()
}
pub(crate) fn volume(level: u16, muted: bool) -> String {
    serde_json::json!({"_objectType": "playerVolume", "volume": level, "muted": muted, "fixed": false})
        .to_string()
}
fn ok() -> (u16, String) {
    (200, "{}".into())
}

pub(crate) struct Recorded {
    method: String,
    url: String,
    key: String,
    content_type: String,
    body: String,
}
pub(crate) type Fixture = (String, std::thread::JoinHandle<Vec<Recorded>>);
/// Serve exactly the scripted replies, then assert nothing further was sent:
/// a refusal path must not leave a mutation on the wire.
pub(crate) fn server(replies: Vec<(u16, String)>) -> Fixture {
    server_with(replies, vec![])
}
fn server_with(
    replies: Vec<(u16, String)>,
    headers: Vec<(usize, &'static str, String)>,
) -> Fixture {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let SocketAddr::V4(address) = server.server_addr().to_ip().unwrap() else {
        panic!("expected IPv4 fixture address")
    };
    let thread = std::thread::spawn(move || {
        let mut requests = vec![];
        for (index, (status, body)) in replies.into_iter().enumerate() {
            let mut request = server
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .expect("missing request");
            let header = |name: &'static str| {
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv(name))
                    .map(|h| h.value.as_str().to_owned())
                    .unwrap_or_default()
            };
            let (key, content_type) = (header(KEY_HEADER), header("Content-Type"));
            let mut data = String::new();
            request.as_reader().read_to_string(&mut data).unwrap();
            requests.push(Recorded {
                method: request.method().as_str().to_owned(),
                url: request.url().to_owned(),
                key,
                content_type,
                body: data,
            });
            let mut response = tiny_http::Response::from_string(body).with_status_code(status);
            for (at, name, value) in &headers {
                if *at == index {
                    response.add_header(
                        tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap(),
                    );
                }
            }
            request.respond(response).unwrap();
        }
        assert!(server
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .is_none());
        requests
    });
    (format!("http://{address}/api/v1"), thread)
}
fn connect(base: &str) -> Client {
    Client::connect_url(base, KEY).unwrap()
}
/// A directory this run alone owns. A fixed name under a shared temporary
/// directory is a symlink somebody else can plant and a collision between two
/// concurrent runs, so the name carries the process, the clock and a counter,
/// and `create_dir` refuses to adopt anything already there.
fn scratch() -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "couch-sonos-test-{}-{nanos}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).expect("a scratch directory of this run's own");
    path
}

#[test]
fn connect_identifies_the_player_and_sends_the_api_key() {
    let (base, thread) = server(vec![(200, info())]);
    let client = connect(&base);
    assert_eq!(client.player().uuid, PLAYER);
    assert_eq!(client.player().name, "Living room & kitchen");
    assert_eq!(client.player().model, "Arc");
    let requests = thread.join().unwrap();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, "/api/v1/players/local/info");
    assert_eq!(requests[0].key, KEY);
}
#[test]
fn hosts_without_local_playback_control_are_rejected() {
    // A bonded subwoofer answers the API but advertises no PLAYBACK capability.
    let sub = serde_json::json!({
        "_objectType": "discoveryInfo",
        "playerId": "RINCON_SUB",
        "householdId": "Sonos_test",
        "device": {"name": "Sub", "modelDisplayName": "Sub", "capabilities": ["CLOUD"]},
    })
    .to_string();
    for body in [sub, r#"{"hello":"world"}"#.into(), "{}".into()] {
        let (base, thread) = server(vec![(200, body)]);
        assert_eq!(
            Client::connect_url(&base, KEY).err(),
            Some(Error::Unsupported)
        );
        thread.join().unwrap();
    }
    let (base, thread) = server(vec![(200, "not json".into())]);
    assert_eq!(Client::connect_url(&base, KEY).err(), Some(Error::Response));
    thread.join().unwrap();
}
#[test]
fn playback_commands_use_official_group_paths() {
    let commands = [
        ("play", "play"),
        ("pause", "pause"),
        // The Control API has no stop; Couch maps it to pause.
        ("stop", "pause"),
        ("play-pause", "togglePlayPause"),
        ("next", "skipToNextTrack"),
        ("previous", "skipToPreviousTrack"),
    ];
    let mut replies = vec![(200, info())];
    for _ in commands {
        replies.push((200, groups(PLAYER, "PLAYBACK_STATE_IDLE")));
        replies.push(ok());
    }
    let (base, thread) = server(replies);
    let client = connect(&base);
    for (command, _) in commands {
        client.command(command).unwrap();
    }
    let requests = thread.join().unwrap();
    for (index, (_, action)) in commands.iter().enumerate() {
        let topology = &requests[1 + index * 2];
        assert_eq!(topology.method, "GET");
        assert_eq!(topology.url, "/api/v1/households/local/groups");
        let write = &requests[2 + index * 2];
        assert_eq!(write.method, "POST");
        assert_eq!(
            write.url,
            format!("/api/v1/groups/RINCON_TEST:1/playback/{action}")
        );
        assert_eq!(write.content_type, "application/json");
        assert_eq!(write.key, KEY);
        assert_eq!(write.body, "{}");
    }
}
#[test]
fn volume_and_mute_address_one_player() {
    let (base, thread) = server(vec![
        (200, info()),
        ok(),
        ok(),
        ok(),
        (200, volume(10, true)),
        ok(),
        ok(),
        ok(),
    ]);
    let client = connect(&base);
    assert_eq!(client.set_volume(101), Err(Error::Volume));
    client.set_volume(25).unwrap();
    client.command("volume-up").unwrap();
    client.command("volume-down").unwrap();
    // A toggle is the only volume path that still needs a read before its write.
    client.command("mute").unwrap();
    client.command("mute-on").unwrap();
    client.command("mute-off").unwrap();
    let requests = thread.join().unwrap();
    let expected = [
        ("", r#"{"volume":25}"#),
        ("/relative", r#"{"volumeDelta":1}"#),
        ("/relative", r#"{"volumeDelta":-1}"#),
        ("", ""),
        ("/mute", r#"{"muted":false}"#),
        ("/mute", r#"{"muted":true}"#),
        ("/mute", r#"{"muted":false}"#),
    ];
    for (index, (command, body)) in expected.iter().enumerate() {
        let request = &requests[index + 1];
        assert_eq!(
            request.url,
            format!("/api/v1/players/{PLAYER}/playerVolume{command}")
        );
        assert_eq!(request.body, *body);
        assert_eq!(request.key, KEY);
        assert_eq!(request.method, if body.is_empty() { "GET" } else { "POST" });
        if !body.is_empty() {
            assert_eq!(request.content_type, "application/json");
        }
    }
}
#[test]
fn group_member_playback_is_not_forwarded() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(OTHER, "PLAYBACK_STATE_PLAYING")),
    ]);
    let client = connect(&base);
    assert_eq!(
        client.playback(Playback::Pause),
        Err(Error::NotCoordinator {
            coordinator: "Sonos Arc".into()
        })
    );
    assert_eq!(thread.join().unwrap().len(), 2);
}
#[test]
fn coordinator_moving_during_a_write_is_reported() {
    let moved = serde_json::json!({
        "_objectType": "groupCoordinatorChanged",
        "groupStatus": "GROUP_STATUS_MOVED",
        "groupName": "Sonos Arc",
        "websocketUrl": "wss://192.0.2.11:1443/websocket/api",
        "playerId": OTHER,
    })
    .to_string();
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
        (404, moved),
    ]);
    let client = connect(&base);
    assert_eq!(
        client.command("play"),
        Err(Error::NotCoordinator {
            coordinator: "Sonos Arc".into()
        })
    );
    assert_eq!(thread.join().unwrap().len(), 3);
}
#[test]
fn api_error_json_is_typed_and_never_retried() {
    let empty = r#"{"errorCode":"ERROR_PLAYBACK_NO_CONTENT","reason":"queue is empty"}"#;
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
        (400, empty.into()),
    ]);
    let client = connect(&base);
    assert_eq!(
        client.command("play"),
        Err(Error::Api("ERROR_PLAYBACK_NO_CONTENT".into()))
    );
    assert_eq!(
        Error::Api("ERROR_PLAYBACK_NO_CONTENT".into()).to_string(),
        "Sonos API error ERROR_PLAYBACK_NO_CONTENT"
    );
    assert_eq!(thread.join().unwrap().len(), 3);
}
#[test]
fn missing_api_key_error_is_surfaced_from_the_player() {
    let refused = r#"{"_objectType":"globalError","errorCode":"ERROR_API_KEY_VALIDATION_FAILED","reason":"Invalid api key"}"#;
    let (base, thread) = server(vec![(400, refused.into())]);
    assert_eq!(
        Client::connect_url(&base, KEY).err(),
        Some(Error::Api("ERROR_API_KEY_VALIDATION_FAILED".into()))
    );
    thread.join().unwrap();
    // A key that cannot be a header value never reaches the network.
    assert_eq!(
        Client::connect_url("https://192.0.2.10:1443/api/v1", "bad\nkey").err(),
        Some(Error::Api("ERROR_API_KEY_VALIDATION_FAILED".into()))
    );
}
#[test]
fn status_reports_group_state_and_player_volume() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_PLAYING")),
        (200, volume(17, false)),
    ]);
    let client = connect(&base);
    let status = client.status().unwrap();
    assert_eq!(status.player.uuid, PLAYER);
    assert_eq!(status.coordinator, PLAYER);
    assert_eq!(status.coordinator_name, "Living room & kitchen");
    assert_eq!(status.transport, "PLAYING");
    assert_eq!(status.volume, 17);
    assert!(!status.muted);
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["player"]["name"], "Living room & kitchen");
    assert_eq!(json["transport"], "PLAYING");
    assert_eq!(json["muted"], false);
    thread.join().unwrap();
}
#[test]
fn an_incomplete_volume_reading_is_refused_rather_than_defaulted() {
    // A mute toggle decides its write from `muted`; a field the player never
    // sent must not become `false` and turn into a write nobody asked for.
    let no_mute = serde_json::json!({"_objectType": "playerVolume", "volume": 20}).to_string();
    let (base, thread) = server(vec![(200, info()), (200, no_mute)]);
    let client = connect(&base);
    assert_eq!(client.command("mute"), Err(Error::Response));
    assert_eq!(
        thread.join().unwrap().len(),
        2,
        "an unusable reading must not be followed by a write"
    );
    // And "did not say" is not volume zero.
    for body in [
        serde_json::json!({"_objectType": "playerVolume", "muted": false}).to_string(),
        serde_json::json!({"_objectType": "playerVolume"}).to_string(),
        volume(101, false),
    ] {
        let (base, thread) = server(vec![
            (200, info()),
            (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
            (200, body.clone()),
            (200, body),
        ]);
        let client = connect(&base);
        assert_eq!(client.status().err(), Some(Error::Response));
        assert_eq!(client.volume(), Err(Error::Response));
        thread.join().unwrap();
    }
}
#[test]
fn ambiguous_or_missing_topology_fails_closed() {
    let none = serde_json::json!({"groups": [], "players": []}).to_string();
    let twice = serde_json::json!({
        "groups": [
            {"id": "a", "coordinatorId": PLAYER, "playerIds": [PLAYER]},
            {"id": "b", "coordinatorId": OTHER, "playerIds": [PLAYER]},
        ],
        "players": [],
    })
    .to_string();
    // A group id is spliced into a URL path: refuse one that could leave the origin.
    let escaping: Vec<String> = ["../../players", "..", "."]
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "groups": [{"id": id, "coordinatorId": PLAYER, "playerIds": [PLAYER]}],
                "players": [],
            })
            .to_string()
        })
        .collect();
    for body in [none, twice].into_iter().chain(escaping) {
        let (base, thread) = server(vec![(200, info()), (200, body)]);
        let client = connect(&base);
        assert_eq!(client.coordinator(), Err(Error::Response));
        thread.join().unwrap();
    }
    assert!(segment("RINCON_TEST:1").is_ok());
    for bad in ["", ".", "..", "a/b", "a?b", "a#b", "a%2fb", "a b"] {
        assert_eq!(segment(bad), Err(Error::Response));
    }
}
#[test]
fn expired_mute_command_stops_after_read_without_writing() {
    let (base, thread) = server(vec![(200, info()), (200, volume(10, false))]);
    let client = connect(&base);
    let checks = std::cell::Cell::new(0);
    assert_eq!(
        client.command_if_current("mute", &|| {
            let n = checks.get();
            checks.set(n + 1);
            n == 0
        }),
        Err(Error::Cancelled)
    );
    assert_eq!(thread.join().unwrap().len(), 2);
}
#[test]
fn expired_playback_command_stops_after_topology_read() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
    ]);
    let client = connect(&base);
    let checks = std::cell::Cell::new(0);
    assert_eq!(
        client.command_if_current("play", &|| {
            let n = checks.get();
            checks.set(n + 1);
            n == 0
        }),
        Err(Error::Cancelled)
    );
    assert_eq!(thread.join().unwrap().len(), 2);
    assert_eq!(client.command("power-on"), Err(Error::Command));
}
#[test]
fn oversized_response_is_rejected() {
    let (base, thread) = server(vec![(
        200,
        format!("{{\"pad\":\"{}\"}}", "x".repeat(LIMIT as usize)),
    )]);
    assert_eq!(Client::connect_url(&base, KEY).err(), Some(Error::Response));
    thread.join().unwrap();
}
#[test]
fn redirects_are_not_followed() {
    let (base, thread) = server_with(
        vec![(302, String::new())],
        vec![(0, "Location", "http://192.0.2.99/api/v1".into())],
    );
    assert_eq!(
        Client::connect_url(&base, KEY).err(),
        Some(Error::Http(302))
    );
    assert_eq!(thread.join().unwrap().len(), 1);
}
#[test]
fn a_player_is_addressed_over_tls_on_the_control_api_port() {
    assert_eq!(
        api_root(Ipv4Addr::new(192, 168, 1, 114)),
        "https://192.168.1.114:1443/api/v1"
    );
    assert_eq!(check_base(&api_root(Ipv4Addr::new(10, 0, 0, 1))), Ok(()));
    // And the agent that address is used with does not verify the player's
    // certificate: that is the documented trust level, not an accident of
    // configuration, so a change to the TLS wiring fails here.
    let (base, thread) = server(vec![(200, info())]);
    let client = connect(&base);
    let config = client.agent.config();
    assert!(config.tls_config().disable_verification());
    assert_eq!(config.max_redirects(), 0);
    thread.join().unwrap();
}
#[test]
fn only_loopback_may_drop_tls() {
    for base in [
        "https://192.0.2.10:1443/api/v1",
        "http://127.0.0.1:8080/api/v1",
        "http://localhost:8080/api/v1",
    ] {
        assert_eq!(check_base(base), Ok(()));
    }
    for base in [
        "http://192.0.2.10:1443/api/v1",
        "ftp://192.0.2.10/api/v1",
        "https://user@192.0.2.10/api/v1",
        "https://192.0.2.10/api/v1?x=1",
        "https://192.0.2.10/api/v1/",
        "https://",
    ] {
        assert_eq!(check_base(base), Err(Error::Unsupported));
    }
}
#[test]
fn api_key_prefers_environment_then_file_then_built_in_then_placeholder() {
    // Hermetic: the crate under test may itself have been compiled with a
    // built-in key, so every assertion here names the built-in explicitly.
    let choose = |candidates: [Option<String>; 2]| resolve_key_with(candidates, None).key;
    let resolve = |candidates: [Option<String>; 2]| resolve_key_with(candidates, None);
    assert_eq!(choose([Some("env".into()), Some("file".into())]), "env");
    assert_eq!(choose([None, Some(" file \n".into())]), "file");
    assert_eq!(choose([Some("  ".into()), Some("file".into())]), "file");
    assert_eq!(choose([None, None]), PLACEHOLDER_API_KEY);
    // Header-unsafe or oversized values are ignored rather than sent, and the
    // caller can tell that happened without ever being handed the value.
    assert_eq!(choose([Some("a\nb".into()), None]), PLACEHOLDER_API_KEY);
    assert_eq!(choose([Some("k".repeat(257)), None]), PLACEHOLDER_API_KEY);
    assert!(resolve([Some("a\nb".into()), None]).rejected);
    assert!(resolve([Some("a\nb".into()), Some("good".into())]).rejected);
    assert_eq!(
        resolve([Some("a\nb".into()), Some("good".into())]).key,
        "good"
    );
    // Nothing configured, and whitespace where a key would go, are not mistakes
    // worth reporting: both mean the same as an absent file.
    assert!(!resolve([None, None]).rejected);
    assert!(!resolve([Some(String::new()), Some("  \n".into())]).rejected);
    // A key compiled into the build sits below every configured value and above
    // the placeholder, and is held to the same header rule.
    assert_eq!(
        resolve_key_with([None, None], Some("built-in")).key,
        "built-in"
    );
    assert_eq!(
        resolve_key_with([None, Some("file".into())], Some("built-in")).key,
        "file"
    );
    assert_eq!(
        resolve_key_with([Some("a\nb".into()), None], Some(" built-in\n")).key,
        "built-in"
    );
    let blank = resolve_key_with([None, None], Some("  "));
    assert_eq!(blank.key, PLACEHOLDER_API_KEY);
    assert!(!blank.rejected, "an empty build variable is not configured");
    let broken = resolve_key_with([None, None], Some("a\nb"));
    assert_eq!(broken.key, PLACEHOLDER_API_KEY);
    assert!(broken.rejected, "an unusable built-in key is reported");
    if let Some(built_in) = BUILT_IN_API_KEY {
        assert_eq!(resolve_key([None, None]).key, built_in.trim());
    }
    let directory = scratch();
    let file = directory.join(KEY_FILE);
    std::fs::write(&file, "from-file\n").unwrap();
    if std::env::var_os(KEY_ENV).is_none() {
        assert_eq!(api_key_at(&file), "from-file");
        assert_eq!(
            api_key_at(Path::new("/nonexistent/sonos-api-key")),
            PLACEHOLDER_API_KEY
        );
        std::fs::write(&file, "one two\n").unwrap();
        let choice = api_key_choice_at(&file);
        assert_eq!(choice.key, PLACEHOLDER_API_KEY);
        assert!(choice.rejected, "an unusable configured key is reported");
    }
    std::fs::remove_dir_all(&directory).ok();
    assert!(key_file().ends_with(KEY_FILE));
    // The daemon knows its own configuration directory; the override still wins,
    // so no reader can end up looking at a different file from the others.
    if std::env::var_os(KEY_FILE_ENV).is_none() {
        assert_eq!(
            key_file_in(Path::new("/srv/couch")),
            Path::new("/srv/couch").join(KEY_FILE)
        );
        assert_eq!(
            key_file_in(Path::new("/srv/couch")),
            key_file_in(Path::new("/srv/couch"))
        );
    }
}

// Multicast DNS fixtures. Names are compressed exactly as players compress them.
fn labels(text: &str) -> Vec<u8> {
    let mut out = vec![];
    for label in text.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}
fn pointer(offset: usize) -> [u8; 2] {
    [0xc0 | (offset >> 8) as u8, offset as u8]
}
fn announcement(service: &str, instance: &str, host: &str, address: Option<[u8; 4]>) -> Vec<u8> {
    let additional = if address.is_some() { 2 } else { 1 };
    let mut packet: Vec<u8> = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, additional];
    let service_at = packet.len();
    packet.extend_from_slice(&labels(service));
    packet.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 10]);
    let mut rdata = vec![instance.len() as u8];
    rdata.extend_from_slice(instance.as_bytes());
    rdata.extend_from_slice(&pointer(service_at));
    packet.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    let instance_at = packet.len();
    packet.extend_from_slice(&rdata);
    packet.extend_from_slice(&pointer(instance_at));
    packet.extend_from_slice(&[0, 33, 0, 1, 0, 0, 0, 10]);
    let target = labels(host);
    packet.extend_from_slice(&((6 + target.len()) as u16).to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0, 0, 0x05, 0xa3]);
    let host_at = packet.len();
    packet.extend_from_slice(&target);
    if let Some(address) = address {
        packet.extend_from_slice(&pointer(host_at));
        packet.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 10, 0, 4]);
        packet.extend_from_slice(&address);
    }
    packet
}
#[test]
fn mdns_answer_yields_the_advertised_address() {
    let peer = Ipv4Addr::new(192, 0, 2, 50);
    let packet = announcement(
        SERVICE,
        "RINCON_TEST@Living Room",
        "sonos-test.local",
        Some([192, 0, 2, 10]),
    );
    assert_eq!(addresses(&packet, peer), vec![Ipv4Addr::new(192, 0, 2, 10)]);
    // Without a usable address record the responder itself is the candidate.
    let no_address = announcement(SERVICE, "RINCON_TEST@Living Room", "sonos-test.local", None);
    assert_eq!(addresses(&no_address, peer), vec![peer]);
    // Another service, a truncated packet and a query are all ignored.
    let other = announcement(
        "_airplay._tcp.local",
        "Some Speaker",
        "other.local",
        Some([192, 0, 2, 11]),
    );
    assert!(addresses(&other, peer).is_empty());
    for cut in [11, packet.len() - 2] {
        assert!(addresses(&packet[..cut], peer).is_empty());
    }
    let mut question = packet.clone();
    question[2] = 0;
    assert!(addresses(&question, peer).is_empty());
    assert!(addresses(&query(SERVICE), peer).is_empty());
}
#[test]
fn mdns_query_asks_for_the_sonos_service() {
    let packet = query(SERVICE);
    assert_eq!(packet[4..6], [0, 1]);
    assert_eq!(&packet[12..19], b"\x06_sonos");
    assert_eq!(packet[packet.len() - 4..], [0, 12, 0, 1]);
    // A pointer that does not move backwards must not be followed.
    let mut looping = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    looping.extend_from_slice(&pointer(12));
    assert!(read_name(&looping, 12).is_none());
}

#[test]
fn sources_list_the_players_inputs_then_favourites_then_playlists() {
    let (base, thread) = server(vec![
        (200, info_with(&["PLAYBACK", "HT_PLAYBACK", "LINE_IN"])),
        (200, favorites()),
        (200, playlists()),
    ]);
    let client = connect(&base);
    let sources = client.sources().unwrap();
    let rows: Vec<(String, String)> = sources
        .iter()
        .map(|s| (s.name.clone(), s.detail.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            ("TV", "Living room & kitchen input"),
            ("Line-in", "Living room & kitchen input"),
            ("Dreaming", "By Marshmello · Apple Music"),
            (
                "Made for Spatial Audio",
                "Apple Music Playlist · Apple Music"
            ),
            ("Hotel Lobby", "Apple Music"),
            ("Radio", "Sonos favourite"),
            ("All Songs", "Sonos playlist · 2760 tracks"),
            ("One", "Sonos playlist · 1 track"),
            ("Unknown size", "Sonos playlist"),
        ]
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(sources[0].id, SourceId::HomeTheater);
    assert_eq!(sources[1].id, SourceId::LineIn);
    assert_eq!(sources[2].id, SourceId::Favorite("4".into()));
    assert_eq!(sources[6].id, SourceId::Playlist("1".into()));
    let requests = thread.join().unwrap();
    assert_eq!(requests[1].url, "/api/v1/households/local/favorites");
    assert_eq!(requests[2].url, "/api/v1/households/local/playlists");
    assert!(requests.iter().all(|r| r.method == "GET" && r.key == KEY));
}
#[test]
fn players_without_tv_or_line_in_do_not_offer_them() {
    let (base, thread) = server(vec![(200, info()), (200, favorites()), (200, playlists())]);
    let client = connect(&base);
    let sources = client.sources().unwrap();
    assert!(sources
        .iter()
        .all(|s| !matches!(s.id, SourceId::HomeTheater | SourceId::LineIn)));
    assert_eq!(
        client.select_source(&SourceId::HomeTheater),
        Err(Error::Command)
    );
    assert_eq!(client.select_source(&SourceId::LineIn), Err(Error::Command));
    thread.join().unwrap();
}
#[test]
fn group_sources_load_through_the_coordinator_group() {
    let mut replies = vec![(200, info_with(&["PLAYBACK", "LINE_IN"]))];
    for _ in 0..3 {
        replies.push((200, groups(PLAYER, "PLAYBACK_STATE_IDLE")));
        replies.push(ok());
    }
    let (base, thread) = server(replies);
    let client = connect(&base);
    client
        .select_source(&SourceId::Favorite("4".into()))
        .unwrap();
    client
        .select_source(&SourceId::Playlist("1".into()))
        .unwrap();
    client.select_source(&SourceId::LineIn).unwrap();
    let requests = thread.join().unwrap();
    // Each load is one topology read and one write; `playOnCompletion`
    // starts playback, so nothing follows the write.
    assert_eq!(requests.len(), 7);
    let writes: Vec<(String, serde_json::Value)> = [2, 4, 6]
        .iter()
        .map(|&i| {
            assert_eq!(requests[i - 1].url, "/api/v1/households/local/groups");
            assert_eq!(requests[i].method, "POST");
            assert_eq!(requests[i].content_type, "application/json");
            (
                requests[i].url.clone(),
                serde_json::from_str(&requests[i].body).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        writes,
        [
            (
                "/api/v1/groups/RINCON_TEST:1/favorites".to_owned(),
                serde_json::json!({"favoriteId": "4", "playOnCompletion": true})
            ),
            (
                "/api/v1/groups/RINCON_TEST:1/playlists".to_owned(),
                serde_json::json!({"playlistId": "1", "playOnCompletion": true})
            ),
            (
                "/api/v1/groups/RINCON_TEST:1/playback/lineIn".to_owned(),
                serde_json::json!({"deviceId": PLAYER, "playOnCompletion": true})
            ),
        ]
    );
}
#[test]
fn tv_source_is_the_players_own_and_reads_no_topology() {
    let (base, thread) = server(vec![(200, info_with(&["PLAYBACK", "HT_PLAYBACK"])), ok()]);
    let client = connect(&base);
    client.select_source(&SourceId::HomeTheater).unwrap();
    let requests = thread.join().unwrap();
    // One write and nothing after it: home theatre starts itself.
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].url,
        format!("/api/v1/players/{PLAYER}/homeTheater")
    );
    assert_eq!(requests[1].body, "{}");
}
#[test]
fn group_member_source_selection_is_refused_before_any_write() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(OTHER, "PLAYBACK_STATE_PLAYING")),
    ]);
    let client = connect(&base);
    assert_eq!(
        client.select_source(&SourceId::Favorite("4".into())),
        Err(Error::NotCoordinator {
            coordinator: "Sonos Arc".into()
        })
    );
    thread.join().unwrap();
}
#[test]
fn expired_source_selection_stops_after_topology_read() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
    ]);
    let client = connect(&base);
    let calls = std::cell::Cell::new(0);
    let current = || {
        calls.set(calls.get() + 1);
        calls.get() == 1
    };
    assert_eq!(
        client.select_source_if_current(&SourceId::Favorite("4".into()), &current),
        Err(Error::Cancelled)
    );
    thread.join().unwrap();
}

#[test]
fn a_snapshot_is_one_topology_read_then_playback_metadata_and_volume() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_PLAYING")),
        (200, playback("PLAYBACK_STATE_PLAYING", 61500)),
        (200, metadata()),
        (200, volume(26, false)),
    ]);
    let client = connect(&base);
    let snapshot = client.snapshot().unwrap();
    assert_eq!(snapshot.status.transport, "PLAYING");
    assert_eq!(snapshot.status.volume, 26);
    assert_eq!(snapshot.playback.state, "PLAYING");
    assert_eq!(snapshot.playback.position_ms, 61500);
    assert_eq!(
        snapshot.playback.modes,
        PlayModes {
            shuffle: true,
            repeat: false,
            repeat_one: true,
            crossfade: false
        }
    );
    assert!(
        snapshot.playback.can_seek
            && snapshot.playback.can_skip
            && !snapshot.playback.can_skip_back
    );
    assert_eq!(snapshot.now_playing.container, "Made for Spatial Audio");
    assert_eq!(snapshot.now_playing.container_type, "playlist");
    let track = snapshot.now_playing.current.unwrap();
    assert_eq!(
        (
            track.name.as_str(),
            track.artist.as_str(),
            track.album.as_str(),
            track.duration_ms,
            track.service.as_str()
        ),
        (
            "Sun Ain't Even Gone Down Yet",
            "Brothers Osborne",
            "Brothers Osborne",
            Some(182000),
            "Apple Music"
        )
    );
    assert_eq!(track.image_url, "http://192.0.2.10:1400/getaa?s=1&u=x");
    let next = snapshot.now_playing.next.unwrap();
    assert_eq!(
        (
            next.name.as_str(),
            next.artist.as_str(),
            next.album.as_str()
        ),
        ("Now And Then", "The Beatles", "")
    );
    let requests = thread.join().unwrap();
    assert_eq!(
        requests.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(),
        [
            "/api/v1/players/local/info",
            "/api/v1/households/local/groups",
            "/api/v1/groups/RINCON_TEST:1/playback",
            "/api/v1/groups/RINCON_TEST:1/playbackMetadata",
            "/api/v1/players/RINCON_TEST/playerVolume",
        ]
    );
    assert!(requests.iter().all(|r| r.method == "GET" && r.key == KEY));
}
#[test]
fn an_empty_metadata_document_is_a_quiet_group_not_an_error() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_IDLE")),
        (200, r#"{"_objectType":"metadataStatus"}"#.into()),
    ]);
    let client = connect(&base);
    let now = client.now_playing().unwrap();
    assert_eq!(now, NowPlaying::default());
    thread.join().unwrap();
}
#[test]
fn seek_and_play_modes_are_group_writes_refused_on_members() {
    let (base, thread) = server(vec![
        (200, info()),
        (200, groups(PLAYER, "PLAYBACK_STATE_PLAYING")),
        ok(),
        (200, groups(PLAYER, "PLAYBACK_STATE_PLAYING")),
        ok(),
        (200, groups(OTHER, "PLAYBACK_STATE_PLAYING")),
    ]);
    let client = connect(&base);
    client.seek(90_000).unwrap();
    client
        .set_play_modes(PlayModeChange {
            shuffle: Some(true),
            repeat_one: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        client.seek(1),
        Err(Error::NotCoordinator {
            coordinator: "Sonos Arc".into()
        })
    );
    assert_eq!(
        client.set_play_modes(PlayModeChange::default()),
        Err(Error::Command)
    );
    let requests = thread.join().unwrap();
    assert_eq!(
        requests[2].url,
        "/api/v1/groups/RINCON_TEST:1/playback/seek"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&requests[2].body).unwrap(),
        serde_json::json!({"positionMillis": 90000})
    );
    assert_eq!(
        requests[4].url,
        "/api/v1/groups/RINCON_TEST:1/playback/playMode"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&requests[4].body).unwrap(),
        serde_json::json!({"playModes": {"shuffle": true, "repeatOne": false}})
    );
    assert_eq!(requests.len(), 6);
}
#[test]
fn artwork_is_fetched_plain_without_the_api_key_and_bounded() {
    let (base, thread) = server(vec![(200, info())]);
    let client = connect(&base);
    thread.join().unwrap();
    let (art_base, art_thread) = server(vec![(200, "not really a jpeg".into())]);
    let bytes = client
        .artwork(&format!("{art_base}/getaa?s=1&u=x"))
        .unwrap();
    assert_eq!(bytes, b"not really a jpeg");
    let requests = art_thread.join().unwrap();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].key, "");
    assert_eq!(
        client.artwork("ftp://192.0.2.10/x"),
        Err(Error::Unsupported)
    );
    assert_eq!(client.artwork("/getaa?s=1"), Err(Error::Unsupported));
}
