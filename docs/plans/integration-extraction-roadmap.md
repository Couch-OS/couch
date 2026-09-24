# Integration extraction roadmap

Status: **plan, not a schedule.** Written against `dev` at `94f8a3d`
(2026-09-19). It supersedes the wave list at the end of
[integration separation](../integration-separation.md), which was written
before protocol 2 shipped, before the pluggable fake device landed, and before
the decisions to auto-convert Denon and to bake the Sonos key in at publish
time.

## Summary in plain English

**The goal.** Every integration (Sonos, Kodi, the TVs, Hue, Home Assistant...)
leaves the main Couch repository and becomes its own small package with its own
repository, installed and updated from the feed without waiting for a Couch
release. Denon already works this way.

**Why it cannot all happen at once.** A package is a separate small program that
Couch talks to through a narrow, fixed set of questions: "do this command",
"what is your status", "what are your inputs", and (since protocol 2) "set the
volume to this many dB". Anything outside those questions - pairing with a TV or
a Hue bridge, one bridge that is really forty lights, album art, a brightness
level - has no way to cross that boundary yet.

**Where things stand today (2026-09-19).**

- **Denon**: a package. The built-in copy is being removed and old connections
  will convert themselves.
- **Sonos** and **Kodi**: preview packages now exist in their own repositories
  (`couch-integration-sonos`, `couch-integration-kodi`), but only as *simple
  remotes*: buttons, volume, a source list. The rich player screen still needs
  the built-in code, so the built-ins stay for now.
- **Hue**: its client has its own repository, but **no package is possible yet**.
  Without pairing and "many lights behind one bridge" it would be a toy.
- Everything else is still built in.

Three separate pieces of work (Sonos, Kodi, Hue) each wrote down what they are
missing. The lists overlap heavily and every item lives in the same small set of
"frozen" files. So the recommendation is:

**One big addition to Couch - "protocol 3" - built as one train of seven steps
and released once.** Then integrations move out in waves, with no further change
to the frozen files until live camera video.

| Step | What it adds to Couch | Who is waiting for it |
| --- | --- | --- |
| T1 | Groundwork: version 3 plumbing, safe rollback to an older Couch, richer error messages, packages naming a few buttons of their own | everyone |
| T2 | **Many devices behind one connection**, and a proper **light** control (on/off, brightness; colour later), scenes, plus blinds and thermostats | Hue, Home Assistant, cameras |
| T3 | **Pairing** (press the button / approve on the TV / type the code) with the secret kept safely by Couch; packages that stay connected; packages separated from each other | Hue, all four TVs |
| T4 | **Media player**: now playing, artwork, seek, shuffle/repeat, 0-100 volume, extra lists, hold-a-key | Sonos, Kodi |
| T5 | **Staying up to date**: cheap "what changed since last time", and a "something changed" nudge | Hue, Kodi, Sonos |
| T6 | **Finding devices on the network** and **app lists** | the TVs (nice-to-have for Hue, Sonos, Kodi) |
| T7 | Switch protocol 3 on, extend the admission tests, renew the evidence once, release, let the feed accept it | - |

Then the packages:

| Wave | Who moves | Why this order | Size |
| --- | --- | --- | --- |
| 0 (now) | Denon finishes. Two small fixes that need no protocol change (see below). | - | S |
| 1 | **Sonos** in full, **Kodi** in full, **Hue** | Designs are finished; Sonos and Kodi clients are proven on real devices; Hue is proven for pairing and reading | L |
| 2 | **LG webOS**, then **Android TV**, **Samsung Tizen**, **Apple TV** | Four integrations share pairing; LG is the only one fully proven on a real TV, so it leads | L |
| 3 | **Home Assistant**; **UniFi Protect** with still pictures | Builds on what Hue proved; Home Assistant needs the voice decision first | L |
| later | Live camera video (would be "protocol 4") | A different kind of data; nothing else needs it | XL |
| never | **Infrared, Bluetooth, voice**. **Matter** stays built in for now. The **Echo** sample stays as the developer template. | They need the remote's own hardware, or hold the keys to the house | - |

**What it costs besides the work itself.** Couch keeps a signed record that says
"this exact core was tested with this exact Denon package". That record freezes
the files that define the package boundary. Any change to them means the record
must be renewed before the next release: about 30 minutes, simulated on the build
host, no remote or speaker needed. The record is checked **whenever a build is
made for a remote (a release or a dev build), not on every pull request**. So the
whole train costs **one renewal** only if nothing is built for a remote from the
middle of it. Each build made mid-train (a dev build to try something on the test
remote, or an urgent release) still works (protocol 3 stays switched off until T7)
and costs one extra renewal, not an extra protocol version.

**Two rules that must never be broken** (both have already cost reinstalls):

1. The file names `couch-sonos` and `couch-coreelec` must stay in every runtime
   update, even as empty stubs, until the oldest remote still in use has been
   moved forward. Old remotes refuse an update that lacks them.
2. Couch must always be able to *read* an old saved Sonos, Kodi, Hue...
   connection, even after the code that drives it is gone. If it cannot, the
   settings service does not start at all.

**Decided (Bryan, 2026-09-19).** Every recommendation below was accepted:

1. **One protocol 3 train**, T1-T7, switched on once at the end.
2. **Release now, then hold**: an alpha is cut from `dev` just before T1 (it
   carries the Denon removal); no alpha until T7; an urgent fix goes out from a
   branch off the pre-train commit. Dev builds for the test remote continue.
3. **Matter stays built in**; revisit after it has worked on hardware.
4. **Voice gets its own "assistant address and token" setting**, so Home
   Assistant is free to become a package in wave 3.
5. **CoreELEC**: the Kodi half converts to the Kodi package; the SSH
   reboot/shutdown actions stay a small built-in Couch feature.
6. **A built-in is removed in the first release after its package passed a
   hands-on check on a real device**, with automatic conversion. Tizen and Apple
   TV, never proven on hardware, may convert as soon as their package exists.
7. **Power by infrared becomes a Couch feature above any package.**
8. **Stable opens on evidence**: each integration repository carries an evidence
   file and the feed refuses a stable package without one.
9. **One user id per package**, in T3, before any package stores a secret.

The questions as they were put:

**What had to be decided.**

1. **One train or three?** Recommended: one protocol 3 covering Sonos, Kodi, Hue
   and the TVs, landed in the order T1-T7, one renewal. The alternative (media
   first as protocol 3, pairing as 4, lights as 5) gets Sonos out a few weeks
   sooner and costs three renewals, three rollback layers and three versions for
   outside authors to track.
2. **Hold Couch releases during the train?** Recommended: cut a release just
   before T1, then none until T7. Urgent fixes in between go out from a branch
   off the pre-train commit.
3. **Matter: keep it built in?** Recommended yes. It holds the keys to the
   house's Matter fabric, has never paired a real device from the remote, and
   pulls in a large library. Revisit after it has worked on hardware.
4. **Home Assistant and voice.** Voice sends the microphone to "the first Home
   Assistant connection" and reads its address and token directly. If Home
   Assistant becomes a package, it cannot. Recommended: give voice its own small
   "assistant address and token" setting before wave 3.
5. **CoreELEC.** It is Kodi plus "log in over SSH and reboot the box".
   Recommended: the Kodi half converts to the Kodi package; the SSH half stays in
   Couch (a package cannot hold a private key file or run `ssh`) or is retired.
   It has never been used against a real CoreELEC box.
6. **When does a built-in get removed?** For Denon you chose "immediately, with
   automatic conversion". Recommended rule for the rest: the built-in is removed
   in the first release **after** the package has passed a hands-on check on a
   real device. For integrations that have *never* worked on real hardware
   (Samsung Tizen, Apple TV) there is nothing proven to lose, so they can convert
   as soon as their package exists.
7. **LG power by infrared.** The LG integration turns the TV on and off with the
   remote's infrared blaster by default. A package cannot touch the blaster.
   Recommended: "power this device by infrared instead" becomes a Couch feature
   that sits above any package (Couch already does this per key for Kodi).
8. **When does the "stable" channel open?** Today everything is "preview". The
   feed only lets a "production" package into stable, but nothing checks that a
   production label has evidence behind it. Recommended: a small evidence file in
   each integration repository that the feed insists on (section 5, step 9).
9. **Separate the packages from each other** (part of T3). Today all packages run
   as the same unprivileged user, so in principle one could peek at another's
   memory. Harmless while packages hold only an IP address; not harmless once
   they hold TV pairing keys or a Hue bridge key. Recommended: one user id per
   package.

The rest of this document is for implementers.

---

## 1. What a package can do today

Read from `clients/couch-plugin/src/{protocol,manifest,host}.rs`,
`clients/couch-sdk`, `docs/development/{protocol,components,admission}.md`.

- **Six requests**, strictly request/response, the child never speaks first:
  `hello`, `configure`, `command { function }`, `action { set_volume_db }`
  (protocol 2), `status`, `inputs`.
- **Status** is seven optional fields: `on`, `muted`, `volume` (0-100),
  `volume_db`, `input`, `playing`, `title`.
- **Errors** are nine fixed codes with no text.
- **Settings** are typed fields (`text`, `secret`, `integer`, `boolean`) sent
  host to package, each one line of at most 4096 bytes. Nothing flows back: a
  package cannot hand the host a credential to store, and a pinned certificate
  or private key has nowhere to live.
- **Screen**: five declarative components (`command_group`, `status_text`,
  `toggle`, `input_selector`, `volume_db_control`). On the remote a packaged
  device opens the core control screen (`ui/couch-gui/src/tv_plugin.rs`): power
  tile, current source, source list and command tiles. A package with the full
  standard television capability profile uses the native TV hero and transport
  row and receives the physical D-pad; incomplete profiles retain the generic
  tile navigator. The volume, mute and power keys work from the room row
  (`activity_buttons.rs`), and each command is followed by a `status` read.
- **One connection is one device.** `Integration::Plugin` has a `resource_id`
  field, but no request carries it.
- **The sandbox** (`Host::spawn_with_policy`): environment cleared, cwd `/`,
  stderr discarded, uid/gid 65534, `no_new_privs`, only the `AID_INET` group so
  it can open ordinary sockets. It can reach the LAN and the internet, send UDP
  multicast and broadcast, and nothing else: no `/dev` nodes, no D-Bus, no files
  under `/opt/couch` (root-owned), no `PATH`, and no helper program such as `ssh`
  or `ffmpeg` that a package may rely on being installed. All packages
  share that one uid. The code comment is honest about it: "privilege separation,
  not a sandbox".
- **Lifetime**: one child per connection, started on first use, killed on any
  protocol or transport error, reaped after 60 s idle. Limits: 3 s startup, 12 s
  per request, queue of 8 with a 750 ms lifetime, 64 KiB frames, never retry a
  command.
- **Admission**: four literal cases (`conformance`, `failure`,
  `timeout_no_retry`, `spike`) plus the offline concurrent-startup test, run
  against the package's own fake device. The fake can be any transport since the
  `FakeDevice` trait landed (Sonos uses loopback HTTP), provided it is reachable
  through *ordinary settings* - no test-only trust bypass in shipping code.

## 2. Survey of the built-in integrations

Sizes are lines of Rust under `src/`. "GUI direct" means the GUI process opens
its own socket to the device; "broker" means it goes through `couch-control`
(`control.sock`, one network owner per endpoint shared by GUI and daemon).

### 2.1 Media and TV

| | What it does on the remote | Network and system needs | Pairing and stored credentials | Live state | Size, notable dependencies | Hardware status |
| --- | --- | --- | --- | --- | --- | --- |
| **Sonos** `couch-sonos` | Full player screen (`sonos_player.rs`): art, track, seek, sources, shuffle/repeat/crossfade, up next, group awareness. Room-row keys (`room_sonos.rs`): volume with held-key coalescing, mute, skip, source picker. Web: address form + test controls. | HTTPS to the player on 1443 (certificate not verifiable: Sonos' device CA is in no trust store). Artwork over plain HTTP 1400 or the music service. mDNS `_sonos._tcp` discovery exists but only the CLI calls it. **GUI direct.** | None. One household **API key**: env, per-connection setting, `/opt/couch/sonos-api-key`, or the **build-time key**. Decided: packages get it baked in at publish time from a feed secret. | Polled: every 3 s while the player is open and after each command. No events. | 3026; `ureq`, `rustls`. Already has `DeviceClient`, `plugin.json`, package binary and a fake Control API. Preview package: `couch-integration-sonos` (protocol 1, simple remote). | **Client validated on real players** (four-player household, firmware 97.1; re-validated 2026-09-13). The *package* has never run on one. |
| **Kodi** `couch-kodi` | Full player screen (`activity.rs`, "Cinema"): backdrop and logo art, title, seek, chapters / audio / subtitle sheets, D-pad passthrough with contextual OK. Activities name a Kodi source. Web: address + optional web-server credentials. | JSON-RPC over TCP 9090 (persistent, pushes notifications) or HTTP 8080; artwork from the web port with Basic auth. No TLS. **Broker.** | None on TCP. Optional web user/password in `kodi-connection.json`; legacy `kodi-web.json`. | **Push** notifications plus a 5 s poll; HTTP mode polls only. | 1980; serde only. No `DeviceClient` in the main repository; the preview package `couch-integration-kodi` (protocol 1, simple remote) has one. | Built-in validated against a physical CoreELEC player (playback, art, chapters, volume). Preview package checked read-only against two real Kodi 22 boxes. |
| **CoreELEC** `couch-coreelec` | No GUI screen of its own: on the remote it *is* Kodi. Web only: SSH enrolment, OS status, reboot / power off / restart Kodi. | Kodi as above, plus **spawns the system `ssh` program** with a private key and `known_hosts` file; SSDP discovery (CLI only). Daemon only. | Pasted SSH private key + verified host key under `connections/<id>/coreelec-ssh-*/`. | On demand. | 632. **`couch-coreelec` is a required file name in the deployed updater.** | "No physical CoreELEC device has been contacted" for the OS half. |
| **LG webOS** external `couch-integration-webos` package | Native package TV screen: D-pad, volume, inputs, apps and playback. | `wss://` 3001 with a pinned certificate; package-owned SSAP/pointer transport. Core IR remains available as a device transport. | Approve a prompt on the TV; package credential stores client key + certificate. Old `webos-connection.json` converts automatically and remains for rollback. | Package status/inputs/apps through the protocol host. | Removed from the core image. | **Extracted and validated on a physical TV** (pairing, status, inputs, apps, physical and touchscreen controls, power off). |
| **Android / Google TV** `couch-androidtv` | Shared TV screen (`tv_android.rs`); now-playing text, art and timeline from Cast (`tv_media.rs`); app tray from *configured* shortcuts (no app list). | Mutual TLS 6466/6467 with its own RSA identity, protobuf; **must answer keep-alives at least once a second**. Separate read-only Cast connection on 8009 (**GUI direct**). mDNS `_androidtvremote2._tcp` browsed by the daemon. **Broker** for control. | Six-character code shown on the TV, two-step (`pair-start`, `pair-finish`); stores TV certificate + own certificate and private key in `androidtv-connection.json`. | Remote state polled every 4 s; Cast status is a true push stream. | 1973; `prost`, `rustls`, `rcgen`, `rsa`, `x509-parser` (largest dependency set of the TVs). | Partly validated (Xiaomi MiTV: pairing, status, a few keys; Cast metadata seen live). Long-running keep-alive not proven. |
| **Apple TV** `couch-appletv` | Shared TV screen (`tv_apple.rs`), app launch by bundle id, sleep/wake; now-playing text from AirPlay metadata (`tv_media.rs`, no artwork). | Companion protocol (HAP: SRP + X25519/Ed25519 + ChaCha20, no TLS) on an advertised port; a second AirPlay 2 connection with three encrypted channels (**GUI direct**). mDNS `_companion-link._tcp`, `_airplay._tcp` via the daemon. **Broker** for control. | Four-digit PIN on the TV, **twice** (control and metadata); two credential files. | Control polled every 4 s; metadata is a real subscription. | 3404; nine crypto crates, `plist`, `prost`. | **Never paired with a real Apple TV.** |
| **Samsung Tizen** `couch-tizen` | Shared TV screen (`tv_tizen.rs`): keys, six fixed inputs, app list and launch, Frame TV power. | `wss://` 8002 pinned, legacy `ws://` 8001; plain REST on 8001; SSDP discovery (daemon calls it); Wake-on-LAN. **Broker.** | Allow/Deny prompt on the TV; stores token, certificate, MAC, model in `tizen-connection.json`. | No events; polled every 4 s. | 1422; `tungstenite`, `rustls`. | **Written without a Samsung TV; never paired.** |
| **Denon** `couch-denon` | Core control screen as a package; room-row volume keys with dB. | Line protocol on TCP 23. | None. | On demand, only values not seen lately. | 1015. Package since 0.1.1; protocol 2 in 0.2.x. | Read-only status and inputs on a real receiver; commands outstanding. |

### 2.2 Home, cameras, hardware

| | What it does on the remote | Network and system needs | Pairing and stored credentials | Live state | Size | Hardware status |
| --- | --- | --- | --- | --- | --- | --- |
| **Philips Hue** `couch-hue` | Light rows in the room list (`lights.rs`, `room_devices.slint`): on/off and brightness with optimistic targets; grouped rooms; scenes from a room's Scenes button (`scenes.rs`); shortcut-key toggles. **No colour or colour-temperature UI exists.** One bridge is many devices: a real bridge read during the Hue work had 48 lights, 14 rooms, 181 scenes. | HTTPS to the bridge, certificate pinned on first pairing. No discovery: the bridge address is typed. **GUI direct** (`HueFleet`, one live session per bridge) and daemon. Depends on `couch-ha` only for the shared `Light`/`Command` types. | Press the bridge's link button; stores URL, application key and the pinned certificate in `hue-connection.json`. | **Server-sent event stream per bridge**, 60 s reconcile while streaming, 5 s poll as recovery, 2 s settling window after a write. GUI reads that local cache every 500 ms. | 1481; `ureq`, `rustls`. No `DeviceClient`. Client now also lives in `couch-integration-hue` (no package adapter yet). | Pairing and read-only discovery on a real BSB002 bridge; **commands to real lights still pending**. |
| **Home Assistant** `couch-ha` | Lights and covers in room rows, thermostat screen (`thermostat.rs`), entity picker in the web UI. **Voice streams the microphone to the first Home Assistant connection's Assist pipeline.** | REST only (no WebSocket in this crate). **GUI direct** and daemon. | Long-lived access token typed in; `ha-connection.json`. No discovery. | **Polled**: `/api/states` every 5 s from the GUI. | 1391; `ureq`. No `DeviceClient`. | Fake-server fixtures on the physical remote; **a real Home Assistant and a test light are still needed**; covers and climate never exercised on hardware. |
| **Matter** `couch-matter` | On/off and brightness in room rows and scenes. Web: commission with a pairing code, list devices. | One UDP socket plus mDNS (IPv6 multicast, shares port 5353); the remote **is the fabric administrator**. No Bluetooth commissioning, no Thread. A private `tokio` runtime. | Manual pairing code; stores the fabric's CA and controller keys, node addresses and inventory under `connections/<id>/matter/`. Device attestation is not checked. | None: every read opens a session; no subscriptions. | 906 + the `matc` library. | **Simulated light on a Mac only; never run on the HA100**, whose IPv6 multicast is unproven. |
| **UniFi Protect** `couch-unifi-protect` | Camera screen (`camera.rs`, `camera.slint`): one live view, latest frame only, 60 s view limit. | HTTPS REST with an API key (public roots, private CA or a leaf pin); **RTSPS with SRTP** video, depacketised in Rust and decoded by a **`/usr/bin/ffmpeg` subprocess** under resource limits, frames pushed to the GUI at 480x270. | Console origin + Integration API key in `protect-connection.json`. Cameras listed from the console. | None; on demand. | 2428; `webrtc-srtp`, `webpki-roots`. | **No real NVR was contacted**; synthetic video only; HA100 decode performance unproven. |
| **Infrared** `couch-ir` | Codesets per device (embedded catalog of 5,477 models), per-key overrides for other integrations, LG power. No learning. | **`/dev/irtx`** (kernel driver), root; codesets under `/opt/couch/ir/`. | None. | None; one-way. | 2218; `libc` only. | **Working on the remote** since the 2026-09-10 kernel (LG volume, power, held repeats). |
| **Bluetooth** `couch-bt`, `couch-bt-hid` | The remote acts as a Bluetooth keyboard/remote to a TV; pairing overlay; one active link follows the on-screen device. It is a *transport of a device*, not a connection. | `/dev/vhci` and `/dev/stpbt` bridge, D-Bus to a **patched BlueZ**, raw HCI advertising, its own daemons started by `couch-system`. Root. Ships in the boot ramdisk, not the runtime bundle. | Bonds live in BlueZ; Couch stores address and name on the device. | State file polled by the GUI. | 738 + 2458; `zbus`, `tokio`. | **Working with a real TV** (pairing and volume keys). |
| **Voice** `couch-voice` | Hold the mic key: dictation into the on-screen keyboard, or Assist intent and reply. Not an integration: no connection of its own. | **ALSA capture** straight to the kernel ABI, keypad events; plain `ws://` to Home Assistant. No daemon endpoint can start a recording, by design. | Reuses the first Home Assistant connection's URL and token (`connections.rs::ha_assist`). | One WebSocket run per key press. | 5702; `libc`, `serde_json`. | Microphone path measured and corrected on hardware; full run pending. |
| **Echo** `couch-echo` | Nothing: fictional TV used to test the SDK and host. The only user of the SDK's `Discover` trait. | Loopback only. | - | - | 311. | Not applicable. |

Only three crates implement `couch_sdk::DeviceClient` and have a `plugin.json`
and a package binary: Echo, Denon and Sonos. Every other row above is a library
linked into `couch-confd`, `couch-gui` or both.

`Provider` in `model/couch-model/src/connection.rs` lists every connection kind:
`CoreElec`, `Sonos`, `Kodi`, `Denon`, `HomeAssistant`, `Hue`, `WebOs`,
`AndroidTv`, `AppleTv`, `Tizen`, `BluetoothTv`, `UnifiProtect`, `Matter`,
`Plugin`, `Ir`. `Integration` in `device.rs` mirrors it per device and adds
`None` and `Connection`. There is nothing in either enum that the tables above
do not cover.

## 3. The missing capabilities

Named here so the matrix can refer to them. Each is a change to the core
(protocol, host, daemon, GUI and web), never something a package author can work
around. Three of today's sibling efforts wrote their own lists; this table is the
union, and the designs are referenced rather than repeated:

- **M1-M5, E1, K1, K2, F1**: [media player component design](media-player-component-design.md),
  which answers the Kodi preview's
  [gap list](https://github.com/Couch-OS/couch-integration-kodi#road-to-replacing-the-built-in-integration)
  item by item and the Sonos preview's findings.
- **P1, P2, P3, P5, R1, R2 (lights), R3**: the Hue work's
  [`docs/protocol-needs.md`](https://github.com/Couch-OS/couch-integration-hue/blob/main/docs/protocol-needs.md):
  `PairStart`/`PairContinue`/`PairCancel` with `PressButton` / `ApproveOnDevice`
  / `EnterCode` prompts and an opaque host-stored credential handed back in
  `Configure`; paged `Children` and `resource` on `Command`/`Action`/`Status`;
  `TypedAction::SetLight`; cached `States { since }`; `keep_alive`; daemon-owned
  mDNS with `Probe`. It was written to be generic across Hue and the four TVs and
  this roadmap adopts it. Where this roadmap disagrees (who polls, and whether the
  unprompted "changed" frame waits for a later protocol) is set out in the media
  design, section 5.5.
- **P4, P6, R2 (covers, climate), V1, S1**: not designed anywhere yet; sketched in
  section 4.

| Id | Capability | What is missing today |
| --- | --- | --- |
| **M1** | Now-playing state | Title/artist/album, duration, position, play state, "what is allowed right now". `Status.title` is one free-text field. |
| **M2** | Artwork | No way to move a picture. |
| **M3** | Seek, play modes, percent volume | Only `set_volume_db` exists as a typed action. `Status.volume` can be read but not set. |
| **M4** | Extra lists with a second line and a "current" marker | `Selectable` is id + name; only one list (`inputs`). Needed for favourites, queue, chapters, audio and subtitle tracks. |
| **M5** | Richer errors | Nine codes, no detail: cannot say "open the coordinator". |
| **E1** | Change hints (plugin speaks first) | Strictly request/response. Needed for Kodi, Hue, Cast/AirPlay metadata; optional for Sonos. |
| **P1** | Pairing conversation | No multi-step exchange: start, show "enter the PIN / press the button / approve on TV", finish, cancel, time out. |
| **P2** | Credential write-back | `configure` is one-way. Pairing produces tokens, certificates and private keys (some binary, some several kB) that the host must store privately and hand back next time. |
| **P3** | Discovery | The SDK's `Discover` trait is used by nothing in production. The daemon browses mDNS itself for Android TV and Apple TV and calls Tizen's SSDP scan; Sonos and CoreELEC can scan but only from a CLI; Hue, webOS, Kodi, Home Assistant and Protect addresses are typed by hand. No request lets a package take part. |
| **P4** | Apps | No `apps` request; `manifest.rs` rejects `app:` capabilities outright. |
| **P5** | Stay-connected sessions | The child is reaped after 60 s idle and started lazily. Android TV must answer keep-alives every second; pairing must survive between two HTTP calls; pinned WebSockets are slow to reopen. |
| **P6** | Text entry | Not implemented by any built-in either, so not needed for parity. Listed so it is designed with P1, which needs the same "ask the user for a short string" UI. |
| **R1** | Many resources per connection | No resource list, no `resource_id` on requests, no per-resource status. |
| **R2** | Typed domain controls | Light: on, brightness (colour and colour temperature have no UI today, so they are not needed for parity). Scene: activate. Cover: position. Climate: target, mode. None can cross as a `function` string with a value. |
| **R3** | Per-resource live state | E1 generalised: "light 7 changed". |
| **V1** | Camera pictures | Snapshot: reuse M2. Live video: a byte stream the 64 KiB JSON frames cannot carry. |
| **K1** | Hold, repeat, long-press | `Request::Command` is a bare id; a package cannot tell a press from the 70 ms key repeat, and Kodi's long-press cannot be expressed. Found by the Kodi preview. |
| **K2** | Words Couch does not have | A capability id must parse as a core `Function`; no Info, OSD, next subtitle, number keys. Found by the Kodi preview. |
| **F1** | A form that explains itself | `invalid` carries no text; the person sees only "settings are invalid". Found by the Kodi preview. |
| **H** | Remote hardware | `/dev/irtx`, Bluetooth, microphone, helper programs such as `ssh`. Not offered to packages, by design. |
| **S1** | Isolation between packages | One shared uid. See 6.2. |

### Capability matrix

`X` = blocks a faithful package. `o` = nice to have / partial. `-` = not needed.

| Integration | M1 | M2 | M3 | M4 | M5 | E1 | P1 | P2 | P3 | P4 | P5 | R1 | R2 | R3 | V1 | H |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Denon | - | - | - | - | o | o | - | - | - | - | - | - | - | - | - | - |
| Sonos | X | X | X | X | X | o | - | - | o | - | - | - | - | - | - | - |
| Kodi | X | X | X | X | o | X | - | - | o | - | o | - | - | - | - | o (per-key IR override stays in core) |
| CoreELEC (Kodi half) | as Kodi | | | | | | | | o | | | | | | | - |
| CoreELEC (SSH half) | - | - | - | - | - | - | - | X | - | - | - | - | - | - | - | **X** (`ssh` program, key files) |
| LG webOS | - | - | o | o | o | o | X | X | - | X | X | - | - | - | - | o (core device IR remains independent of the package) |
| Android TV | o | o | - | - | o | X | X | X | X | o (launch only) | **X** | - | - | - | - | - |
| Apple TV | o | - | - | - | o | X | X (twice) | X | X | X | X | - | - | - | - | - |
| Samsung Tizen | - | - | - | o | o | - | X | X | X | X | X | - | - | - | - | - |
| Philips Hue | - | - | - | - | o | o (cached state is enough; hints later) | X (link button) | X | o (address is typed today) | - | X | X | X | X | - | - |
| Home Assistant | - | - | - | - | o | - | - | - | - | - | o | X | X (lights, covers, climate) | X | - | - (but voice depends on it) |
| Matter | - | - | - | - | o | o | X (commissioning) | X (fabric keys) | X | - | X | X | X | X | - | o (IPv6 multicast) |
| UniFi Protect | - | - | - | - | o | o | - | - | o | - | X | X | - | o | **X** | - |
| IR / Bluetooth / voice | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | **X** |

Two smaller gaps that are not protocol items but will bite:

- **Wake-on-LAN and `/proc/net/arp`.** A sandboxed child may send UDP broadcast
  (it has `AID_INET`), but whether it can read the ARP table on the HA100's
  Android-derived kernel is unverified. If not, the host learns the MAC (webOS
  does this in the GUI today) and passes it in as a setting.
- **Refusing a too-new package before download.** The core learns a package's
  protocol version only after unpacking it. With three protocol versions live,
  the feed should publish each version's protocol next to the index. Detailed in
  the media player design, section 8.1.

## 4. One protocol 3 train, then package waves

### 4.1 Why one train

Sonos, Kodi, Hue and the TVs were each heading for "their" protocol version.
Every one of those additions edits the same frozen files
(`clients/couch-plugin/src/{protocol,manifest,host,server}.rs`,
`clients/couch-sdk/src/{client,status}.rs`, `daemon/couch-confd/src/{plugins,api/plugins}.rs`,
`model/couch-model/src/{connection,device,volume,commands,buttons,validate,storage,lib}.rs`),
and every protocol version costs the same fixed overhead: a rollback envelope
(`integration_config_vN` plus a projection), a feed validator change, new literal
admission cases, a documentation pass, and a renewal. Three versions would pay
that three times and leave outside authors tracking five live protocols within a
year. One version pays it once.

The train is ordered so that (a) each step only builds on earlier ones, (b) the
two finished designs land first and the unfinished one last, and (c) protocol 3
stays **switched off** until the last step, so `dev` is releasable at any point
as a protocol 2 core.

| Step | Contract pull request(s) | Design | Size | Unblocks |
| --- | --- | --- | --- | --- |
| **T1** Groundwork | Host-side gating by manifest version with `PROTOCOL_VERSION` still 2; `integration_config_v3` envelope and a `v2_projection` that strips *anything* protocol 3 (components, actions, children, `x:` bindings); more than one action schema per manifest; `Response::Error { code, reason }`, new code `unpaired`; `Function::Custom` (`x:<id>`); `KeyPhase` on `Command`. | media design 2, 3.5, 4.4 | S-M | everything |
| **T2** Children and domain controls | `Children` (paged), `resource` on `Command`/`Action`/`Status`, `LocalRequest.resource_id`, manifest `children`, `Integration::Plugin.child_kind`, `Scene.resource`; `light` and `scene` components, `SetLight`, `Status.light`, `dim:N` mapping. **Add while the file is open:** `SetCover { position }` and `SetClimate { target_tenths, mode }` with `cover`/`climate` components, so Home Assistant does not need a protocol 4. | Hue doc (ii), (iii) | L | Hue, Home Assistant, Protect |
| **T3** Pairing and sessions | `PairStart`/`PairContinue`/`PairCancel`, `credential` in `Configure`, `plugin-credential.json`, `store_credential`, manifest `pairing` and `keep_alive`, no reap while pairing; **per-package uid and non-dumpable children** (6.2). | Hue doc (i), (iv) `keep_alive` | L | Hue, webOS, Android TV, Tizen, Apple TV |
| **T4** Media | `Media`, `Artwork`, `List`, `Choose`; `set_volume_percent`, `step_volume_percent`, `seek`, `seek_by`, `set_mode`, item guard; `Selectable.detail/current`; `media_player`, `volume_percent_control`; `volume:NN` mapping. | media design 3, 4 | M-L | Sonos, Kodi |
| **T5** Freshness | `States { since }`; `Subscribe` and the one-shot `changed` hint; serve loop that waits on stdin *and* the device; daemon cache, `watch` long-poll, artwork cache and HTTP routes. | Hue doc (iv) step 1; media design 5 | M | Hue (first half), Kodi and Sonos (second half) |
| **T6** Discovery and apps | Manifest `discovery { mdns, ssdp }`, daemon-owned browse, `Probe`; `apps` request, `supports_apps`, `app:<id>` allowed; optional `now_playing` component for the TV layout reusing T4. | Hue doc (v); apps not yet designed | M | TVs; nice-to-have elsewhere |
| **T7** Switch on | `PROTOCOL_VERSION = 3`; harness cases `pairing(`, `children(`, `media(`, `hints(` (harness digest update); echo exercises all of them; tested set gains `supported_protocol_versions: [1, 2, 3]` and a protocol 3 package; **evidence renewal**; core release; feed `PROTOCOL_VERSIONS = {1, 2, 3}` and per-version `compatibility.json`. | both | S | publishing |

The first three steps each have a full implementation plan: [T1 groundwork](protocol-3-t1-groundwork.md),
[T2 children and domain controls](protocol-3-t2-children-and-domain-controls.md), and
[T3 pairing and isolation](protocol-3-t3-pairing-and-isolation.md). T1, T2 and T3 are complete on
`dev` (2026-09-20), switched off; T4 (media) has not started.

Rules for running it:

- **T6 may be dropped from the train, never delay it.** It is the only step
  without a finished design. Nothing in wave 1 needs it (Hue, Sonos and Kodi
  addresses are typed today). Of the TVs, LG has no discovery today and Android
  TV has no app list today, so the first two TV packages reach parity without T6
  as well. If T6 misses the train it becomes a small protocol 4 later.
- **Non-contract work runs beside the train from day one**: the device-neutral
  player controller in the GUI, a `Plugin` arm in `lights.rs`, the web pairing
  dialog and device picker, the package adapters themselves
  (`couch-integration-sonos` 0.2, `-kodi` 0.2, `-hue` 0.1 can be written against a
  `dev` commit as soon as their step lands, because the feed pins a tooling
  commit, not a release).
- **Every contract pull request carries golden tests** that protocol 1 and 2
  manifests, requests and responses are byte-identical, and the rollback check
  against the real `.177` source, as PR #194 did against `.171`.

### 4.2 Wave 0 - now, no protocol change (size S)

- **Denon**: built-in removed; old connections convert automatically on first
  start after the update (the generic converter is being built now).
- **Sonos** and **Kodi** preview packages exist as simple remotes beside their
  built-ins. **Hue** deliberately has no preview package.
- Two fixes the Sonos preview asked for that touch no frozen path and help every
  packaged device today: `tv_plugin::run` never shows the `title` and `playing` a
  package already reports; and a held volume key on a packaged row is one full
  device round trip per repeat with no optimistic volume card.
- One fix in the feed repository: `scripts/test_publish.sh`'s install smoke test
  is hard-coded to Denon and should take the ids from `feed-policy.json`. (TLS
  integrations already build there: the feed builder sources the tooling's
  `tools/arm-cc-env.sh`, so `rustls` + `ring` cross-compile for armv7 musl with
  Zig as the C compiler.)

### 4.3 Wave 1 - Sonos, Kodi, Hue (size L)

1. **Sonos 0.2** - full player screen from the package (media design, section 7).
   Read-only check on the LAN players, then a hands-on check with Bryan, then the
   built-in is removed and connections convert. Keep the `couch-sonos` file name.
2. **Kodi 0.2** - player screen with D-pad passthrough, lists, hints (media
   design, section 8). `Provider::Kodi` **and** `Provider::CoreElec` convert to
   it; CoreELEC's SSH half stays in the daemon as a small tool attached to a Kodi
   connection, or is retired (decision 5). Keep the `couch-coreelec` file name.
   Per-key infrared overrides stay a core feature above the package.
3. **Hue 0.1** - in the Hue document's own order: adapter and admission cases
   against a fake bridge, preview release, side-by-side run against built-in Hue
   on the real bridge, then automatic conversion (`hue-connection.json` becomes
   the package credential, `room:`/`scene:` ids become `room/`/`scene/`,
   `Scene.hue` becomes `Scene.resource`), then removal.

The GUI half of wave 1 is the larger half: `sonos_player.rs`, `activity.rs`,
`room_sonos.rs`, `lights.rs`, `scenes.rs` and `connections.rs::HueFleet` are about
5,000 lines written against concrete clients.

### 4.4 Wave 2 - the four TVs (size L)

Uses T3 (and T6 where it exists). Order by how much is already proven:

1. **LG webOS** - validated on a real TV, simplest pairing (`ApproveOnDevice`),
   no discovery. Core per-device IR remains available independently. The legacy
   `webos-power.json` and `webos-wake.json` files are retained only for rollback;
   they are not migrated or used by the package. Its picture and sound cards are
   dropped from parity with a note unless someone asks for them.
2. **Android TV** - partly validated; exercises `EnterCode` pairing, the largest
   credential, and `keep_alive` with sub-second keep-alives through the serve
   loop's idle hook (T5). App shortcuts stay in core configuration. Cast
   now-playing moves into the package through the `now_playing` component, or is
   dropped from parity until T6.
3. **Samsung Tizen** - never on hardware; becomes a preview package with nothing
   to regress.
4. **Apple TV** - never on hardware, two pairings (control and metadata), the
   biggest crypto surface. Last.

New test work in this wave: WebSocket and mutual-TLS fake devices reachable
through ordinary settings. The pinned certificate is already a credential, so no
test-only trust bypass is needed.

### 4.5 Wave 3 - Home Assistant, camera stills (size L)

- **Home Assistant** reuses Hue's children, light rows and `States`, and adds
  covers and climate (in T2 for that reason). `thermostat.rs` moves from the HA
  client to the connection's children. Decision 4 (voice) comes first.
- **UniFi Protect, stills only**: a camera is a child whose picture is fetched
  through the media design's `artwork` request every few seconds. Live view stays
  built in.

### 4.6 Later - live camera video (protocol 4, size XL)

Work began for UniFi Protect on 2026-09-23 after its physical-remote path was
made reliable and extraction was explicitly requested. The settled design uses
a second inherited socket carrying bounded length-prefixed H264 access units;
`ffmpeg` stays core-owned, while SRTP, stream URLs and certificate pins remain
inside the package. The provider-neutral decoder, rollback-safe camera child,
bounded control frames, record codec, SDK server, daemon/GUI forwarding and
package adapter now exist in source, and protocol 4 is the normal host
contract. The standalone UniFi package is published in the preview feed.
Automatic migration from the retained built-in connection and physical
package acceptance remain. See
`docs/plans/camera-package-data-plane.md`.

### What should not become a package

| Integration | Recommendation | Why |
| --- | --- | --- |
| **Infrared** | Stays in core, permanently. | Needs `/dev/irtx`; is also a *service* other integrations use (per-key overrides, LG power). A package with device-node access would be a hole in the one wall the sandbox has. |
| **Bluetooth HID** | Stays in core, permanently. | Root, raw HCI, a patched BlueZ over D-Bus, its own daemon, kernel coupling. It is part of the OS image, not an adapter to a network device. |
| **Voice** | Stays in core. | Microphone and key events; its privacy contract ("nothing listens unless asked", hard recording cap) should be enforced by code Couch ships and signs itself. |
| **Matter** | Stays in core for now; revisit. | The remote is the fabric administrator: the stored keys *are* the house's Matter trust root. Never validated on the HA100. Large dependency and a `tokio` runtime per child. If it ever moves it needs everything in waves 2 and 3 first. |
| **CoreELEC SSH half** | Stays in the daemon or is retired. | Needs an `ssh` executable, key files and a `PATH`; the sandbox has none of them, on purpose. |
| **Echo** | Stays in the main repository as the SDK's worked example and the harness's self-test. | It is test scaffolding for `couch-plugin`, versioned with it. A separate *template repository* for authors is still worth having (section 5). |

## 5. Repository and feed mechanics, per integration

The same nine steps every time; `couch-integration-denon` is the model.

1. **Repository** `Couch-OS/couch-integration-<id>` with `integration.json`
   (exactly nine keys: `schema`, `protocol_version`, `id`, `tier`, `synthetic`,
   `cargo_manifest`, `cargo_package`, `binary`, `manifest`), `plugin.json`
   (`min_core_protocol_version` equal to `protocol_version`), `Cargo.toml`
   pinning `couch-plugin` and `couch-sdk` (normal and dev dependencies) to one
   full Couch commit, `Cargo.lock`, `src/`, `tests/admission.rs`,
   `.github/workflows/admission.yml`, branch protection on its own `admission`
   check.
2. **Admission cases** kept as the literal strings the feed greps for:
   `testing::conformance(`, `testing::failure(`, `testing::timeout_no_retry(`,
   `testing::spike(`, and the per-repository test
   `concurrent_package_startup_is_offline_and_race_free` naming
   `testing::Package::new(`. Protocol 3 adds its own literal cases
   (`testing::pairing(`, `testing::children(`, `testing::media(`,
   `testing::hints(`), each required only when the manifest declares the
   matching feature.
3. **Fake device** in `tests/fake/`, reachable through ordinary settings. For
   pinned-TLS devices the certificate arrives as the stored credential. For
   Sonos it is `api_root`. WebSocket and mutual-TLS fakes (the TVs) are new work
   in wave 2. The feed already cross-builds `rustls` + `ring` for armv7 musl.
4. **Feed** (`couch-integrations`): add the id to `feed-policy.json` under
   `preview`; move `source-pin.json` (`integrations.<id>.commit`, and
   `tooling.commit` to a Couch commit that contains the protocol the package
   needs); merge; `Feed admission` on `main`; `publish.yml` signs and deploys by
   itself. A published version's bytes never change: any source change is a
   version bump first.
5. **Secrets at publish time** only where unavoidable (Sonos' API key from a
   feed secret as `COUCH_SONOS_BUILT_IN_API_KEY`). The feed's *build* job stays
   secret-free for every other integration; say so in review if a new one asks.
6. **Converter**: one table entry in the generic "legacy built-in connection
   becomes the package" converter (being built for Denon now): the `Provider`
   variant it matches, the package id, how its fields and its private credential
   file map to package settings (for example `webos-connection.json`
   `{url, client_key, certificate}` -> settings `url`, credentials `client_key`,
   `certificate`), and any sidecar files that are actually part of the migration
   (for example `kodi-web.json`). With no internet the connection stays
   listed as "needs the <name> package" and retries. Rooms, activities and button
   maps keep pointing at the same connection id.
7. **Retire the built-in** one release after hands-on validation (decision 5):
   delete the client crate and its GUI/daemon/web code; **keep** the `Provider`
   and `Integration` variants parseable; **keep** required updater file names as
   stubs (`couch-sonos`, `couch-coreelec`); run `tools/release/update_floor.py`.
8. **Tier and version policy**: `0.x` while `preview`. A package becomes
   `production` and `1.0.0` when it has at least one hardware evidence record and
   its built-in has been gone for one release without a regression report.
   `stable` lists only `production` ids (the feed already enforces that half).
9. **Hardware evidence**: `docs/development/admission.md` defines the record
   (exact model and firmware, manifest version and source commit, ISO date,
   behaviours exercised, a link or file). `integration.json` has no room for it
   and the feed does not check it. Recommended: a file `evidence/<date>-<model>.json`
   in the integration repository with exactly those fields, and a feed rule that a
   `production` tier requires at least one whose `source_commit` is an ancestor
   of the pinned commit and whose `manifest_version` shares the major version.
   Simulator and fake-device runs never count. Records that need Bryan's hands
   (anything that plays sound or changes volume) are scheduled, not improvised.

Two pieces of shared plumbing worth building once, before repository number
three exists: a **reusable admission workflow** (`uses:` pinned like the SDK) so
the bar is raised in one place, and a **template repository** cut from Echo.

| Integration | Repository | First package protocol | Converter inputs | Notes |
| --- | --- | --- | --- | --- |
| Denon | `couch-integration-denon` (exists) | 2 | host, port | auto-convert in progress |
| Sonos | `couch-integration-sonos` (exists, preview) | 1 now, then 3 | host; key comes from the build | keep `couch-sonos` file name as a stub |
| Kodi | `couch-integration-kodi` (exists, preview) | 1 now, then 3 | host, port; from `kodi-connection.json`: `http_control` -> `http`, `web_port`, `username`, `password`; legacy `kodi-web.json` | also absorbs `Provider::CoreElec`'s Kodi half; keep `couch-coreelec` file name |
| LG webOS | `couch-integration-webos` | 3 | `webos-connection.json`; legacy power and wake sidecars remain only for rollback | removed from core after physical-TV validation |
| Android TV | `couch-integration-androidtv` | 3 | `androidtv-connection.json`; `config.app_shortcuts` stays in core config | |
| Samsung Tizen | `couch-integration-tizen` | 3 | `tizen-connection.json` | preview, never validated |
| Apple TV | `couch-integration-appletv` | 3 | both credential files | preview, never validated |
| Philips Hue | `couch-integration-hue` (exists, client only) | 3 | `hue-connection.json` -> package credential; `light_id` -> resource id, `room:`/`scene:` -> `room/`/`scene/`; `Scene.hue` -> `Scene.resource` | |
| Home Assistant | `couch-integration-home-assistant` | 3 | `ha-connection.json`; `entity_id` -> resource id | after the voice decision |
| UniFi Protect | `couch-integration-unifi-protect` | 4 | address/API key -> package settings; pins -> credential; `camera_id` -> resource id | Extract once as a complete camera package; the provider-neutral decoder and protocol 4 data-plane plan are in `docs/plans/camera-package-data-plane.md`. |

## 6. Cross-cutting work

### 6.1 The evidence gate, honestly

`tools/release/tested-integrations.json` names a `tested_commit` and freezes 22
`contract_paths`: all of `clients/couch-plugin`, `clients/couch-sdk`,
`clients/couch-control` and `daemon/couch-integrations`; nine `couch-confd` files
(`main.rs`, `store.rs`, `plugins.rs`, `api.rs`, `api/{connections,plugins,
integration_packages,integration_migrations}.rs`); and ten `couch-model` files
(`lib`, `seed`, `storage`, `validate`, `volume`, `commands`, `connection`,
`device`, `buttons`, `integration_migration`). `verify_integration_set.py` fails
if any of them differs between the tested commit and the release candidate. The
one carve-out is `clients/couch-plugin/src/testing.rs`, pinned by digest: editing
it costs an admission rerun and a one-line digest update, not a renewal.

What that means in practice:

- **Every protocol addition touches frozen paths** (protocol, manifest, host,
  SDK, model, daemon bridge). So does removing a built-in (`main.rs`,
  `connections.rs`, `connection.rs`), and so does the converter.
- The verifier runs **whenever a runtime is staged** (`runtime_inventory.py` takes
  the receipt): every release and every dev build for the test remote. In
  pull-request CI it runs only through `tools/release/test_verify_integration_set.py`
  (`test_committed_manifest_matches_current_contract_and_limits_claims`), and the
  `runtime-os-compatibility` workflow that runs it is filtered to pull requests that
  touch `tools/release/**`. So a contract change does not turn its own pull request
  red, but it blocks the next dev build, the next release, and the next pull request
  that touches `tools/release`, until the evidence is renewed. The cost is **one
  renewal per staged build that contains a new contract change**: a train that wants
  one renewal must also do without dev builds on the remote until it ends, or accept
  a renewal per dev build (they are cheap and need no hardware).
- A renewal is the simulated host-compatibility run on the build host in an ARM
  Alpine container (about 30 minutes, no remote), against the exact signed APKs in
  the feed snapshot. The tested commit must stay in the release's ancestry:
  **merge commits only**, never squash or rebase across it.
- GUI (`ui/`), web (`web/`), the integration crates themselves
  (`clients/couch-sonos`, ...), docs and the feed repository are **not** frozen.

Recommendations:

1. **One train, one renewal.** Cut a core release immediately before T1 so the
   train starts from a freshly renewed commit. Land T1-T6 on `dev` with protocol
   3 documented as "unreleased" and switched off. T7 switches it on, renews once
   and releases.
2. **If a release cannot wait**, there are two honest options. Cut it from a
   branch off the pre-train commit with only non-contract fixes (no renewal: the
   frozen paths are unchanged and the tested commit is its ancestor). Or cut it
   from `dev` mid-train: it behaves as a protocol 2 core and costs one extra
   renewal, but no extra protocol version. Prefer the first.
3. **Put the removal of built-in Denon, the generic converter, and the per-package
   uid change inside the same window.** They touch the same frozen files
   (`main.rs`, `plugins.rs`, `connections.rs`, `connection.rs`, `host.rs`). If the
   Denon removal ships before the train, that release is the "release immediately
   before T1" and its renewal is the starting point.
4. **Removals of later built-ins (Sonos, Kodi, Hue...) also touch frozen files.**
   Batch them: one "retirement" release per wave, after that wave's hands-on
   checks, rather than one per integration. Expected total for the whole
   programme: the pre-train release, T7, and one retirement release per wave -
   about five renewals, against ten or more if each protocol addition and each
   removal shipped alone.
5. **Front-load and keep moving.** A half-landed train blocks every unrelated
   change to `couch-model`'s `lib.rs`, `commands.rs` and `buttons.rs` from
   shipping without a renewal, so the contract pull requests should be reviewed
   ahead of GUI work and the train should not idle.
6. **Grow the set at T7.** The receipt covers Denon only. Add a protocol 3 package
   (Sonos 0.2 is the natural one) to `integrations` in the tested set so the
   renewal exercises pairing-free protocol 3 too, and extend `HOST_CHECKS` with a
   fake-bridge pairing and children check using Echo. Otherwise the gate proves
   less with every addition.
7. **Do not shrink `CONTRACT_PATHS` to save renewals.** Thirty simulated minutes
   is a fair price. What is worth doing is scripting the renewal end to end so it
   is one command on the build host.

### 6.2 Isolation between packages (S1), part of T3

All children run as uid 65534 with `no_new_privs`. Processes that share a uid can
normally signal each other and, unless the kernel's ptrace restrictions say
otherwise, attach to each other or read `/proc/<pid>/mem`. While packages hold a
receiver's IP address that is academic. Once they hold a TV's pairing key, an
Android TV private key or a Hue application key, it is not. As part of T3,
before any package stores a credential:

- give each *package id* its own uid from a reserved range (allocated by the
  package store, recorded beside the installed version), and
- set `PR_SET_DUMPABLE` to 0 in the child before exec, and
- check what the HA100's 3.18 kernel actually enforces (Yama, `hidepid`), and
  record it in `docs/integration-architecture.md`, which today says plainly that
  this is "not a sandbox for hostile code".

Per-package **network** permissions (this package may only talk to its configured
host) are not feasible on this kernel without per-uid firewall rules, and
per-uid rules become possible exactly when step one is done. Treat it as a
follow-up, not a blocker: packages come from a curated, reviewed, reproducibly
built feed, and that review is the real control.

### 6.3 Where the GUI talks to devices directly

Sonos, Hue, Home Assistant, Matter, UniFi Protect, Cast and AirPlay metadata and
Kodi artwork are all reached **from the GUI process** today; only Kodi, webOS,
Denon and the three streaming TVs go through the `couch-control` broker. A
package always sits behind `couch-confd` and `plugin.sock`. So extraction is never
just "wrap the client": each wave also moves a screen from "call the client" to
"ask the daemon", and the screen work is usually the larger half. The media
player design shows the pattern to reuse: a device-neutral controller, two
backends (built-in and packaged), one fake device, identical screenshots.

## 7. Risks

- **Stranding old remotes** by dropping `couch-sonos` or `couch-coreelec` from
  the runtime bundle. Silent in review, permanent per device.
- **A daemon that will not start** because a `Provider` variant was deleted.
- **Parity drift.** Every built-in has behaviour nobody wrote down (held-key
  coalescing, contextual OK, IR overrides, wake retries). The "same fake, two
  backends" test is the defence; budget for it in every wave.
- **Unvalidated packages replacing unvalidated built-ins** (Tizen, Apple TV) is
  fine; an unvalidated package replacing a *validated* built-in (webOS, Sonos,
  Kodi, Hue) is not. Hence decision 5.
- **A train that stalls.** One protocol 3 is cheaper only if it keeps moving;
  half-landed, it makes every release more expensive. T6 is droppable for that
  reason, and T1-T5 each have a finished design before they start.
- **Designing for devices nobody has tried.** Pairing is specified from Hue (real
  bridge) and the built-in TV clients, two of which have never met a real TV. The
  first real Tizen or Apple TV pairing may still find a gap; `store_credential`
  and the three prompt kinds are the hedge.
- **Feed key and publish secrets.** Sonos introduces the first publish-time
  secret. Keep it the only one.

## 8. Further reading

- [Media player component design](media-player-component-design.md) - wave 1 in detail.
- [Integration separation](../integration-separation.md) - the earlier assessment; its retirement rules still stand.
- [Integration architecture](../integration-architecture.md), [packages](../integration-packages.md), [migration](../integration-migration.md), [release rollout](../integration-release-rollout.md).
- [Protocol](../development/protocol.md), [components](../development/components.md), [admission](../development/admission.md).
- [Connections and private settings](../connections.md), [runtime updates](../runtime-updates.md).
