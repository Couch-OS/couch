# Integration package architecture and migration readiness

An external integration is a network-facing adapter process owned by
`couch-confd`. It has one connection per configured device, receives private
settings over its inherited framed socket, and exposes only the declared
protocol capabilities, status, inputs, actions, and native presentation data.
It does not own browser or Slint code, create a second long-lived device
connection, or gain direct access to HA100 hardware.

```text
browser / remote panel
        │ declared native controls
        ▼
couch-confd ── one framed plugin child per connection ── device transport
        │
        └── private settings and installed-package state
```

## Package decision and ownership

Signed Alpine APKs distribute independently versioned network integration
executables. The core runtime updater continues to own the daemon, device UI,
operating system, and shared native controls. The framed, versioned JSON
subprocess boundary, rather than a Rust dynamic-library ABI, is what permits an
integration to be built and installed independently.

This is a developer preview. Echo and Denon demonstrate the package path;
existing built-in integrations remain available. Denon is the first
independently sourced package. The published
`v0.1.0-alpha.20260916.171.dev` prerelease provides the protocol-v1 host and
paired web package management. It is not a production compatibility guarantee.
The older `.170` runtime does not contain the package host.

`couch-confd` owns one worker and plugin process per configured connection.
The browser API and remote panel submit requests to that same worker. Its queue
and request age are bounded, stale presses expire instead of replaying after a
slow recovery, and an ambiguous command is never retried automatically. A later
explicit request may restart a failed child.

Private settings live beside the house configuration in the connection store.
They travel over the inherited socket, never in arguments or environment
variables. On a root-started HA100 daemon, the host clears inherited groups,
drops the child to UID and primary GID 65534, enables `no_new_privs`, and grants
only the `AID_INET` supplemental group needed for ordinary network sockets.
Other root-started targets receive no supplemental groups. Non-root development
hosts retain their existing credentials. Plugins share an unprivileged UID and
LAN access; this is privilege separation, not a sandbox for hostile code.

The installer invokes `apk` in a temporary root, verifies signatures, disables
scripts and network access during extraction, and accepts only the integration
payload. It does not install dependencies into the live Alpine root. A plugin
ships a self-contained executable. Activation atomically switches the active
and previous immutable version/hash pair only after validation. Configuration
and credential writers hold shared package-store leases until durable; admission
and rollback hold the exclusive lease while validating saved connections and
switching the selected version.

The paired web **Integrations** page owns refresh, install, update, restore,
and removal while retaining saved settings. One management operation runs at a
time. The SSH CLI remains for signed sideloads and repair. Official repository
URLs and their public key are built into the signed core. Custom repositories
require an HTTPS URL, supplied public key, and a confirmed displayed
fingerprint; their trust directories never enlarge official or Alpine system
trust. Removing a repository leaves packages and connections intact.

## Compatibility and independent source

The integration-capable core writes an atomic configuration envelope. Ordinary
fields retain a legacy-native projection. `integration_config` keeps a
protocol-v1 projection, while optional `integration_config_v2` preserves the
complete enhanced configuration. The v1 projection strips protocol-v2 dB
components and actions, and makes space-containing input actions inactive. The
legacy-native projection also omits bindings the old input parser cannot read.
This is deliberate degradation for an older core, not v2 package support.

The current core validates the projections against the preserved v2 extension
before writing. It restores v2-only controls and bindings only from a matching
extension. If a rollback core writes configuration, that old-core edit drops
the extension and remains authoritative after re-upgrade; discarded bindings
do not silently return. An explicitly restorable recovery export can restore a
prior state. Missing packages leave saved connection and activity configuration
intact, with execution unavailable. A v2 package remains incompatible with an
older host; existing v1 previous-slot fallback is unchanged.

Each payload is immutable by integration ID and manifest version. Any payload
change needs a new manifest version and matching APK `pkgver`; changing only an
APK revision such as `-r0` to `-r1` cannot replace an existing slot. Sideloading
still requires an explicitly trusted signing key.

An independent source repository pins `couch-plugin` and `couch-sdk` to the
same full Couch commit and runs the shared admission harness. It must not carry
a private protocol copy. The schema-2 feed source graph separately pins the
shared tooling and each integration repository; its provenance records source,
SDK, tooling revisions, plus binary and manifest hashes. Each integration
repository owns its `integration.json`, `plugin.json`, `Cargo.lock`, adapter,
and reusable-harness tests.

This is a contribution-planning matrix, not a delivery schedule. “Can migrate
now” means the current protocol boundary represents the adapter's supported
user-facing contract. It does not mean that an APK exists, that a package has
passed admission, or that a device has been validated.

## Current adapters at the package boundary

| Adapter | Current implementation evidence | Package readiness | Blocking contract or migration work |
| --- | --- | --- | --- |
| Denon | `couch-denon` has a `DeviceClient`, manifest, package binary, catalog cases, and an explicit per-connection pilot. | **Can migrate now as protocol-v1 preview.** | Full receiver hardware evidence remains outstanding. dB status/control and space-containing source bindings wait for protocol v2 and the shared input grammar work. |
| Sonos | `couch-sonos::sdk::Client` already implements `DeviceClient` for playback, volume, mute, status, and inputs. | **Package-shaped adapter; not yet a migration.** | Needs a package binary/manifest, four package cases, catalog/feed admission, and an explicit owner/configuration migration. Discovery is not carried by v1. |
| Echo | `couch-echo` is a packaged `DeviceClient` with fake-peer admission coverage. | **Test-only fixture.** | It is fictional and must never become a device-support migration. |
| Kodi | The client has JSON-RPC playback, chapters, streams, and notification handling. | **Needs a richer media/event contract.** | Protocol v1 cannot express unsolicited notifications, chapters, stream selection, or richer media state. |
| LG webOS | The client performs explicit pairing, subscriptions, inputs, app listing, and app launch. | **Needs pairing, apps, and event contracts.** | v1 has no interactive pairing, subscription, app-list, or launch request. |
| Samsung Tizen | The client performs user-approved pairing, app discovery/launch, and WebSocket control. | **Needs pairing, apps, and event contracts.** | The same v1 gaps block a faithful migration. |
| Apple TV | The client maintains pairing credentials and Companion/media metadata flows. | **Needs pairing and media/event contracts.** | v1 cannot carry its pairing exchange, metadata flow, or unsolicited state. |
| Android / Google TV | The client performs mutual-TLS pairing, remote control, Cast status, and launch URLs. | **Needs pairing, discovery, apps, and media/event contracts.** | v1 lacks those interactive and streamed contracts. |
| CoreELEC | The client combines Kodi control, SSDP discovery, and explicitly configured OS management. | **Needs discovery, media/event, and privileged management boundaries.** | Package v1 cannot own OS-management behavior or Kodi's richer state. |
| Home Assistant / Hue | These clients discover entities or bridges and perform pairing plus device-domain-specific control. | **Needs discovery, pairing, and typed domain controls.** | A generic command list cannot faithfully represent changing entities, light/cover/climate semantics, or bridge enrollment. |
| Matter | The controller owns commissioning, a fabric, discovery, and cluster interactions. | **Needs commissioning, fabric, discovery, and typed-cluster contracts.** | Those credentials and operations cannot cross v1 as ordinary settings/commands. |
| UniFi Protect | The client discovers cameras and supports authenticated media/snapshot and RTSPS handling. | **Needs discovery and media-stream contracts.** | v1 supplies neither media delivery nor the required live event/control boundary. |
| IR, Bluetooth, voice | These clients open `/dev/irtx`, Bluetooth transport, ALSA/input, or other HA100 services. | **Remain built in.** | Packages have no privileged hardware access; a separate narrowly scoped hardware contract would be required. |

The matrix deliberately names contracts visible in the source today. A network
protocol being reachable from a package does not by itself make it ready to
migrate: the persisted configuration, one-owner rule, native controls,
admission harness, and rollback behavior must all still be represented.

## Migration sequence

1. Compare the built-in adapter's actual settings, commands, status, inputs,
   pairing/discovery behavior, events, media data, and hardware access with the
   selected protocol version.
2. If the boundary can represent it, add the package wrapper, manifest, fake
   device tests, and four catalog cases. Keep the adapter's transport logic in
   the package rather than duplicating it in the UI.
3. Add a named, opt-in per-connection migration. Prepare and validate package
   settings before changing provider ownership; never leave two live owners for
   one device endpoint.
4. Test migration, restore, core rollback, package removal/reinstall, and
   configuration edits with the existing daemon/product-flow checks. Record
   unsupported behaviors as limitations.
5. Advance a feed only after its independent source pin, locked Couch SDK and
   plugin dependencies, ARM build, package admission, and device evidence have
   been reviewed.

Denon's [migration guide](integration-migration.md) is the implemented pilot.
It shows the required preservation and rollback behavior; it is not a template
for claiming that every built-in integration is ready.

## Protocol v2 boundary: unreleased

Protocol v2 adds an optional `volume_db` status measurement and a bounded,
declared `set_volume_db` action. It is **unreleased** and does not change this
matrix's protocol-v1 readiness. For a receiver such as Denon, it can close the
dB readout and absolute-control gap only after a release contains protocol 2,
the package declares `min_core_protocol_version: 2`, and its action and UI
coverage pass admission. It does not add pairing, discovery, apps, media,
events, or privileged hardware access.

Protocol v1 remains limited to commands, status, inputs, and typed settings.
Pairing, discovery, application launching, unsolicited events, and privileged
hardware access stay built in until a separately reviewed contract covers them.
Every new protocol operation or native component needs compatibility and UI
coverage before it can change this matrix.

Read the [developer protocol reference](development/protocol.md),
[component rules](development/components.md), and
[admission policy](development/admission.md) before proposing a new boundary.
