# Protocol 3, step T4 "Media": implementation plan

Status: **approved by the owner 2026-09-21.** Lands switched off, like T1-T3.

## Owner decisions (2026-09-21)

Asked in plain words, all answered with the recommendation:

1. **Kodi screen keys.** Arrows, OK, Back, Home and Menu go straight to Kodi
   from the moment the screen opens; holding Back leaves the screen. As the
   built-in Kodi behaves today.
2. **Colour keys.** A package may suggest what Red, Green, Yellow and Blue do
   on its own screen, and nothing else. The owner's activity mappings always
   win.
3. **The "Commands" list** is dropped from the remote once the row opens the
   player. It stays in the browser and every command stays mappable.
4. **An open list** (Chapters, Audio, Subtitles) takes the arrows, OK and Back;
   when it closes they go to Kodi again.

Taken as recommended, no objection: channel up/down is next/previous chapter
(or channel on live TV) on Kodi and next/previous track on Sonos; the "nothing
playing" sentence on Kodi stays as it is; pictures are asked for at panel size,
aimed at about 1 MB and refused above 4 MB; holding OK does nothing special
(Menu opens Kodi's context menu); the trial is a read-only part without the
owner and then about thirty minutes of his hands on the D-pad, before T5.

Question 2 adds one small optional manifest field for the colour keys to the
base plan below (section 3, "Keys"), which otherwise left it out.


Studied at origin/dev 397d053 (T1-T3 complete, switched off). Evidence is fresh today: `tested_commit` 9f453d5, and no contract path has changed since. Latest release tag is `.188`. Package repos were read from local clones: `~/Projects/cw-sonos` fb763ff, `~/Projects/cw-kodi` a494396, `~/Projects/cw-hue` 7aaefdd. Nothing was built, run or contacted.

## Summary
- T4 lets a package say "I am a media player". Couch then opens the real player screen for its row: now playing, artwork, seek, transport, sheets.
- For Kodi, the D-pad, OK, Back, Home and Menu go straight to the device from the moment the screen opens. This is the owner's complaint: today the packaged Kodi row opens the generic controls screen with a "Commands" tile.
- The wire gains four requests (`media`, `artwork`, `list`, `choose`), five typed actions, an item guard, a second line and a current marker on source rows, and five error reasons.
- The manifest gains `media_player` and `volume_percent_control`. `volume:NN` becomes a typed action at the one host gate.
- The GUI adds a `Packaged` backend on the shared controller in `media_player.rs`. It polls `media` at the existing cadence and pulls artwork in chunks through the daemon. The daemon gains no cache and no long poll, so T5 can add them behind the same `Backend::watch(after, wait)` without touching the wire.
- Everything lands switched off. Published packages receive identical bytes. Everything new sits in `integration_config_v3` and is stripped for `.188`.

## 0. Findings that shape the plan

### Where the documents and the code disagree
1. **The published packages are protocol 1, not 2.** Both Sonos and Kodi 0.1.0 say so (`cw-sonos/plugin.json:2`, `cw-kodi/plugin.json:2`), and both pin SDK 00ab4da. The brief says protocol 2.
2. **The daemon never reaches the design's "queue of 8, 750 ms" (design 5.4).**
   - `Runtime::execute` try-locks the connection for the whole request with a 250 ms patient wait (`daemon/couch-confd/src/plugins.rs:1010-1013`, `:87`).
   - A second request is therefore told Busy after 250 ms, as the Hue trial found (#268).
   - A slow `media` read or an artwork fetch that holds that lock drops a fresh key press.
3. **The design says `jpeg | png | webp` (4.2), but the GUI decodes only JPEG and PNG** (`ui/couch-gui/Cargo.toml:29`).
4. **The design's list page size does not fit a frame (4.3).**
   - It says 100 list items "fit by construction".
   - 100 x (256 + 256 + a 128-byte id + keys) is about 69 KB, which is over `MAX_FRAME` 64 KiB (`protocol.rs:22`).
5. **The roadmap puts the host-side item guard in T4, but the cache it needs is T5.**
   - The roadmap's T4 row lists the "item guard".
   - The design refuses a request when the host's cached `media.item` differs (3.3).
   - The cache is T5, so in T4 the guard is package-side only.
6. **No per-device key overrides exist in the model.**
   - Bindings live on `Activity.buttons` only (`model/couch-model/src/lib.rs:260`).
   - On a device's own screen the keys are `row_bindings` (`activity_buttons.rs:113-141`, `:286-294`): volume, mute and power only.
   - Every other key goes to the screen's `FocusScope` (`cinema.slint:195-218`).
7. **The remote has no play/pause, transport or number keys** (`buttons.rs:7-30`, `:64-91`). It has D-pad, OK, Back, Home, Menu, volume up/down, channel up/down, Mute, Power, four colour keys, and the Lights/Activity/Music/Tv shortcut keys.
8. **The in-tree Sonos adapter is the wrong place for Sonos 0.2.**
   - The design's PR 7 puts the adapter in `clients/couch-sonos`.
   - That crate's `sdk.rs` is the protocol 1 fixture that `tools/tests/old-package-wire.sh` builds at 00ab4da.
   - Hue set the precedent of doing the adapter in the package repo.
   - Do Sonos 0.2 in `couch-integration-sonos` and leave the in-tree adapter alone.
9. **`set_mode`.** The design's own corrections say it must be a partial set.
10. **`seek` and `modes` on the `media_player` component** duplicate the declared actions.

### Code facts
- **`Selectable` is tolerant.** It has no `deny_unknown_fields` (`clients/couch-sdk/src/status.rs:121-125`), now or at 00ab4da. An old host ignores `detail` and `current`; an old child never writes them.
- **Unit variants ignore fields even with deny-unknown** (`volume.rs:20-21`). New field-less variants must be empty struct variants.
- **`plugin.sock` relays `Request`/`Response` verbatim.**
  - Each request is its own connection and thread.
  - Connections beyond 8 are dropped silently (`api/plugins.rs:728`, `:924-931`).
  - `execute` allow-lists only Command, Action, Status and Inputs (`plugins.rs:1000-1008`).
- **The controller has one worker.** Refresh, artwork and commands share it, and `request()` silently drops a press while `busy` (`media_player.rs:446-455`, `:968-1107`). Design correction 6 already says watch must not block keys.
- **The Cinema screen's `music` flag couples three things** (`cinema.slint:56-63`, `:195-218`): transport icons, sheet icons and whether the D-pad moves focus.
  - Built-in Kodi passes `Input.*` through (`activity.rs:927`).
  - It guards Player commands by `identity(p)` (`activity.rs:329-332`).
  - It leaves only by a held Back (`main.rs:1645-1652`, `input.rs:177-190`).
- **The lift cannot carry artwork.** `lift-ready()` is `!has-art` (`cinema.slint:188`). The player lifts on "Connecting to ...", and artwork arrives afterwards (`docs/slint-notes.md:689-702`).
- **Sites validated by `supports_device`:** activity bindings (`validate.rs:462-476`), and setup on/off commands and page widgets (`activity_setup.rs:99-108`). Steps only parse (`validate.rs:410-431`).
- **`.188` refuses a `volume:30` binding on a plugin device.** Its `supports` is a literal capability match (`commands.rs:246-247`).
- **The GUI is `panic = "abort"`** (`ui/Cargo.toml:19`).
- **T1 deferred "drop stale queued repeats" to T4** (t1 plan, settled question 4).

## 1. Scope and non-goals

### Delivers
- **For Sonos 0.2:**
  - the player screen from the package, pixel-equal to built-in Sonos for the 30 states in `ui/couch-gui/tests/golden/player-screen.txt`;
  - percent volume with a coalesced hold;
  - sources with a second line;
  - modes, the group-member refusal, and artwork.
- **For Kodi 0.2:**
  - the player screen with D-pad passthrough and `KeyPhase`;
  - backdrop and logo;
  - seek and seek_by;
  - chapters, audio and subtitles sheets;
  - the item guard;
  - `x:` buttons mappable in activities.

### Waits for T5
- `Subscribe`/`changed`, the serve loop that waits on stdin and the device, and the `events` manifest flag.
- The daemon `MediaCache`, `watch` and `age_ms`.
- The daemon artwork LRU and `GET .../artwork/<id>`.
- The host-side item pre-check.
- Re-reading `media` 300 ms after a command.
- `States`.

### Waits for T6 and T7
- T6: the `now_playing` component.
- T7: `PROTOCOL_VERSION = 3`, `testing::media(` moving into digest-pinned `testing.rs`, the feed's `{1,2,3}`, and `compatibility.json`.

### Not in T4
- The Kodi, CoreELEC and Sonos converters, and removing the built-ins.
- Moving built-in Kodi onto the shared controller. It is to be deleted; it is only photographed, in PR-D0.
- Per-device key overrides outside activities.
- Group control.
- Text entry.

## 2. Wire
New file `clients/couch-sdk/src/media.rs`, re-exported by couch-plugin. Changes to `protocol.rs`, `host.rs`, `server.rs`, `model/couch-model/src/volume.rs`.

### Typed actions (`volume.rs`)
`TypedAction` and `PluginActionSchema` stay `Copy + Eq`, integers and bools only.

```rust
SetVolumePercent  { percent: u8 },        // 0..=schema.max_percent
StepVolumePercent { delta: i8 },          // never 0; |delta| <= schema.max_delta
Seek   { position_ms: u64 },              // <= MAX_MEDIA_MS = 7 days
SeekBy { delta_ms: i64 },                 // never 0; |delta| <= schema.max_delta_ms
SetMode { #[serde(default, skip_serializing_if="Option::is_none")] shuffle: Option<bool>,
          /* same attributes */ repeat: Option<bool>, repeat_one: Option<bool>, crossfade: Option<bool> },
          // at least one field; only modes the schema lists
// PluginActionSchema
SetVolumePercent { max_percent: u8 /*1..=100*/ }, StepVolumePercent { max_delta: u8 /*1..=50*/ },
Seek {}, SeekBy { max_delta_ms: u32 /*1_000..=3_600_000*/ }, SetMode { modes: PlayModeSet /*non-empty*/ }
pub enum PlayMode { Shuffle, Repeat, RepeatOne, Crossfade }
```

- **`PlayModeSet`:** a `Copy` bit set. Serde via `into`/`try_from` `Vec<PlayMode>`, written as `["shuffle","repeat",...]` in fixed order; duplicates are refused.
- **`ActionKind`:** gains five kinds. `MAX_ACTIONS` stays 8 (`volume.rs:99`).
- **Deviation from the design:** `set_mode` takes a partial set, per design correction 2. Leaving "repeat one" has to clear two modes in one write (`media_player.rs:80-89`, `:690-697`).

### Requests (`protocol.rs:125`)
```rust
Action  { action, resource: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] item: Option<String> },
Media {},                               // empty STRUCT variant; bytes {"method":"media"}
Artwork { art: String, offset: u32 },
List    { list: String, offset: u32 },
Choose  { list: String, id: String, #[serde(default, skip_serializing_if="Option::is_none")] item: Option<String> },
```
- **Builders:** `Request::media()`, `::artwork(art, offset)`, `::list(..)`, `::choose(..)`, `fn for_item(self, Option<String>) -> Self`. Extend `Request::at`/`resource()` exhaustively. Move every literal to a builder, the T2 pattern.
- **`item`:** at most 256 bytes of `[A-Za-z0-9:._/+-]`. For a manifest below protocol 3 the gate strips it. This is a downgrade like `phase` (`host.rs:565-569`), never a refusal, so a SetVolumeDb frame to Denon is byte-identical.

### Responses (`protocol.rs:315`)
```rust
Media   { media: couch_sdk::Media },
Artwork { art: String, total: u32, offset: u32, mime: ArtMime, data: String /*base64, standard, padded*/ },
List    { list: String, total: u32, offset: u32, items: Vec<ListItem> },
```

### `media.rs`
Every struct has `deny_unknown_fields`; every Option has `default` and skip-none; every bool has `default` and skip-false.

```rust
pub struct Media { item: Option<String>, state: PlayState /*playing|paused|buffering|stopped|idle, required*/,
  title, artist, album, subtitle, source, device_name: Option<String>,        // <=512 B, no control chars
  duration_ms: Option<u64>, live: bool, position_ms: Option<u64>,
  rate_percent: Option<i16> /* -3200..=3200; absent = 100 if playing else 0 */,
  #[serde(default, skip_serializing_if="Art::is_empty")] art: Art { cover, backdrop, logo: Option<String> },
  #[serde(default, skip_serializing_if="Can::is_none")]  can: Can { play_pause, next, previous, seek, modes: bool },
  #[serde(default, skip_serializing_if="Modes::is_empty")] modes: Modes { shuffle, repeat, repeat_one, crossfade: Option<bool> },
  next: Option<NextItem { title: String, detail: Option<String> }>,
  group: Option<Group { role: GroupRole /*standalone|coordinator|member*/, coordinator: Option<String>, members: Vec<String> /*<=32, <=128 B*/ }> }
pub enum ArtMime { Jpeg, Png, Webp }
pub struct ListItem { id: String /*1..=128 B printable*/, title: String /*<=256*/, detail: Option<String> /*<=256*/, current: bool }
impl Media { pub fn same_except_position(&self, other: &Self) -> bool }   // one revision rule: GUI in T4, confd in T5
```

- **`device_name`** is design correction 3.
- **`subtitle`** is computed by the package (correction 4). Couch never derives line two.
- **No idle-sentence field.** Couch words idle itself (section 5).
- **Art ids:** at most 256 bytes of `[A-Za-z0-9:._-]`.
- **Worst-case `Media`** is about 10 KB.
- **SDK constructors** truncate at a character boundary and strip control characters. A well-behaved package cannot breach a limit, the T3 "constructors clamp" rule.

### Artwork limits
All held in `host.rs`.
- `ART_CHUNK = 45_000` raw bytes. That is 60 000 base64 characters; the frame stays under 64 KiB.
- `total` is 1 to 4 MiB.
- The reply's `offset` equals the request's.
- **Sharpened:** the decoded length must equal `min(ART_CHUNK, total - offset)`. The chunk count is then exactly `ceil(total / ART_CHUNK)`, at most 94. A package cannot dribble one-byte chunks, which the design's "at most 128 chunks" alone allowed.
- New `host::read_artwork(ask: &mut dyn FnMut(Request) -> Result<Response, Failure>, art) -> Result<(ArtMime, Vec<u8>)>`, the `list_children` pattern (`host.rs:977`):
  - `total` and `mime` are identical in every chunk;
  - the magic bytes match the mime;
  - the whole transfer has a 15 s deadline;
  - a violation is `Protocol` and the caller retires the child.
- The GUI uses `read_artwork` in T4; confd uses it in T5.
- **WebP** is accepted on the wire; adding an enum tag after T7 would cost a protocol 4. The GUI cannot decode it, so it shows no art, as `decode` already returns `None`.
- **Base64** is hand-rolled in `media.rs`, about 40 lines with RFC 4648 vectors:
  - the packages stay dependency-thin;
  - `daemon/Cargo.lock`, a gated path, does not move.

### Non-blocking artwork
This deviates from the design's "fetch on the offset-0 request, 5 s".
- **Rule:** a package must answer `artwork` within 500 ms.
  - If the bytes are not in hand, it starts its own fetch and answers `Error{code: busy}`.
  - The caller asks again every 300 ms, for at most 8 s, at offset 0 only.
- **Reason:** finding 2. A five-second fetch holds the connection lock, and every D-pad press in that window is told Busy.
- **SDK helper:** `couch_sdk::media::ArtFetcher`. One background thread; it keeps the most recent picture's bytes only; 4 MiB cap; 5 s fetch deadline.
- The harness enforces the rule.

### Lists
- **Sharpened:** a page holds at most 48 items, and a list at most 256.
- `ListPage::fill` also stops at 48 KiB, as `ChildPage` does.
- `host::read_list(ask, list)` checks that `total` is stable, ids are unique, at most one row is `current`, and a non-final page is non-empty. At most 8 pages, 10 s overall.
- `choose` has command semantics and is never retried.

### Selectable (`status.rs:121`)
- Gains `detail: Option<String>` (at most 256 bytes) and `current: bool`, both skipped when unset.
- `validate_response` (`host.rs:923-932`) adds the bounds and "at most one current".
- `serve` clears both for a manifest below protocol 3.
- `accept` treats either one from a child below protocol 3 as `Protocol`, the strict rule of T1's settled question 1.

### Reasons (`error.rs:24`)
Adds `NotCoordinator { coordinator: String /*<=128*/ }`, `NothingToPlay {}`, `NotSeekable {}`, `Unavailable {}`, `ItemChanged {}`.
- They are empty struct variants.
- `text()` returns "" for them; Couch words each one.
- Pairing of code and reason:

| Code | Reason |
| --- | --- |
| `rejected` | `not_coordinator`, `nothing_to_play`, `not_seekable`, `unavailable` |
| `expired` | `item_changed` |

### Write ack
- `validate_response` also accepts `Response::Status` in answer to `Action{SetVolumePercent | StepVolumePercent}` with no resource.
- Those actions are only reachable for a protocol 3 package, because `requires` is 3.
- It saves the read-back behind every coalesced volume burst. `Ok` stays legal.

### Gate
Stays the one place.
- **`requires` (`host.rs:874`):** 3 for Media, Artwork, List and Choose. The new actions are already 3, by `!= SetVolumeDb`.
- **`admit`:**
  - Media needs a `media_player` component.
  - Artwork needs a non-empty `artwork` list and a well-formed art id.
  - List and Choose need a declared list id. Choose also needs `choose: true` and a well-formed id.
  - `offset <= 4 MiB`, and `<= 256` for lists.
  - Otherwise Unsupported or Invalid before any I/O.
- **Volume mapping:** new `fn volume_level` beside `level()` (`host.rs:720`).
  - A `Command` with no resource that parses to `Function::Volume(n)`, on a manifest declaring SetVolumePercent, becomes `Action{SetVolumePercent{percent: n}}`.
  - `n > max_percent` is Invalid.
  - A manifest below protocol 3 cannot declare the schema, so `volume:30` stays a command that `supports` refuses. Its bytes are unchanged.
- **`accept`:**
  - Media, Artwork or List from a package below protocol 3 is Protocol.
  - A bounds breach is Protocol.
  - A `can`/`modes`/`art` key the manifest did not declare is Protocol.
- **`accept_credential` (`host.rs:788-799`):** the four new requests count as ordinary.

### Stale repeats (T1's deferral)
- **Host worker:** `Pending` with `phase == Repeat` expires after `REPEAT_TTL = 150 ms` (worker at `host.rs:1103`).
- **Daemon:** `execute` waits for the lock only `REPEAT_LOCK_WAIT = 70 ms` for a repeat, and answers `Expired`, never Busy. The GUI is silent on Expired.

### SDK (`client.rs`)
All methods are defaulted, so every existing client compiles and stays byte-identical.
```rust
fn media(&mut self) -> Result<Media> { Err(Unsupported) }
fn artwork(&mut self, _art: &str) -> Result<ArtBytes /*mime + Arc<[u8]>*/> { Err(Unsupported) }  // Busy = not yet
fn list(&mut self, _list: &str, _offset: u32) -> Result<ListPage> { Err(Unsupported) }
fn choose(&mut self, _list: &str, _id: &str, _item: Option<&str>) -> Result<()> { Err(Unsupported) }
fn media_action(&mut self, action: TypedAction, _item: Option<&str>) -> Result<Option<Status>> { self.action(action).map(|_| None) }
```
- `serve` routes the four requests, slices chunks from the client's bytes, and strips per protocol, as `shaped` does (`server.rs:313`).
- `serve` stays blocking in T4.

### Harness: `testing_v3::media(adapter, MediaCase)`
`testing.rs` and its digest are untouched.
- `media` conforms, and two reads are equal except for the position.
- Artwork chunking is exact.
- An artwork request answers within 500 ms while the fake device stalls the picture.
- An unknown art id answers `unavailable`.
- Every declared list pages and terminates.
- `choose` and `seek` aimed at a stale item are refused with `item_changed`, and the fake sees no write.
- An oversized step is refused before I/O.
- Nothing is answered for an undeclared list.

### Echo (`couch-echo/src/v3.rs`)
- A fake player: playing, paused, idle and member states; an in-memory PNG of settable size; chapters.
- A `hostile` setting selects one of:
  - an oversized `total`;
  - a short non-final chunk;
  - a changing `total`;
  - a wrong magic;
  - 49 items on a page;
  - two current rows;
  - a 600-byte title;
  - a bad art id;
  - a stalled artwork.

### Goldens and mirrors
- `wire-00ab4da.tsv`: its 44 rows must not move.
- `wire-v3-preview.tsv` gains rows for:
  - every new request, with and without `item`;
  - each action;
  - `volume:30` as the gate emits it;
  - `media` full and minimal;
  - one artwork chunk and one list page;
  - inputs with `detail` and `current`;
  - each new reason;
  - the Status ack;
  - the Sonos 0.2 and Kodi 0.2 manifests.
- `wire_mirror.rs` asserts:
  - the gate never emits the new words, or `item`, to a protocol 1 or 2 manifest, and that includes `volume:30`;
  - a new-SDK protocol 2 package's `inputs` parse on the old host.
- `old-package-wire.sh` stays unchanged and green.
- Run `tools/tests/denon-v1-host-compatibility.py` on the build host before merging A, B and C.

### How the wire maps onto `Backend` (`media_player.rs:223-241`)

| Backend call | Wire |
| --- | --- |
| `open` | No-op: every request is its own socket. |
| `close` | No-op. T5 drops the watch here. |
| `watch(after, _)` | `media`. `revision = after + 1` when `!same_except_position(last)`, else `after`. `age = 0`. Busy returns the last `Watched` with its real age and no error flash. Transport or timeout is `Unreachable`. Unpaired shows the pairing hint. Incompatible shows "This integration needs a newer Couch". |
| `perform(PlayPause / Next / Previous / ToggleMute)` | The command (`play-pause`, or `play`/`pause` by state; `next`; `previous`; `mute`, then one `status` for `Done::Muted`). |
| `perform(Seek)`, new `SeekBy`, `Modes` | `action(...).for_item(media.item)`. |
| `perform(StepVolume)` | `StepVolumePercent` clamped to `max_delta`. The Status ack becomes `Done::Volume`. |
| `perform(Source{id})` | `command("input:<id>")`. |
| new `Op::Choose{list, id}` | `choose(...).for_item(..)`. |
| new `Op::Key{function, phase}` | `Request::key`, on the key lane; never read back. |
| `sources` | `inputs`, with `detail` and `current`. |
| new `list(id)` | `host::read_list`. |
| `artwork(art)` | `host::read_artwork` with the Busy retry. |

Failure mapping:

| Wire failure | Screen |
| --- | --- |
| `not_coordinator{c}` | `Follows{leader: c}` |
| `nothing_to_play` | `NothingToPlay` |
| `item_changed` | "Playback changed. Try again." |
| `message{t}` | `Message(t)` |
| queue `Expired` | silent |
| Busy on a tap | one retry after 120 ms. It was refused before I/O, so this is not a replay. Then "Connection busy. Try again." |

## 3. Manifest and how a row chooses the player

### The components (`connection.rs:29`)
`PluginComponent` stays `Eq` and deny-unknown.
```rust
MediaPlayer { layout: MediaLayout /*music|video*/,
  #[serde(default, skip_serializing_if="Vec::is_empty")] artwork: Vec<ArtRole> /*cover|backdrop|logo, <=3 distinct*/,
  #[serde(default, skip_serializing_if="Vec::is_empty")] lists: Vec<MediaList { id, label, #[serde(default)] choose: bool }>,
  #[serde(default, skip_serializing_if="Not::not")] up_next: bool,
  #[serde(default, skip_serializing_if="Not::not")] navigation: bool,
  #[serde(default="three_seconds", skip_serializing_if="is_three_seconds")] refresh_ms: u32 /*1000..=30000*/ },
VolumePercentControl { label: String },
```

### Deviations from the design
- **`seek` and `modes` are dropped from the component.**
  - They duplicate the declared `seek` and `set_mode` actions.
  - Two sources of truth need a cross-check rule, and the GUI already reads `actions` (`activity_buttons.rs:1303-1318`).
  - Seek shows when a `Seek` schema is declared. The Modes sheet shows when `SetMode` is declared, with its set.
- **`up_next` is added** (correction 1's second option).
  - A third sheet "Up next" is built from `media.next`; choosing it sends `next`.
  - Sonos needs no list code, and the pictures match by construction.
- **`events` is left to T5.** It means nothing until hints exist, and an optional field added before T7 is free.

### Validation (`manifest.rs`)
- At most one `media_player`. It is protocol 3 only.
- It needs `play-pause`, or both `play` and `pause`.
- `up_next` needs `next`.
- `navigation` needs all of `up down left right ok back home menu`.
- Sheet count: `supports_inputs as usize + declares(SetMode) as usize + up_next as usize + lists.len() <= 3`.
- List ids are distinct identifiers, and labels follow the 128-byte rule.
- `video` layout may name `backdrop` and `logo`; `music` layout may name `cover`.
- `VolumePercentControl` needs a SetVolumePercent schema and protocol 3.
- Schema bounds are as in section 2.
- `serve` requires `manifest.actions == C::actions()`, as today (`server.rs:29`).
- Mirror every rule on the saved snapshot in `validate.rs:590-610`. The model does not depend on couch-plugin.

### Choosing the screen
- `shortcuts::opens_player` (`shortcuts.rs:65`) gains an arm: `Integration::Plugin { presentation, child: None, .. }` with any `MediaPlayer`. A child is never a player in T4.
- The three callers need no edit: `lights.rs:1859`, `:2135`, `:2154`.
- `tv_connection` still names `plugin:<id>` (`lights.rs:2202`), but the player check runs first.
- With the switch off, no protocol 3 package runs. A row left over from a preview build opens the player and reads "This integration needs a newer Couch".

### Keys
- Couch owns the key map. The manifest chooses the mode through `navigation`, never the map. That honours "Packages may name extra buttons ... nothing more".
- The key table is in section 5.
- An optional `keys` field for the colour keys is owner question 2. It is not in the base plan.

## 4. Model and rollback (PR-A)

### Model
- `commands.rs::supports`, Plugin arm (`commands.rs:207`): with no child, `Volume(_)` is supported exactly when the actions declare SetVolumePercent.
- `buttons.rs::function_choices` offers "Set volume..." for such a device.
- `lib.rs` re-exports.

### `v2_projection` (`storage.rs:120`), exact rules
1. `v2_action_schemas` is unchanged. It already keeps only SetVolumeDb, so the five new schemas go.
2. `v2_component` returns false for MediaPlayer and VolumePercentControl.
3. **New rule.** Where the target device resolves to a Plugin, a `volume:` command becomes `action = None` on an activity binding, and is removed from a setup on/off command or a page widget.
   - Those three sites are validated by `supports_device`, and `.188` refuses the file there.
   - Steps and scene steps are left alone. `.188` only parses them and can already hold them, so the projection stays the identity on anything `.188` can write.
4. `x:` ids are already stripped (T1).
5. An activity whose `source` is a packaged player is left as it is. `.188` opens its pages screen.

### Properties to keep
- The projection is idempotent.
- It is the identity on `.188` files.
- The projected config validates.
- A `.188` save never resurrects what it never saw.
- A tampered v3 layer fails closed.

### `config-crossload.rs` states
| State | Content |
| --- | --- |
| N | Sonos-like player: `volume:30` in a binding, a widget, a setup step and a plain step; `volume_percent_control`; four actions. |
| O | Kodi-like: navigation, three lists, `x:` ids, `seek_by`. |
| P | N on a Denon-converted connection holding both the dB and the percent schema. v2 keeps the dB one. |
| Q | An activity whose source is the player. |

- `leaks` fails on any of these outside the v3 layer: `media_player`, `volume_percent_control`, `set_volume_percent`, `step_volume_percent`, `"seek"`, `seek_by`, `set_mode`.
- Refused states:
  - two players;
  - four sheets;
  - a percent control with no schema;
  - `max_percent` 0;
  - `volume:30` bound to a plugin device without the schema;
  - `navigation` without `menu`.
- One test: a config with a player and one without project to identical v2 bytes.

## 5. Daemon (PR-C: `plugins.rs`, `api/plugins.rs`)

### Changes
- **Allow-list:** `execute` also accepts Media, Artwork, List and Choose (`plugins.rs:1000-1008`).
- **Still no cache.** No long-lived state at all in T4.
- **Lock wait by class** (`plugins.rs:1012`):

| Request class | Wait |
| --- | --- |
| Reads: Status, Inputs, Media, Artwork, List | 250 ms |
| A person's writes from `plugin.sock`: Command tap, Action, Choose | 600 ms, still under `QUEUE_TTL` 750 ms |
| Repeats | 70 ms, then Expired |
| HTTP | 250 ms. The 250 ms comment (`plugins.rs:75-87`) is about the API's four workers. |

- **HTTP:** `GET .../plugin/media`, a JSON passthrough with no cache.
  - It enables a read-only check on hardware by curl, and the web card.
  - Artwork and lists over HTTP wait for T5.
- **`volume:NN` over `POST .../plugin/action`** works through the gate.

### Busy arithmetic, from the Hue trial
- The trial measured reads at a median of 37 ms and a slowest of 281 ms. One read in ten was refused with two readers before #268 (`hue-package-hardware-trial.md`).
- The player adds:
  - one `media` every `refresh_ms`;
  - one artwork pull per item change.
- **Sonos `snapshot` is four HTTPS reads today** (`clients/couch-sonos/src/lib.rs:958-975`). The package must cut `media` to two.
  - Cache the group id for 30 s.
  - Leave volume out of `media`; `Status` has it.
  - Target a median under 100 ms, recorded in the trial.
- **Artwork** holds the lock one in-memory chunk at a time, with no device I/O under the lock, by the non-blocking rule.
  - The GUI sends chunk requests only while its key lane is idle.
  - It starts the first one 600 ms after the screen opens, clear of the 400 ms lift.
- **Cost per chunk:** one socket, one thread, and one reload of the settings file (`plugins.rs:1018`). Measure it.
  - Expected: about 50 ms for 180 KB, about 0.5 s for 1 MiB, about 2 s for 4 MiB.
- **`plugin.sock` cap.** The cap is 8, and excess connections are dropped silently (`api/plugins.rs:728`).
  - A player uses at most 3 connections; rows use 1.
  - Add a daemon test with 3 player lanes and 2 row readers: no tap is refused.

### Isolation
- Artwork bytes are fetched by the unprivileged per-package uid and cross into root confd as an opaque base64 string.
  - confd only measures it and relays it.
  - confd never decodes the image, in T4 or in T5.
- The root GUI decodes with the existing limits: 8192 px a side, 64 MiB of allocation, 20 MP (`activity_art.rs:96-107`), plus the magic check.
- **New exposure.** The attacker is now a package, including one from a custom repository, not only a LAN device.
  - With `panic = abort`, a decoder panic restarts the GUI.
  - Risk accepted for T4. The design's unprivileged decode helper stays a follow-up.
- An art id never becomes a path or a URL in any privileged process.
- `connection_id` alone selects the child, as in T2.

## 6. GUI (not frozen)

### PR-D0: photograph the built-in Kodi screen
Add `COUCH_KODI_GOLDENS` in the style of `media_player.rs:1575-1640`. Pictures: playing, paused, idle, the three sheets, error.

### PR-D: generalise the controller
- **`Target` gains a descriptor** built from the declaration: `layout`, `navigation`, `refresh`, `sheets: Vec<Sheet{Sources | Modes | UpNext | List{id, label, choose}}>`, `art_roles`, `device`, and the config `Arc`.
- **Sheets.** `SHEETS` (`media_player.rs:30`) becomes per-target. Built-in Sonos gets the same three, so the 30 pictures do not move.
- **New trait and ops:** `Backend::list` (defaulted), `Op::SeekBy` wired to the "skip" action (a no-op at `media_player.rs:571`), `Op::Choose` and `Op::Key`.
- **A second worker, the key lane.**
  - It carries nav keys and commands with no read-back.
  - Depth 2: a repeat is dropped when the lane is full, and a tap is held with latest-wins. Precedent: 3980a43.
  - The state lane keeps refresh, artwork, lists and sources.
- **`try_device_ir`** (`activity_buttons.rs:1375`) is called in the worker before any op that has a `Function`. The per-key IR override stays above the package.
- **Slint: decouple keys from `music`.** Add `in property <bool> navigate` and `<bool> sheet-keys` to Cinema.
  - Built-in Kodi sets them so it is unchanged.
  - With `sheet-keys`, an open sheet takes Up, Down, OK and Back, whatever the layout.
  - No pixel moves.
- **Art.** `cover` or `backdrop` decode to `Shape::Backdrop`; `logo` decodes to `Shape::Logo`, feeding `player-logo`. Backdrop first.
- **Idle sentence.**
  - Navigation: "Connected to {label}. Use the remote to choose something on your TV."
  - Otherwise: "{name} is idle." plus "Press Sources..." when the device has sources.
- **Performance.** No per-frame work is added. The clock tick stays at 1 s (`media_player.rs:915`).

### PR-E: `packaged_player.rs`
- The `Packaged` backend of section 2, over `tv::plugin::ask_within`. Timeouts: `media` 3 s, a chunk 3 s, a key 2 s, a list 5 s.
- `opens_player` gains the Plugin arm.
- `activity.rs::open` gets `packaged_player_target` before `plugin_target` (`activity.rs:547-567`). It serves `device:<id>` and an activity whose source is a player. The `sonos` field is renamed `player`.
- `row_bindings` (`activity_buttons.rs:113`) returns only Power for such a device, as it already does for Sonos. The screen's own volume path, with its optimistic card, then gets F23/F24.
- **Pictures.** The `Packaged` backend is driven by closures in place of the socket, the T2 PR-D pattern; `ui/` may not enable the preview feature. They return `media_player::fixtures` states and are held to the same goldens:
  - Sonos states to `COUCH_PLAYER_GOLDENS`;
  - video states to `COUCH_KODI_GOLDENS`.
- Add a lift test: the first picture has no art, so `lift-ready()` holds.
- Record D-pad latency (key to child write) in the PR.

### PR-F: a packaged player's room row
`room_sonos`-style coalescing: step 2, burst 20 (`room_sonos.rs:25-27`) through `StepVolumePercent`, with an optimistic card. This closes wave 0's held-key complaint.

### Physical keys

| Key | `navigation: true` (Kodi) | `navigation: false` (Sonos) |
| --- | --- | --- |
| D-pad | `up`, `down`, `left`, `right`, as Tap or Repeat from `physical_input` (`activity.rs:492`) | moves on-screen focus |
| OK | `ok`, tap only. The package does the contextual OK (`docs/kodi-contextual-ok.md`). | activates the focused control |
| Back tap | `back` | closes the sheet, else leaves |
| Back hold | leaves the screen (`main.rs:1650-1652`); reserved by Couch in both modes | same |
| Home | `home` | leaves the screen |
| Menu tap | `menu` (`main.rs:1866-1867`) | opens Sources |
| Channel up/down | `channel-up`/`channel-down` if declared, else `next`/`previous`, else nothing | same |
| Volume, Mute | the screen's `StepVolumePercent` (5 per press, summed while held) and `mute` | same |
| Colour keys | unbound (owner question 2) | same |
| A sheet open | the sheet takes D-pad, OK and Back | same |

- **Channel keys, deviation.**
  - The design walks the chapters list from the GUI (design 8, item 3).
  - One declared command lets the Kodi package use Kodi's own "chapter or big step", which is also a channel change on live TV.
  - It needs no list fetch, no item race, and one round trip.
- **Conflict resolved.**
  - On a navigating player the keys drive Kodi, and touch drives the screen (`selected` stays -1, `cinema.slint:69`). This is what built-in Kodi does.
  - The one change: an open sheet takes the keys.
- **LongPress** is not produced on the device screen. It would delay every OK. It reaches a package only through an activity's Long binding, already wired in T1 PR3.
- **Inside an activity.**
  - `button_controls.handle` runs first (`main.rs:1853`), so the user's activity bindings always win. That includes volume bound to a receiver, and `x:` ids with their phase (`activity_buttons.rs:1216-1219`).
  - Unmapped keys fall to the player screen exactly as above.
  - There are no per-device overrides. An activity is where a person remaps.

### Web (PR-G, parallel)
- A `volume_percent_control` slider, draft-then-apply, over `typed-action`.
- A text-only now-playing card from `GET .../media`. Artwork arrives in T5.
- The compile arm at `connections.rs:910`.

## 7. Packages

### `couch-integration-sonos` 0.2.0_pre1
Follows cw-hue's pattern: pin a `dev` commit, enable `protocol-3-preview`, and set `integration.json` to protocol 3.
- **Manifest:** as design section 7, minus `seek`/`modes` on the component, plus `up_next: true`.
- **`media`:** two reads with the group cached. The mapping table of design section 7, plus `device_name`, and `subtitle` per correction 4.
- **`artwork`:** through `ArtFetcher`. It never sends the API key, as `Client::artwork` already does (`lib.rs:1041-1063`).
- **`media_action`:** Seek, SetMode and both volume actions, with the Status ack.
- **Refusals:** `not_coordinator` and `nothing_to_play`.
- **Fake Control API:** gains an image, a member group, and a stall switch.
- **Tests:** the four admission cases plus `testing_v3::media(`.

### `couch-integration-kodi` 0.2.0_pre1
- **Manifest:** as design section 8, plus `channel-up`/`channel-down`, plus a `web_port` setting.
- **`media`:** video player only; `item = "player:file:id"`; `rate_percent = speed * 100`; `live`.
- **`artwork`:** fetched from the web port with Basic auth.
  - Ask Kodi for a panel-sized rendition. The image transform URL is to be verified read-only on a Kodi 22 box.
  - Aim at 1 MiB or less, and refuse above 4 MiB.
- **Lists:** `chapters` (a -32601 answer means no chapters), `audio`, and `subtitles` with an "Off" row and `current`. `choose` is guarded by `item`.
- **Actions:** `seek_by`.
- **Extra buttons:** `x:info`, `x:osd`, `x:subtitle-next`.
- **Contextual OK** stays in the package.
- **Fake JSON-RPC server:** serves `/image/` with auth, and gets a stall switch.
- **`changed`** is T5.

### Trial
Modelled on the Hue trial: a `.p3.dev` build through `tools/protocol-3-preview-env.sh`, the development remote only, a scratch feed with a throwaway key, and both built-ins left alone throughout.
- **Without the owner. Reads only: no playback, volume or navigation writes.**
  - Install; check the uid and RSS.
  - Run `GET .../media` 50 times against each LAN Sonos player and each Kodi box. Record the median, the slowest, and refusals.
  - With the player open for 15 minutes: memory, and the Busy rate with a second reader.
  - Artwork pull time by size, read from the GUI log.
  - Key-path latency against the fake Kodi on the build host, by `POST .../action` at 70 ms spacing.
  - An ordinary build over the preview and back: the layers are intact.
  - Screenshots compared with the built-ins.
- **With the owner, about 30 minutes.**
  - D-pad tap and hold feel in Kodi's menus.
  - Back, Back hold, OK, Menu, channel up/down.
  - A volume hold ramp, a seek drag, and the three sheets.
  - Sonos transport, modes, a source start, and a grouped member.
  - An activity with the receiver's volume bound.
  - One per-key IR override.
- **Still open from the Hue trial:** the feed metadata is compared with the literal 2.

## 8. PR split
Merge commits; every PR is green and shippable as a protocol 2 core.

| PR | Content | Frozen paths | Proof | Model |
| --- | --- | --- | --- | --- |
| A | Model: section 4. Compile arms in web and `activity_pages.rs:812`. | volume, connection, commands, buttons, validate, storage, lib | Unit tests; `config-crossload.sh` states N-Q; Denon harness | Opus. Top-model adversarial review of the projection. |
| B | Wire, SDK, echo and harness: sections 2 and 3 | couch-plugin, couch-sdk; not `testing.rs` | Goldens; mirrors; `old-package-wire.sh`; hostile echo cases; Denon harness | Opus. Top-model design check of the gate and review. |
| C | Daemon: section 5 | `plugins.rs`, `api/plugins.rs` | Scripted-child tests for lock classes, stale repeats, five readers, allow-list; switch-off guards; Denon harness | Sonnet; Opus review |
| D0 | Kodi pictures | none | Goldens | Sonnet |
| D | Controller generalised; key lane; Slint decoupling | none | The 30 pictures unmoved; D-pad and sheet tests | Opus |
| E | Packaged backend; routing; `row_bindings` | none | Same goldens by closures; lift test; latency recorded | Opus |
| F | Room-row volume | none | A coalescing test in the style of `room_sonos.rs:560` | Sonnet |
| G | Web | none | `plugin-components.mjs` | Sonnet |
| H | Docs: `protocol.md`, `client-sdk.md`, roadmap status | none | - | Haiku |
| S, K | Sonos 0.2_pre1 and Kodi 0.2_pre1, in their own repos | - | Admission cases plus `media(` | Sonnet (S), Opus (K) |
| R | Evidence renewal, then the `.p3.dev` build and the trial | `tools/release` | The verifier | Procedural; the owner for the second half |

- **Order:** A, then B, then C. D0 and D start now, in parallel. E needs B, C and D. F and G need C. S and K need B. R comes after A, B and C.
- **Evidence.**
  - Evidence goes stale from A's merge until the renewal in R.
  - Say so in each PR description.
  - Never edit `tested-integrations.json` in a train PR.
  - `devbuild.sh` refuses to build until R, so R precedes any trial build.

## 9. Risks
- A `media` read held under the lock still costs taps on a slow speaker. Mitigated by the 600 ms write wait, the GUI's single retry and the two-read `media`; measure it.
- The non-blocking artwork rule asks packages to run a thread. Mitigated by `ArtFetcher` and the harness case.
- The Slint key decoupling could move the Sonos or Kodi pictures. Caught by both golden sets.
- Literal churn on `Request::Action`. Handled with builders.
- The projection's new binding rule is the only new on-disk rule. Crossload states N-Q cover it.
- A hostile picture reaches a root process that aborts on panic.
- Kodi is polled at `refresh_ms` until T5, up to 5 s stale; use 2000 for the pre-release.
- The Kodi image-transform URL is unverified.
- T5 will move the revision rule and the artwork assembly into confd. Both are already library functions, so T5 changes no wire.

## 10. Questions for the owner
1. **Kodi screen keys.** Arrows, OK, Back, Home and Menu go straight to Kodi the moment the screen opens; holding Back leaves the screen. That is exactly how the built-in Kodi behaves today. Keep it? *Recommended: yes.*
2. **Colour keys.** May a package suggest what Red, Green, Yellow and Blue do on its own screen (Kodi: Info, on-screen display, next subtitle, stop)? Your own activity mappings would always win. *Recommended: yes, colour keys only.* It costs one small optional field.
3. **The "Commands" list.** Once the Kodi row opens the player, that list is no longer reachable from the row. Drop it on the remote? It stays in the browser, and every command stays mappable. Or do you want a fourth "More" sheet? *Recommended: drop it, and lean on the colour keys.*
4. **A sheet open on the Kodi screen** (Chapters, Audio, Subtitles). Arrows and OK would move in that list instead of going to Kodi, and Back closes it. The built-in keeps sending them to Kodi. *Recommended: the list takes them.*
5. **Channel up/down on the player.** Kodi decides: next or previous chapter, or channel on live TV. Sonos: next or previous track. *Recommended: yes.*
6. **Nothing playing on Kodi.** Keep today's sentence, "Connected to Kodi. Use the remote to choose something on your TV."? Live TV would show "LIVE" with no seek bar. *Recommended: yes.*
7. **Pictures.** Aim for about 1 MB by asking Kodi for a panel-sized version, and refuse above 4 MB (the built-in allows 8 MB). A refused picture means no backdrop. *Recommended: yes; measure first.*
8. **Holding OK** does nothing special on the Kodi screen; Menu opens Kodi's context menu. A hold can still be mapped inside an activity. *Recommended: yes.*
9. **The trial.** A read-only check can run without you. Then about 30 minutes of your hands on the D-pad, before T5 starts, since feel is the complaint. *Recommended: yes.*

### Critical Files for Implementation
- /Users/bryanhoban/Projects/couch/clients/couch-plugin/src/host.rs
- /Users/bryanhoban/Projects/couch/clients/couch-plugin/src/protocol.rs
- /Users/bryanhoban/Projects/couch/model/couch-model/src/storage.rs
- /Users/bryanhoban/Projects/couch/ui/couch-gui/src/media_player.rs
- /Users/bryanhoban/Projects/couch/daemon/couch-confd/src/plugins.rs

Other files read closely for this plan:
- /Users/bryanhoban/Projects/couch/model/couch-model/src/volume.rs
- /Users/bryanhoban/Projects/couch/clients/couch-plugin/src/manifest.rs
- /Users/bryanhoban/Projects/couch/clients/couch-plugin/src/server.rs
- /Users/bryanhoban/Projects/couch/ui/couch-gui/src/activity.rs
- /Users/bryanhoban/Projects/couch/ui/couch-gui/src/activity_buttons.rs
- /Users/bryanhoban/Projects/couch/ui/couch-gui/ui/screens/cinema.slint
- /Users/bryanhoban/Projects/couch/daemon/couch-confd/src/api/plugins.rs
- /Users/bryanhoban/Projects/couch/tools/tests/config-crossload.rs