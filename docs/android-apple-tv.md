# Android TV and Apple TV clients

The new `couch-androidtv` and `couch-appletv` crates are native Rust libraries in
`clients/`. They do not invoke Python, ADB or an external remote-control program.
Both are **experimental**. Connections now offers network discovery and explicit
PIN pairing. The GUI and daemon are deployed on the development remote; initial
Android TV pairing and authenticated status reads have passed on a Xiaomi TV.
Apple TV physical validation remains pending. Protocol-peer tests and ARMv7
compilation are not physical TV validation.

## Android TV / Google TV

`Identity::generate()` creates a separate RSA-2048 client identity. Keep this
identity and the returned `Credentials` in the connection's private credential
store, never in exported house configuration or Git. `Pairing::begin()` opens
TLS port 6467 and requests the six-character hexadecimal code displayed by the
TV; `finish(code)` binds that code to both endpoints' RSA keys. Successful pairing
returns the TV certificate pin plus the reusable client identity.

`Remote::connect()` opens a persistent mutually authenticated TLS connection to
port 6466 and requires the saved TV certificate. Poll the connection at least
once per second to answer keepalives and consume power/volume updates. Navigation,
volume, playback, channel and power key codes use `press(Button::...)`; `launch()`
sends an app link. A successful write means the command was sent, not that the TV
performed it. No command is automatically retried. These calls belong on the
shared control service's worker, not the render thread.

The target needs Android TV Remote Service, ordinarily present on Android/Google
TV. It does not require enabling ADB. Feature negotiation excludes voice and IME
for this first version. Text input, current-app tracking and voice streaming remain
future additions. [Protocol implementation and schemas](https://github.com/tronikos/androidtvremote2)

Needed for hardware testing: **TV address/model and its displayed pairing code**.
Default ports are configurable. The Connections finder uses Bonjour and also permits manual address entry.

## Android TV now playing

The TV screen also observes the standard Cast media namespace on port 8009 of
that connection's address. This is independent of Remote v2: it attaches only to
an already running media receiver and requests status. It never launches an app,
loads content, or changes playback. Cast metadata does not use the Remote v2
certificate pin; no pairing credentials are sent on this separate connection.

Supported sessions supply title, subtitle, artwork, position, duration and player
state. The screen shows a read-only timeline, freezes it while paused/buffering,
and marks live streams without inventing an end time. Missing/expired metadata
returns to the ordinary control screen. App/session changes discard old artwork.
Artwork uses bounded HTTP(S) downloads and decoding on a separate worker; neither
metadata nor images delay physical button commands. HTTPS artwork certificates
are validated using bundled public roots.

Plex, Jellyfin, YouTube, Netflix and other apps are handled through the same
interface **when their active session exposes it**. Remote v2 alone cannot provide
this information, and having Cast installed does not guarantee that every native
app exposes its current playback. No ADB access, companion app or app-specific
scraper is used. Per-app interoperability must be recorded after physical tests.

The initial live check on the development TV received SmartTube’s title, duration,
advancing position and play/pause state. Its media metadata contained no artwork
field. A second live check of Wholphin (a Jellyfin client) reported *1917*, an
artwork URL, duration, position and paused state. These observations verify the
metadata path for those sessions, not every app or successful image rendering.

The GUI runs in the initramfs while DHCP updates Alpine’s resolver. Stage 2
links `/etc/resolv.conf` and `/etc/hosts` to their Alpine counterparts so hostname
artwork works and follows network changes. The exact ARM image pipeline was
verified against a 1,039,160-byte, 2000×3000 JPEG in the GUI’s environment after
this fix. Failed image requests retry after 15 seconds; switching media starts
an immediate new attempt, and late results from another screen are discarded.

References: [Cast MediaStatus](https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.MediaStatus),
[Cast MediaInformation](https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.MediaInformation),
[Cast Connect media sessions](https://developers.google.com/cast/docs/android_tv_receiver/core_features).

## Apple TV

Use the Bonjour-advertised `_companion-link._tcp.local.` port with `Settings`;
there is no universal fixed Companion port. `Pairing::begin()` requests the TV's
four-digit PIN and retains that connection while the user enters it. `finish()`
performs SRP-3072/SHA-512 authentication and verifies the server proof and signed
accessory identity before returning credentials.

`Remote::connect()` verifies the saved device identity with X25519/Ed25519,
derives directional keys, and opens a ChaCha20-Poly1305 authenticated Companion
session. `press()`, `playback()`, `launch()` and `apps()` provide navigation,
media controls and app management; `close()` ends the remote session. Frames,
OPACK depth/object counts, queued events, and operation waits are bounded.
Credentials are redacted in Debug output and must be stored privately per TV.

Modern tvOS playback metadata is **not** supplied by a plain standalone MRP
socket: tvOS 15 moved that path into AirPlay 2. The separate native
`couch_appletv::metadata` API now implements AirPlay pairing and MRP metadata
observation; see [Apple TV metadata](apple-tv-metadata.md) for its probe and
validation limits. The Couch screen uses Companion controls with optional
separately paired AirPlay now-playing metadata. Pair it in Connections under
Optional now playing; the guide covers private storage and known limitations.
[pyatv protocol documentation](https://pyatv.dev/documentation/protocols/),
[Companion implementation](https://github.com/postlund/pyatv/tree/master/pyatv/protocols/companion)

Needed for hardware testing: **Apple TV address, tvOS version and on-screen PIN**.
Discovery fills the Companion port. No Apple ID password is requested; physical
TV validation remains pending.

## Validation

Run `cargo test -p couch-androidtv -p couch-appletv` from `clients/`.
Tests include a real mutual-TLS Android peer through pairing, negotiation,
keepalive and key/app commands; an encrypted Apple Companion peer through
pair-verify/session setup/navigation/playback/app commands; malformed framing,
OPACK/TLV limits, authenticated-encryption rejection and certificate-bound codes.
Apple SRP output is compared with an independent `srptools==1.0.1` vector, and
accessory signatures/server proofs must validate before pairing completes.

The libraries compile for `armv7-unknown-linux-musleabihf`. The host browser bundle
and configuration daemon also build. Model, broker and daemon regression tests
cover provider resolution, private credentials and pairing session handling. A
Playwright fixture verified both connection types through mocked discovery, PIN
entry, saved status and mobile layout without contacting a TV. This is client-side
validation; every kernel build remains on Ollie.

## Couch integration

Add a named **Android / Google TV** or **Apple TV** connection, find/select the
TV (or enter its address), request its code, then **Verify & save pairing**.
Pairing sessions expire after two minutes and can be cancelled; private JSON
credentials are written atomically with mode 0600 under
`connections/<id>/{androidtv,appletv}-connection.json`. Ordinary connection reads
return only paired status, address and port. Exported configuration contains only
the named provider reference.

Add the TV to a room. The room editor provides basic test controls; activities
can map its supported functions to physical buttons, ordered start/stop steps,
and custom command pages. Selecting an Android TV from a room opens its TV control screen immediately.
D-pad, OK, Back, Home, menu, volume, mute, channel and power control that TV;
long Back leaves the screen. Playback controls are available by touch. Android
TV and Apple TV can also be an activity’s main screen. Selecting an Apple TV
opens its Companion control screen directly. Unsupported
Apple functions such as mute and stop are omitted from the capability catalog.

The shared control broker owns each endpoint, answers Android keepalives while
idle, expires stale queued commands, and reconnects only on a later request after
failure. Activity navigation releases retained connection handles. Credentials
changed through pairing replace the next mapped command's connection settings.
Discovery is an explicit three-second IPv4 Bonjour search; IPv6 addresses can be
entered manually, but scoped/link-local IPv6 discovery is not implemented.


## Initial Android TV hardware check

The HA100 paired with a Xiaomi `MiTV-AFMU0` using its six-character on-screen
code. Credentials were saved on the remote, and the authenticated Remote v2
connection reported model/vendor, power and volume state. Home, Right and Left
command requests completed successfully, and the operator confirmed visible
navigation on the TV. A later connection using saved credentials also passed.
Long-running keepalive and network-loss recovery remain separate checks; a
successful command write alone does not prove that the TV performed it. No Apple TV has
been paired during this validation.


## Android TV app shortcuts

The Android Remote v2 schema supports app-link launch requests, but no installed
app list query. The Android TV screen therefore uses explicitly configured,
named app shortcuts rather than presenting a guessed list as discovered apps.
Configure links for this TV in Connections; the Apps selector uses those links.
The target app must be installed and registered to handle the supplied URL.
Couch does not enable ADB or install software on the TV to obtain an app list.
[Protocol implementation](https://github.com/tronikos/androidtvremote2),
[message schema](https://github.com/tronikos/androidtvremote2/blob/main/src/androidtvremote2/remotemessage.proto).

## Apple TV app discovery in activities

For a paired Apple TV, selecting its device in an activity's **Devices & sequences**
command library or **Physical buttons** picker now loads launchable apps. Choose
an **App · name** entry to store the existing `app:<bundle-id>` command. Discovery
runs through the shared control broker and the authenticated connection API;
it does not launch an app until a configured command is executed. A discovery
failure leaves standard commands and saved mappings available.

`GET /api/connections/<id>/appletv/apps` returns only `apps: [{id, title}]`.
The Companion response is a bundle-ID-to-name dictionary, normalized to a sorted,
bounded catalog. Invalid IDs, names, duplicate IDs or oversized catalogs are
rejected. Android keeps its configured app-link shortcuts because Remote v2 has
no corresponding installed-app-list operation. [Companion app-list implementation](https://github.com/postlund/pyatv/blob/master/pyatv/protocols/companion/__init__.py#L150).

The encrypted local Companion peer now exercises discovery as well as launch.
Additional tests cover invalid catalogs and provider/pairing route guards; the
web UI compiles for WebAssembly. This addition has not been tested on a physical
Apple TV and does not add playback metadata. The dedicated screen is described below.

## Remaining client work, in priority order

1. Pair and validate Apple TV on real hardware: PIN, saved reconnection,
   navigation, app discovery/launch and sleep/wake commands. This needs an
   available TV and its on-screen PIN.
2. Validate the dedicated Apple TV device/activity view against a paired TV,
   including immediate physical-control takeover and installed-app launch.
   Its routing and Slint controls are implemented and headless-tested.
3. Exercise Android keepalive and network-loss recovery over longer sessions;
   its basic pairing, saved reconnection and visible navigation have passed.
4. Extend Android text input/current-app tracking. Physically validate the new
   Apple AirPlay/MRP metadata client and its separate pairing/rendering flow.

Already implemented: both Rust protocol clients, discovery and private pairing
flows, multi-connection storage, room test controls, activity commands, Android's
dedicated screen and configured app shortcuts. The README's older hardware
roadmap does not track these client milestones; use this list for their status.


## Android recovery and command-expiry regression coverage

Loopback mutual-TLS peers now exercise the real shared broker lane, not a mock
replacement for the Android client. Tests cover repeated keepalives during
continuous key traffic, a dropped TLS socket, a later explicit reconnect, and a
slow replacement handshake with another key queued behind it. The peer asserts
that expired keys never arrive and only the fresh requested key is sent after
reconnection. Forty consecutive ping/key exchanges check that traffic does not
starve keepalives; these are bounded regression tests, not a multi-hour hardware
soak or a claim about every TV's firmware.

Two broker fixes follow from those tests. Android input is serviced before
command dispatch even when the queue stays busy. Command age is checked again
after connection setup and that input read: a key older than 750 ms is rejected
without sending it, while a successfully reopened connection remains available
for the next fresh request. A transport failure never triggers replay of the
failed command. The original queue-entry expiry and per-endpoint isolation
remain in place. Real-TV network-loss and longer standby/reconnect checks are
still outstanding.


## Dedicated Apple TV screen

Selecting a room's Apple TV now opens its control screen immediately, using that
specific connection. Apple TV is also selectable as an activity's main screen.
D-pad/OK, Back, Home, volume and channel buttons control Companion; Menu uses
Companion Back. Long Back remains the global return to Couch. Activity mappings
retain precedence over those defaults.

Touch controls offer previous/play/pause/next and an installed-app selector using
the shared animated bottom tray. Mute, color keys, stop and seek are unsupported
and are not offered. There is no invented current program, artwork, timeline or
power state. The physical Power button explicitly requests **Sleep**; the touch
**Wake** action requests power-on. This is not an inferred toggle. App choices
load when connecting or reconnecting and can update an already open Apps tray.

Headless tests dispatch physical keys and touch events through the actual Slint
component; routing tests check per-TV selection and reject unsupported commands.
The adapter uses the already peer-tested Rust Companion broker. Physical Apple
TV validation still needs its address, advertised Companion port, tvOS version
and the PIN displayed during pairing. No Apple ID password is needed.

## Device deployment smoke check (2026-09-09)

The combined GUI and configuration daemon, including the Apple TV screen,
Android broker recovery fixes and LG power settings, were deployed atomically
after their remote SHA-256 hashes matched the build artifacts:

- GUI: `ec815337b8a17e8fcfda32cf939779eb967f040aa87a68abb21dc767c343ee66`
- Daemon: `98ca7c9209f20a82880d93702879b30a548d4bc4da6f916e67b270d0487753da`

The GUI heartbeat advanced from 613 to 615 and the configuration server returned
HTTP 200 on port 8090. Deployment preserved configuration and required no reboot
or kernel flash. This verifies software startup only: physical Apple TV pairing
and controls remain untested, and built-in IR transmission is not hardware-ready.
