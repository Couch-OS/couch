Title: Media player component (design)
Description: Proposed protocol 3 addition that lets an installed package drive Couch's full player screen.
Order: 21

# Media player component: design

Status: **proposal, nothing here is implemented.** It is the "media" part of the
single protocol 3 effort described in the
[integration extraction roadmap](integration-extraction-roadmap.md): the change
that lets Sonos, and after it Kodi, leave the main repository without losing the
player screen. The other parts of protocol 3 (pairing with a stored credential,
many devices behind one connection, a light control, cached state) are specified
by the Hue work in
[`couch-integration-hue/docs/protocol-needs.md`](https://github.com/Couch-OS/couch-integration-hue/blob/main/docs/protocol-needs.md);
section 5.5 says where the two fit together and where this document disagrees.
The Kodi preview package's
[gap list](https://github.com/Couch-OS/couch-integration-kodi#road-to-replacing-the-built-in-integration)
(checked read-only against two real Kodi 22 boxes) is the requirements list
section 8 answers item by item.

## Summary in plain English

**What this is.** Today an installed package (Denon, the Sonos preview) can only
fill in the simple control screen: a power tile, a source list, some command
tiles. The rich player screen - album art, track and artist, the progress line
you can drag, shuffle and repeat, "up next", the volume card - only works for
the Sonos and Kodi code that is compiled into Couch itself. This document
designs the missing piece so that a *package* can drive that same screen.

**Why it matters.** It is the one thing keeping Sonos inside the main
repository. Once it exists, Sonos can be updated on its own schedule like Denon,
and Kodi can follow with a small amount of extra work.

**How it works, without the jargon.**

- The package says in its description file "I am a media player" and lists what
  it can do (seek, shuffle, sources, artwork...). Couch draws the screen. The
  package never supplies any screen code, exactly as today.
- Couch asks the package "what is playing?" and gets back one tidy answer: title,
  artist, album, how long the track is, where it is up to, whether it is playing,
  which picture goes with it, and which buttons make sense right now.
- Pictures come *through the package*, in small pieces, because only the package
  knows how to reach the device (passwords, certificates). Couch keeps a small
  cache so a picture is fetched once.
- To stay up to date without pestering the speaker, the package may give Couch a
  tiny "something changed" nudge. Couch then asks once for the new state. A
  package that cannot notice changes is simply asked every few seconds, and only
  while the player screen is actually open.
- When something cannot be done, the package can now say *why* in a way the
  screen can act on: "this speaker is following Kitchen - open Kitchen".

**What you have to decide.**

1. **Call it protocol 3** (recommended), rather than squeezing it into protocol 2
   as an optional extra. Old Couch versions then refuse the new package cleanly
   instead of choking on it. Same approach as protocol 2 for Denon.
2. **Pictures travel through the package** (recommended), not fetched by Couch
   from an address the package hands over. Slightly slower, but works for Kodi's
   password-protected pictures and gives a package no way to make Couch's
   privileged processes fetch arbitrary addresses.
3. **Move the built-in Sonos screen onto the new shared player code first**
   (recommended). It costs one extra pull request, but it means built-in Sonos
   and packaged Sonos are the *same screen*, and a test can prove they look
   identical before built-in Sonos is removed.
4. **Put Kodi's extra needs (chapter, audio and subtitle lists) into protocol 3
   now** (recommended), even though the Kodi package comes later. Otherwise Kodi
   needs a protocol 4 and another evidence renewal.
5. **Packages may name a few extra buttons of their own** (recommended). Kodi has
   Info, on-screen display, next subtitle; Couch has no words for them. Rather
   than a Couch release per button, a package may declare extra commands under
   its own label. Couch lists them and lets you map them to keys, nothing more.
6. **One short line of the package's own wording may appear on screen** for
   errors Couch has no fixed wording for (recommended, capped at 160
   characters). The alternative is a fixed list only, which means a Couch release
   for every new device quirk.

**What it costs.** Seven pull requests in the main repository, of which three
touch the frozen "contract" files. Those three ride in the same protocol 3
"train" as the pairing and lights work, so the whole train costs **one evidence
renewal** (about 30 minutes, simulated on the build host) before the release that
carries it. Then one feed change to accept protocol 3, and the Sonos package
update. Size: **medium to large** - roughly the same effort as protocol 2 plus
the Denon parity work, with more GUI work.

**What it does not do.** No pairing, no discovery, no app lists, no lights, no
cameras. Those are the other parts of protocol 3 (see the roadmap). No hands are
needed on a remote or a speaker for any step up to the final hardware check.

**Two things the Sonos preview package already showed** (and that need no
protocol change, so they can be fixed straight away): the control screen never
shows the `title` and `playing` a package already reports, and a held volume key
costs one full round trip to the speaker per repeat with no instant feedback on
the volume card.

The rest of this document is for implementers.

---

## 1. What the built-in player does today

Everything below was read from the tree at `94f8a3d`.

### 1.1 How a device reaches the player screen

`ui/couch-gui/src/shortcuts.rs::opens_player` is the single switch:

```rust
pub fn opens_player(integration: &Integration) -> bool {
    matches!(integration, Integration::Kodi { .. } | Integration::Sonos { .. })
}
```

The room list (`lights.rs:860`) and the shortcut keys both ask it. A packaged
device is deliberately excluded and opens the TV-style core control screen
(`tv_plugin.rs`). The player opens as an "activity" addressed `device:<id>`;
`activity.rs` resolves that to a Kodi target, a Sonos target
(`sonos_target`) or a package pages target (`plugin_target`).

There is **one** player screen in Slint (`ui/screens/cinema.slint`, the `Cinema`
component) fed by about 25 `player-*` properties on `App` (`ui/app.slint:219-245`):
`player-title`, `player-metadata`, `player-elapsed`, `player-remaining`,
`player-progress`, `player-can-seek`, `player-paused`, `player-fanart`,
`player-logo`, `player-room`, `player-message`, `player-panel`,
`player-choices` (rows of `PlayerChoice { title, detail }`), `player-sheets`
(three sheet names), `player-music` (music layout vs. cinema layout) and
`player-selected` (D-pad focus). All actions come back through one callback,
`player-action(string, float)`, in a small vocabulary: `play`, `next`,
`previous`, `seek` (0-100), `volume`, `mute`, `chapters` / `audio` /
`subtitles` (open sheet 1/2/3), `choose` (row index), `Input.Up` ... `Input.Back`.

### 1.2 The two backends

| | Sonos (`sonos_player.rs`, 1144 lines) | Kodi (`activity.rs`, 1320 lines) |
| --- | --- | --- |
| Where the client runs | **Inside the GUI process**: `couch_sonos::Client::connect(host)` on a worker thread. The GUI (outer root) opens its own HTTPS socket to the player on 1443. | Through the `couch-control` broker (`couch_control::Kodi`), one owner per Kodi endpoint shared with the daemon. |
| State shape | `Snapshot { status, playback, now_playing }`: player + coordinator id and name, volume, mute; state string, `position_ms`, play modes, `can_seek`/`can_skip`; container, current and next `Track { name, artist, album, image_url, duration_ms, service }`. | `Playback { player, item: Value, properties: Value }` straight from `Player.GetItem`/`GetProperties`, plus `Vec<Chapter>`. |
| Freshness | **Polled**: `REFRESH = 3 s` while the screen is open, and once after every command. No events. | **Events + poll**: the worker checks `next_notification(1 ms)` every 40 ms and re-reads on any notification, else every 5 s. |
| Position | Read once per snapshot; the UI interpolates every second from the receive time (`clock`). | Same, scaled by Kodi's `speed`. |
| Seek | `seek` percent -> `position_ms` -> `seek_if_current`. | `Player.Seek` by percentage; chapter rows seek by time. |
| Modes | Sheet 2: shuffle, repeat (off -> all -> one), crossfade via `PlayModeChange`. | none |
| Sheets | Sources (favourites, playlists, TV, line-in; second line such as "Apple Music"), Modes, Up next (one row). | Chapters, Audio streams, Subtitles. |
| Volume | `nudge_volume(delta)` then read back; shows the shared volume card. Room rows coalesce a held key into one relative write (`room_sonos.rs`, step 2, burst 20). | `Application.SetVolume` step, or the device's IR override. |
| Groups | A member cannot take transport commands. The UI checks `coordinator != player.uuid` *before sending* and says "Playback is controlled by Kitchen. Open that speaker to change it." The client also returns `Error::NotCoordinator { coordinator }`. The room line reads "Lounge - Playing from Kitchen". | n/a |
| Artwork | `Client::artwork(url)`: absolute URL from the player (its own proxy on plain HTTP 1400, or the music service over HTTPS), 4 MiB limit, 5 s timeout, 2 redirects, no API key sent. | `activity_art.rs`: Kodi `image://` path -> `http://host:web_port/image/...` with the connection's **Basic auth**, 8 MiB limit, 4 s timeout; fetches fanart and a clear logo. |
| Decode | `activity_art::decode` in the GUI: format guessed, max 8192 px a side, 64 MiB allocation, 20 MP; resized to 480x800 with the legibility gradient pre-composed (`Shape::Backdrop`), 384x140 logo, 112x112 thumbnail. | same |
| Reopen | The last presentation (art included) is kept 30 s per device and restored instantly while the worker re-reads. | Bounded presentation cache with the same idea. |
| D-pad | Moves focus over seek / transport / sheets. | Passed through to Kodi as `Input.*` with contextual OK; this is how the user browses Kodi's own menus on the TV. |

Two facts shape the design:

- **Built-in Sonos parity does not need push events.** It polls every three
  seconds today. Events are an improvement (and a need for Kodi), not a blocker.
- **The GUI talks to Sonos directly today.** A package moves that socket behind
  `couch-confd` and the plugin child, which adds two local hops to every request.

### 1.3 The packaged path today

```text
GUI (outer root)                       Alpine chroot
  couch_plugin::local_request  --->  /opt/couch/plugin.sock  (0600, same-uid peers only,
  one connection per request          reached as /mnt/alpine/opt/couch/plugin.sock,
  one frame in, one frame out         at most 8 requests in flight)
                                         |
                                  couch-confd  plugins::Runtime
                                  one Endpoint per connection: queue of 8,
                                  750 ms queue lifetime, 60 s idle reap
                                         |
                                  Host: socketpair as the child's stdin/stdout,
                                  strict request -> response with matching ids,
                                  64 KiB frames, 12 s absolute deadline,
                                  any protocol/transport failure kills the child
                                         |
                                  child: uid/gid 65534, AID_INET only, no_new_privs,
                                  env cleared, cwd "/", stderr discarded
```

The protocol (`clients/couch-plugin/src/protocol.rs`) is **strictly
request/response**: `Hello`, `Configure`, `Command`, `Action`, `Status`,
`Inputs`. The child never speaks first. `Status` carries `on`, `muted`,
`volume` (0-100), `volume_db`, `input`, `playing`, `title`. Errors are nine fixed
codes with no text. Every wire type is `deny_unknown_fields` and every enum is
closed, so an older core **cannot parse** a manifest that names a new component.

`integrations/catalog.json` already lists, under the Sonos preview's
limitations, exactly what is missing: artwork, seek bar and clocks, volume
meter, shuffle/repeat/crossfade, queue, group awareness, which coordinator to
open, on-demand status only, the source list's second line and current marker,
and now-playing text. This design closes each of those.

---

## 2. Versioning: protocol 3

**Recommendation: a new protocol version, 3, introduced exactly the way
protocol 2 was (PR #194).**

Why not "an optional capability inside protocol 2":

- `Manifest`, `PluginComponent`, `Request` and `Response` all reject unknown
  fields and unknown enum tags. A protocol-2 core (`.177` onwards) handed a
  manifest containing `"kind": "media_player"` fails to *parse* it and reports
  "invalid or incompatible integration manifest". That is safe, but it is
  indistinguishable from a corrupt package, and the core cannot tell the user
  "this needs a newer Couch".
- `Manifest::validate` already requires
  `min_core_protocol_version == protocol_version`, and the feed's
  `validate_feed.py` enforces the same pair against `PROTOCOL_VERSIONS =
  {1, 2}`. The version number *is* the capability flag everywhere already.
- The saved configuration copies a package's `presentation` into
  `config.json`. A rollback core that meets an unknown component in the
  top-level document fails to parse the file, and `couch-confd` exits. Protocol
  2 solved this with the `integration_config_v2` envelope and a `v1_projection`
  (`model/couch-model/src/storage.rs`). Protocol 3 needs the same treatment, and
  that machinery is keyed on version.

**One protocol 3, not several.** The Hue work independently reached the same
conclusion for its own needs and also calls its proposal protocol 3. They are
the same version: every addition here and there is gated on `protocol_version
>= 3`, none of them overlap on the wire, and they share the mechanical parts
(more than one action schema per manifest, the `integration_config_v3` envelope,
the feed validator change). Landing them as one version means one envelope
projection, one feed change and one renewal instead of three of each.

### What each combination does

| Core | Package | Result |
| --- | --- | --- |
| protocol 1 or 2 core | protocol 3 package | Install is refused after download at manifest validation; the previously active version (for Sonos, the 0.1.x simple remote) stays active. Nothing is converted. Improvement worth making in the same wave: refuse *before* download, see 8.1. |
| protocol 3 core | protocol 1 or 2 package | Unchanged byte-for-byte. Opens the TV-style core control screen as today. |
| protocol 3 core | protocol 3 package without `media_player` | Treated like a v2 package that may also use the smaller v3 additions (percent volume action, rich errors). |
| protocol 3 core | protocol 3 package with `media_player` | Opens the player screen. |
| protocol 3 core rolled back to a protocol 2 core | any | The v2 core reads `integration_config_v2`, which is the v3 document with media components, media actions and v3-only bindings stripped. The device falls back to the control screen or, if the package itself is v3-only, shows as "package unavailable". An edit saved by the old core is authoritative after re-upgrade, as today. |

Rules carried over from v2 unchanged: a v3 package cannot downgrade to a v2
host; the hello exchange uses the manifest's version and requires the identical
manifest back; v1 and v2 manifests keep their byte shape.

---

## 3. Manifest additions

### 3.1 The `media_player` component

```json
{
  "protocol_version": 3,
  "min_core_protocol_version": 3,
  "presentation": [
    {
      "kind": "media_player",
      "layout": "music",
      "seek": true,
      "modes": ["shuffle", "repeat", "repeat_one", "crossfade"],
      "artwork": ["cover"],
      "lists": [
        { "id": "queue", "label": "Up next", "choose": true }
      ],
      "navigation": false,
      "events": true,
      "refresh_ms": 3000
    }
  ]
}
```

| Field | Meaning | Validation |
| --- | --- | --- |
| `layout` | `music` (cover art, artist/album line) or `video` (backdrop plus optional clear logo). Picks the existing `player-music` presentation. | required |
| `seek` | The package implements the `seek` action. | needs a declared `seek` action |
| `modes` | Which play modes exist on this device. | subset of the four; needs a declared `set_mode` action listing the same modes |
| `artwork` | Which artwork roles `media` may name: `cover`, `backdrop`, `logo`. Empty or absent: no artwork requests are ever sent. | at most 3, distinct |
| `lists` | Up to **three** extra sheets (the screen has three sheet buttons; "Sources" takes one when `supports_inputs` is true). `choose: false` makes a read-only list. | `id` is an identifier, labels follow the 128-byte rule, ids distinct |
| `navigation` | D-pad keys are sent to the device (`up`, `down`, `left`, `right`, `ok`, `back`, `home`, `menu`) instead of moving on-screen focus, **from the moment the screen opens, playing or idle**. Kodi sets this. (The control layout spends the D-pad on its own tiles and never emits a navigation action, which is why a Kodi package cannot be steered today except through activity button maps.) | every one of those capabilities must be declared |
| `events` | The package will send `changed` hints (section 5). | none |
| `refresh_ms` | How often the host may re-read `media` while someone is watching and no hint has arrived. | 1000-30000, default 3000 |

Manifest rules: at most one `media_player`; it requires the capability
`play-pause`, or both `play` and `pause`; `next`/`previous`/`stop` show only when
declared; a manifest with `media_player` must be protocol 3.

### 3.2 `volume_percent_control`

A sibling of `volume_db_control` for devices with a 0-100 scale:

```json
{ "kind": "volume_percent_control", "label": "Volume" }
```

It requires a declared `set_volume_percent` action. On the control screen and in the
web UI it renders as a slider with the same "draft, then apply" rule the dB
control has. On the player screen it feeds the existing volume card. `Status`
already carries `volume` (0-100) and `muted`, so no status change is needed.

### 3.3 Actions

`manifest.actions` is limited to one entry today (`actions.len() > 1` is
invalid). Protocol 3 allows **one schema per action kind**, at most eight.

```json
"actions": [
  { "action": "set_volume_percent", "max_percent": 100 },
  { "action": "step_volume_percent", "max_delta": 20 },
  { "action": "seek" },
  { "action": "seek_by", "max_delta_ms": 600000 },
  { "action": "set_mode", "modes": ["shuffle", "repeat", "repeat_one", "crossfade"] }
]
```

```rust
// model/couch-model/src/volume.rs (contract path)
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedAction {
    SetVolumeDb { tenths: i16 },                 // v2
    SetVolumePercent  { percent: u8 },           // v3, 0..=max_percent
    StepVolumePercent { delta: i8 },             // v3, 1..=max_delta either way, never 0
    Seek        { position_ms: u64 },            // v3, absolute (scrub bar, chapter)
    SeekBy      { delta_ms: i64 },               // v3, relative (-10 s / +30 s keys)
    SetMode     { mode: PlayMode, on: bool },    // v3
}
pub enum PlayMode { Shuffle, Repeat, RepeatOne, Crossfade }
```

All variants stay `Copy`; nothing with a string goes in here (list choices are
their own request, 4.3). `max_percent` lets a package declare a safety ceiling;
the host refuses a larger value before the child sees it, the same way it
refuses an off-step dB value now. `step_volume_percent` exists because the GUI coalesces
a held volume key into one relative write today (`room_sonos.rs`), and a string
command cannot carry the summed delta. `seek` is refused by the host when the
last known `media.can.seek` is false or `position_ms` exceeds the known
duration. The scrub bar's percentage is turned into `position_ms` by the screen,
so the three forms Kodi's `Player.Seek` takes (percentage, relative, absolute)
need only the two actions.

**Aimed at one item.** `Request::Action` and `Request::Choose` gain an optional
`item` (the opaque id from `media.item`, 4.1). When present, the host refuses the
request with `expired` and `reason: item_changed` if its cached `media.item`
differs, and the package checks again before writing. This is the built-in Kodi
screen's "Playback changed; try again" rule: a seek or a subtitle choice aimed at
an episode must never land on the next one.

**`volume:NN` and `dim:N` in button maps.** `Function::Volume(n)` already exists
and works for built-in Sonos, Kodi and webOS. For a package that declares
`set_volume_percent`, the host accepts `volume:NN` from a button map or an
activity step and turns it into that action, exactly as the Hue proposal turns
`dim:N` into `SetLight`. No new capability ids are needed.

Volume keys keep working the old way too: `volume-up` / `volume-down` / `mute`
remain ordinary capabilities so activity button maps and the room rows do not
change.

### 3.4 Richer source rows

`Selectable` gains two optional fields in protocol 3 responses:

```rust
pub struct Selectable {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,   // "Apple Music", "Sonos playlist - 12 tracks"; <= 256 bytes
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub current: bool,            // at most one row
}
```

The host strips both from a v1/v2 package's reply so those wire contracts do not
move. `inputs` remains the bindable list (`input:<id>` in button maps); the
player's "Sources" sheet is simply that list with its second line.

### 3.5 Keys: holds, and words Couch does not have

Two gaps the Kodi preview package found. Both live in `commands.rs` /
`protocol.rs`, so they belong in the same train.

**Hold and repeat.** `Request::Command` carries a bare id, so Kodi's long-press
(`Input.ButtonEvent` with `holdtime`, the basis of contextual OK) cannot be
expressed, and a package cannot tell a fresh press from the 70 ms key repeat.

```rust
Command { function: String,
          #[serde(default)] resource: Option<String>,     // Hue proposal
          #[serde(default)] phase: KeyPhase }             // v3; default = Tap
pub enum KeyPhase { Tap, Repeat, LongPress }
```

The panel already knows which of the three a key event is. A v1/v2 package is
always sent `Tap` semantics (the field is omitted), so nothing changes for it.
A repeat that waited in the queue longer than one repeat interval is dropped
rather than delivered late; that is today's 750 ms rule tightened for `Repeat`
only.

**Package-defined function ids.** A capability id must parse as a
`couch_model::commands::Function`, and there is no word for Info, the on-screen
display, subtitles next/off, next audio stream, page up/down or the number keys.
Adding each to the core vocabulary means a Couch release per device quirk.
Proposal: a namespaced form, `x:<id>` (`Function::Custom(String)`, identifier
characters, at most 48 bytes), that a protocol 3 package may declare with its own
label. Couch never interprets it: it appears in the Commands list and the button
picker under the package's label, can be bound to a key or an activity step, and
is refused for any device that did not declare it. The v2 rollback projection
strips bindings to `x:` functions, as the v1 projection strips spaced input ids.

---

## 4. New requests

```rust
pub enum Request {
    // v1/v2, unchanged
    Hello { protocol_version: u32 }, Configure { settings: Value },
    Command { function: String }, Action { action: TypedAction }, Status, Inputs,
    // v3: Command gains `phase` (3.5); Action and Choose gain `item` (3.3); Command,
    // Action and Status gain `resource` (Hue proposal). All optional, all omitted
    // for a v1/v2 child.
    Media,
    Artwork { art: String, offset: u32 },
    List { list: String, offset: u32 },
    Choose { list: String, id: String },
    Subscribe { topics: Vec<Topic>, lease_ms: u32 },
}
pub enum Response {
    Hello { manifest: Manifest }, Ok, Status { status: Status }, Inputs { inputs: Vec<Selectable> },
    Media { media: Media },
    Artwork { art: String, total: u32, offset: u32, data: String, mime: ArtMime },
    List { list: String, total: u32, offset: u32, items: Vec<ListItem> },
    Error { code: Error, #[serde(default)] reason: Option<Reason> },
}
```

The host refuses every v3 request to a v1/v2 package with `Unsupported` before
writing to the child, as it already does for `Action` on a v1 package.

### 4.1 `media`: what is playing

```json
{"id": 41, "body": {"method": "media"}}
```

```json
{"id": 41, "body": {"type": "media", "media": {
  "item": "q:1:9f3c52e1",
  "state": "playing",
  "title": "Weird Fishes / Arpeggi",
  "artist": "Radiohead",
  "album": "In Rainbows",
  "source": "Apple Music",
  "duration_ms": 318000,
  "position_ms": 74210,
  "rate_percent": 100,
  "art": { "cover": "a1:9f3c52e1" },
  "can": { "play_pause": true, "next": true, "previous": true, "seek": true, "modes": true },
  "modes": { "shuffle": false, "repeat": true, "repeat_one": false, "crossfade": false },
  "next": { "title": "All I Need", "detail": "Radiohead - In Rainbows" },
  "group": { "role": "coordinator", "members": ["Lounge", "Kitchen"] }
}}}
```

```rust
// clients/couch-sdk/src/media.rs (new file inside a contract path)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    /// Opaque identity of what is playing (Kodi: "player:file:id"). Changes when
    /// the item changes; guards `seek`, `choose` and mode changes (3.3).
    pub item: Option<String>,
    pub state: PlayState,                  // playing | paused | buffering | stopped | idle
    pub title: Option<String>,             // every string <= 512 bytes, no control characters
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Second line when there is no artist/album: "Severance - S2 E4", "2019", a station name.
    pub subtitle: Option<String>,
    /// Where it is playing from: a service, a playlist, "TV", "Line-in".
    pub source: Option<String>,
    pub duration_ms: Option<u64>,          // None = unknown
    pub live: bool,                        // Kodi reports a live stream as total time zero
    pub position_ms: Option<u64>,          // as of the moment this reply was written
    pub rate_percent: i16,                 // 100 normal, 0 paused, 200 = 2x, negative = rewind
    pub art: Art,                          // opaque ids, 4.2
    pub can: Can,                          // what makes sense *now*
    pub modes: Modes,                      // each Option<bool>: None = device has no such mode
    pub next: Option<NextItem>,
    pub group: Option<Group>,
}
pub struct Group {
    pub role: GroupRole,                   // standalone | coordinator | member
    pub coordinator: Option<String>,       // display name, when role = member
    pub members: Vec<String>,              // display names, <= 32
}
```

Design points:

- **Nothing is fabricated**, the rule `Status` already follows: an unobserved
  field is absent, not zero.
- **Position has no timestamp on the wire.** Plugin and host have no shared
  clock worth trusting across a chroot, and the GUI already interpolates from
  the moment it *receives* a snapshot. The plugin reports the position as of the
  reply; `couch-confd` records when the reply arrived and adds `age_ms` when it
  hands a cached copy to the GUI or the browser (section 5.3). The screen then
  computes `position_ms + (age_ms + time since receipt) * rate_percent / 100`.
- **`can` replaces a round trip.** The built-in Sonos screen refuses a transport
  press on a group member locally and words the reason itself. `can` plus `group`
  lets the core screen do the same for any package, in Couch's own words, with no
  device I/O: `role = member` and `coordinator = "Kitchen"` produce today's
  exact message.
- **`media` must be cheap.** For Sonos it is the same three reads
  `Client::snapshot` does today. A package must not enumerate favourites or a
  queue to answer it.
- `Status` is untouched and stays the cheap call used by the volume card and
  the power tile.

### 4.2 `artwork`: pictures through the package

```json
{"id": 42, "body": {"method": "artwork", "art": "a1:9f3c52e1", "offset": 0}}
{"id": 42, "body": {"type": "artwork", "art": "a1:9f3c52e1", "total": 183442,
                     "offset": 0, "mime": "jpeg", "data": "<base64, at most 45000 raw bytes>"}}
```

An art id is an opaque string chosen by the package (at most 256 bytes,
identifier characters plus `:._-`). It must change when the picture changes and
should stay the same while it does not - the hash of the device's image URL is
the obvious choice. The host asks for offset 0, then `offset += len` until
`offset + len == total`.

Limits the host enforces: `total <= 4 MiB` (today's Sonos limit), chunk at most
45 000 raw bytes so the base64 frame stays under the 64 KiB frame limit, `mime`
one of `jpeg | png | webp`, `total` identical in every chunk, at most 128
chunks. A violation is `Protocol` and retires the child as any other protocol
error does.

Package side: fetch on the offset-0 request with its own short deadline (5 s
recommended; the host's 12 s still bounds it), keep only the **most recent**
picture's bytes in memory, serve later chunks from memory. If the child was
restarted mid-transfer, a non-zero offset for an unknown id is answered by
fetching again; artwork reads are idempotent, so unlike commands they are safe to
repeat.

Why through the package rather than "give the host a URL":

| | URL fetched by the host | Bytes through the package (**recommended**) |
| --- | --- | --- |
| Kodi | Needs the connection's Basic-auth password, which lives in the package's private settings. The host would need it too. | Works: the package already holds it. |
| Pinned-certificate devices (later waves) | Host would need each package's trust material. | Works. |
| Privilege | `couch-confd` and the GUI run as root; a package could point them at any address, including loopback services. | No privileged process fetches anything. The unprivileged child does the network I/O it could do anyway. |
| Speed | One HTTP GET. | About 4 local round trips per 180 KB, 90 for a 4 MiB picture; each is a few milliseconds on a socketpair. Commands interleave between chunks, so a key press waits for at most one chunk. |
| New moving parts | URL allow-list rules, redirect rules, a second HTTP stack in confd. | One request type. |

A file drop (the package writes into a host-made cache directory) was
considered and rejected: every package shares uid 65534, so any package could
replace another's file, and a root process would be opening paths an
unprivileged process controls.

**Where it is decoded and cached.** `couch-confd` assembles the chunks and keeps
the encoded bytes in a small LRU: at most 4 pictures per connection and 16 MiB
in total, dropped when the endpoint is reaped. The GUI asks confd for the
picture over `plugin.sock` with the same chunked request (the local socket has
the same 64 KiB frame), decodes it with the existing `activity_art::decode`
limits into the existing shapes, and keeps its 30-second reopen cache. The web UI
gets `GET /api/plugins/<connection>/artwork/<art-id>` from the same cache.
Decoding untrusted bytes in the root GUI is no worse than today (the bytes
already come from the LAN); moving the decode into an unprivileged helper is a
worthwhile hardening but is not part of this change.

### 4.3 `list` and `choose`: the extra sheets

```json
{"method": "list", "list": "chapters", "offset": 0}
{"type": "list", "list": "chapters", "total": 14, "offset": 0, "items": [
  {"id": "0", "title": "Opening", "detail": "0:00", "current": false},
  {"id": "1", "title": "Chapter 2", "detail": "6:42", "current": true}]}
{"method": "choose", "list": "chapters", "id": "1"}      -> {"type": "ok"}
```

`ListItem { id, title, detail: Option<String>, current: bool }`, at most 100
items per reply and 1000 per list, strings capped at 256 bytes. `choose` has
command semantics: never retried, the no-retry admission case covers it. A list
the manifest did not declare is refused by the host.

Sonos needs only one list ("Up next", and even that fits in `media.next`).
Kodi needs three. They are specified now so Kodi does not need a protocol 4.

### 4.4 Richer errors

```rust
pub enum Response { /* ... */ Error { code: Error, reason: Option<Reason> } }

#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reason {
    /// A group member was asked to do the coordinator's job.
    NotCoordinator { coordinator: String },
    NothingToPlay,
    NotSeekable,
    /// The chosen source or list row no longer exists or cannot start.
    Unavailable,
    /// The item the request was aimed at is no longer playing (3.3).
    ItemChanged,
    /// A `configure` or pairing form was refused: which field, and why. Today
    /// `invalid` tells the person filling in the form nothing at all.
    InvalidSetting { field: String, text: String },
    /// Display-only, package-worded, <= 160 bytes, no control characters.
    Message { text: String },
}
```

The nine codes keep their meaning and remain what the host acts on (retire the
child, map to an HTTP status). `reason` is only for the person looking at the
screen. Couch words every variant except `Message` itself, so they stay
consistent and translatable. For `NotCoordinator` the GUI looks for a configured
device whose name matches and offers "Open Kitchen" instead of only saying so.
`reason` is dropped from any v1/v2 child's reply.

---

## 5. Staying fresh without hammering devices

### 5.1 What exists

Nothing: the child cannot speak first, and the GUI only learns about a change by
asking. The packaged control screen re-reads after each command and otherwise
never.

### 5.2 Plugin to host: a data-free, one-shot hint

```json
{"id": 50, "body": {"method": "subscribe", "topics": ["media"], "lease_ms": 30000}}
{"id": 50, "body": {"type": "ok"}}
...later, unprompted, from the child:
{"id": 0, "body": {"type": "changed", "topic": "media"}}
```

Rules, all enforceable in the host and testable in the harness:

1. Request ids start at 1, so **id 0 marks an event frame**. It is the only thing
   a child may ever write unprompted, and only after a successful `subscribe`.
2. The hint carries **no data**. The host reacts by sending an ordinary `media`
   (or `status`) request. All state still arrives through the validated
   request/response path, and stdout remains protocol-only.
3. **One-shot, re-armed by reading.** After sending `changed` for a topic the
   child must stay silent on that topic until the host has read it. So at most
   one unread event frame per topic can ever sit in the socket, the child can
   never block on a full pipe, and the *host* sets the pace. That is the whole
   backpressure story.
4. The host reads a topic no more than once every 500 ms however many hints
   arrive; a second `changed` before the read is a protocol error (child
   retired), which the admission harness checks.
5. **Leased.** A subscription lapses after `lease_ms` (at most 60 s) unless
   renewed by another `subscribe`. The host renews only while somebody is
   watching. If the GUI dies or the unsubscribe is lost, the package stops
   watching the device within a minute by itself. `topics: []` unsubscribes.
6. Topics are `media`, `status` and `states` (the children of a connection, from
   the Hue proposal; the hint then triggers a `States { since }` read). A media
   package opts in with `events: true` on `media_player`; no screen consumes
   `status` hints yet.

Host changes (`clients/couch-plugin/src/host.rs`): while waiting for a response,
a frame with id 0 is recorded in a one-slot-per-topic flag and reading
continues under the same deadline. While idle, the `Endpoint` worker waits on
its request queue with a 200 ms timeout and does a non-blocking read for an
event frame between requests. No extra thread, and `Host::request` keeps its
"one in flight, failure retires the stream" shape.

SDK side (`clients/couch-plugin/src/server.rs`, `clients/couch-sdk`): `serve`
stays single-threaded. It polls stdin with a timeout and, while subscribed,
spends idle time in a new optional trait method:

```rust
pub trait DeviceClient {
    /// Block for at most `wait` for a sign that `topic` changed. Must not write
    /// to the device. Default: not supported.
    fn changed(&mut self, _topic: Topic, _wait: Duration) -> Result<bool> { Ok(false) }
    fn media(&mut self) -> Result<Media> { Err(Error::Unsupported) }
    fn artwork(&mut self, _art: &str) -> Result<(ArtMime, Vec<u8>)> { Err(Error::Unsupported) }
    fn list(&mut self, _list: &str, _offset: u32) -> Result<ListPage> { Err(Error::Unsupported) }
    fn choose(&mut self, _list: &str, _id: &str) -> Result<()> { Err(Error::Unsupported) }
}
```

Kodi implements `changed` with the `next_notification(wait)` it already has.
Sonos 0.2 does not implement it (`events: false`) and is polled; a later Sonos
version can use the Control API's event subscription without any protocol
change.

### 5.3 Host to GUI and browser: watch

The GUI should not care whether a package pushes or is polled. `couch-confd`
hides the difference behind a per-connection cache:

```rust
struct MediaCache { media: Media, revision: u64, received: Instant }
```

`revision` increases whenever the state differs in anything except
`position_ms`. Two new **local** requests on `plugin.sock` (they never reach a
child):

```json
{"connection_id": "lounge-sonos", "request": {"method": "watch", "topic": "media",
                                               "after": 17, "wait_ms": 10000}}
{"type": "watched", "revision": 18, "age_ms": 120, "media": { ... }}
```

`watch` returns immediately when `revision > after`, otherwise when it changes
or after `wait_ms` (at most 10 s) with the same revision and a fresh `age_ms`.
It is a long poll over the socket that already exists, so `local_request` only
needs a longer timeout. While at least one watcher exists confd subscribes to
the child (if `events`) and otherwise re-reads every `refresh_ms`; with no
watcher it does neither, so a closed screen costs the device nothing. After a
`command`, `action` or `choose` on that connection confd re-reads `media` once,
**300 ms after the last one**, which is what both built-in screens do after a
command today - except for navigation commands (`up` ... `menu`) and key repeats,
which trigger no read at all. The control layout's habit of following *every*
command with a `status` read (`tv_plugin::run`) costs Kodi two to four extra
round trips per key and must not be copied into the player layout.

Limits: `plugin.sock` allows 8 requests in flight today. Watches get their own
small allowance (2) so a player screen can never starve key presses. The browser
gets the same data as `GET /api/plugins/<connection>/media?after=17`.

### 5.4 Timeouts, as the admission cases enforce them

| Limit | Value | Applies to the new messages |
| --- | --- | --- |
| Startup and configure | 3 s each | unchanged; still must not touch the device |
| Any request | 12 s absolute, across partial frames | `media`, `artwork` (each chunk), `list`, `choose`, `subscribe` |
| Queue | 8 requests, 750 ms lifetime | artwork chunks and background re-reads are queued one at a time and only when the queue is empty, so they never expire a key press |
| Frame | 64 KiB | artwork chunks sized to fit; `media` and one `list` page fit by construction of the string caps |
| Hints | one unread per topic; host reads at most every 500 ms | new |
| Lease | at most 60 s | new |
| No retry | a lost reply to `command`, `action`, `choose` is never re-sent | `media`, `artwork`, `list` are reads and *may* be re-asked by a later explicit request |

New admission cases for a package that declares `media_player` (names kept
literal so the feed's textual check can find them, as it does for the four
existing ones): `testing::media(` - conformance of `media`, `artwork`
chunking, declared lists and typed actions against the package's fake device;
and `testing::hints(` - only when `events` is true: one hint per change, silence
until read, lapse after the lease, and a flood is fatal to the child and leaks
no device I/O.

### 5.5 How this fits with the Hue proposal

The Hue document specifies, for the same protocol 3: `PairStart` /
`PairContinue` / `PairCancel` with an opaque host-stored `credential` handed back
in `Configure`; paged `Children` and an optional `resource` on `Command`,
`Action` and `Status`; `TypedAction::SetLight` and a `light` component;
`States { since }` answered from the package's cache; `keep_alive`; and
unprompted event frames as a later step. This document adopts all of that
unchanged except for the three points below.

| Point | Hue proposal | This document | Recommendation |
| --- | --- | --- | --- |
| **Unprompted frames** | A later step, "should not be bundled": `{"id":0,...,"type":"event","revision":N}` demultiplexed by a reader thread. | In protocol 3 now, but as a data-free one-shot hint read between requests by the existing worker (5.2), which leaves `Host::request` and its deadline logic as they are. | Include the hint in protocol 3. Kodi needs it, it is the smaller of the two host changes, and deferring it means a protocol 4 for one frame type. Land it **late** in the train so Hue never waits for it. Hue's `States` does not need it and works first. |
| **Who polls** | The panel asks the child `States { since }` every 500 ms while a room is open. | The daemon keeps the cache and the panel long-polls `watch` (5.3). | Same data path, different place. Each panel request today is a new socket connection, a new daemon thread and a child round trip; twice a second per open room adds up on this hardware. Let `couch-confd` ask the child (`States` for children, `media` for a player) and let the panel `watch` one revision number. `States { since }` is kept exactly as specified as the daemon-to-child request. |
| **Revision numbers** | The package owns `revision`. | The daemon computes it for `media`. | Keep both: a package with many children is the only one that can say cheaply what changed, so it owns the `States` revision; `media` is one small struct, so the daemon diffing it saves every media package from keeping a counter. |

Two smaller alignments, already applied above: the percent volume action is named
`set_volume_percent` (what the Sonos preview work asked for), and "the device
needs pairing again" is the Hue proposal's new error code `unpaired`, not a
`reason`, because the host acts on it.

---

## 6. The GUI: a second core layout

Core layouts are chosen by what the package declares, never by its id:

| Declares | Opens | Code |
| --- | --- | --- |
| a `media_player` component | the **player layout** (`Cinema`) | new `media_player.rs` |
| anything else | the **control layout** (TV-style: power, source, tiles) | `tv_plugin.rs`, unchanged |

`opens_player` becomes:

```rust
pub fn opens_player(integration: &Integration) -> bool {
    match integration {
        Integration::Kodi { .. } | Integration::Sonos { .. } => true,
        Integration::Plugin { presentation, .. } => presentation
            .iter()
            .any(|c| matches!(c, PluginComponent::MediaPlayer { .. })),
        _ => false,
    }
}
```

**Recommended shape:** split today's `sonos_player.rs` into a device-neutral
controller and a backend trait, then give it two backends.

```rust
// ui/couch-gui/src/media_player.rs
pub(crate) trait Backend: Send {
    fn open(&mut self) -> Result<(), Failure>;
    fn watch(&mut self, after: u64, wait: Duration) -> Result<Watched, Failure>;
    fn perform(&mut self, op: Op, current: &dyn Fn() -> bool) -> Result<(), Failure>;
    fn sources(&mut self) -> Result<Vec<Selectable>, Failure>;
    fn list(&mut self, id: &str) -> Result<Vec<ListItem>, Failure>;
    fn artwork(&mut self, art: &str) -> Result<Vec<u8>, Failure>;
}
struct BuiltInSonos(couch_sonos::Client);      // maps Snapshot -> Media; removed with built-in Sonos
struct Packaged { connection: String }          // couch_plugin::local_request to plugin.sock
```

The controller owns everything device-neutral that exists twice today: the
generation counter, the busy flag, focus movement, sheets, the clock, the
message timer, the volume card, the 30-second reopen cache and the art key. The
`Media` struct is its only view of the device. Because both backends feed the
*same* controller, "packaged Sonos looks and behaves like built-in Sonos" becomes
a test rather than a hope (section 9, PR 4 and PR 5).

Sheets are built from the declaration: "Sources" when `supports_inputs`,
"Modes" when `modes` is non-empty, then the declared lists, three at most. With
`navigation: true` the D-pad keys are sent as commands (Kodi); otherwise they
move focus (Sonos).

D-pad latency for a navigating package: GUI -> `plugin.sock` -> confd thread ->
endpoint queue -> child -> device. Built-in Kodi already crosses one broker
socket (`couch-control`); the package path adds one more local hop and a
thread spawn per press, low single-digit milliseconds on the HA100, against
tens of milliseconds for Kodi's own JSON-RPC round trip. Held keys are covered by
the existing queue (8 deep, 750 ms lifetime). Worth measuring in PR 5 rather than
assuming: if the per-request connect shows up, keep one `plugin.sock` connection
open per screen instead of one per request.

The web UI renders `media_player` as a compact now-playing card (art, title,
line two, transport, a seek slider, a volume slider) in
`web/couch-web/src/screens/connections.rs` beside the existing component
renderers.

---

## 7. Worked example: Sonos

`plugin.json` for `couch-integration-sonos` 0.2.0 (capabilities as in 0.1.0,
abbreviated):

```json
{
  "protocol_version": 3,
  "min_core_protocol_version": 3,
  "id": "sonos", "label": "Sonos", "version": "0.2.0",
  "executable": "bin/couch-plugin-sonos",
  "capabilities": [
    {"id": "play", "label": "Play"}, {"id": "pause", "label": "Pause"},
    {"id": "play-pause", "label": "Play / pause"}, {"id": "stop", "label": "Stop"},
    {"id": "next", "label": "Next"}, {"id": "previous", "label": "Previous"},
    {"id": "volume-up", "label": "Volume up"}, {"id": "volume-down", "label": "Volume down"},
    {"id": "mute", "label": "Mute"}, {"id": "mute-on", "label": "Mute on"},
    {"id": "mute-off", "label": "Mute off"}
  ],
  "actions": [
    {"action": "set_volume_percent", "max_percent": 100},
    {"action": "step_volume_percent", "max_delta": 20},
    {"action": "seek"},
    {"action": "set_mode", "modes": ["shuffle", "repeat", "repeat_one", "crossfade"]}
  ],
  "settings": [
    {"id": "host", "label": "Player address", "kind": "text", "required": true},
    {"id": "api_key", "label": "Sonos developer API key", "kind": "secret", "required": false},
    {"id": "api_root", "label": "API root (leave empty for https://<address>:1443/api/v1)", "kind": "text", "required": false}
  ],
  "supports_inputs": true,
  "presentation": [
    {"kind": "media_player", "layout": "music", "seek": true,
     "modes": ["shuffle", "repeat", "repeat_one", "crossfade"],
     "artwork": ["cover"], "lists": [], "navigation": false,
     "events": false, "refresh_ms": 3000},
    {"kind": "volume_percent_control", "label": "Volume"},
    {"kind": "toggle", "label": "Mute", "state": "muted", "on": "mute-on", "off": "mute-off"},
    {"kind": "input_selector", "label": "Source"}
  ]
}
```

Mapping from the existing client, nothing new to write against the speaker:

| Protocol | `couch-sonos` today |
| --- | --- |
| `media` | `Client::snapshot()` -> `state` from `playback.state` (`PLAYING`/`BUFFERING`/`PAUSED`...), `title/artist/album` from `now_playing.current`, `source` from `current.service` or `container`, `subtitle` from `container` when there is no track, `duration_ms`, `position_ms`, `can.seek = can_seek && duration.is_some() && role != member`, `can.next = can_skip`, `can.previous = can_skip_back`, `modes`, `next`, `group.role` from `status.coordinator == status.player.uuid`, `group.coordinator = status.coordinator_name`, `art.cover = "a1:" + hash(image_url)` |
| `artwork` | `Client::artwork(image_url)`, already limited to 4 MiB with no API key sent |
| `inputs` | `Client::sources()`, with `detail` from `Source.detail`; `current` stays false because a player does not say which favourite it is playing |
| `action seek` | `seek_if_current` |
| `action set_mode` | `set_play_modes_if_current` with a one-field `PlayModeChange` |
| `action set_volume_percent` / `step_volume_percent` | `set_volume` / `nudge_volume` |
| error on a member | `Error::NotCoordinator { coordinator }` -> `rejected` + `reason: not_coordinator` |
| `ERROR_PLAYBACK_NO_CONTENT` | `rejected` + `reason: nothing_to_play` |

A session, player screen open on a grouped speaker:

```text
GUI  -> confd  watch media after=0                     (long poll)
confd -> child media                                   -> state playing, group member of "Kitchen"
confd -> GUI   watched revision=1 age_ms=4
GUI  -> confd  artwork a1:9f3c52e1 offset=0            (confd pulls 4 chunks from the child, caches)
GUI            shows "Lounge - Playing from Kitchen"; transport presses are refused locally
                with "Playback is controlled by Kitchen" and an "Open Kitchen" action
... every 3 s while watched: confd -> child media; only a change bumps the revision
GUI  -> confd  action step_volume_percent +6                  (a held key, coalesced)
confd -> child action ...; then media once
GUI closes     -> no watcher -> no more reads; child reaped after 60 s idle
```

The API key is unaffected by this design: it stays a build-time constant baked
in by the feed's publish job, with the per-connection secret as the override.

---

## 8. Worked example: Kodi

```json
{
  "protocol_version": 3, "min_core_protocol_version": 3,
  "id": "kodi", "label": "Kodi", "version": "0.1.0",
  "executable": "bin/couch-plugin-kodi",
  "capabilities": [
    {"id": "up", "label": "Up"}, {"id": "down", "label": "Down"},
    {"id": "left", "label": "Left"}, {"id": "right", "label": "Right"},
    {"id": "ok", "label": "OK"}, {"id": "back", "label": "Back"},
    {"id": "home", "label": "Home"}, {"id": "menu", "label": "Menu"},
    {"id": "play-pause", "label": "Play / pause"}, {"id": "stop", "label": "Stop"},
    {"id": "next", "label": "Next"}, {"id": "previous", "label": "Previous"},
    {"id": "volume-up", "label": "Volume up"}, {"id": "volume-down", "label": "Volume down"},
    {"id": "mute", "label": "Mute"},
    {"id": "x:info", "label": "Info"}, {"id": "x:osd", "label": "On-screen display"},
    {"id": "x:subtitle-next", "label": "Next subtitle"}
  ],
  "actions": [{"action": "seek"}, {"action": "seek_by", "max_delta_ms": 600000},
              {"action": "set_volume_percent", "max_percent": 100}],
  "settings": [
    {"id": "host", "label": "Kodi address", "kind": "text", "required": true},
    {"id": "port", "label": "JSON-RPC port", "kind": "integer", "required": true, "default": 9090},
    {"id": "web_port", "label": "Web server port (artwork)", "kind": "integer", "required": false, "default": 8080},
    {"id": "username", "label": "Web server user", "kind": "text", "required": false},
    {"id": "password", "label": "Web server password", "kind": "secret", "required": false}
  ],
  "presentation": [
    {"kind": "media_player", "layout": "video", "seek": true, "modes": [],
     "artwork": ["backdrop", "logo"],
     "lists": [
       {"id": "chapters", "label": "Chapters", "choose": true},
       {"id": "audio", "label": "Audio", "choose": true},
       {"id": "subtitles", "label": "Subtitles", "choose": true}],
     "navigation": true, "events": true, "refresh_ms": 5000}
  ]
}
```

| Protocol | `couch-kodi` today |
| --- | --- |
| `media` | `Kodi::playback()`: `title` from `item.title`/`label`; `subtitle` = "Show - S2 E4" or the year; `duration_ms`/`position_ms` from `totaltime`/`time`; `rate_percent = speed * 100`; `can.seek = canseek`; `state = idle` when there is no video player ("Connected to Kodi. Use the remote to choose something on your TV."); `art.backdrop`/`art.logo` = hash of `art.fanart`/`art.clearlogo` |
| `artwork` | the fetch in `activity_art.rs::fetch` moves into the package: `image_url(path)`, Basic auth from its own settings. The 4 MiB limit is below today's 8 MiB, so a very large fanart falls back to no backdrop; the package should prefer the smaller `thumb`/`poster` rendition when fanart exceeds the limit |
| `changed(media)` | `next_notification(wait)`; the re-read after a notification replaces the GUI's 40 ms loop |
| `list chapters` / `choose` | `chapters(player)`; `choose` -> `Player.Seek` to the chapter time |
| `list audio` / `subtitles` | `properties.audiostreams` / `subtitles`; subtitles list starts with an "Off" row; `current` from `currentaudiostream` / `currentsubtitle` |
| D-pad | `navigation: true`: `up`...`menu` -> `Input.*`, with the contextual-OK logic (`docs/kodi-contextual-ok.md`) inside the package |

The Kodi preview package's gap list, item by item:

| Gap list item | Answer here |
| --- | --- |
| 1. Player screen with the D-pad going to the device | `media_player` with `navigation: true` (3.1), player layout chosen by declaration (6). |
| 2. A key path cheap enough for a held D-pad; long-press | No read-back after navigation keys and repeats (5.3); `KeyPhase` (3.5); measure on the HA100 at the 70 ms repeat rate in pull request 5 and keep one `plugin.sock` connection per screen if the per-request connect shows up. |
| 3. Now playing: identity, speed, three seek forms, chapters, audio and subtitle lists, video player only | `media.item` guard, `rate_percent`, `live`, `seek` + `seek_by` (3.3, 4.1); `list`/`choose` with `current` (4.3); "-32601 means none" and "video player only" stay inside the package. Channel keys step through the `chapters` list's `current` row, as the built-in screen does. |
| 4. Artwork from the web port with Basic auth | Bytes through the package (4.2), which is why that option was chosen; the package gains a `web_port` setting (above). |
| 5. Events; `serve` blocks on stdin | One-shot hints and a serve loop that waits on stdin and the device (5.2). The notifications the package already queues and drops become `changed(media)`. |
| 6. Percent volume and `volume:NN` | `set_volume_percent`, `step_volume_percent`, and `volume:NN` mapped by the host (3.3). |
| 7. Words for Kodi's other keys | `x:<id>` package-defined functions (3.5). |
| 8. Converting `Provider::Kodi` and `Provider::CoreElec` | Roadmap, section 5: `host`, `port`, and from `kodi-connection.json` `http_control` -> `http`, `web_port`, `username`, `password`. CoreELEC's SSH half stays in the daemon. |
| 9-11. Text entry, power / Wake-on-LAN, discovery | Not needed for parity (the built-in lacks them too). Discovery and text prompts are the pairing/discovery part of protocol 3. |
| 12. A form that can explain itself | `reason: invalid_setting { field, text }` (4.4). |

What Kodi needs that this design does **not** give, to be settled before Kodi is
extracted (tracked in the roadmap, not here):

- **IR overrides per key.** `activity.rs` lets a device send volume or D-pad by
  infrared instead (`try_device_ir`). That is a core feature layered *above* the
  device and keeps working for packaged devices if the generic controller calls
  it before `perform`; it must be re-tested, not redesigned.
- **Cinema activities** name a Kodi *source device*; `plugin_target` already
  resolves an activity source to a package, so this is wiring in PR 5.
- **Text entry** into Kodi's on-screen keyboard is outside this component (it is
  the "text input" capability in the roadmap's TV wave).
- **The `kodi-web.json` legacy settings file** read by `activity_art.rs` has to
  be folded into the converted package settings by the built-in-to-package
  converter.

### 8.1 Refusing before download

Not part of the wire protocol, but it belongs in this wave. The core learns a
package's protocol version only after downloading and unpacking it. With three
protocol versions live, "Sonos 0.2.0 is available" followed by "invalid or
incompatible integration manifest" on a protocol-2 core is a poor experience,
and it repeats on every refresh. The feed already knows each package's protocol
(`integration.json`); publishing it next to the index (for example a small
signed `compatibility.json` mapping `id -> version -> protocol_version`) lets the
core offer only versions it can run and say "needs a Couch update" for the rest.

---

## 9. Implementation plan

"Contract" marks a pull request that touches paths frozen by
`tools/release/tested-integrations.json` (`clients/couch-plugin`,
`clients/couch-sdk`, the listed `couch-confd` files, the listed `couch-model`
files). The evidence gate runs when a release is cut, not on each pull request,
so **all the contract pull requests below cost one renewal in total** provided no
release is cut between them. Until that release exists, documentation calls
protocol 3 "unreleased", as it did for protocol 2.

These are the *media* pull requests. In the roadmap's protocol 3 train the three
contract ones are steps T4 (pull request 1 here, minus the shared scaffolding,
which is T1), T5 (pull request 2 and the daemon half of 3) and T7 (the final
`PROTOCOL_VERSION = 3` flip, which pull request 1 must therefore *not* do on its
own: until T7 the host keeps refusing protocol 3 manifests, so a core release cut
mid-train still behaves as a protocol 2 core). Pull requests 4 to 7 touch no
frozen path and can proceed in parallel with the whole train.

| # | Pull request | Contract? | Tests |
| --- | --- | --- | --- |
| 1 | **model + plugin: protocol 3 types.** `PluginComponent::{MediaPlayer, VolumePercentControl}`, new `TypedAction`/`PluginActionSchema` variants, `Media`, `Reason`, `Selectable` additions, new `Request`/`Response` variants, manifest validation, host-side gating by manifest version (the `PROTOCOL_VERSION = 3` flip itself is the train's last step). `storage.rs`: `integration_config_v3` plus a `v2_projection` that strips media components, v3 actions and v3-only bindings. `validate.rs` rules. | **yes** | Unit tests for every validation rule and every refusal-before-I/O. Byte-for-byte golden tests that v1 and v2 manifests, requests and responses are unchanged. Rollback: the real `.177` source reads a config written by the new core (the check PR #194 did against `.171`), and an old-core save is authoritative after re-upgrade. |
| 2 | **plugin host + sdk: hints, serve loop, harness.** id-0 event frames, one-slot flags, lease, idle read in `Endpoint`; `serve` polls stdin and calls `changed`; `DeviceClient` gains the five optional methods. `testing.rs` gains `media(` and `hints(` cases; echo gains a fake player so the harness tests itself. | **yes** (plus the harness digest in `tested-integrations.json`) | Protocol tests: flood is fatal, silence until read, lease lapse, a hint during a pending response does not corrupt it, a dribbled event frame cannot outlive the deadline. Echo runs both new cases in CI. |
| 3 | **confd: media cache, watch, artwork, HTTP.** `plugins.rs` cache and watcher bookkeeping, chunk assembly and LRU, `watch` local request, `/media` and `/artwork` routes, re-read after commands, separate watch allowance on `plugin.sock`. | **yes** | Daemon tests with the echo fake player: no watcher means no device reads; a watcher on a non-event package is read every `refresh_ms`; LRU bounds; oversized, inconsistent or non-image artwork refused; a watch cannot starve a command. Product-flow test: HTTP and panel share one owner. |
| 4 | **gui: device-neutral player controller; built-in Sonos moves onto it.** No behaviour change intended. | no | The existing `music_player_screen_renders_and_its_controls_dispatch` test keeps passing. Add rendered 480x800 screenshots in the style of `COUCH_CORE_SCREENSHOTS` (`COUCH_PLAYER_SCREENSHOTS=<dir>`): playing, paused, idle, group member, each sheet, error. Commit nothing binary; CI uploads them as an artefact. |
| 5 | **gui: packaged backend and `opens_player`.** Room rows and shortcut keys open the player for a declaring package; held-volume coalescing uses `step_volume_percent` with an optimistic volume card; "Open Kitchen" for `not_coordinator`. | no | **Same fake, two backends:** drive `BuiltInSonos` and `Packaged` (real daemon, real package subprocess) from one fake Control API script and assert the rendered screenshots are identical. D-pad latency measured and recorded in the PR. |
| 6 | **web: `media_player` and `volume_percent_control` renderers.** | no | `web/tests/plugin-components.mjs` cases in Chromium, desktop and mobile; screenshots retained by the admission workflow. |
| 7 | **couch-sonos: protocol 3 adapter.** In `clients/couch-sonos` while built-in Sonos remains (it is the source the standalone repository is cut from), catalog entry updated, the limitations list in `integrations/catalog.json` shortened to what is still true. | no (`clients/couch-sonos` is not a contract path) | The four existing admission cases plus `media(` against `tests/fake/`; artwork served by the fake; member and no-content refusals carry their reasons. |
| 8 | **Release and feed.** Evidence renewal on the build host (simulated, merge commit, about 30 minutes), cut the core release, then in `couch-integrations`: `PROTOCOL_VERSIONS = {1, 2, 3}` with the release named in the comment, the two new literal case names required when a manifest declares `media_player`, optional `compatibility.json`. Then `couch-integration-sonos` 0.2.0 and the feed pin. | feed repo | Feed admission. **Read-only hardware check on the LAN players:** install the preview package on the dev remote build or a host daemon, open the player, confirm title/artist/art/position/group line against the Sonos app. No playback or volume writes while Bryan is away. |
| 9 | **Hardware sign-off, then retire built-in Sonos.** With Bryan present: transport, seek, modes, volume, source start, grouped member. Then the automatic converter takes over the built-in connections, as decided for Denon. | yes (model/daemon) - the wave's retirement release | See the roadmap for the retirement rules: keep the `couch-sonos` file name in the runtime bundle (deployed updater allow-list) and keep the `Provider::Sonos` variant parseable. |
| 10 | **Kodi package.** Lists sheets in the generic controller, `navigation`, video layout with logo, `couch-integration-kodi`. | no | Fake JSON-RPC server with notifications; same-fake-two-backends screenshots against built-in Kodi. |

Order and parallelism: 1 -> 2 -> 3 are sequential. 4 can start immediately and
in parallel with 1-3. 5 needs 3 and 4. 6 needs 3. 7 needs 2. 10 needs 5.

## 10. Open questions

1. Should `couch-confd` downscale artwork before handing it to the GUI? It would
   cut 4 MiB transfers to tens of kilobytes but puts an image decoder in the
   daemon. Proposed answer: no; measure first.
2. Multi-room control (join/leave a group, per-member volume) is deliberately
   absent. `group` is read-only. It would be a separate `group` component later.
3. `status` hints (so the control layout and room rows update live) are named in
   the topic list but no screen consumes them yet.
4. Android TV and Apple TV show now-playing text and artwork *on the TV layout*
   today (`tv_media.rs`, fed by Cast and AirPlay metadata). The `media` and
   `artwork` requests are deliberately independent of the player layout so a
   later `now_playing` component can reuse them for the TV wave without another
   wire change. That component is not specified here.
5. Whether the built-in Kodi screen should move onto the generic controller
   before or together with PR 10. Doing it first repeats the PR 4 trick and makes
   PR 10 smaller.

## Source references

- `ui/couch-gui/src/shortcuts.rs`, `sonos_player.rs`, `activity.rs`,
  `activity_art.rs`, `tv_plugin.rs`, `activity_buttons.rs`, `room_sonos.rs`
- `ui/couch-gui/ui/app.slint`, `ui/couch-gui/ui/screens/cinema.slint`
- `clients/couch-plugin/src/{protocol,manifest,host,server,testing}.rs`
- `clients/couch-sdk/src/{client,status}.rs`
- `clients/couch-sonos/src/lib.rs`, `clients/couch-kodi/src/playback.rs`
- `daemon/couch-confd/src/plugins.rs`, `daemon/couch-confd/src/api/plugins.rs`
- `model/couch-model/src/{connection,volume,storage,validate}.rs`
- `tools/release/tested-integrations.json`, `tools/release/verify_integration_set.py`
- `integrations/catalog.json` (the Sonos preview's limitations list)
