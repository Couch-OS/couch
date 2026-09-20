> Updated configuration flow: configure the server in **Connections**. Then open
> a room under **Rooms & devices**, select the saved Home Assistant connection,
> and add discovered lights there. Controls are on assigned devices, not Connections.

# Home Assistant lights

`clients/couch-ha` is a Rust library and CLI for Home Assistant light discovery,
state, on/off and brightness. It uses bounded blocking HTTP requests in a worker,
with connection reuse and verified HTTPS through ureq/rustls. It neither records
voice nor depends on the existing `couch-voice` Assist client. The remote's
microphone key does: it streams to the Assist pipeline of the first saved Home
Assistant connection, using this connection's URL and token, see `docs/voice.md`.

## Configure and test

Open the remote's configuration editor at `http://REMOTE_IP:8090`, substituting
the remote's address, pair with
the on-screen PIN, and open **Connections → Home Assistant**. Enter the server
URL and a long-lived access token from your Home Assistant profile. **Test &
save connection** reads the light inventory before saving. A failed test leaves
the previous connection intact. A blank token keeps the saved token only when
the server URL is unchanged.

**Find lights** lists light entities, including unavailable lights. Choose a
room and **Add light** to create a `Light` device with its Home Assistant entity
ID. Create the room under **Rooms & devices** first if needed. Discovery alone
does not add devices or send commands. The light's explicit **Turn on**, **Turn
off**, and **Apply brightness** buttons operate real lights. **Refresh state**
reads confirmed state; service acceptance is not proof of physical delivery.

Credentials live in `/opt/couch/ha-connection.json`, mode 0600, separately from
exportable `/opt/couch/config.json`. API responses never return the token.
`COUCH_HA_CONNECTION` overrides the credential path for isolated host tests.
The CLI alternatively reads `/opt/couch/ha-token` or `--token-file`:

```sh
couch-ha --url http://homeassistant.local:8123 lights
couch-ha --url http://homeassistant.local:8123 state light.study
couch-ha --url http://homeassistant.local:8123 brightness light.study 35
```

Only `light.*` targets are accepted. Unknown/unavailable states are not treated
as off. Brightness follows advertised color modes, with the legacy feature bit
as a fallback; zero explicitly turns the light off. Commands use
`/api/services/light/turn_on` and `turn_off`, never a write to the state cache.
Redirects and environment proxies are disabled. Requests have a five-second
limit and responses a 4 MiB cap; tokens and upstream bodies are excluded from
error messages. Call this client outside the Slint rendering thread.

## Blinds and thermostats on a key

Cover and climate entities added as `Blind` and `Thermostat` devices are
bindable, not only openable on their own screen. Covers offer **Open**,
**Close**, **Stop** and a **Position** level (`position:70`); thermostats offer
each HVAC mode (`mode:off`, `mode:heat`, `mode:cool`, `mode:heat_cool`,
`mode:auto`, `mode:dry`, `mode:fan_only`) and **Warmer** / **Cooler**, which
shift the target by the entity's own advertised increment, read back first.
Lights additionally offer a **Brightness** level (`dim:30`).

The catalog follows the entity domain, so a light never offers `open` and a
thermostat never offers `on`. Home Assistant refuses a mode or a motion the
entity does not advertise and nothing is inferred locally: an entity without
`set_cover_position` rejects a position rather than approximating it. Cover and
climate button mappings have not been exercised against real hardware.

## Validation and current scope

Nine client tests cover discovery, service payloads, zero brightness, unavailable
and non-dimmable lights, authentication, malformed responses, invalid targets,
timeouts, and private atomic settings. Run `(cd clients && cargo test -p couch-ha)`.
`node web/tests/home-assistant.mjs` runs a loopback-only fake HA server and a
throwaway daemon, checking the real browser setup, rejected credential replacement,
light controls, room import, and mobile layout. The existing 17 daemon tests pass.

`tools/build-webui.sh` builds the WASM bundle and ARM daemon; macOS uses Zig for
ring's cryptographic C primitives, with rust-lld for the final static link.
For the standalone CLI, use the same compiler environment:

```sh
export ZIG="$PWD/build/toolchains/zig-aarch64-macos-0.15.2/zig"
export CC_armv7_unknown_linux_musleabihf="$PWD/tools/arm-musl-cc.py"
(cd clients && cargo build -p couch-ha --release --target armv7-unknown-linux-musleabihf)
```

The ARM CLI, editor and Slint room controls are deployed. The GUI reads saved
area/room order and reloads changes on the home screen; ALL ROOMS keeps rooms
accessible during setup. Rooms show a flat device list immediately; highlight a
light and press **Power** to switch its live on/off state, or **OK** to open its
controls: a tall brightness bar, and beside it a colour temperature bar where
the entity reports a range (Home Assistant is asked in Kelvin). A blind opens
the same screen with a position bar on it and Open, Stop and Close under it.
The volume keys are the brightness bar and the channel keys the colour
temperature, wherever the highlight is; see
[the key table](philips-hue.md#the-keys-on-a-lights-or-blinds-controls).
Physical Back returns home, with a 180 ms slide in both directions.
Requests run on a bounded worker queue; unavailable lights show an error in the
room and old responses cannot replace a newer screen. State refreshes automatically
every five seconds without moving focus. Aggregate room
status remains hidden until all device state is available. PIN pairing blocks
interaction with covered controls.

The physical GUI was exercised against an isolated HA fixture: room discovery,
D-pad on, and 40% brightness reached the Rust client's real HTTP service path.
Screenshots are in ignored `build/ha-device-{room,light,brightness}.png`.
`COUCH_HOME_DIR` overrides the directory containing `config.json` and
`ha-connection.json` for isolated GUI tests. Thirty GUI tests cover existing
input/Wi-Fi logic and home projection. A real Home Assistant connection and a
user-designated test light are still needed to validate household hardware. Color, temperature,
groups, other entity domains and push subscriptions are deferred until light
power and brightness have passed live testing.

Protocol references: [Home Assistant REST API](https://developers.home-assistant.io/docs/api/rest/),
[light actions](https://www.home-assistant.io/integrations/light/), and
[ureq configuration](https://docs.rs/ureq/latest/ureq/).

The new GUI modules pass rustfmt checks. Workspace-wide `cargo fmt --check`
still reports existing formatting differences in files such as `keypad.rs`;
unrelated formatting was left unchanged. Final ARM GUI is 4,060,196 bytes.
