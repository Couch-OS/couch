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
released or ordinary development build can install a protocol 3 package. The
one exception is a preview build made on purpose for a single development
remote, which is labelled, signed and refused as described under
[A preview build for one development remote](#a-preview-build-for-one-development-remote).
Everything in this section may change until the step that switches it on. Do
not write a package against it yet.

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
  daemon, never the browser, fills the snapshot in from the package's own
  listing: see [the daemon's side](#the-daemon-listing-the-children-of-a-connection).
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

  A package declares all of this in its manifest, in `children`, and only a
  protocol 3 manifest may: the three components, the three actions and
  `children` itself are all invalid in a protocol 1 or 2 manifest, so no
  package Couch can already run is ever asked to list a child or told about
  one.
- **Pairing.** A manifest may say `"pairing": {"required": true, "max_seconds": 120}`
  (10 to 300 seconds) and `"keep_alive": true`. Only a protocol 3 manifest may:
  an older one that declares either is invalid, so no package Couch can already
  run is ever sent a pairing request, told a key, or exempted from the idle
  reaper. The package describes the steps; Couch draws the dialog and keeps the
  key. Nothing about pairing is written to `config.json`: "paired" is derived
  from the key file's existence and the rules come from the live manifest, so
  a Couch rolled back to protocol 2 needs no new projection rule.
- **`integration_config_v3`.** The saved configuration gains a third layer for
  whatever a protocol 2 core cannot read; see
  [Compatibility and independent source](https://github.com/Couch-OS/couch/blob/main/docs/integration-architecture.md#protocol-3-layer-unreleased).

### On the wire

Every addition is absent from the bytes whenever it says nothing new, because
every protocol 1 and 2 package refuses unknown fields:

```json
{"method":"command","function":"x:info","phase":"long_press"}
{"type":"error","code":"unpaired","reason":{"kind":"message","text":"Pair this TV again"}}
{"type":"error","code":"invalid","reason":{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}}
{"method":"children","cursor":"room/9d2b7c10"}
{"type":"children","children":[{"id":"5f0c9a52","kind":"light","name":"Desk","room_hint":"Study","light":{"dimmable":true,"mirek":[153,500]}}],"next":"room/9d2b7c10"}
{"method":"status","resource":"5f0c9a52"}
{"method":"action","action":{"action":"set_light","brightness":30},"resource":"5f0c9a52"}
{"type":"status","status":{"light":{"on":true,"brightness":30,"mirek":366}}}
{"method":"configure","settings":{"host":"bridge.local"},"credential":{"application_key":"…"}}
{"method":"pair_start","settings":{"host":"bridge.local"}}
{"type":"pairing","session":"p1","step":{"step":"waiting","prompt":{"kind":"press_button","message":"The button is on top"},"poll_after_ms":2000}}
{"method":"pair_continue","session":"p1","input":{"kind":"code","code":"0417"}}
{"type":"pairing","session":"p1","step":{"step":"done","credential":{"application_key":"…"},"summary":"Paired with the hall bridge"}}
{"method":"pair_cancel","session":"p1"}
{"id":7,"body":{"type":"status","status":{"on":true}},"store_credential":{"application_key":"…"}}
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
- **`children`**, a request for one page of the children behind this
  connection, and the `children` response that answers it. `cursor` is absent
  on the first page; `next` is absent on the last one, and a page that carries
  a cursor is never empty. A page holds at most 32 children, a whole listing
  at most 1024 over at most 64 pages, and the host gives the listing ten
  seconds end to end. A cursor that comes round again, a page that is too big,
  an empty page with a cursor, the same id twice anywhere in the listing, a
  kind the manifest never declared, traits that are not the ones that kind's
  control has, or an id that is not a resource: every one of them is a
  protocol error and the package is retired. `couch_plugin::list_children`
  owns all of it, so no caller has to.
- **`resource`** on `command`, `action` and `status`: which child of the
  connection the request is for. It is the id the package itself gave out in
  its listing, and the same grammar as a saved `resource_id`. A request that
  names one carries nothing else new - the kind is *not* on the wire, because
  the package knows what its own children are and Couch only has to know what
  it may say to them.
- **light, cover and climate state** on `status`, which only a request that
  named a child can get back. A write that named a child may be answered with
  the state the child is in afterwards (`status` instead of `ok`), which saves
  the panel a second round trip after a slider; a plain `ok` stays legal and
  the caller then reads.
- **`credential`** on `configure` and on `pair_start`: the key Couch is
  holding for this connection, an opaque JSON object of at most 16 KiB. Absent
  whenever there is none. Couch never looks inside it, never sends it over
  HTTP and never exports it.
- **`pair_start`, `pair_continue` and `pair_cancel`**, and the `pairing`
  answer to the first two. A package names the session on its first step and
  repeats it on every one after; `pair_cancel` is answered `ok`. A step is
  `waiting` with a prompt and how long to wait, `done` with the key, an
  optional corrected `settings` and a one-line `summary`, or `failed` with
  `unreachable`, `refused`, `wrong_code`, `timed_out` or `unsupported` and an
  optional message. The three prompts are `press_button`, `approve_on_device`
  and `enter_code` (a length of 1 to 16 and an alphabet of `digits`, `hex` or
  `alphanumeric`); Couch writes its own headline for each and the package's
  `message` goes under it.
- **`store_credential`** beside a reply, which is the only thing that travels
  with one. It is a key the device rotated, and it is legal only from a
  protocol 3 package that declares `pairing`, and only on the answer to a
  `command`, `action`, `status`, `inputs` or `children` request - never to a
  `hello`, a `configure` or any `pair_*`, each of which has its own way of
  saying what it means. It is surfaced only through
  `Host::request_full(..) -> Result<(Response, Option<Credential>), Failure>`;
  every other signature drops it, so no existing caller can store one by
  accident and no log line can carry one.

Every limit is the host's, checked before the bytes are written or as soon as
they are read, and a package that breaks one is answering nonsense: the reply
is a protocol error and the child is retired.

| | |
| --- | --- |
| credential | a JSON object, at most 16 KiB serialized |
| session | 1 to 64 bytes of `[A-Za-z0-9._-]`, and equal to the one the host is holding |
| `poll_after_ms` | 0 only with `enter_code`, otherwise 500 to 10 000 |
| summary, prompt and failure message | at most 160 bytes, no control characters |
| `done.settings` | passes the package's own `manifest.validate_settings` |
| `enter_code.length` | 1 to 16 |
| `pairing.max_seconds` | 10 to 300 |

A code the person typed is measured against the prompt that asked for it -
its length and its alphabet - **before** anything is sent, so a mistyped code
costs no round trip and the package is never asked. A prompt that asked for
nothing is never given anything, and a prompt waiting for typed input is never
polled.

A package that declares `pairing` must also have made itself undumpable. The
SDK's `serve` calls `prctl(PR_SET_DUMPABLE, 0)` as its first act, which has to
happen in the child because `execve` puts the flag back for a program the new
user can read, and every package slot is. The host checks it after the
handshake: when Couch runs as root and the child's `/proc/<pid>/environ` still
belongs to the package's own user rather than to root, a manifest that declares
`pairing` is refused `incompatible` and nothing is executed. An unprivileged
host - a developer's machine, CI, every host test - shares its user with its
children, where that ownership says nothing, and skips the check. Packages
published before that SDK stay dumpable and are unaffected, because they
declare no pairing and hold no key.

A level is the one thing that changes shape on the way out. `dim:30`,
`position:40` and `mode:heat` are ordinary commands in a button map, in a scene
step and in the configuration file, because that is what a person binds and
what the file has always held. Aimed at a child, the host turns each one into
the typed action that child's kind declares - `set_light` with a brightness,
`set_cover` with a position, `set_climate` with a mode - and it does so in the
gate and nowhere else, so no caller has to know which children are lamps. Aimed
at the connection itself the command is untouched and passes or fails on the
connection's own capabilities exactly as it did before protocol 3; the golden
bytes for all three are pinned. A level whose kind is drawn with another
control is `unsupported` before any I/O, and whether this *particular* lamp can
be dimmed at all was decided earlier still, when the key was bound
(`ChildSnapshot::accepts`).

In the SDK a client says these with `Error::Unpaired`,
`Error::Invalid.because(Reason::InvalidSetting { .. })` and
`DeviceClient::execute_phased`, whose default ignores the phase.
`Host::request_detailed`, `Endpoint::request_detailed` and
`local_request_detailed` return a `Failure { code, reason }`; `request` and
`local_request` keep their signatures and return the code alone. A request that
names a child goes through `request_child_detailed(kind, request)` instead: the
kind is what the gate checks against and is never written. On the package's
side the defaulted `DeviceClient::child_kinds`, `children`, `child_command`,
`child_action` and `child_status` answer for one child at a time; see
[`docs/client-sdk.md`](https://github.com/Couch-OS/couch/blob/main/docs/client-sdk.md).

### What a protocol 1 or 2 package never sees

The host decides what to send from the protocol version in the package's
manifest, in one place (`Host::request_detailed`), before any I/O:

- a key phase is **downgraded to a tap**, not refused: the key still works and
  the frame is the one the package has always received;
- an `x:` function is `unsupported`, as is one a protocol 3 package did not
  declare, and a protocol 1 or 2 manifest that declares one is invalid;
- `requires(&Request)` names the oldest protocol a request can be sent to, and
  anything newer than the package is `unsupported` without a round trip. It is
  3 for a listing, for any request that names a child, and for the three child
  actions, which is what keeps `resource` and `children` out of the bytes a
  published package reads;
- a **key is stripped, not refused**, the same way and for the same reason as a
  phase: `Configure.credential` is set to `None` for any manifest below
  protocol 3 **or** without `pairing`, even when Couch is holding one, so a
  package rolled back from protocol 3 to 2 with a key file beside it is still
  configured, with today's exact bytes. That is the only place a key can leave
  the host;
- `pair_start`, `pair_continue` and `pair_cancel` `require` protocol 3, and are
  then `unsupported` again unless the manifest declares `pairing`;
- a `status` **with** a resource is the one frame an old package would not
  refuse. Its `Status` was a unit variant, and serde lets a unit variant ignore
  the fields of an internally tagged frame even with `deny_unknown_fields`, so
  such a child would answer for the whole connection as though no child had
  been named. Nothing on the package's side can prevent that; the host's gate
  is the only thing that does, and `wire_mirror` asserts it.

In the other direction, a `pairing` answer or a `store_credential` from a
protocol 1 or 2 package, or from a protocol 3 one that never declared
`pairing`, is a protocol error and retires the child. So is a `reason` or the
`unpaired` code from a protocol 1 or
2 package, as a `volume_db` reading
from a protocol 1 package always has. So is a listing, a light, cover or
climate reading, and a `status` as the answer to a write - the last needs no
version rule of its own, because only a request that named a child may be
answered that way and such a package is never sent one. From a protocol 3 package, a reason whose
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

### The daemon: listing the children of a connection

A bridge's lamps reach a person through the daemon, and everything below is
unreachable in a shipped build: no manifest it accepts may declare a kind of
child, so `Provider::Plugin.children` is always empty, every route here answers
404 "This integration does not list devices", and nothing is ever stamped.

**The routes**, under `/api/connections/<id>/plugin/`. The router has no query
strings, so a child's id is the path between `children` and the verb, and the
verb is always the last segment. Empty segments never reach the router and the
resource grammar refuses `.` and `..`, so an id cannot climb out of its
connection.

| Route | Body | Answers |
| --- | --- | --- |
| `GET …/plugin/children` | - | the listing, from the cache if it is fresh |
| `POST …/plugin/children/refresh` | - | the same, always read from the package |
| `GET …/plugin/children/<id…>/status` | - | that child's `status` |
| `POST …/plugin/children/<id…>/action` | `{"command":"toggle"}` | `{"accepted":true}`, or the state the write left |
| `POST …/plugin/children/<id…>/typed-action` | a `TypedAction`, as the connection's own route takes one | the same |

The listing:

```json
{"kinds": [{"kind": "light", "label": "Light", "device_kind": "light",
            "component": "light", "capabilities": [{"id": "toggle", "label": "Toggle"}],
            "actions": [{"action": "set_light"}]}],
 "children": [{"id": "lamp/1", "kind": "light", "name": "Desk", "room_hint": "Study",
               "light": {"dimmable": true, "mirek": [153, 500]},
               "assigned": {"room": "living-room", "device": "living-lamp"}},
              {"id": "lamp/2", "kind": "light", "name": "Reading", "assigned": null}],
 "missing": [{"id": "lamp/9", "kind": "light", "name": "Corner lamp",
              "assigned": {"room": "living-room", "device": "living-lamp"}}],
 "fetched_ms": 0}
```

- `kinds` is the connection's saved snapshot of what its package declares, so
  a page can label a kind and know which device a child becomes.
- Each child is exactly what the package listed, plus `assigned`: `null`, or
  `{"room":…,"device":…}` for a child already in a room, or `{"scene":…}` for
  one already saved as a package scene.
- `missing` is the other direction: a device or scene made from a child the
  connection has stopped listing. Nothing is ever deleted or altered because of
  it - the device, its keys and its scenes stay as they are - it is named so
  the page can badge it and offer a manual Remove. `kind` is `null` for a
  device a rollback stripped that has not healed yet.
- `fetched_ms` is how long ago the package was asked, in milliseconds.

A refusal is the `code`/`reason` body every other integration route uses. An id
the connection does not list is 404 on these routes and 400 when a device or
scene is being saved with it.

**The cache.** One listing per connection, in memory only, read on demand and
kept for five minutes. It is dropped when the connection's settings are saved,
when the connection is deleted, when a reap finds it stale, and whenever the
package selection or the settings it was read through have changed. A listing
that ends in a protocol error retires the package's process and keeps the last
good listing: the devices already made from it are real, and a stale list is
more use to somebody choosing than none.

Reading a listing is up to 64 round trips and up to ten seconds, and the daemon
holds the connection's lock for all of it, so anything else aimed at that
connection is answered `busy` while a cold listing runs. Another connection is
not held up. **The panel never starts a listing**: it works from the saved
devices, which carry their own snapshot, so its 750 ms queue is never behind
one. Only the browser and the saving of a device or scene do.

A listing is also the one request `Runtime::execute` refuses outright, so
neither `plugin.sock` nor an HTTP body can start one: its limits are kept by
`couch_plugin::list_children`, which only the daemon calls.

**The kind of a request's resource.** Every request that names a child needs to
know which kind that child is, because that is what the host's gate checks it
against. The daemon derives it and the caller never supplies it:

1. the saved configuration - a device's `child.kind` or a scene's
   `resource.kind` - so a device already in a room works with the cache cold;
2. failing that, the cached listing, which is how a page can try a lamp before
   adding it;
3. failing that, the request is refused without any I/O.

Both are scoped to the connection the request named. The connection id alone
selects the package process and its private settings, so a resource borrowed
from another connection's device finds no kind and can never be carried to
another connection's package - two connections may each have a `lamp/1` and
they are not the same thing.

`plugin.sock` is unchanged: `LocalRequest` is still `{connection_id, request}`,
the resource rides inside the request as it does over HTTP, and the daemon
derives the kind the same way.

**Stamping.** A device that is one child, and a package scene, carry what they
are; it decides which commands validate and how the row is drawn, so it is the
package's to say. On create and on update the daemon throws away whatever the
request carried under `child` or under a scene resource's `kind`, and fills it
in itself from the connection's listing. A browser sends only:

```json
{"name": "Desk lamp", "integration": {"via": "connection", "connection_id": "bridge", "resource_id": "lamp/1"}}
{"name": "Relax", "rooms": ["living-room"], "resource": {"connection_id": "bridge", "resource_id": "scene/1"}}
```

The device's kind is forced to the one its child kind says, so a lamp cannot be
saved as a television. A scene kind cannot be saved as a device, and a device
kind cannot be saved as a scene. An id the connection does not list is 400.

Two things keep this out of the way of everything else. A connection whose
package declares no kinds of child is never touched - which is every connection
in a shipped build - and neither is a device of one that names no resource,
because that device is the connection itself. And an edit that leaves the
connection and the resource where they were keeps the snapshot already saved
and asks the package nothing, so renaming a lamp works with its bridge
unplugged; only a new device, or one pointed at a different child, reads the
listing.

**Healing.** A Couch without children strips a device's snapshot on the way out
and keeps its connection and resource (`v2_projection`), so the Couch that
reads the file next knows what the device was. The next listing read afresh
puts the snapshot back and the device kind with it. Only devices of the
connection just listed are looked at.

**`refresh_plugin_metadata`** copies `children` from the package's manifest as
it copies the label and the capabilities, and keeps a kind the manifest has
dropped while a saved device or scene still says it is one. A saved snapshot is
validated against that list, so forgetting such a kind would stop the daemon
starting over a device nobody had touched.

**A package for a newer Couch.** `couch_integrations::read_manifest` reads
`protocol_version` on its own before parsing the manifest strictly, and a
version above this build's says "This integration needs a newer Couch (protocol
N)". Such a manifest cannot be parsed strictly at all - it carries tags and
fields this build's `Manifest` does not know - and "invalid" is the wrong thing
to say about a package that is only ahead of the remote. The feed refuses one
before it is downloaded, from its signed metadata ("Needs a newer Couch"); this
is the backstop for one already on the remote, and it reaches the Integrations
page and the conversion of a former built-in through the same reader.

### The daemon: pairing a connection

A package describes the steps; Couch draws the dialog, collects what the person
types and keeps the key. None of this is reachable in a shipped build: only a
protocol 3 manifest may declare `pairing`, and no protocol 3 manifest is
accepted, so every route below answers 400 "This integration does not pair" and
`paired` is absent from the settings reply.

**The routes**, under `/api/connections/<id>/plugin/`, inside the connection's
read gate like every other one there.

| Route | Body | Answers |
| --- | --- | --- |
| `POST …/plugin/pair` | `{"settings": {…}}`, or nothing | `{"session": …, "step": …, "expires_in": …}` |
| `POST …/plugin/pair/<session>` | `{"input": {…}}`, or nothing | `{"step": …}` |
| `DELETE …/plugin/pair/<session>` | - | `{"cancelled": true}` |
| `DELETE …/plugin/credential` | - | `{"paired": false}` |

Only the first and the last ask the package store anything, because only they
have to know what the package is. Continuing and cancelling are answered by
whether the session is there, and a package that does not pair can never have
one - so a poll is never refused because an install happens to be holding the
store, and with the switch off a continue is 404 and a cancel is
`{"cancelled": true}` rather than "does not pair".

`<session>` is 128 bits of the host's own, in hexadecimal. It is **not** the
session the package minted for itself, which never leaves the host, and it only
means anything on the connection it was issued for.

`settings` on `pair_start` is what the package is asked to pair with: whatever
the person has typed, merged over what is saved. It is **not saved** - a
pairing that fails leaves the connection exactly as it was - so a page that
wants the typed settings kept saves them through `POST …/plugin/settings`
first. Settings the manifest refuses are 400 before anything is started.

Every step:

```json
{"step":"waiting","prompt":{"kind":"press_button","message":"The button is on top"},"poll_after_ms":2000}
{"step":"waiting","prompt":{"kind":"approve_on_device"},"poll_after_ms":2000}
{"step":"waiting","prompt":{"kind":"enter_code","message":"It is on the screen","length":4,"alphabet":"digits"},"poll_after_ms":0}
{"step":"done","summary":"Paired with the hall television","settings":{"settings":{"host":"tv.local","port":9299},"configured":true,"secrets":[]}}
{"step":"failed","reason":"wrong_code","message":"That was not the code on the screen"}
```

A `done` may also carry `warning`, one sentence of Couch's own, and only when
something beside the key could not be kept - settings the device corrected
that would not save, or that the manifest refused. It is **absent** from every
ordinary pairing. The key itself is stored either way: it is the device's and
it is good, and the alternative to saying this would be answering `done` over
settings that were quietly dropped.

A prompt is exactly what the package sent, `message` and all, and `message` is
absent when it said nothing. **A `done` never carries the key**: it is taken out
inside `Runtime`, before anything in `api/` can see it, and what is left is the
summary and the redacted settings view the settings route answers with.
`reason` is one of `unreachable`, `refused`, `wrong_code`, `timed_out` and
`unsupported`; `message` is absent unless the package wrote one.

Statuses: 400 for settings the manifest refuses (the `code`/`reason` body every
other integration route uses), 400 "This integration does not pair", 400 "That
is not what this device asked for" for an input the prompt did not ask for, 404
"That pairing is no longer in progress", 409 when eight pairings are already in
flight, 503 while a call on the same session is still out, and 500 if the key
cannot be written. A cancel is idempotent: it answers `{"cancelled": true}`
whether or not there was anything to cancel.

`GET …/plugin/settings` gains three keys, and only for a package that pairs:

```json
{"settings": {"host": "tv.local"}, "configured": true, "secrets": [],
 "paired": true, "pairing": {"required": true}, "summary": "Paired with the hall television"}
```

`paired` is whether the key file exists; `pairing.required` is the manifest's;
`summary` is the line the pairing left and is absent until there is one.

**The session.** One pairing per connection, eight across the remote. A second
start on a connection cancels and replaces the first, and the first token stops
working. The child is this pairing's own - never in the endpoint registry, so no
reap, package update or settings write takes it away - and it is never restarted:
a package that stops answering ends the attempt as `failed {"reason":
"unreachable"}`. **The connection's ordinary child keeps serving on the old key
throughout**, so a television that is working keeps working while it is being
paired again.

A package that is **updated or removed** while one of its connections is being
paired ends that attempt: `failed {"reason": "unsupported"}` with Couch's own
sentence, and nothing written. Which version is selected is read from the
package's state file, never through the store's lock, so an install running
somewhere else on the remote - which holds that lock for seconds - does not end
a pairing; a store that cannot say just now is "unknown", not "it moved".

**A pairing that has been ended writes nothing, whatever the package says
next.** A call may be inside the package for up to twelve seconds. A cancel, a
second start, a deleted connection or a sweep takes the session out of the map
and marks it, without waiting: the call sees that the moment the package
answers, tells the package its conversation is over and stores nothing - not
even a `done` that arrived a millisecond too late.

Each call gets the ordinary twelve second request timeout. The attempt as a
whole gets `min(pairing.max_seconds, 300)` seconds. A dialog that polls and then
stops - the tab was closed, the browser was killed - ends the attempt after
`max(3 × poll_after_ms, 15 s)` of silence; a prompt waiting for a typed code has
only the overall deadline, because a person typing is not a browser that has
gone. The existing ten-second reaper sweeps both, tells the package with two
seconds to hear it, and kills the child.

**The locks.** A pairing call does not hold the connection's settings lock while
it is talking to the package, so a status read on the same connection is not
answered `busy` for twelve seconds. Only the write at the end takes it, and it
waits fifteen seconds for it: the key is already in hand and asking the device
again would mean pairing again. If it is still busy the result is parked and the
dialog is told to come back in a second with the prompt it was already showing;
the next poll retries the write and the package is never asked twice.

**What a `done` writes**, in this order: the key to
`connections/<id>/plugin-credential.json` (atomic, mode 0600, in a folder forced
to 0700, under the same lock as `plugin-connection.json`); then, only if the
step carried corrected settings, those settings through `with_defaults` and
`validate_settings`; then `plugin-pairing.json`, `{"summary": …, "paired_at":
…}`, which is not a secret and is removed with the key. The connection's child
is then dropped, so the next request starts one configured with the new key.
**A pairing that fails, is cancelled or runs out writes nothing at all**, and a
key that was already there is untouched.

`plugin-pairing.json` also records **which package** the key was made for. A
connection whose package has been changed since is not paired: what the old one
left behind is not a key the new one can use, and handing it over would be
handing one package's secret to another. A file an earlier Couch wrote names no
package and is taken at face value.

**A key Couch cannot read is a key Couch has not got.** A credential file that
is corrupt, truncated or over the limit counts as absent - said in the log once,
not once per key press - so the connection answers `unpaired`, the settings page
still loads and "Pair again" is reachable. Refusing the request instead would
strand the connection with no way back, and it would do so for a protocol 1 or 2
package too, which never asked for a key at all.

**A line a package asks Couch to show is shown to a person**, and a person's
screen is not where a key goes. A `summary`, a prompt line or a failure message
that carries any string of eight characters or more out of the key - the one
being handed over, or the one Couch already held - ends the attempt, stores
nothing and retires the child.

**Nothing of this is in `config.json`.** `paired` is derived from the file and
the rules come from the live manifest, so a paired connection's configuration is
byte for byte an unpaired one's and a rollback needs no new projection rule. The
key is never returned over HTTP, never logged, and is not in the configuration
export or the recovery export, which are both built from `config.json` alone.
Deleting the connection removes it with the rest of the folder.

**Forgetting a pairing** (`DELETE …/plugin/credential`) removes Couch's copy and
the line beside it under the connection's lock, along with any half-written
temporary either write left behind, drops the child and any pairing in flight,
and **says nothing to the device**: Couch cannot revoke what it was given, and a
television that still lists Couch as paired is the device's business. The
pairing in flight is ended before the lock is taken, not after, so a pairing
that is finishing and a forget cannot wait on each other.

**`unpaired` without a package process.** When the manifest says
`pairing.required` and there is no key, a request is refused `unpaired` (409)
before a child is started at all - the same answer the package would give,
without the process.

**A key the device rotated** (`store_credential` beside a reply) is written
under the connection's lock before the body is returned, and only then: on the
reply to a `command`, `action`, `status` or `inputs` request, on the
connection's ordinary child, when a key is already there (a rotation cannot
create a pairing), when it is within the 16 KiB limit, and when no pairing on
that connection has finished since the request was queued. Anything else is
logged, without the key, and dropped. A failure to write it still returns the
reading: the device answered.

The host also lets a key ride on a `children` reply, but the daemon reads a
listing through `Endpoint::request_detailed`, which drops it, so **a key offered
on a listing is never stored**. That is deliberate: a listing is up to
sixty-four round trips and the daemon would have no one reply to write it
against.

Once it is written, the connection's child is **left standing but counted as
stale**, so the next request starts one configured from the file. It has to be:
the endpoint keeps its own copy of the key for the replacement child it launches
after a failure, and that copy is the one from before the rotation.

**`plugin.sock` is untouched.** `Runtime::execute` is the whole of what the
panel's socket and the HTTP command routes reach, and it has only ever accepted
`command`, `action`, `status` and `inputs`; every pairing request and
`configure` are `unsupported` there, as they were.

**`keep_alive`.** A manifest that declares it exempts that connection's child
from the two idle retains, and from nothing else. It is honoured only while a
room device still refers to the connection, and for at most **eight always-on
package processes** at once - one connection is one child, so a package with
three connections would be three of the eight. Beyond that cap the least
recently used lose the exemption and are reaped exactly as any other idle child
is, and so does one whose package has since been updated or removed, which
nothing else would notice because a pinned child is never asked for anything.

The reaper works out which connections a device still refers to **before** it
locks the package registry, never while holding it. Deleting a connection takes
the configuration store and then the registry, in that order; a sweep that asked
the configuration from inside the registry would take them the other way round,
and one sweep and one delete would wait on each other for good while every
configuration read on the remote queued behind them.

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
executed. Tests turn it on, and so does the one deliberate preview build
described below:

```sh
cd clients
cargo test --features couch-plugin/protocol-3-preview,couch-echo/protocol-3-preview
```

`couch-echo` has two fixtures for it, built only with the feature and both in
`src/v3.rs`, exercised by `tests/protocol3.rs`. `couch-plugin-echo-v3` is the
television: it declares `x:info`, passes the key phase to its fake set, and
explains its refusals. `couch-plugin-echo-bridge` is a connection with
children: seventy lamps, three room groups, five scenes, a blind and a
thermostat - eighty children, which is three pages - all in memory, where a
write is acknowledged with the state it left behind. Its `hostile` setting
picks one of four ways a bridge can fail to end a listing (a cursor that comes
round again, an oversized page, a kind it never declared, the same child on two
pages); each one is a protocol error and costs the package its process.
`couch-plugin-echo-pair` is a television that has to be paired: all three
prompts, an approve that takes several polls, a wrong code, an expiry, a `done`
that corrects the settings, and a key the set rotates on the next reading.
`couch-plugin-echo-pair-hostile` is a package written by hand rather than
through `serve`, because `serve` cannot emit any of what it does - its
constructors clamp a step, it mints the session itself and it never attaches a
key to a reply that may not carry one. Its `hostile` setting picks one of five
ways to answer nonsense about a key: `oversized`, `wrong_session`, `bad_poll`
and `credential_on_configure`, each of which the host sees in the single answer
and retires the child for, and `leaks_credential`, which copies the key into a
later reading - something the host cannot see at all, and only someone who
knows the key can.

`couch_plugin::testing_v3::children` and `couch_plugin::testing_v3::pairing`
are the admission cases a real package with children, or with pairing, will
use.

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

### A preview build for one development remote

A real protocol 3 package can only be tried on hardware by a daemon that admits
protocol 3. There is one way to make such a daemon, and every step after it is
built to keep that daemon on the one development remote it was made for.

**Making it.** On the release host:

```sh
COUCH_PROTOCOL_3_PREVIEW=dev-remote-only tools/build-release.sh
```

The value has to be exactly `dev-remote-only`; anything else that is not empty
is a usage error (exit 2) and not an ordinary build. The script prints a banner
first and last. It passes the variable to `tools/build-webui.sh`, which builds
`couch-confd` alone with `-p couch-confd --features
couch-plugin/protocol-3-preview`, the form CI uses. Everything else -
`couch-gui`, `couch-system`, `couch-sonos`, `couch-coreelec`, the Bluetooth
binaries - is built exactly as in an ordinary build: only the daemon reads a
manifest. `tools/build-webui.sh --host` takes the same variable.

**The marker.** A Cargo feature shows in no file name, tree hash or version. So
a daemon that admits protocol 3 says so in its own bytes: a `#[used]` static in
`daemon/couch-confd/src/assets.rs` holds the line
`COUCH-PREVIEW-BUILD protocol-3`, and its value is computed from
`accepted_protocol_version() > PROTOCOL_VERSION`, not from a second `cfg`, so it
cannot be separated from the switch. An ordinary daemon does not contain the
line at all. It survives the release profile (LTO, one codegen unit, `strip`)
on the ARM target. A test in that file reads its own test binary and asserts
the line is there exactly when the switch is on, whichever way it was built.

After **every** build, preview or not, `tools/build-webui.sh` searches the
daemon it just built (`grep -a -F`) and stops unless the line is present
exactly when a preview was asked for. That is also what catches a preview
daemon left in `daemon/target` by an earlier build. `tools/build-release.sh`
checks again at the end, and checks that no other binary carries the line.
Cargo keys its artefacts on the feature set, so going from a preview build to
an ordinary one in the same target directory recompiles `couch-plugin` and
`couch-confd`; the check is there so that nobody has to take that on trust.

**The label.** `build.json` holds the version and nothing else, and the update
channels are decided from the version, so the version is the label:
`v0.1.0-alpha.<date>.<N>.p3.dev`. `dev` keeps it off the Alpha and Stable
channels; `p3` says what it is. It sorts above `<N>.dev` and below
`<N+1>.dev`, so the next ordinary dev build replaces it
(`docs/runtime-updates.md`, "Release channels"). The remote's panel shows
`.p3.dev`; the web UI shows the whole version, and a red notice, "Protocol 3
preview build. Development remote only.", on the Updates and Integrations
pages whenever the installed version has a `p3` identifier.

**What refuses what.**

| Where | Refuses |
|-------|---------|
| `couch_updates::bundle`, the only code that holds the signing seed | A `couch-confd` carrying the line under any version whose prerelease lacks a `p3` or a `dev` identifier, or that the Alpha or Stable channel would take: "A protocol 3 preview runtime may only be signed as a .p3.dev build". An ordinary `couch-confd` under a version with a `p3` identifier: "Only a protocol 3 preview runtime may be signed as a .p3.dev build; this couch-confd is an ordinary one". Nothing is written in either case. The bytes searched are the bytes archived. |
| `tools/release/update_floor.py --tree` | A tree whose `couch-confd` carries the line, unless `--allow-preview` is given. |
| `tools/release/runtime_inventory.py` | Nothing, but `payload-inventory.json` records `"preview_features": ["protocol-3"]` (`[]` for an ordinary daemon) and lists the preview among the release blockers. |
| `tools/build-webui.sh`, `tools/build-release.sh` | A daemon whose marker does not match what was asked for, and the marker in any other binary. |

`tools/release/verify_integration_set.py` needs no change and gives no
protection here: it compares source trees at the contract paths, the marker is
in none of them, and a Cargo feature is not a source change. The publisher's
check is the one that matters. It is in the `couch-updates` binary, so the
publisher on the signing machine has to be rebuilt from a tree that has it
(`cargo build --release -p couch-updates` in `daemon`) before it protects
anything.

**What the preview daemon still says.** `couch-confd
--supports-integration-protocol=1` and `=2` exit 0 and `=3` does not, exactly
as in an ordinary build. That answer is what package installation scripts and
the release evidence rely on, protocol 3 is not released, and the preview must
not teach anything to believe otherwise.

**Afterwards.** The preview release is deleted from GitHub as soon as the test
session ends, and the remote is moved to the next ordinary dev build, or rolled
back. The remotes that could be offered it meanwhile are the ones on `.173` or
later whose owner chose the Dev channel, and no others: an updater up to `.170`
lists only the archived `dangerouslaser/couch` repository and accepts only
assets and signed URLs under it, so it never sees a release published on
`Couch-OS/couch`
([Who can see a `.dev` release](https://github.com/Couch-OS/couch/blob/main/docs/runtime-updates.md#who-can-see-a-dev-release)).

## Source references

- [`clients/couch-plugin/src/protocol.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/protocol.rs)
- [`clients/couch-plugin/src/manifest.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/manifest.rs)
- [`clients/couch-sdk/src/client.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-sdk/src/client.rs)
- [`clients/couch-sdk/src/pairing.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-sdk/src/pairing.rs)
- [`clients/couch-sdk/src/children.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-sdk/src/children.rs)
- [`model/couch-model/src/domain.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/domain.rs)
- [`model/couch-model/src/commands.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/commands.rs)
- [`model/couch-model/src/storage.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/storage.rs)
