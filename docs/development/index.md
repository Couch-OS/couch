Title: Build an integration
Description: Start here to extend Couch with a network device integration.
Order: 1

# Build an integration

Couch integrations turn a device protocol into commands, status, and selectable
inputs that the remote can use. The current integration package work is a
**developer preview**. The published `v0.1.0-alpha.20260916.171.dev`
prerelease contains the protocol-v1 package host, but it is not a production
compatibility promise. A package must still run only against the protocol
version and Couch source or release that it declares and tests.

You can develop and test an integration on Linux or macOS without a remote or a
real device. The example client uses a local fake television. Hardware is still
needed before you can claim that a real product works.

## Choose a path

For a new network integration, start with a package. It runs as a separate
process, can be installed independently of the base OS, and uses a versioned
JSON protocol at the process boundary.

Existing built-in clients remain linked into Couch itself. Echo and Denon are
the external-package pilot. Use the built-in path when a feature needs
privileged hardware, a pairing or discovery flow that protocol v1 cannot
express, application launching, or unsolicited device events. It requires a
core change and ships with a Couch release.

## What protocol v1 supports

- A fixed, declared command vocabulary
- Device status: power, playback, mute, volume, input, and title
- Enumerated inputs and `input:<id>` selection
- Typed settings kept in Couch's private connection store
- Native Couch controls assembled from a small component library

Protocol v1 does not provide general pairing, discovery, application lists,
application launching, unsolicited events, privileged hardware access, or
package-supplied HTML, JavaScript, Slint, and arbitrary layouts.

## The shortest path to a working package

1. Copy `clients/couch-echo` and rename the crate, client type, binary, and
   package ID.
2. Implement `DeviceClient` for one connection to one device.
3. Declare the same commands in `plugin.json` and
   `DeviceClient::capabilities()`.
4. Add subprocess tests against a fake device.
5. Cross-compile the binary for `armv7-unknown-linux-musleabihf`.
6. Package the binary and manifest into a signed APK, then test a sideload.

The [getting-started tutorial](getting-started.md) walks through those steps.
Read the [protocol reference](protocol.md) before writing an adapter by hand,
use the [native component reference](components.md) to design its controls, and
check the [migration readiness matrix](https://github.com/dangerouslaser/couch/blob/main/docs/integration-architecture.md)
before planning a move from a built-in client.

## Is a built-in integration ready to move?

Denon is the only current protocol-v1 migration pilot; its package path is
usable as a preview, with its recorded receiver-validation and dB/input limits.
Sonos already has a `DeviceClient` adapter, but still needs a package wrapper,
admission, and an opt-in ownership migration. Kodi, webOS, Tizen, Apple TV,
Android TV, CoreELEC, Home Assistant, Hue, Matter, and UniFi Protect need at
least one missing pairing, discovery, media, events, apps, or typed-domain
contract. IR, Bluetooth, and voice retain HA100 hardware access and stay built
in. The full matrix identifies the source evidence and exact blocking boundary
for each adapter.

## Architecture

```text
Couch web UI / remote panel
          │
          ▼
   couch-confd host
          │  one owned subprocess per configured connection
          │  u32 big-endian length + JSON
          ▼
  couch-plugin adapter
          │
          ▼
     your device
```

The host sends private settings across the inherited socket. It never places
credentials in command-line arguments or environment variables. Standard
output belongs exclusively to framed protocol messages.

## Keep source and published documentation together

The project site renders these Markdown files from one exact Couch commit. Its
links back to source use that same immutable revision. A documentation change
does not appear on the site merely because it lands in this repository: the
site repository must deliberately advance its `source-pin` after reviewing the
Couch commit. Preview a local documentation change with the renderer described
in the site repository; do not represent a local preview as published guidance.

## Read the source

The public documentation is generated from this directory at the exact Couch
commit pinned by the website build. The implementation lives in
[`clients/couch-plugin`](https://github.com/dangerouslaser/couch/tree/main/clients/couch-plugin),
the reusable client contract in
[`clients/couch-sdk`](https://github.com/dangerouslaser/couch/tree/main/clients/couch-sdk),
and the complete example in
[`clients/couch-echo`](https://github.com/dangerouslaser/couch/tree/main/clients/couch-echo).
