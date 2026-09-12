# The config web UI
## Configuration editor (September 2026)

The editor starts with **Overview**, then offers **Rooms & devices**,
**Connections**, **Areas**, **Scenes**, and **Activities**. Existing
`/areas/...`, `/rooms/...`, `/scenes/...`, and `/activities/...` links still work.
The configuration now includes saved connections; legacy inline device definitions remain readable.

### Set up a home

1. Open **Connections**, choose **Add a connection**, and select Kodi, Home Assistant,
   Philips Hue or Infrared. Save the player address, server credentials or bridge pairing.
2. Open **Rooms & devices**, create or open a room, then choose **From connection**.
   Search discovered HA/Hue lights or name a Kodi/infrared device and choose
   **Add to this room**. Infrared devices carry a codeset name.
3. Open **Activities** or **Scenes** to arrange device commands. Saving a command
   does not execute it; execution remains pending.
4. Open **Areas** to choose which rooms, activities and scenes appear
   together. Rooms always remain accessible in All rooms on the physical remote.

Rooms remain discoverable even when no area includes them. A room or scene can
appear on several screens without duplication. **Unlink** removes an area's
reference; **Delete** removes the entity and its dependent references. Room
removal also deletes its devices and room-bound activities.

Device edits are local drafts with explicit **Save device** and **Discard changes**
buttons. Connection settings are edited separately on Connections. Other names,
selectors and ordering save on change. Writes are serialized, controls disable
while saving, and the header reports Saved, Saving or Not saved. Requests send
the loaded revision in `If-Match`; a stale write reloads saved configuration with
an explanation instead of overwriting another browser's work. Ordinary
validation/network failures keep local drafts for correction or retry.

### Current product boundary

The Slint GUI reads saved rooms and area order and reloads changes while on its
home screen. Opening a room provides Home Assistant and Philips Hue light controls. Activities,
scenes and other device domains are still pending. The screen preview is a
configuration visualization, not a live screenshot or a free-form layout editor.
Server and bridge setup live in Connections; discovery, assignment and light
controls live inside rooms. See [Home Assistant lights](home-assistant.md).
Infrared learning/discovery and execution of configured scenes remain pending. The separate `stage2/www` portal handles Wi-Fi and SSH setup.

### Validation

- `(cd web && cargo fmt --check && cargo test)` checks formatting and tests route
  compatibility and device connection validation (including blank addresses,
  invalid/zero/overflowing ports and optional unconfigured devices).
- `(cd web && cargo check --target wasm32-unknown-unknown)` checks browser builds.
- `(cd web/couch-web && env -u NO_COLOR trunk build --release)` creates the bundle;
  unsetting `NO_COLOR=1` avoids Trunk's boolean CLI parsing error.
- Browser review uses a host daemon on loopback with throwaway configurations;
  it must not edit the physical remote's configuration during development.

The repeatable browser regression lives in `web/tests/browser.mjs`. It replaces
its test server's configuration, requires loopback and `--no-auth`, and refuses
other hosts. Install its isolated browser tooling and run a disposable server:

```sh
npm install --prefix build/webui-review playwright@1.57.0
build/webui-review/node_modules/.bin/playwright install chromium
daemon/target/release/couch-confd --addr 127.0.0.1:18092 --no-auth \
  --config build/webui-review-empty.json --www web/couch-web/dist
# In another terminal, from the repository root:
COUCH_TEST_URL=http://127.0.0.1:18092 node web/tests/browser.mjs
```

The browser flow covers keyboard room creation, invalid connection fields,
device save/discard and failed-save draft preservation, screen membership,
preview, unlinking and order, activity sources, scene commands, conflicting
browser revisions, seeded configurations and 360-pixel mobile overflow. All
checks passed in Chromium with no browser exceptions. Screenshots are written under ignored `build/webui-review/`.



How the house gets described: `couch-confd`, a static binary on the remote that
owns `/opt/couch/config.json`, and `couch-web`, the page it serves to a phone on
the same LAN.

> **A browser pairs with a four-digit PIN shown on the remote's own screen.**
> The credential is being able to see the panel - see [Pairing by a PIN on the
> remote's screen](#pairing-by-a-pin-on-the-remotes-screen), which also lists
> what this still does not defend against. Traffic is plain HTTP.

## Three crates, three targets

```
model/couch-model     the types, and the rules about them   armv7 + wasm32 + host
daemon/couch-confd    the HTTP server and the REST API      armv7 musl (and host)
web/couch-web         the page, as a WASM bundle            wasm32
```

`couch-model` is the reason the other two agree. `Config`, `Area`, `Room`,
`Device`, `Scene` and `Activity` are defined once, with their `serde`
derivations, and both sides import them: the daemon deserialises the file into
the same structs the browser edits, and the icon and device-kind pickers are
built from the model's own `ALL_ICONS` and `ALL_DEVICE_KINDS` rather than from a
copy that can drift. Renaming a field is a compile error on both sides instead
of a JSON key that silently stops matching.

It is `no_std` + `alloc`. Nothing in it needs to be - it is types, `Vec` and
`String` - but staying off `std` costs nothing and means the wasm build has no
reason to pull one in.

Each crate is its own cargo workspace, for the same reason `clients/` and `ui/`
are separate: one lockfile that has to satisfy `armv7-unknown-linux-musleabihf`,
`wasm32-unknown-unknown` and the host at once is a lockfile where one consumer's
dependency is the other's problem.

### The daemon is not a hub

`couch-confd` never talks to a light or a TV. It reads and writes one JSON file
and serves the page that edits it, which is what lets it stay a megabyte and why
a crash in it cannot take the remote's UI with it. It also does not replace
`stage2/portal.sh`: the setup portal answers "which network should this join",
runs on port 80 out of an AP the device hosts itself, and has to work before
there is any network at all. This answers "what is in your house", and needs one.

## Why these libraries

**tiny_http, not axum.** axum is the better library for a service and the wrong
one for this device. It arrives with tokio, hyper, tower and their transitive
halves - a reactor, a work-stealing scheduler and a connection pool - to answer
a handful of requests from one browser. tiny_http is blocking sockets and a
request queue, four dependencies deep; a fixed pool of four threads covers the
six connections a browser opens to one origin. It also contains no C, which is
what keeps the static musl cross-build a one-liner (`rust-lld` as the linker, no
cross-gcc, no Docker, no sysroot).

**Leptos CSR, not a JS framework.** The page shares `couch-model` with the
daemon, which is the whole argument: any JavaScript framework means maintaining
a second description of the same types, in a language where the compiler cannot
check it against the first. Leptos in client-side-rendering mode is the smallest
way to get that - no server half, no hydration, no islands, one wasm file and a
loader. The cost is measured below, and 387 KB of wasm is real; against it, the
config model has exactly one definition and the icon list on the phone cannot
disagree with the icon list on the device.

**Leptos' own signals, no router crate.** `leptos_router` would add a matcher, a
nested-route tree and a set of macros; `route.rs` is one enum, two string
functions and a `popstate` listener, with explicit routes for overview, catalogs and detail pages. The URL is
real - `/rooms/kitchen` reloads to that room - because on a phone Back means "up
one level", and without history entries Back leaves the app from the first
screen a user drills into.

**No CSS framework and no CDN.** The remote serves this over its own WiFi, which
may have no route to the internet at all. Everything is in the bundle: one
hand-written stylesheet, the system font stack, and an inline SVG favicon (a
browser asks for `/favicon.ico` on every load, and a 404 per load for want of
300 bytes is silly). The palette is `stage2/www/index.html`'s, so the setup
portal and the config UI look like the same device talking.

**The UI is baked into the binary.** `daemon/couch-confd/build.rs` walks
`web/couch-web/dist` and writes an `include_bytes!` table. A deploy is then
`scp` of one file, and the API and the page that calls it are versioned
together. Anything that has to exist on disk at a fixed path is something a
partial copy can leave half-present, and a half-present web root serves a blank
page with no error anywhere. A build with no `dist` is not an error - it is the
state of every fresh clone - and the daemon serves a page saying what to run.

## Two conventions hold the API together

**Every mutating response is the whole config.** A house is a few kilobytes. A
client that re-reads it after every edit cannot drift out of step with the
server, and the alternative - patching a local copy from partial responses - is
where "the room disappeared until I reloaded" bugs come from. The browser holds
exactly one piece of state, `RwSignal<Option<Config>>`, and every screen is a
pure function of it.

**Every response carries `X-Couch-Revision`.** The store increments a counter on
each accepted write. Mutations accept `If-Match: <revision>`; a mismatch is a
409 rather than a silent overwrite. Creates additionally answer with
`X-Couch-Created: <id>`, because the id is the one thing the client cannot work
out for itself.

Writes are validated before they are kept. `Store::mutate` applies the edit to a
*copy*, runs `Config::validate`, and only then replaces the in-memory config and
writes the file - temp file in the same directory, `fsync`, `rename`, `fsync` of
the directory. A rejected edit changes nothing at all, and a battery pull cannot
leave a present-but-empty config behind. A *corrupt* file at startup is an
error, not a reset: overwriting somebody's house because one brace is missing is
data loss with extra steps. A *missing* file is first boot and starts with an
empty configuration, without example rooms, devices, or connections.

## The API

Everything under `/api`; everything else is the page. Bodies and responses are
JSON. `{id}` is a slug like `living-room`.

| Method   | Path                                   | What it does |
|----------|----------------------------------------|--------------|
| `POST`   | `/api/auth/challenge`                  | put a PIN on the remote's screen; idempotent while one is showing. Unguarded |
| `POST`   | `/api/auth/verify`                     | `{pin}`; on success sets the session cookie. Unguarded |
| `GET`    | `/api/auth/status`                     | paired? pairing? seconds left, tries left. Never the PIN. Unguarded |
| `POST`   | `/api/auth/logout`                     | ends this browser's session. Unguarded |
| `GET`    | `/api/health`                          | service, version, schema version, config path, revision, embedded asset count |
| `GET`    | `/api/meta`                            | the vocabularies the editor builds its pickers from: icons, device kinds, integrations, activity kinds |
| `GET`    | `/api/config`                          | the whole document |
| `PUT`    | `/api/config`                          | replace it wholesale (the revision stays the server's to hand out) |
| `POST`   | `/api/config/reset`                    | back to the seed house |
| `GET`    | `/api/areas`                           | the areas, in order |
| `POST`   | `/api/areas`                           | `{name, icon?}` |
| `GET`    | `/api/areas/{id}`                      | one area |
| `PUT`    | `/api/areas/{id}`                      | `{name, icon?}` - name and icon travel together so saving one cannot clear the other |
| `DELETE` | `/api/areas/{id}`                      | the area only; its rooms, scenes and activities belong to the house |
| `PUT`    | `/api/areas/{id}/rooms`                | `["living-room", ...]` - replace the list, which is also how it is reordered |
| `POST`   | `/api/areas/{id}/rooms`                | `{room}` to attach an existing one, `{name, icon?}` to create and attach |
| `DELETE` | `/api/areas/{id}/rooms/{room}`         | detach; the room stays in the house |
| `PUT`    | `/api/areas/{id}/scenes`               | replace/reorder |
| `POST`   | `/api/areas/{id}/scenes`               | `{scene}` or `{name, icon?}` |
| `DELETE` | `/api/areas/{id}/scenes/{scene}`       | detach |
| `PUT`    | `/api/areas/{id}/activities`           | replace/reorder the activity strip |
| `POST`   | `/api/areas/{id}/activities`           | `{activity}` to attach, or `{name, room, kind?, source?}` to create and attach |
| `DELETE` | `/api/areas/{id}/activities/{act}`     | take it off this area's strip |
| `GET`    | `/api/rooms`                           | every room in the house |
| `POST`   | `/api/rooms`                           | `{name, icon?}` |
| `GET`    | `/api/rooms/{id}`                      | one room, with its devices |
| `PUT`    | `/api/rooms/{id}`                      | `{name, icon?}` |
| `DELETE` | `/api/rooms/{id}`                      | and every reference to it: area lists, its devices, activities anchored to it, scene steps naming those devices |
| `GET`    | `/api/rooms/{id}/devices`              | the room's devices |
| `POST`   | `/api/rooms/{id}/devices`              | `{name, kind?, icon?, integration?}` |
| `PUT`    | `/api/rooms/{id}/devices/{device}`     | the whole device; the path names it, so a body with a different id cannot move it |
| `DELETE` | `/api/rooms/{id}/devices/{device}`     | and every scene step and activity step pointing at it |
| `GET`    | `/api/scenes`                          | every scene |
| `POST`   | `/api/scenes`                          | `{name, icon?}` |
| `GET`    | `/api/scenes/{id}`                     | one scene |
| `PUT`    | `/api/scenes/{id}`                     | `{name, icon?, steps}` |
| `DELETE` | `/api/scenes/{id}`                     | and its references from every area |
| `GET`    | `/api/activities`                      | every activity |
| `POST`   | `/api/activities`                      | `{name, room, kind?, source?}` |
| `GET`    | `/api/activities/{id}`                 | one activity |
| `PUT`    | `/api/activities/{id}`                 | `{name, kind?, room, source?, steps}` |
| `DELETE` | `/api/activities/{id}`                 | and its references from every area |

Status codes, and what each actually means here:

| Code  | When |
|-------|------|
| `200` | including every mutation, whose body is the new config |
| `400` | unparseable body, or a create missing a field it cannot default (an activity with no room) |
| `404` | no such route, or the thing named is not there any more - the usual cause is a second phone that deleted it |
| `405` | a write to the web root |
| `409` | `If-Match` no longer matches |
| `413` | body over 512 KB. A whole house is a few KB; the cap is there so a stuck client cannot make a device with 1 GB and no swap allocate |
| `422` | the edit would leave the config invalid. The body carries `problems: [{at, message}]`, each naming a path like `areas[1].rooms[0]` |
| `500` | the write failed - a full partition |

Static assets: `GET`/`HEAD` only. Anything not found and without a file
extension falls back to `index.html`, because the frontend routes in the
browser and a reload on `/areas/kitchen` must reach the app. A path *with* an
extension that misses is a genuine 404 - serving HTML where a script was asked
for produces a syntax error in the console and nothing that says what really
happened. Trunk fingerprints its output, so those get
`Cache-Control: public, max-age=31536000, immutable`; `index.html` names them
and is `no-cache`.

```console
$ curl -s localhost:8090/api/health
{"config_path":"/opt/couch/config.json","embedded_assets":4,"revision":26,
 "schema_version":1,"service":"couch-confd","version":"0.1.0"}

$ curl -si -X POST localhost:8090/api/areas -d '{"name":"Loft"}' | head -4
HTTP/1.1 200 OK
Content-Type: application/json
X-Couch-Revision: 27
X-Couch-Created: loft
```

## Sizes, measured

Release builds, `opt-level="z"` and LTO for the wasm, `opt-level="s"` for the
daemon, both stripped. `wasm-opt -Oz` runs as part of the trunk build.

| Artefact | Bytes | gzip -9 |
|----------|------:|--------:|
| `couch-web-*_bg.wasm` | 428,746 (418 KB) | 170,300 |
| `couch-web-*.js` (wasm-bindgen loader) | 38,393 | 7,070 |
| `style-*.css` | 7,908 | 2,611 |
| `index.html` | 2,205 | 1,284 |
| **bundle total** | **477,252 (466 KB)** | **181,265 (177 KB)** |

`tools/build-webui.sh` prints this table at the end of every run. The last few
bytes wobble between builds because trunk's content hashes are not always the
same length, and `index.html` names two of them.

| Binary | Bytes |
|--------|------:|
| `couch-confd`, `armv7-unknown-linux-musleabihf`, static musl | 1,225,080 (1.17 MB) |
| `couch-confd`, host (macOS arm64) | 1,115,200 (1.06 MB) |
| `couch-gui`, same target, for comparison | 1,689,532 (1.61 MB) |

The ARM binary carries the whole bundle inside it, so the daemon is about
730 KB of code and 466 KB of page.

Pairing cost `couch-gui` 162 KB, which is not the PIN logic - it is one font
size. Glyphs are rasterised into the binary per size used, for the whole
charset rather than the ten characters the PIN needs, so the overlay's 48px
digits carry every other character at 48px with them. Drawing them at 72px, as
the first version did, cost 327 KB. The daemon does not compress responses: the
bundle would go over the wire at a third the size gzipped, which is the obvious
next saving if first load on the remote's own AP ever feels slow.

## Building and running, locally

```sh
tools/build-webui.sh          # frontend, then the armv7 daemon that embeds it
tools/build-webui.sh --host   # the same, for this machine
tools/run-webui.sh            # serve it on http://127.0.0.1:8090
```

`build-webui.sh` exists because the order matters and cargo cannot express it:
`build.rs` bakes `web/couch-web/dist` into the binary, so trunk has to have run
first or the daemon ships the placeholder page. It prints every artefact's size
at the end.

Prerequisites, once:

```sh
rustup target add wasm32-unknown-unknown armv7-unknown-linux-musleabihf
cargo install --locked trunk
```

`run-webui.sh` serves the page from `dist/` with `--www` rather than from the
embedded copy, so a CSS change is a `trunk build` away rather than a daemon
rebuild, and points the daemon at `build/couch-config.json` so the loop cannot
touch a real house. Both are overridable:

```sh
COUCH_CONFD_ADDR=0.0.0.0:8090 COUCH_CONFIG=/tmp/house.json tools/run-webui.sh
```

The daemon takes the same three settings as flags or environment
(`--addr`/`COUCH_CONFD_ADDR`, `--config`/`COUCH_CONFIG`, `--www`/`COUCH_WWW`);
`--help` prints them.

For the markup-and-CSS loop, `trunk serve` rebuilds on save and proxies `/api`
to a daemon you start yourself (see `web/couch-web/Trunk.toml`):

```sh
daemon/target/release/couch-confd --config /tmp/couch.json --addr 127.0.0.1:8090
cd web/couch-web && trunk serve      # http://127.0.0.1:8080
```

Tests: `cd model && cargo test` covers the model, its validation rules and the
seed; `cd daemon && cargo test` covers the store's atomic write, the revision
gate, path traversal and the asset fingerprinting.

## Deploying to the device

The binary is over a megabyte, so `tools/push.py` - 512 bytes a line over the
USB serial shell - is the wrong tool. Copy it over ssh, which means the device
is already on WiFi with sshd running (`couch-system` only starts it once a key
or password has been enrolled through the setup portal).

`sshd` runs inside the Alpine chroot, so `/opt/couch` in an ssh session is the
directory the initramfs sees as `/mnt/alpine/opt/couch`.

```sh
tools/build-webui.sh
IP=192.168.1.79                        # COUCH_IP in tools/screenshot.sh
KEY=~/.ssh/couch_dev
SSH="ssh -i $KEY -o IdentitiesOnly=yes"

scp -i $KEY daemon/target/armv7-unknown-linux-musleabihf/release/couch-confd \
    root@$IP:/opt/couch/couch-confd.new
$SSH root@$IP '
    killall couch-confd 2>/dev/null
    mv /opt/couch/couch-confd.new /opt/couch/couch-confd
    chmod 755 /opt/couch/couch-confd
    nohup /opt/couch/couch-confd </dev/null >/tmp/confd.log 2>&1 &
'
```

Copied beside its destination and moved into place, because a half-written
binary at the real path is what the next restart runs.

That sequence has been run against the HA100. What it looked like: assets served
with the right types and a real `Content-Length` (396446 for the wasm, byte-exact
on arrival), a create round-tripping to `/opt/couch/config.json` and coming back
after a restart at the revision it was left at, and 740 kB RSS alongside a
running `couch-gui` - the two do not contend for anything, since the daemon
touches neither the framebuffer nor the keypad. Then
`http://192.168.1.79:8090` from a phone on the same network.

Stage2 now starts the editor automatically; see Starting it at boot below.

## Pairing by a PIN on the remote's screen

The credential is being able to see the remote. Open the page, and four digits
appear on the panel; type them in, and that browser is paired for a week. There
is nothing to enrol ahead of time, no password to store, and nothing to reset
when it is forgotten - the same boundary the SSH enrolment button already draws,
and the reason a stolen config is a burglary rather than a port scan.

```
browser                      couch-confd                  couch-gui
   |  GET /  (page, unguarded)    |                            |
   |  POST /api/auth/challenge -> |  writes 0600 /tmp/couch.pin |
   |                              |                     reads it, draws it
   |  <- {pairing, expires_in}    |                            |
   |         (you read the digits off the panel)               |
   |  POST /api/auth/verify ----> |  constant-time compare     |
   |  <- Set-Cookie: couch_session|  deletes the PIN file      |
   |                              |                     overlay clears
   |  GET /api/config ----------> |  cookie checked            |
```

Four digits is ten thousand guesses, so the digits are not the defence. The
defence is that a guess costs an attempt against a challenge that dies:

* a PIN lives **120 seconds**;
* **five** wrong answers destroy it, and the right answer no longer works after
  that either;
* a new challenge is a new prompt **on the remote**, so brute force means making
  the panel in someone's living room flash a fresh PIN a thousand times over.

That visibility is the second feature. A PIN appearing when nobody opened the
page means somebody else on the network just tried.

Details worth knowing:

* **The PIN is never in an HTTP response.** `/api/auth/status` reports that
  pairing is in progress, how long is left and how many tries remain; the digits
  themselves exist only in the daemon's memory, in a `0600` file, and on the
  panel.
* **Asking twice does not reroll.** A reload or a second tab would otherwise
  change the digits halfway through someone typing them.
* **Sessions are in memory.** Restarting the daemon logs everyone out, which is
  the right way round: a config daemon holding sessions across a reboot is one
  that cannot be reset by turning it off and on again.
* **`SameSite=Strict; HttpOnly`** is the CSRF defence. A page on another origin
  cannot make the browser attach the cookie at all, so a form post from a
  hostile tab arrives unpaired. There is no `Secure` flag, because this is plain
  HTTP.
* **The page itself is unguarded**, because it has to load in order to ask for a
  PIN. It contains no house data.
* **`/api/health` is unguarded** and answers with less when nobody is paired -
  enough to say something is listening and what schema it speaks, not the config
  path or the revision count.
* **`--no-auth`** exists for a laptop with no remote to read a PIN off. It
  announces itself at startup in the loudest terms the log has. Never on a
  device.

### What this still does not do

* **Plain HTTP.** Anything on the path can read the traffic and lift the session
  cookie. On a home LAN with WPA2 that is a smaller problem than it sounds, but
  it is real. Use this editor on a trusted local network.
* **No rate limit on challenges.** Attempts against a PIN are limited; asking
  for new PINs is not. The cost is deliberate and physical - each one lights up
  the remote - rather than enforced in code.
* **Anyone who can see the panel can pair**, including through a window. That is
  the trust model, not a bug, and it is the same one the SSH button uses.
* **No audit trail.** Nothing records which session made which change.

### Starting it at boot

`stage2/stage2.sh` starts `stage2/confd.sh` inside Alpine after normal network
setup. Recovery mode skips it. The supervisor restarts the daemon after two
seconds and uses `flock` to prevent duplicate supervisors. Configuration stays
at `/opt/couch/config.json`; `/tmp/couch.pin` is shared with the Slint GUI.
PIN authentication remains enabled. Logs are in `/tmp/confd.log`.

For an immediate start after copying the helper to `/opt/couch/confd.sh`:

```sh
nohup /bin/sh /opt/couch/confd.sh </dev/null >/tmp/confd-supervisor.log 2>&1 &
```

The redesigned editor was deployed on September 8, 2026 at
`http://192.168.1.127:8090`. The existing configuration checksum was unchanged.
The embedded ARM bundle, unauthenticated health endpoint and enabled pairing
were verified; 17 daemon tests, three web tests and the browser regression passed.
The startup script passed shell syntax checks; no reboot was required to deploy.

## Known gaps

* **No compression.** See the size table.
* **Free-text commands.** A scene step's `command` is a string (`on`,
  `input:hdmi2`, `dim:30`) because what a device accepts is the integration's
  business. The editor suggests, and validates nothing.
* **Device ids stutter.** A device created as "Cellar light" in the Cellar
  becomes `cellar-cellar-light`: the id is built from the room's id and the
  name, which keeps ids unique and readable everywhere else.
* **No schema migration.** `SCHEMA_VERSION` is 1 and the daemon refuses a file
  from the future. There is no code to upgrade an older file, because there is
  no older file yet.

## What `couch-gui` would need

Not done here - `ui/couch-gui/` is untouched - but this is what connects it.

The GUI's hard-coded `areas` vector in `main.rs` is now shaped exactly like the
model: an `Area` with a name, its own `activities`, its `rooms` and its
`scenes`. That was the reconciliation this work had to make. `couch-model`
originally derived an area's activity strip from room membership, which cannot
reproduce the mock-up - WHOLE HOME shows three activities in rooms that host
five, DOWNSTAIRS shows five across three rooms, and no derivation gives both.
An area now *lists* its activities the way it already listed its scenes, and
`Config::seed()` reproduces the mock-up's counts exactly: 3/1/5/0 activities and
5/3/3/4 scenes, asserted by a test in `model/couch-model/src/lib.rs`.

To render a real config, `couch-gui` would:

1. Take `couch-model` as a dependency (it builds for armv7 already; the GUI is
   the third consumer the crate was written for) and read
   `/opt/couch/config.json` at startup, falling back to `Config::seed()` when
   there is no file - which is what it effectively shows today.
2. Build each `RoomRow` from a `Room`: `device_summary()` gives `"5 devices"`
   and `device_detail()` gives `"Kodi, Hue, LG C3"`, which are the two strings
   the mock-up hard-codes. `effective_icon().glyph_index()` gives the `glyph`
   field, which is why the config stores an icon *name* and not the index -
   inserting one icon into `tools/mkuiicons.sh` would otherwise renumber every
   room in the file.
3. Build each `SceneCell` from `Config::scenes_in_area`.
4. Build the activity strip from `Config::activities_in_area`, which returns the
   configured activities in the area's own order. `LiveActivity`'s `title`,
   `source` and `place` are *runtime* state - "Paused - Andrei Rublev" is not
   something anybody configures - so those come from whatever hub daemon
   eventually tracks what is playing. The configured `Activity` supplies the
   name, the room, the source device and `kind.glyph_index()`; `active` on a
   `SceneCell` is the same kind of live state.
5. Reload when the file changes, so an edit on the phone appears on the remote
   without a restart. `couch-confd` writes by rename, so a watch on the
   directory is the reliable form.

The counts in the seed are what make step 4 checkable: a first cut can be held
against the screen it replaces.

## Connections and room devices

**Connections** manages named Kodi players, the Home Assistant server, the Philips
Hue bridge, and the built-in infrared transmitter. Add a connection by type; edit
its server or pairing settings there. This page has no device assignment or light
controls. Kodi supports multiple connections; HA, Hue and infrared each support
one. Infrared sending remains unavailable on the current production kernel.

**Rooms & devices** creates rooms and assigns devices from saved connections.
Open a room, select **From connection**, then search discovered HA/Hue lights or
name a Kodi/infrared device. Infrared codesets belong to devices. Light controls
appear on assigned devices. Credentials and server addresses never appear in the
room creation flow.

Connections are non-secret records in `config.json`. New devices reference a
connection ID and resource ID; changing a Kodi connection updates all references.
The model rejects deleting an in-use connection. Removing a connection retains
private HA/Hue credentials so **Use saved connection** can restore it without
another pairing. Existing inline device configurations remain readable. On first
upgrade, existing HA/Hue private settings are adopted as saved connections.

Tests: `node web/tests/hue.mjs`, `node web/tests/home-assistant.mjs`, and
`COUCH_TEST_URL=http://127.0.0.1:PORT node web/tests/browser.mjs` against disposable
host daemons. The latter covers Kodi/IR setup, room assignment, shared settings,
removal protection, drafts, screen ordering and stale edits. See
[Philips Hue setup](philips-hue.md) for certificate pinning and real-device status.

## Remote accent color

Open **Areas → Appearance**. Choose Purple, Teal, Blue, Rose or Orange,
or use the custom picker / `#RRGGBB` field. The dark preview is local until
**Save appearance**; **Discard color changes** restores the saved value. The
remote updates its shared accent and recessed tint within the next configuration
poll (about one second), including open overlays. The choice persists in
`config.json` as `appearance.accent`. Existing configurations retain their original
orange until changed. `PUT /api/appearance` validates the color and uses the same
revision guard as other edits; it cannot overwrite room or connection changes.

Room lists on the physical remote render saved device names before querying
Home Assistant or Hue. Status refreshes run in the worker and update rows in
place without resetting D-pad focus. A short navigation cache does not replace
live validation before commands.

## Icon catalog and room activities

The Icon control opens a searchable visual grid of all 2,077 icons from the
pinned Lucide 1.43.0 catalog. It renders 60 choices at a time; Show more loads
another page. Room, area and scene choices save immediately. Device icons are
part of the Edit device draft and use Save device / Discard changes. Automatic
restores the device or room default. All SVG previews are served locally.

The remote renders the same selected room/device icons from a compiled 24px
alpha atlas; no runtime SVG decoder or React dependency is needed. Attribution,
version and regeneration instructions are in assets/lucide/README.md.

The navigation label is Areas. Rooms now include an Activities section to
create an activity in that room, edit it, or move an existing activity there.
An activity has one owning room; moving it preserves its existing area
shortcuts. Activity execution on the physical remote remains a separate,
unimplemented integration; these controls configure ownership/source/steps.

Regression: node web/tests/icons.mjs tests icon previews, search, persistence,
mobile layout and room activity ownership with an isolated local daemon.

## Connection and remote settings updates

See [Connections](connections.md) for multiple bridges/servers/TVs and private Kodi
credentials, and [Remote settings](remote-settings.md) for timezone, clock format
and the docked clock display.

Runtime update channels and publishing: [runtime-updates.md](runtime-updates.md).
