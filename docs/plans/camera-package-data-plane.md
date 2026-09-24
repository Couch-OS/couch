# Camera package data plane

Status: package data plane complete in source 2026-09-24. Protocol 4 is the
normal host contract; package SDK serving, inherited fd 3, daemon forwarding,
native GUI decoding and the standalone UniFi package are connected. Automatic
migration from the retained built-in integration and physical HA100 acceptance
remain.

## Outcome

A camera integration is an ordinary independently installed Couch package.
The package owns provider APIs, credentials, certificate pins, stream URLs,
RTSP and any transport encryption. Couch owns camera selection, the native UI,
the bounded H264 decoder and the framebuffer. No provider-specific API or
certificate format crosses the package boundary.

For UniFi Protect, setup asks for exactly two values: the NVR's LAN IP address
and an Integration API key. Certificate observation and verification remain an
implementation detail of the package.

## Why this is protocol 4

Protocol 3 frames are strict JSON capped at 64 KiB and are request/response.
That is appropriate for children, commands and small state, but not for a live
H264 stream. Reusing protocol 3 would either overload its compatibility promise
or encourage large video data to pass through JSON.

Protocol 4 adds a camera child component and one inherited binary socket. A
protocol 1-3 package receives exactly the process descriptors and wire bytes it
does today. The core continues to accept versions 1-3 until the full protocol 4
host, package harness, UI and migration land together.

## Ownership and secrets

```text
UniFi package                 couch-confd                 couch-gui
-------------                 -----------                 ---------
API key + certificate pins
HTTPS discovery
RTSPS + SRTP
H264 depacketisation  ->  bounded local H264 records  ->  couch-camera
                                                        FFmpeg -> RGB
```

- The API key is a secret package setting. It is never placed in the house
  configuration, process arguments, environment, browser responses or logs.
- Exact API and media leaf pins are an opaque package credential stored by
  Couch at mode 0600. The package creates them during its connection flow.
- Stream URLs, RTSP session IDs and SRTP keying material never leave the
  unprivileged package process.
- The core sees only a connection ID, camera resource ID, safe status codes and
  Annex-B H264 access units.
- FFmpeg remains core-owned and gets only `pipe:0`; it has no network protocol
  and no provider secrets.

## Manifest and child model

Protocol 4 adds `camera` to `ChildComponent`. A camera kind must use
`DeviceKind::Camera`, declares no commands or typed actions, and may be saved as
a room device like any other package child. A listed camera carries only its
stable resource ID, display name and optional room hint. Availability is read
when a view or snapshot is explicitly requested; it is not persisted as a
truth claim.

The Protect manifest has:

```json
{
  "protocol_version": 4,
  "min_core_protocol_version": 4,
  "id": "unifi-protect",
  "settings": [
    {"id":"address","label":"NVR IP address","kind":"text","required":true},
    {"id":"api_key","label":"API key","kind":"secret","required":true}
  ],
  "children": [
    {"kind":"camera","label":"Camera","device_kind":"camera","component":"camera"}
  ],
  "pairing": {"required":true,"max_seconds":30},
  "keep_alive": true
}
```

`configure` remains validation-only and performs no network I/O. The generic
connection flow saves the two settings and starts pairing. Protect's pairing
step observes both certificate leaves without sending the API key, constructs
the exact pinned clients, verifies the key by listing cameras, and returns the
two pins as the opaque credential. This is still only two values from the
person; “pairing” is the existing package mechanism for securely writing back
device-derived credentials.

## Control frames

All control messages retain the existing length-prefixed JSON envelope and
limits. Protocol 4 adds:

- `camera_snapshot { resource, offset }`: a bounded JPEG read. Responses carry
  at most 48 KiB of base64 data plus `total` and `offset`; the host assembles at
  most 8 MiB and verifies the JPEG signature. Reads are idempotent.
- `camera_open { resource }`: start one explicit view. The successful response
  says `codec: h264_annex_b` and a duration no greater than 60 seconds.
- `camera_close { resource }`: cancel that view and answer `ok`. Dropping the
  host or side channel has the same effect.

The host gates all three by the manifest's camera child kind and by the cached
child/resource association before the package sees them. There is at most one
open camera per package child process. A second open is `busy`; it never steals
or silently replaces the first view.

## Binary side channel

At package spawn the host creates a second `UnixStream::pair`. The package end
is inherited as descriptor 3; the core end stays private to the endpoint owner.
The environment remains empty, stdin/stdout remain exclusively JSON, stderr
remains closed, and the package still runs under its assigned unprivileged user
with `no_new_privs` and no core dumps.

After `camera_open` succeeds, the package writes records to descriptor 3:

```text
u32 big-endian payload length | payload bytes
```

- `1..=2 MiB` is one complete Annex-B H264 access-unit group.
- `0` is a clean end of stream.
- A length above 2 MiB, a write before a successful open, or bytes after the
  terminal record is a protocol violation and retires the child.
- The package never writes a URL, key, JSON or provider metadata on this
  channel.

The socket buffer is the queue. The package uses an absolute 60-second view
deadline and cancellable reads/writes, so a hidden UI or stalled decoder cannot
accumulate video or leave a network session behind. `couch-confd` forwards one
record at a time to the authenticated local camera socket; it never buffers
more than one record. The GUI writes that payload to `couch-camera`, which
keeps exactly one latest 480x270 RGB frame.

The side channel is deliberately not a general package file descriptor API.
It exists only for protocol 4 camera children, has one codec and one bounded
record type, and is never exposed to the browser.

## Failures

The package returns provider-neutral stages so the UI can remain useful without
knowing UniFi:

- `not_configured`: settings or stored credential are missing;
- `unpaired`: a certificate changed or the saved pins no longer authenticate;
- `unavailable`: the camera or requested low-bandwidth stream is unavailable;
- `transport`: the device or stream cannot be reached;
- `protocol`: the device or package sent invalid data.

Messages may carry a short safe reason, subject to the existing protocol 3
reason bounds. They must never include addresses with path/query data, API
keys, certificate bytes, RTSP headers or decoder stderr.

## Migration and rollback

The external repository is `couch-integration-unifi-protect`. Its first useful
release is protocol 4; a still-only protocol 3 release would create a second,
partial setup and would not retire the built-in integration.

When the package is installed, the converter moves each built-in Protect
connection as one transaction:

- address and API key become package settings;
- API/media pins become the opaque package credential;
- every saved `camera_id` becomes that connection's camera resource ID;
- the connection provider becomes the package and records its camera child
  kind.

The legacy `Provider::UnifiProtect` and resolved integration variant remain
readable for rollback, but new connections use only the package after the
converter ships. Core removal happens only after package installation,
migration, snapshots and live video pass on the physical remote and one core
release has shipped without a regression.

## Delivery order

1. Move the FFmpeg process and fixed RGB contract into provider-neutral
   `couch-camera`. **Complete in source.** The built-in bridge's `media`
   feature composes that decoder with a separate `media-transport` feature, so
   the future package can take RTSPS/SRTP without pulling FFmpeg ownership back
   out of core.
2. Add protocol 4 camera model/control types with byte-for-byte protocol 1-3
   golden tests. **Complete in source.** The v4 storage layer projects camera
   kinds and snapshots out for a protocol-3 rollback. Snapshot chunks, view
   duration and camera-child routing are bounded at the host gate, and
   `couch_sdk::camera` owns the binary record codec.
3. Add the inherited side channel, bounds, cancellation and hostile-package
   tests to `couch-plugin` and `couch-confd`. **Complete in source.** Only a v4
   child receives fd 3; the host enforces the open/view deadline, bounded
   Annex-B records, the clean terminal marker and retires a child that writes
   before open. The daemon forwards one record at a time and a closed local
   view closes the package view.
4. Make the GUI camera controller use the local camera socket and
   `couch-camera`, with no UniFi dependency. **Complete in source.**
5. Cut `couch-integration-unifi-protect` from the API/RTSPS code, add package
   admission tests, and keep the core copy until migration acceptance passes.
   **Published in the preview feed.**
6. Add the converter, install the preview package on the development remote,
   and validate discovery, snapshot, live view, close, screen-off cancellation,
   certificate change and rollback.
7. Publish only with an explicit repository/feed scope and provenance record;
   then remove the built-in client in a later core release.
