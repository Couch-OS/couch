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
saves protocol 3 content can always be rolled back:

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
  maps a panel key event to it. No request carries it yet; when one does, a
  protocol 1 or 2 package will keep receiving the bytes it receives today.
- **More than one typed action.** A saved package snapshot may hold up to eight
  action schemas of distinct kinds, and a request finds its schema by kind
  (`PluginActionSchema::find`). `set_volume_db` is still the only kind, so
  nothing can declare a second one yet.
- **`integration_config_v3`.** The saved configuration gains a third layer for
  whatever a protocol 2 core cannot read; see
  [Compatibility and independent source](https://github.com/Couch-OS/couch/blob/main/docs/integration-architecture.md#protocol-3-layer-unreleased).

## Source references

- [`clients/couch-plugin/src/protocol.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/protocol.rs)
- [`clients/couch-plugin/src/manifest.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/src/manifest.rs)
- [`clients/couch-sdk/src/client.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-sdk/src/client.rs)
- [`model/couch-model/src/commands.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/commands.rs)
- [`model/couch-model/src/storage.rs`](https://github.com/Couch-OS/couch/blob/main/model/couch-model/src/storage.rs)
