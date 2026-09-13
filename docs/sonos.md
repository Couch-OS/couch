# Sonos LAN client

`clients/couch-sonos` is a blocking Rust library and JSON-output CLI for existing
Sonos systems on the local IPv4 network. Call it from a worker thread. It speaks
the official Sonos Control API directly to a player over HTTPS on port 1443, with
no cloud gateway, no Sonos account and no OAuth: `api.ws.sonos.com` is never
contacted. It does not provision speakers.

## Usage

Build for the remote with `tools/build-sonos.sh`; use `--host` for a desktop binary.
On macOS the ARM build compiles ring's crypto primitives, so the script exports
`CC_armv7_unknown_linux_musleabihf=tools/arm-musl-cc.py` and needs Zig, exactly
like `tools/build-webui.sh`. The runtime inventory requires the static ARM
executable at `clients/target/armv7-unknown-linux-musleabihf/release/couch-sonos`
and packages it as `/opt/couch/couch-sonos`, mode 0755, with a pinned SHA-256.
Build it before running `tools/release/runtime_inventory.py`. The inventory also
records the clients workspace dependency licenses and lockfile. Packaging includes
the CLI alongside the GUI and configuration server integrations.

```sh
couch-sonos discover
couch-sonos 192.168.1.50 status
couch-sonos 192.168.1.50 play
couch-sonos 192.168.1.50 pause
couch-sonos 192.168.1.50 volume 25
couch-sonos 192.168.1.50 mute
couch-sonos 192.168.1.50 unmute
```

`play-pause`, `stop`, `next`, and `previous` are also available. Play resumes the
existing source/queue; it does not select music. A successful mutation prints
`{"acknowledged":true}`; query `status` separately to observe current state.
A network timeout has an unknown command outcome and is never retried automatically.

Library entry points are `discover`, `Client::connect(Ipv4Addr)`,
`Client::connect_with_key`, `Client::connect_url` (an explicit API root, for
loopback fixtures), `player`, `coordinator`, `status`, `playback(Playback)`,
`command`/`command_if_current`, `volume`, `muted`, `set_volume`, `nudge_volume`,
and `set_muted`. Errors distinguish transport, malformed responses, unsupported
hosts, HTTP status, Control API error codes, invalid volume, cancelled commands,
and non-coordinator playback.

## API key

Every request carries an `X-Sonos-Api-Key` header; without it a player answers
HTTP 400 `ERROR_API_KEY_VALIDATION_FAILED`. The key is read, in order, from:

1. the `COUCH_SONOS_API_KEY` environment variable, one operator override for the
   whole remote;
2. `api_key` in this connection's private settings file,
   `connections/<id>/sonos-connection.json`, written at mode 0600 by
   `couch_sdk::save_private` like every other credential (SDK path only);
3. the shared `sonos-api-key` file beside `config.json`
   (`/opt/couch/sonos-api-key` on the remote). One rule resolves it for every
   reader: `COUCH_SONOS_API_KEY_FILE` overrides the path outright, otherwise the
   daemon uses the directory holding the `config.json` it owns and the GUI and
   CLI use `COUCH_HOME_DIR` or the installed root. Moving the key with the
   override moves it for all three;
4. the developer key compiled into the binary at build time (see
   "Release builds" below);
5. a built-in placeholder UUID that is not a credential.

Both file locations exist because the key is a *household* credential, not a
per-player one: the same developer key works for every speaker. The SDK's
per-connection file is the right home when the daemon manages a connection, and
the shared file avoids copying one key into five connection directories. Neither
value ever reaches `config.json` or an exported house configuration.

The supported value is a developer key from integration.sonos.com. Players that
report `"allowGuestAccess":true` with `"credentialTypeAllowed":"API_KEY"` accept
any non-empty key today, which is why the placeholder works on this network; do
not rely on that. Keep the real key out of Git and off the browser: the daemon
never echoes it, nothing logs it, `Settings`' own `Debug` redacts it, and a value
that cannot be a header (newlines, control characters, over 256 bytes) is refused
before any request is sent. Refusing one falls back to the placeholder, which
otherwise looks exactly like configuring no key at all, so the CLI prints a line
on stderr saying a configured key was ignored - never the value itself.

### Release builds

The key identifies Couch as an integration, not the person using it, so a
shipped remote carries it the way a phone app carries its client ID: compiled
in. Put the developer key, on its own line, in `build/sonos-api-key` on the
build machine. `build/` is gitignored, and this repository is public, so the
key must never be committed, pasted into a document, or written into the
runtime payload as a file. `tools/build-sonos.sh`, `tools/build-webui.sh` and
`tools/build-gui.sh` source `tools/sonos-build-env.sh`, which exports the
file's contents as `COUCH_SONOS_BUILT_IN_API_KEY` for cargo; `couch-sonos`
reads that variable with `option_env!` and exposes it as `BUILT_IN_API_KEY`.
Those three scripts cover every shipped binary that links the crate (the CLI,
the daemon, the GUI); a bare `cargo build` in `ui/` does not see the key
unless the variable is exported first. Without the file the build says so on
stderr and falls back to the placeholder, which is fine for development. Cargo
recompiles the crate when the variable changes.

## Web and remote controls

In the web UI, open **Connections → Sonos**, enter the speaker’s IPv4 address,
and save. **Test connection / refresh** displays playback state, volume, mute,
and whether the selected player coordinates its group. Playback buttons remain
disabled for a member; create/select the coordinator’s connection explicitly.
Volume and mute still address the selected player. Use **Rooms & devices** to add
that connection as a speaker. An activity may use Sonos as its main screen or map
individual playback/volume/mute buttons to it.

In a room on the remote, the physical keys drive a highlighted Sonos row
without opening it: Volume Up/Down changes the player's volume (holding repeats,
and a hold is sent as one bounded relative write), Mute toggles mute, Channel
Up/Down skip to the next and previous track, and Menu opens a source picker.
Each action reports through the large feedback card that brightness and scene
changes use, with the speaker's name in bold: a volume shows its meter and
level, mute shows "Muted"/"Unmuted", a skip shows "Next track"/"Previous
track", a chosen source shows its name over "Playing on Kitchen", and an error
shows the problem in bold with the speaker as its caption. After a skip the
card names the track it landed on, with "Next track · artist · album" as the
caption and the track's cover as a thumbnail when the player reports one (one
metadata read and one bounded artwork fetch after the skip). A group member is
told which room controls its playback rather than being forwarded. The Sonos
worker keeps its connection to the last player between presses and reconnects
on the next press after a transport failure.

The source picker lists the player's own TV input (`HT_PLAYBACK`) and line-in
(`LINE_IN`) when the hardware advertises them, then the household's favourites
(`GET /households/local/favorites`) and Sonos playlists
(`GET /households/local/playlists`) in the order the player returns them.
Choosing one loads it with `playOnCompletion`: `POST /groups/ID/favorites`,
`POST /groups/ID/playlists`, `POST /groups/ID/playback/lineIn` (with this
player's `deviceId`), or `POST /players/ID/homeTheater` for TV, which is the
player's own and needs no topology. Group loads are refused on a member and
checked for freshness after the topology read, like playback. Bench note
(Sonos One SL, firmware 97.1, 2026-09-13): a Sonos playlist load is accepted
and starts playing by itself, but on that household every Apple Music track
buffered, was skipped, and the group fell back to idle, and every Apple Music
favourite was refused outright with `ERROR_PLAYBACK_NO_CONTENT` ("No tracks
added to queue") whatever the load options. That is the service's content
failing to stream on that player, not the request shape; the remote shows the
refusal as "Sonos found nothing to play there" and never retries a load. The CLI exposes
the same two calls as `sources` and `source tv|line-in|favorite:ID|playlist:ID`.

## The player screen

Opening a Sonos speaker from a room, or an activity whose source device is
one, shows the same full-screen player the Kodi Cinema activity uses, in its
music form: the track's cover as the backdrop, title, "artist · album", a
live progress line with seek, previous / play-pause / next, and three sheets:
**Sources** (the source picker: TV and line-in when the hardware has them,
then favourites and Sonos playlists), **Modes** (shuffle, repeat off / all /
this track, crossfade) and **Up next** (the queued track; choosing it skips).
TV, line-in and streams without a track show the container's name and no
progress; an idle group says so and points at Sources.

The screen is fed by one worker thread that keeps its connection to the
player and reads `Client::snapshot()` (one topology read, then the group's
`playback`, its `playbackMetadata` and this player's volume) on open, after
every command and every three seconds while the screen is up; position is
interpolated between reads. Artwork comes from the track's `imageUrl` through
`Client::artwork` (bounded, no API key, normal certificate checks off the
player), decoded into the 480×800 backdrop with the legibility gradient the
Kodi screen uses; text stands on its own when art is missing. A group member
sees the coordinator's track and volume but is told which room controls
playback instead of having transport, seek, sources or modes sent. The D-pad
moves an on-screen selection (seek line, transport, sheets) and OK activates
it; inside a sheet Up/Down walk the list and OK picks. Channel keys skip
tracks, Volume keys nudge this player's volume by five and show the volume
card, Mute toggles, Menu opens Sources, Back closes a sheet or the screen.

The remote’s older speaker card (playback, volume, mute and refresh) remains
for the TV-style screen path. Playback failures on group members name the
coordinating room; there is no automatic forwarding. Stale queued physical
commands are cancelled before writes, including after preparatory network
reads; leaving the screen or changing configuration invalidates the queued
target.

Authenticated daemon routes are `GET /api/connections/ID/sonos/status` and
`POST /api/connections/ID/sonos/command`. Command bodies use a closed `command`
vocabulary (`play`, `pause`, `play-pause`, `stop`, `next`, `previous`,
`volume-up`, `volume-down`, `mute` for toggle, `mute-on`, `mute-off`), or
`{"command":"volume","value":25}`. The saved connection supplies the address;
command requests cannot override it. Mutations return acknowledgement separately
from refresh failures so a failed observation does not invite replaying a write.

## Developer SDK

`clients/couch-sonos/src/sdk.rs` implements `couch_sdk::DeviceClient` for
`Client` and `couch_sdk::ClientSettings` for `Settings` (`FILE_PREFIX` `"sonos"`,
so `sonos-connection.json`), following `clients/couch-denon/src/sdk.rs` and the
checklist in [docs/client-sdk.md](client-sdk.md). `KIND` is `"sonos"` and `LABEL`
is `"Sonos"`, matching `Provider::kind()` and `Provider::label()`, and
`capabilities()` is exactly
`couch_model::buttons::functions(&Integration::Sonos { .. })` in the same order -
`catalog_matches_the_model` fails if that stops being true.

`execute` does not restate the command table: the declared ids are the closed
vocabulary `Client::command` already parses, so the group coordinator check and
the mute toggle's read-then-write pair stay in one place. `status` fills the
SDK's `Status` with `muted`, `volume` and `playing` (from the group's playback
state), and leaves `on` empty because a Sonos player has no power state to
observe. Errors map to the SDK vocabulary: `Transport` stays transport, a
malformed reply or an unexpected HTTP status is `Protocol`, an unknown command is
`Unsupported`, and anything the player explained - an API error code, a
non-coordinator refusal - arrives as `Remote` with that explanation.

This is additive. The daemon and the GUI still call the crate's own API
directly, as the other clients do; `docs/client-sdk.md` records that no
migration is in progress and none is required, and Sonos is not registered with
the `couch-control` broker.

## Commands and the requests they make

| Couch command | Request |
| --- | --- |
| `status` | `GET /households/local/groups` then `GET /players/{id}/playerVolume` |
| `play`, `pause` | `POST /groups/{group}/playback/{play,pause}` |
| `play-pause` | `POST /groups/{group}/playback/togglePlayPause` |
| `stop` | `POST /groups/{group}/playback/pause` |
| `next`, `previous` | `POST /groups/{group}/playback/{skipToNextTrack,skipToPreviousTrack}` |
| `volume 0..100` | `POST /players/{id}/playerVolume` `{"volume":N}` |
| `volume-up`, `volume-down` | `POST /players/{id}/playerVolume/relative` `{"volumeDelta":±1}` |
| `mute-on`, `mute-off` | `POST /players/{id}/playerVolume/mute` `{"muted":bool}` |
| `mute` | `GET` the player volume, then the same mute write |

The Control API has no stop command, so `stop` pauses the group and the button
keeps its familiar name. `play-pause` is one toggle request rather than a read
followed by a guess, and relative volume is a single write, so only the mute
toggle still reads before writing. Every write re-checks command freshness first
and is never retried.

Playback bodies are `{}` with `Content-Type: application/json`. Failures come back
as JSON `{errorCode, reason}` with HTTP 400 or 499 and surface as
`Error::Api(code)`, for example `ERROR_PLAYBACK_NO_CONTENT` when the group has an
empty queue.

## Group behavior and compatibility

Playback targets the selected player only after reading the household group list
and verifying that the player coordinates its group. It affects that coordinator's
current playback group. Selecting a member returns `NotCoordinator` with the
coordinator's room name; the caller must explicitly select the coordinator's
address. `status.coordinator` remains the coordinator's player id, comparable with
`status.player.uuid`, and `status.coordinator_name` carries the name for display.
The client never forwards commands or changes group membership; `createGroup`,
`setGroupMembers` and `modifyGroupMembers` are not used. Topology can change
between the check and the command; there is no atomic group lock. A player that
answers for a group it no longer coordinates replies HTTP 404 with
`groupCoordinatorChanged`, which is reported as the same `NotCoordinator` refusal.
Volume and mute target the selected player, not group volume.
`status.transport` is the group's playback state with the `PLAYBACK_STATE_`
prefix removed: `IDLE`, `PLAYING`, `PAUSED` or `BUFFERING`.

Compatibility is capability-based: `GET /players/local/info` must return a
`discoveryInfo` object with a player id, a household id, a device name, and the
`PLAYBACK` capability. Bonded devices without their own playback, such as a Sub,
are refused as unsupported rather than half-controlled, as are non-Sonos hosts.
Validation covers the models on the network below, not a full model matrix.
No grouping, queue editing, favorites, music-service authentication, push events,
or artwork is implemented yet.

## Network, trust and parser boundaries

Discovery multicasts one mDNS PTR query for `_sonos._tcp.local` to
224.0.0.251:5353 and listens for three seconds, returning at most 256 IPv4
candidates. Replies are parsed with a small in-crate DNS reader (PTR proves the
service, SRV names the host, the A record supplies the address; name compression
pointers must point backwards) and fall back to the responder's own address when
a reply carries no usable A record. The advertised TXT `location` URL, which
points at the legacy port 1400 description, is never fetched. Candidates stay
untrusted until `connect` validates them. SSDP is no longer used. Multicast
routing and the host's selected interface determine reachability; explicit IP
addresses work without discovery.

Player certificates are leaves issued by the "Sonos Device Authentication Root
CA", which is in no system trust store and is not sent in the handshake chain, so
this client disables certificate verification for the player connection. That is
the same trust level as the legacy plain-HTTP protocol it replaces: a trusted LAN
and no peer authentication. Traffic is encrypted but the peer is not
authenticated, and the API key is not a secret the transport can protect.

HTTP requests have a five-second total deadline and a 512 KiB body limit; a
composite status operation makes two sequential requests. Environment proxies and
redirects are disabled and every request stays on the connected origin: plain HTTP
is accepted only for loopback test fixtures, and device-supplied group and player
ids are restricted to an identifier alphabet before they are spliced into a URL
path. Responses are parsed with serde as JSON only; ambiguous topology (no group,
or the player listed in two groups) fails closed.

## Validation

Run `cargo test -p couch-sonos --locked` and `cargo fmt -p couch-sonos --check`
from `clients/`, `cargo test --locked` from `daemon/`, `cargo check --locked`
from `ui/`, and `cargo check --locked --target wasm32-unknown-unknown` from
`web/`. Tests use loopback JSON fixtures, not speaker commands. They cover the API
key header and JSON content type, the path and body of every command, typed
Control API errors, the 404 `groupCoordinatorChanged` refusal, member refusal from
the group listing, the body limit, redirect refusal, freshness cancellation before
a write, volume validation before the network, key sourcing, mDNS parsing of a
hand-built compressed packet, and status composition.

The SDK contract is checked with `couch_sdk::testing::contract_findings`, whose
mock host is a scripted line protocol: it cannot speak HTTPS and JSON, and these
settings carry no port to point at one, so the checker reports exactly one
finding, `connect failed`, after passing every declaration, canonicality and
0600 settings round-trip check it makes before connecting. The capability gate,
the refusal of an undeclared function without a round trip, and the absence of a
retry after a refused write are proved against the tiny_http fixture instead,
which is the same accommodation `couch-ha` and `couch-hue` make.

Physical acceptance on a four-player household (Arc, Amp, One SL, bonded Sub),
firmware 97.1-80312, API version 1.54.1, using the placeholder key:

```
$ couch-sonos discover
["192.168.1.27","192.168.1.114","192.168.1.217","192.168.1.245"]
$ couch-sonos 192.168.1.114 status
{"player":{"uuid":"RINCON_C43875B87D3B01400","name":"Sonos Arc","model":"Arc"},
 "coordinator":"RINCON_C43875B87D3B01400","coordinator_name":"Sonos Arc",
 "transport":"IDLE","volume":17,"muted":false}
$ couch-sonos 192.168.1.217 status
{"player":{"uuid":"RINCON_38420B7AA9D601400","name":"Laundry Room","model":"One SL"},
 "coordinator":"RINCON_38420B7AA9D601400","coordinator_name":"Laundry Room",
 "transport":"IDLE","volume":63,"muted":false}
$ couch-sonos 192.168.1.27 status        # bonded Sub, no PLAYBACK capability
couch-sonos: Host is not a Sonos player with local playback control
$ couch-sonos 192.168.1.217 volume 63    # same value it already had
{"acknowledged":true}
$ couch-sonos 192.168.1.217 mute
{"acknowledged":true}                    # status then reported "muted":true
$ couch-sonos 192.168.1.217 unmute
{"acknowledged":true}                    # restored to volume 63, "muted":false
$ couch-sonos 192.168.1.217 pause        # group idle with an empty queue
couch-sonos: Sonos API error ERROR_PLAYBACK_NO_CONTENT
$ couch-sonos 192.168.1.217 play
couch-sonos: Sonos API error ERROR_PLAYBACK_NO_CONTENT
```

The SDK path was checked read-only against the same speaker:
`DeviceClient::status` returned `Status { on: None, muted: Some(false),
volume: Some(63), input: None, playing: Some(false), title: None }` and
`DeviceClient::command(.., "power-off")` returned `Err(Unsupported)` without a
request.

Discovery returned all four players including the bonded Sub, which `connect`
then refused. Every speaker was left in the state it was found in: nothing
started playing, no group was created or modified, and no volume changed. The
`groupCoordinatorChanged` refusal was observed on the device by asking the One SL
about the Arc's group id; it is covered in the test suite because a single-player
household cannot reproduce it through this client, which checks membership first.

## Protocol sources

Sonos documents the API in [Control](https://docs.sonos.com/docs/control),
[playback](https://docs.sonos.com/reference/playback.md),
[playerVolume](https://docs.sonos.com/reference/playervolume-object.md) and
[groups](https://docs.sonos.com/reference/groups.md); appending `.md` to any
documentation URL returns markdown, and [llms.txt](https://docs.sonos.com/llms.txt)
indexes the set. Those pages describe the same namespaces and paths served by the
cloud gateway, which Couch does not use. The local bindings used here - the
HTTPS port, `players/local/info`, `households/local/groups`, and the API key
header without OAuth - were confirmed against players on this network rather than
from a Sonos commitment. LAN requirements are in
[Configure your firewall](https://support.sonos.com/en/article/configure-your-firewall-to-work-with-sonos).
