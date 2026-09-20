Title: SDK and protocol
Description: The DeviceClient contract, protocol-v1 framing, and unreleased typed dB controls.
Order: 3

# SDK and protocol

An integration has two boundaries. `couch-sdk` defines how Rust code talks to a
device. `couch-plugin` carries that contract across a subprocess socket with a
versioned JSON protocol. The Rust crates are maintained in this repository and
can be consumed by an independent integration through one full Git commit pin;
they are not crates.io releases. Protocol version 1 is the installed preview
boundary. Protocol version 2 is specified in the current source but has not
shipped in a Couch release.

## Implementing `DeviceClient`

One client instance owns one connection. It declares its fixed capabilities,
opens the transport once, and performs commands without internal retry.

| Method | Responsibility |
| --- | --- |
| `capabilities` | List every fixed command the package implements. |
| `connect` | Validate settings and open one device transport. |
| `execute` | Perform one already-authorized command. |
| `status` | Return supported fields or `Unsupported`. |
| `inputs` | Return selectable inputs, or an empty list. |
| `supports_input` | Validate a dynamic input ID before it can be sent. |
| `actions` | Declare typed actions when the selected protocol supports them. |
| `validate_action` | Refuse an invalid typed value before device I/O. |
| `action` / `execute_action` | Perform one already-authorized typed action. |

Do not retry a command after a lost reply. The device may have completed it,
and repeating a power or input operation can produce the wrong state. Couch
retires a failed child and lets a later, explicit request reconnect.

## Framing

Each message is UTF-8 JSON prefixed by its byte length as an unsigned 32-bit
big-endian integer. The maximum frame is 64 KiB. Each envelope contains an
integer request ID and a `body`. Unknown fields are rejected.

```text
[4-byte length] {"id":1,"body":{"method":"hello","protocol_version":1}}
```

Compute the prefix from the encoded JSON bytes.

## Requests and responses

The host begins with `hello`, then sends `configure`. Handshake and
configuration must not contact the device.

```json
{"id":1,"body":{"method":"hello","protocol_version":1}}
{"id":2,"body":{"method":"configure","settings":{"host":"192.0.2.10","port":23}}}
{"id":3,"body":{"method":"command","function":"volume-up"}}
{"id":4,"body":{"method":"status"}}
{"id":5,"body":{"method":"inputs"}}
```

Successful response bodies are:

```json
{"id":1,"body":{"type":"hello","manifest":{"protocol_version":1}}}
{"id":2,"body":{"type":"ok"}}
{"id":4,"body":{"type":"status","status":{"on":true,"muted":false,"input":"hdmi1"}}}
{"id":5,"body":{"type":"inputs","inputs":[{"id":"hdmi1","name":"Console"}]}}
```

The shown hello manifest is abbreviated. The real reply contains the complete
manifest and must match the installed manifest before activation.

An error is a typed response:

```json
{"id":3,"body":{"type":"error","code":"rejected"}}
```

Codes are `invalid`, `unsupported`, `incompatible`, `protocol`, `transport`,
`timeout`, `busy`, `expired`, and `rejected`.

## Manifest

The manifest declares identity, executable, capabilities, settings, inputs,
and optional presentation:

```json
{
  "protocol_version": 1,
  "id": "example-receiver",
  "label": "Example receiver",
  "version": "0.1.0",
  "executable": "bin/couch-plugin-example-receiver",
  "capabilities": [
    {"id": "power-on", "label": "Power on"},
    {"id": "power-off", "label": "Power off"}
  ],
  "settings": [
    {"id": "host", "label": "Device address", "kind": "text", "required": true},
    {"id": "port", "label": "Port", "kind": "integer", "required": true, "default": 23},
    {"id": "token", "label": "Token", "kind": "secret", "required": false}
  ],
  "supports_inputs": true
}
```

Setting kinds are `text`, `secret`, `integer`, and `boolean`. Secret settings
cannot define defaults. Unknown settings are rejected. Couch persists private
settings outside exported house configuration and sends them through the
socket, never through arguments or environment variables.

## Deadlines and capacity

- Startup and configuration deadline: 3 seconds each
- Device operation deadline: 12 seconds, absolute across partial reads
- Endpoint queue: 8 requests
- Queue lifetime: 750 milliseconds
- Frame size: at most 64 KiB

These are host limits, not targets. A client should use a device-specific
deadline short enough to keep the UI responsive.

## Protocol-v1 limits

Protocol v1 carries commands, status, and inputs. It has no request for apps,
pairing, general discovery, or subscription to events. Adding a command outside
the shared Couch vocabulary requires a core update. Packages cannot request
privileged hardware access.

## Protocol v2: unreleased typed dB control

Protocol v2 is **unreleased**. Do not publish a v2 package or describe it as
available on the `v0.1.0-alpha.20260916.171.dev` prerelease until a Couch
release carrying protocol 2 exists. Its purpose is to represent a receiver's
real dB value without changing the meaning of the existing percentage
`volume` field.

When implemented, status may omit `volume_db`, report a reading in tenths of a
dB, or report the distinct minimum-volume state:

```json
{"volume_db":{"kind":"reading","tenths":-345}}
{"volume_db":{"kind":"minimum"}}
```

`tenths` is an integer measurement, not a floating-point approximation or a
percentage. The shared measurement range is -1000 through 300 tenths of a dB.

A v2 manifest declares its exact bounded action separately from presentation.
Denon's planned action range is shown here as a contract example, not a claim
that a package is available:

```json
{
  "protocol_version": 2,
  "min_core_protocol_version": 2,
  "actions": [
    {
      "action": "set_volume_db",
      "min_tenths": -800,
      "max_tenths": 180,
      "step_tenths": 5
    }
  ]
}
```

The action request carries a typed value rather than placing a value in a
function string:

```json
{"method":"action","action":{"action":"set_volume_db","tenths":-345}}
```

The host rejects a value outside the declared inclusive range or off its
declared step before the adapter contacts a device. The direct HTTP API uses
the same typed action object as its request body. `min_core_protocol_version`
must equal the manifest protocol version. A v2 package cannot downgrade to a
v1 host: the hello exchange selects the manifest's version and requires the
same manifest in response. Existing v1 manifests retain their byte shape and
default to a minimum core protocol version of 1.

## Protocol 3: unreleased and switched off

Protocol 3 is being built in steps on `dev`. It is **switched off**: the host
accepts protocol 1 and 2 manifests only, the feed accepts only those, and no
released or development build can install a protocol 3 package. Everything in
this section may change until the step that switches it on. Do not write a
package against it yet.

What exists so far is vocabulary in `couch-model`, so that a Couch which later
saves protocol 3 content can always be rolled back, and the wire types and host
gate in `couch-plugin` and `couch-sdk`, described under
[On the wire](#on-the-wire) below. The vocabulary:

- **Package-named buttons.** A capability id of the form `x:<id>`, where `<id>`
  is 1 to 48 bytes of lowercase letters, digits, `-` and `_`
  (`Function::Custom`). It is for the words Couch has none for (Info, the
  on-screen display, subtitles). Couch never interprets it. It is offered in the
  button picker under the package's own label, only for a device whose package
  declares that exact id, and is refused everywhere else, including in scene and
  activity steps. A package may declare at most 32. It never repeats while held.
  A protocol 1 or 2 manifest that declares one is invalid.
- **Key phase.** `KeyPhase` is `tap`, `repeat` or `long_press`, default `tap`,
  and `tap` is never written. `couch_model::buttons::key_phase(gesture, repeat)`
  maps a panel key event to it. The `command` request can carry it (below), and
  the panel sends it for every mapped key; the host passes it on only to a
  protocol 3 package, so a protocol 1 or 2 package keeps receiving the bytes it
  receives today.
- **More than one typed action.** A saved package snapshot may hold up to eight
  action schemas of distinct kinds, and a request finds its schema by kind
  (`PluginActionSchema::find`).
- **Children of a connection.** One connection (a bridge) may offer many
  devices. The saved package snapshot lists the *kinds* of child it declares
  (`Provider::Plugin.children`, at most 8): a kind has a name, a label, the
  kind of room device it becomes, one built-in control (`light`, `cover`,
  `climate` or `scene`), the commands it takes (at most 32) and the one typed
  action that goes with its control. A light may be saved as a light or a
  switch, a cover as a blind, a thermostat as a thermostat; a scene kind takes
  exactly `on` and is never a device. A package's `x:` buttons count towards
  its 32 whichever kind names them.
- **A child is an ordinary room device with a snapshot.** It is saved as
  `{"via":"connection","connection_id":…,"resource_id":…,"child":{"kind":"light","light":{"dimmable":true,"mirek":[153,500]}}}`:
  the kind, and what this particular lamp, blind or thermostat can do. With it
  a binding validates and a row can be drawn while the package is not running.
  The device resolves to what its *kind* can do, never to what the connection
  can; a kind the package no longer declares resolves to nothing it can be
  told. A child's `resource_id` is 1 to 128 bytes of `[A-Za-z0-9._/+-]` read as
  segments between `/`, none empty, `.` or `..` (`couch_model::valid_resource`);
  a connection that is one device keeps the looser rule it always had. The
  daemon, never the browser, will fill the snapshot in from the package's own
  listing; until that step exists the device routes refuse a snapshot that
  arrives in a request.
- **Light, cover and climate.** `couch_model::domain` holds the traits
  (`LightTraits`, `CoverTraits`, `ClimateTraits`) and the state a status read
  will carry (`LightState`, `CoverState`, `ClimateState`; an absent value is
  "unknown", never an inferred "off"). Everything is an integer: brightness and
  position 0 to 100, colour temperature in mirek (100 to 1000), colour as CIE xy
  in ten-thousandths, temperatures in tenths of a degree (-500 to 1500). Three
  typed actions join `set_volume_db`: `set_light` (`on`, `brightness`, `mirek`,
  `xy`; brightness 0 is off), `set_cover` (`position`) and `set_climate`
  (`target_tenths`, or `low_tenths` below `high_tenths`, and `mode`). Every
  field but `position` is optional, an absent one is left alone and is not
  written, and at least one has to be there. Their schemas carry no numbers
  (`{"action":"set_light"}`): what one lamp accepts is a trait of that child
  (`ChildSnapshot::accepts`). `dim:N`, `position:N` and `mode:<m>` are
  supported on a child when its kind declares the action and the child's traits
  allow it, and a quick-access key can toggle a light or cover child whose kind
  declares `toggle`. A connection that is itself one lamp, blind or thermostat
  may compose a `light`, `cover` or `climate` component over the matching
  action.
- **Package scenes.** `Scene.resource` (`connection_id`, `resource_id`, and the
  scene kind) is a scene that belongs to a package. It has no steps and is not
  a Hue scene; `Scene.hue` is unchanged.

  None of this can be declared yet: `couch-plugin` refuses the three components
  in every manifest and the three actions in a protocol 1 or 2 manifest, and the
  gate names protocol 3 for them. Listing children, addressing one on the wire
  and the manifest's `children` are the next step.
- **`integration_config_v3`.** The saved configuration gains a third layer for
  whatever a protocol 2 core cannot read; see
  [Compatibility and independent source](https://github.com/Couch-OS/couch/blob/main/docs/integration-architecture.md#protocol-3-layer-unreleased).

### On the wire

Four additions, each of which is absent from the bytes whenever it says nothing
new, because every protocol 1 and 2 package refuses unknown fields:

```json
{"method":"command","function":"x:info","phase":"long_press"}
{"type":"error","code":"unpaired","reason":{"kind":"message","text":"Pair this TV again"}}
{"type":"error","code":"invalid","reason":{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}}
```

- **`phase`** on `command`: `repeat` or `long_press`. A tap is never written,
  and a frame without the field is a tap. `Request::command(id)` builds a tap
  and `Request::key(id, phase)` anything else. `Request::Command` is no longer
  built as a struct literal.
- **`reason`** on `error`: `message` (text) or `invalid_setting` (the id of a
  setting the manifest declares, and text). The text is at most 160 bytes with
  no control characters and is written for the person holding the remote. An
  error without a reason is written exactly as before.
- **`unpaired`**, a tenth error code: the device wants pairing again.
- **`x:` functions**, declared as capabilities like any other.

In the SDK a client says these with `Error::Unpaired`,
`Error::Invalid.because(Reason::InvalidSetting { .. })` and
`DeviceClient::execute_phased`, whose default ignores the phase.
`Host::request_detailed`, `Endpoint::request_detailed` and
`local_request_detailed` return a `Failure { code, reason }`; `request` and
`local_request` keep their signatures and return the code alone.

### What a protocol 1 or 2 package never sees

The host decides what to send from the protocol version in the package's
manifest, in one place (`Host::request_detailed`), before any I/O:

- a key phase is **downgraded to a tap**, not refused: the key still works and
  the frame is the one the package has always received;
- an `x:` function is `unsupported`, as is one a protocol 3 package did not
  declare, and a protocol 1 or 2 manifest that declares one is invalid;
- `requires(&Request)` names the oldest protocol a request can be sent to, and
  anything newer than the package is `unsupported` without a round trip.

In the other direction, a `reason` or the `unpaired` code from a protocol 1 or
2 package is a protocol error and retires the child, as a `volume_db` reading
from a protocol 1 package always has. From a protocol 3 package, a reason whose
text is too long or has control characters, or whose `field` is not a declared
setting, does the same. The SDK's `serve` follows the manifest as well, not the
SDK it was built with: a protocol 1 or 2 package built with a newer SDK drops a
reason its client attached, reports `Error::Unpaired` as `rejected`, and only
ever tells its client of taps; a protocol 3 package drops a reason the host
would refuse and keeps the code.

This is tested three ways: golden bytes for every protocol 1 and 2 request,
response and manifest, captured from the SDK revision the published packages
were built from (`clients/couch-plugin/tests/golden/`); frozen copies of that
revision's types, which refuse unknown fields, on both sides of the host's own
gate; and `tools/tests/old-package-wire.sh`, which builds `couch-plugin-echo`
and `couch-plugin-sonos` from that revision and runs this tree's admission and
subprocess suites against those executables.

### What reaches the panel and the web page

The daemon carries a refusal whole, in both directions it serves:

- **The panel's socket** (`plugin.sock`). The request frame is unchanged
  (`LocalRequest`); a `command` may now carry a `phase`, which the daemon hands
  to the host untouched, and the host's gate decides whether the package is
  told. A refusal comes back as the `error` frame with the package's `reason`
  when it gave one. Without a reason the frame is the one it always was,
  `{"type":"error","code":"unsupported"}`.
- **HTTP** (`/api/connections/<id>/plugin/...`). The error body keeps `error`,
  the sentence every client already shows, and gains `code` and, when there is
  one, `reason`:

  ```json
  {"error":"The device refused the request","code":"rejected"}
  {"error":"The port must not be 0","code":"invalid",
   "reason":{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}}
  ```

  `error` is the reason's text when there is a reason, so a client that reads
  only `error` still shows the better sentence. `unpaired` answers 409; every
  other status is what it was (400 for `invalid` and `unsupported`, 503 for
  `busy` and `expired`, 502 otherwise, and 400 for anything that stops settings
  from being saved).

What a person sees:

- **On the remote**, a mapped key that fails raises the toast: Couch's own
  sentence for the code, and the package's line under it. The toast holds two
  lines of about 35 characters at the panel's width and ends a longer one with
  an ellipsis, so put the words that matter first: "The TV is locked", not
  "An error occurred because the TV is locked". The device screen shows the
  same two lines in its error box, which wraps, and a package's own pages in
  their two-line status area. Which transport is tried next (infrared, Bluetooth) depends on the
  code alone: a reason changes what is said, never what is done.
- **On the settings form** in the web UI, an `invalid_setting` reason outlines
  the setting it names and puts the text under that control; the form's status
  line says which setting to check. Editing the setting clears it. Any other
  reason, and a refusal with none, is the form's status line, as before. Where
  a setting is refused with no form in sight (a connection saved by a built-in
  client being handed to its package), the sentence names the setting by its
  label: "Port: The port must not be 0".

A held volume key on a device whose package declares a decibel range is not a
stream of `repeat` commands: the panel turns the hold into absolute
`set_volume_db` writes, as it did before protocol 3, and those carry no phase.

None of this is reachable on a remote yet. With the switch off no package can
give a reason, and a phase is dropped by the host before it is written. The
daemon and the panel are tested with constructed failures and with scripted
protocol 1 and 2 packages (the Denon 0.2.1 manifest receives a held and a
long-pressed `volume-up` as the bytes of a tap); the path from the panel's
socket to a protocol 3 package and back runs in `couch-echo`'s
`tests/protocol3.rs`, with the preview on.

### The switch

`couch-plugin` has one Cargo feature, `protocol-3-preview`. It is off by
default, adds no dependency, and changes one function:

```rust
pub const PROTOCOL_VERSION: u32 = 2;          // what a release supports
pub const NEXT_PROTOCOL_VERSION: u32 = 3;
pub const fn accepted_protocol_version() -> u32; // 2, or 3 with the feature
```

`Manifest::validate` accepts `1..=accepted_protocol_version()`. With the feature
off, which is every build that ships, a manifest that says 3 is `incompatible`
(a package that needs a newer Couch) on the host and in `serve`, and nothing is
executed. Only tests turn it on:

```sh
cd clients
cargo test --features couch-plugin/protocol-3-preview,couch-echo/protocol-3-preview
```

`couch-echo` has a fixture for it, `couch-plugin-echo-v3` (`src/v3.rs`,
`tests/protocol3.rs`), built only with the feature: it declares `x:info`, passes
the key phase to its fake television, and explains its refusals.

Cargo unifies features across a build, so a single dependency that enabled the
feature, even a dev-dependency, would enable it for everything built with it.
Two tests keep that from reaching a remote: `couch-confd` and
`couch-integrations` each assert
`accepted_protocol_version() == PROTOCOL_VERSION`, run with the `daemon`
workspace's own feature set in the `daemon` and `product-flow` CI jobs, and fail
the moment anything in that workspace turns the feature on. To look by hand:

```sh
cargo tree --manifest-path daemon/Cargo.toml -e features -i couch-plugin | grep protocol-3
cargo tree --manifest-path ui/Cargo.toml -e features -i couch-plugin | grep protocol-3
```

Both print nothing.

## Source references

- [`clients/couch-plugin/src/protocol.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/protocol.rs)
- [`clients/couch-plugin/src/manifest.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/manifest.rs)
- [`clients/couch-sdk/src/client.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-sdk/src/client.rs)
- [`model/couch-model/src/commands.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/commands.rs)
- [`model/couch-model/src/storage.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/storage.rs)
