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

This is a developer preview. Integrations live in their own repositories and
the OS carries only the host that runs them. Denon, Philips Hue, and LG webOS
have left the OS for independently sourced packages. Their clients are no
longer linked into Couch, and connections saved before that convert by
themselves ([Built-in integrations that became packages](integration-migration.md)).
Other built-in integrations remain until their packages can replace them; Echo
is the in-tree template. The published
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
variables.

Each installed package runs as a user of its own. The package store keeps the
table in `uids.json` at its root: one row per package id, allocated in order
from 60000 to 64999, group id equal to user id, written under the store's
exclusive lock, and never held by two packages at once. A removed package
keeps its row, so a package installed later does not inherit a user something
else once ran as; the only two things that reuse a number are the range
running out, which drops the rows of packages that are no longer installed and
takes the lowest free number, and a table too damaged to read, which is
rebuilt from the sorted installed ids. A rebuild is counted, and the daemon
retires its package children when the count moves, so each comes back as the
user the table now gives it.

A package is given its user when it is installed, and packages installed by an
older Couch are given theirs in one pass when the daemon starts. A busy store
is a busy answer that the next key press retries; a range genuinely full is an
error that says so and is not retried, because waiting changes nothing; a
table that cannot be written keeps the number in memory for the life of the
daemon and says so in the log. No package is ever quietly started as another
package's user, or as the 65534 they used to share.

On a root-started HA100 daemon, the host clears inherited groups, drops the
child to that user and group, enables `no_new_privs`, sets `RLIMIT_CORE` to
zero, and grants only the `AID_INET` supplemental group needed for ordinary
network sockets. Other root-started targets receive no supplemental groups.
Non-root development hosts retain their existing credentials, and the
per-package user simply does not apply there. The package's own half of it is
in the SDK: `serve` makes the child undumpable before anything else, which is
what puts `/proc/<pid>` beyond every other user's reach. It has to happen in
the child, because `execve` undoes it.

Nothing on disk belongs to these users. No package file is chowned and no
package file changes mode, so a core rolled back to a release that knows
nothing of the table runs every package exactly as it did, all under one user,
and leaves `uids.json` alone.

On the HA100's 3.18 kernel there is no Yama and `/proc` carries no `hidepid`,
so what one package cannot do to another comes from the user difference alone;
being undumpable is what separates two connections of one package, and only
for packages rebuilt against this SDK. This is privilege separation, not a
sandbox for hostile code: a package still reaches the LAN and still sees that
other processes exist. What is and is not enforced is listed under
[Isolation between packages](integration-packages.md#isolation-between-packages).

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

### Protocol 3 layer

Protocol 3 is the current package contract. The envelope has one more optional layer,
`integration_config_v3`, written only when the configuration holds something a
protocol 2 core cannot read. The first such thing is a package-named button
(`x:<id>`): as a declared capability, inside a command group or switch, or bound
to a key, a step, an on/off sequence, a page button or a scene. The second is the
children of a connection, below. The layers beneath it are
computed from one another (`integration_config_v2` is the v3 document with
those removed, `integration_config` is the v1 projection of that, the ordinary
fields are the legacy projection of that), so they are exactly what a released
core writes and checks. A configuration with nothing new in it produces the
same bytes as before.

The second thing only that layer holds is the children of a connection: the
kinds of child a package declares, the snapshot (kind and traits) saved with a
room device that is one of them, package scenes (`Scene.resource`), the
`set_light`, `set_cover` and `set_climate` actions, and a `light`, `cover` or
`climate` component. A protocol 2 core ignores a field it does not know on a
device, a connection or a scene, so none of these would stop it parsing; each is
removed from the layer it reads because of what it would do with the rest. It
validates a key against the connection's commands, not the kind's, so
`dim:30` or `toggle` bound to a lamp of a bridge stops its daemon starting; what
it did accept it would send with no resource, to the whole bridge; and it cannot
parse the new action and component tags at all. So in the v2 layer:

1. the kinds a connection declares are cleared;
2. a child's snapshot is cleared and its `connection_id` and `resource_id` are
   kept, so the device stays where it is;
3. everything aimed at a child goes, whatever the command: a key stays as an
   explicitly disabled key, steps, on and off commands and page buttons are
   removed and the activity forgets the device; a quick-access key that toggles
   a child is dropped, one that opens it stays;
4. a package scene is removed, with every area's reference to it;
5. typed actions are cut to the one that core knows, and
6. the three components are removed.

What a rolled-back remote shows, then: the lamps stay in their rooms as rows
that do nothing (that core refuses a protocol 3 package, so nothing is sent);
package scenes are gone until the remote is updated again; keys and steps that
were aimed at those lamps are lost for good if the old version saves, exactly as
`x:` bindings are. After the next update the devices heal by themselves: they
are still there with their connection and resource, and listing the
connection's children says again what each one is.

A rolled-back protocol 2 core ignores the v3 key, loads the v2 layer, and
validates it. If it saves, the v3 key is gone and that save is authoritative
after re-upgrade: stripped bindings stay as explicitly disabled keys and do not
return. A v3 layer that does not match the layers beneath it is refused rather
than half-loaded. `tools/tests/config-crossload.sh` builds the model source of
the last release beside the current one and exchanges real files in both
directions; it runs in CI on every change to the model.

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
| Denon | Lives in `Couch-OS/couch-integration-denon`: `DeviceClient`, manifest, package binary and admission cases, published in the feed. | **Moved; the built-in client is removed.** Saved connections convert automatically. | Full receiver hardware evidence remains outstanding. Space-containing source bindings need a protocol-v2 core (shared input grammar). |
| Sonos | `couch-sonos::sdk::Client` already implements `DeviceClient` for playback, volume, mute, status, and inputs. | **Package-shaped adapter; not yet a migration.** | Needs a package binary/manifest, four package cases, catalog/feed admission, and an explicit owner/configuration migration. Discovery is not carried by v1. |
| Echo | `couch-echo` is a packaged `DeviceClient` with fake-peer admission coverage. | **Test-only fixture.** | It is fictional and must never become a device-support migration. |
| Kodi | The client has JSON-RPC playback, chapters, streams, and notification handling. | **Needs a richer media/event contract.** | Protocol v1 cannot express unsolicited notifications, chapters, stream selection, or richer media state. |
| LG webOS | External `couch-integration-webos` implements pairing, status, inputs, apps, and the television command profile. | **Moved; the built-in client is removed.** Saved connections convert automatically. | Network wake and the old private power sidecar are not package capabilities; use core device IR for power-on where needed. |
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

Denon's [retirement](integration-migration.md) is the implemented case: a
table of departed built-ins, a `Legacy*` variant kept so the file still loads,
and an automatic, one-way conversion that keeps the connection's id. It shows
the required preservation and rollback behavior; it is not a claim that every
built-in integration is ready.

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

## Further reading

- [Web package management and repository trust](integration-management.md)
- [Package format and lifecycle](integration-packages.md)
- [Developer guide](development/index.md)
- [Core runtime updates](runtime-updates.md)
- [Integration separation](integration-separation.md) — moving a built-in
  integration to an independent repository: readiness, missing capabilities,
  and retirement constraints.
