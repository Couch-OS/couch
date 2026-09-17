Title: SDK and protocol
Description: The DeviceClient contract and protocol-v1 framed JSON messages.
Order: 3

# SDK and protocol

An integration has two boundaries. `couch-sdk` defines how Rust code talks to a
device. `couch-plugin` carries that contract across a subprocess socket with a
versioned JSON protocol. The Rust crates are maintained in this repository and
can be consumed by an independent integration through one full Git commit pin;
they are not crates.io releases. Protocol version 1 is the installation boundary.

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

## Source references

- [`clients/couch-plugin/src/protocol.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-plugin/src/protocol.rs)
- [`clients/couch-plugin/src/manifest.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-plugin/src/manifest.rs)
- [`clients/couch-sdk/src/client.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-sdk/src/client.rs)
