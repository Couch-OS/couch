# Philips Hue lights, rooms and scenes

`clients/couch-hue` is a Rust client for direct local Hue API v2 control. It does
not require Home Assistant, a Hue cloud account, or a kernel change. Start with
light on/off and brightness, grouped room on/off and brightness, and recall of scenes saved on
the bridge. Color editing, scene creation and Hue zones as controls are deferred.
One bridge connection is supported per remote.

## Connect and add lights

1. Open the remote's web editor, pair with its on-screen PIN, and open **Connections → Philips Hue**.
2. Enter the bridge IP address (for example, `$BRIDGE_IP`). Press the bridge's
   round link button, then **Pair bridge**. A failed pairing leaves saved settings intact.
3. Open **Rooms & devices**, create or open a room, then choose the saved Hue
   connection under **Add devices to this room**. Search the discovered lights and
   click **Add to this room**. Discovery never assigns devices automatically.
4. Open that room on the remote. Devices appear in one flat list. Highlight a light
   and press OK to toggle it. Tapping selects a row, matching room navigation.
   Physical Back returns home. Use
   **Show light controls** on an assigned device in the web editor for brightness.
   Zero-percent brightness means off.

Unreachable lights display as unavailable. Commands are acknowledged by the
bridge; refresh to see reported state. This is not proof of physical illumination.

## Connection and credentials

Requests use HTTPS with a five-second per-request deadline, no redirects or
proxies, and a 4 MiB response cap. Pairing trusts the selected LAN bridge on first
use and saves its exact certificate. Later requests require the same certificate
and a valid TLS handshake signature. This is certificate pinning, not public-CA
or hostname validation. Pair only on your trusted LAN; deliberate re-pairing is
required after the bridge changes its certificate. Pairing has no certificate
bypass setting for normal control requests.

The application key and certificate are saved atomically with mode `0600` in
`/opt/couch/hue-connection.json`, separate from the exportable house configuration.
The API never returns the key. `COUCH_HUE_CONNECTION` overrides the daemon's path.
New house devices store `{"via":"connection","connection_id":"philips-hue",
"resource_id":"<v2 light UUID>"}`. Existing inline Hue definitions remain readable.
Adding this integration requires updated readers of the shared configuration.

## CLI and validation

```sh
(cd clients && cargo test -p couch-hue)
BRIDGE_IP=bridge.local
couch-hue pair "$BRIDGE_IP"
couch-hue lights
couch-hue on LIGHT_UUID
couch-hue brightness LIGHT_UUID 40
```

Use `--settings PATH` before a command to override the private settings file.
`node web/tests/hue.mjs` uses an isolated HTTPS fixture and disposable host daemon;
first build with `tools/build-webui.sh --host`. It tests pairing, private storage,
certificate mismatch rejection, discovery, controls, unreachable lights, Hue
error envelopes, room import and mobile layout. No household lights are touched.

Protocol references: [Hue getting started](https://developers.meethue.com/develop/get-started-2/),
[API v2](https://developers.meethue.com/new-hue-api/), and
[HTTPS guidance](https://developers.meethue.com/develop/application-design-guidance/using-https/).
Hue advises against continuous rapid updates through its REST API; Couch sends
explicit user commands and refreshes on demand.

## Device validation (2026-09-08)

Deployed ARMv7 CLI, daemon/browser bundle and Slint GUI. All affected workspace
unit tests passed, as did the isolated Hue browser test and existing Home Assistant
browser regression. On the physical remote, D-pad discovery, on and 40% brightness
passed against an isolated HTTPS bridge fixture; production configuration was
restored afterward. The physical BSB002 responds over HTTPS with 48 discovered
lights (42 reachable). Pairing and read-only discovery succeeded;
real household-light command testing remains pending.

## Remote state cache and response time

Room names and devices render from local configuration immediately. Device rows
match the home room list’s cards, icons, typography and moving focus ring. The room
name appears in the status bar, with no duplicate heading above the devices. OK
toggles the focused light; physical Back returns home with a 180 ms slide.

The GUI starts a credential-scoped Hue session in the background. A pinned HTTPS
connection subscribes to `/eventstream/clip/v2`. Add/update/delete events trigger
background state snapshots, preserving connectivity information as well as light
state. This version refreshes snapshots on events rather than merging partial
resource payloads. Commands use a separate, reusable HTTPS connection.

With a valid cached state, OK sends only a PUT. The row adopts the target after
bridge acknowledgement; this does not prove the bulb has finished fading. Missing
or expired state falls back to a fresh read; known-unavailable lights are refused.
Failures invalidate the cache. Snapshot generations prevent an older response from
overwriting a command, and overlapping updates schedule another refresh.

Streaming connections reconcile every 60 seconds. On stream failure, cache validity
is revoked, snapshots poll every five seconds, and stream reconnects back off from
one to 30 seconds. An idle stream has a 45-second read timeout so dead connections
recover. Reconnect, credential replacement, and wake from full standby refresh state.
The GUI checks the local cache every 500 ms in Hue-only rooms (five seconds in mixed
rooms); these checks do not normally query the bridge. A short race remains if
another controller changes a light before its push-triggered snapshot completes.

Validation: 32 GUI tests and eight Hue tests pass, including SSE framing/limits,
cache expiry, disconnected state, and stale snapshot/command ordering. On the
physical remote, an isolated HTTPS/SSE fixture verified one-PUT cached toggles,
external changes via push, stream failure with polling, and reconnect recovery.
Fixture cached acknowledgements took 34–55 ms while streaming; these are not
real-bridge/bulb latency measurements. Production credentials and configuration
were preserved. No household lights were changed by automated fixture tests.

[Hue v2 push support](https://developers.meethue.com/new-hue-api/).

## Hue rooms and scenes

In **Rooms & devices**, open a Couch room and choose the Hue connection. The
**Hue controls** selector switches between individual lights and **Hue rooms**.
Adding a Hue room creates one grouped control; OK toggles its grouped-light
service, and Volume Up/Down adjusts its brightness in 5% steps. Brightness support
and the current level come from the bridge’s grouped-light dimming state, using
the same push-maintained cache as individual lights. It does not
create or rename Couch rooms or duplicate all the bridge room's lights.

In **Rooms & devices**, open a Couch room, select the Hue connection, and choose
**Hue scenes** in the same **Hue controls** picker used for lights and Hue rooms.
Search by scene or bridge room name, optionally filter by Hue room/zone, and click
**Add to this room**. The scene immediately appears in the room's **Scenes** list
and is assigned to its bottom Scenes button on the remote. Adding an already
imported scene to another room reuses it; **Remove from room** only removes that
assignment. The picker keeps its selected category and search after adding items.

Scenes are managed from **Rooms & devices**; there is no standalone Scenes tab.
Open a scene in a room to rename it or edit its assignments. Older /scenes
bookmarks open the rooms list. **Areas** controls scene membership on
custom home pages; ALL ROOMS exposes every saved scene.

The home and room views each have a bottom **Scenes** button. The room picker
contains only scenes assigned to that room. OK recalls the selected scene and
returns to the previous view with acknowledgement or error feedback. Scenes are
not device rows or on/off toggles. Custom device-step scene execution remains
unimplemented and reports that explicitly.

Bridge room controls retain connection references using `room:GROUPED_LIGHT_UUID`;
old bare light UUIDs remain valid. Scenes save an optional `hue` connection/scene
reference and `rooms` list in the existing Scene model. Invalid references and
removing a connection still used by a scene are rejected. Removing a Couch room
removes its scene assignments without deleting the scenes or changing the bridge.

CLI: `couch-hue rooms`, `couch-hue scenes`, `couch-hue on room:UUID`,
`couch-hue off room:UUID`, and `couch-hue recall SCENE_UUID`.
API: GET `/api/hue/rooms` and `/api/hue/scenes`; POST
`/api/hue/scenes/SCENE_UUID/recall`. Room power uses the existing light command
route with a `room:UUID` identifier.

Validation: resource/payload and configuration tests, mobile browser import and
room-assignment checks, and physical-device HTTPS/SSE fixture tests for grouped
power, room-filtered scene selection, home scene selection, recall and Back.
The fixture's room list contained one assigned scene while the home list contained
two. Tests do not send commands to household lights.

Read-only discovery on the paired BSB002 returned 14 Hue rooms and 181 scenes.

## Brightness from the remote

With a dimmable light selected in a room, Volume Up/Down changes brightness by
5 percentage points. Holding a button repeats. A transient brightness meter
shows the light name and requested percentage, then dismisses after one second.
Zero switches the light off; increasing an off light starts at 5%.

Hue uses the same fresh cache and command connection as instant toggles, without
a preflight GET when cached state is valid. Rapid presses replace the unsent
target for that light, with at most one send per 100ms and one command in flight.
Acknowledgements update the row; failed commands hide the meter and show an
error. Non-light rows ignore volume keys, except a Sonos speaker, whose volume
they set (see `sonos.md`); unavailable and non-dimmable lights reject
brightness changes. Home Assistant lights use its existing service API.

## Live room and device icons

Room icons use the accent color when any configured device is known to be on,
and fade when every device is known to be off. Empty rooms, unsupported devices,
and unavailable states stay neutral unless another device is known to be on.
Device icons use the same on/off/unknown colors. Changes animate over 180ms.

The room observer shares the existing Hue push cache and samples it every 500ms;
Home Assistant light status is polled every five seconds. Network work stays off
the GUI thread, and individual model rows update without resetting focus or
scroll position. Integrations without power feedback are treated as unknown.

## Room scene shortcuts and feedback

In a room, Channel Up recalls the next assigned scene and Channel Down recalls
the previous one, wrapping at either end (with a Sonos speaker highlighted the
same keys skip tracks instead). The first forward press starts with
the first scene; the first backward press starts with the last. The cursor
remembers scenes chosen on the remote, including selections from the scene
picker. Rapid presses retain the latest unsent scene while a recall completes.
Only scenes assigned to that room participate.

Brightness and scene feedback share a large card with 22px labels, 26px names,
and a 30px brightness percentage. Cards slide in and out over 200ms without
moving focus. Brightness feedback dwells for one second; scene feedback dwells
for 1.5 seconds. Scene feedback distinguishes applying, activated and failed.

Toggle feedback preserves Hue’s saved brightness while a light is off, so an
acknowledged power-on immediately displays its known level. For up to two seconds
after a successful command, older bridge power/brightness snapshots cannot undo
that target. Background reconciliation continues; unavailable/deleted devices
remain authoritative, and external changes are accepted after this settling
window. Failed commands invalidate the cache for a fresh read.

Toast feedback is scoped to the current screen. Navigation dismisses brightness,
scene and system toasts immediately; delayed scene acknowledgements cannot reopen
feedback on a different screen. Normal timed dismissal still slides out.
Device regression check: Back during brightness and scene feedback leaves the
home screen clear, including after a simulated 2.5-second scene reply; selecting
a new scene from the chooser still displays its feedback.
