# Independent integration packages

## Decision

Use signed Alpine APKs to distribute independently versioned network integration
executables. Keep the core runtime updater for the daemon, device UI, operating
system, and shared native controls. A framed, versioned JSON protocol separates
an integration from the core's Rust build and ABI.

This is a developer preview. Echo and Denon demonstrate the package path;
existing built-in integrations remain available. Denon is also staged as the
first independently sourced package. The curated feed pins its repository and
the shared Couch tooling separately, while automatic package updates remain
outside this design. The integration-capable development runtime now provides
paired web package management and persistent, explicitly trusted custom feeds;
this does not make those capabilities available in the released `.170` runtime.

## Why this boundary

An APK containing another Rust crate would still require rebuilding the host.
The subprocess boundary is what makes independent installation possible. APK
provides signed artifacts, a signed repository index, architecture metadata, and
existing Alpine tooling. Couch owns activation, rollback, configuration, and
the connection to the device.

| Benefit | Cost or constraint |
| --- | --- |
| Ship an integration fix without a whole runtime update | Maintain a separate package release and trust process |
| Develop and test one client on a laptop | Cross-compile its final binary for ARMv7 and validate real hardware before claiming support |
| Avoid a Rust dynamic-library ABI | Serialize requests and supervise a separate process |
| Retain a working package during an update | Store immutable versions and validate both protocol and saved settings |
| Contain a crash and enforce request deadlines | A process boundary is not a complete security sandbox |
| Keep a consistent interface across community clients | New control primitives still require a reviewed core release |

## Runtime ownership

`couch-confd` owns one worker and plugin process per configured connection. The
browser API and device UI submit requests to the same worker. Its queue and
request age are bounded; stale button presses expire instead of replaying after
a slow device recovers. Ambiguous command failures are never automatically
retried. A later explicit request may restart a failed child.

Private settings live beside the house configuration in the daemon's connection
store. They travel to the child over its inherited socket, not in arguments or
environment variables. On Linux the host drops the child's root privileges and
sets `no_new_privs`. On the HA100's ARMv7 musl target, the Android-derived
kernel enables `CONFIG_ANDROID_PARANOID_NETWORK`; the host gives the child only
the `AID_INET` supplementary group (GID 3003), after clearing inherited groups,
so normal TCP/UDP sockets work without `CAP_NET_RAW`. It keeps UID and primary
GID 65534. This applies when the daemon starts it as root; non-root development
hosts retain their existing credentials. Other root-spawned targets receive no
supplementary groups. Plugins share an unprivileged UID and retain network
access; they are trusted code, not a safe way to execute arbitrary hostile
software.

The package installer invokes `apk` in a temporary root, verifies signatures,
disables scripts and network access during extraction, and accepts only the
integration payload. It does not install dependencies into the live Alpine
root. Plugins must ship a self-contained executable. Activation atomically
replaces the active/previous version and hash pair only after validation.
Configuration and credential writers hold shared package-store leases until
their saves are durable; admission and rollback hold the exclusive lease while
checking every saved connection and switching the selected version.

The paired web **Integrations** page owns the user-facing package lifecycle:
refresh signed indexes, install a catalog selection, update, restore the previous
version, and remove a package while retaining connection settings. One management
operation runs at a time with progress and failure status. Installed-package
status distinguishes a verified fallback, invalid selection and missing package.
The CLI remains available for explicit signed sideloads and manual operations.

Official repository URLs and their public key are built into the signed core
executable, avoiding a new OTA payload filename or an SSH key-provisioning step.
Custom repositories require an HTTPS URL, a supplied public key and confirmation
of its displayed fingerprint. Each source has a separate trust directory; custom
keys never expand official or Alpine system trust. Removing a repository leaves
already installed packages and connections intact.

## Interface policy

Contributors declare command groups, status text, toggles, and input selectors
in their manifest. The browser and device render these through Couch's own
controls. Packages cannot supply HTML, JavaScript, Slint, or arbitrary layouts.
This keeps styling, interaction, and accessibility in the core while letting
integrations choose meaningful controls and labels.

Protocol v1 covers commands, status, inputs, and typed settings. Pairing,
discovery, application launching, unsolicited events, and privileged hardware
access remain on the built-in path until their contracts are defined. Adding a
new protocol operation or control must include compatibility and UI tests.

## Rollout and compatibility

The first capable core update adds the host, configuration types, UI and package
manager. It is sufficient even when the retained rollback core predates plugins.
The daemon writes one atomic configuration envelope: ordinary fields contain a
valid legacy projection with external devices unconfigured, and
`integration_config` retains the complete modern configuration. Current daemon
and GUI disk readers use the complete document; old readers ignore the extension.

If an old core writes configuration during rollback, those edits remain
authoritative on re-upgrade. A separate confirmed recovery export supports
explicit restoration without automatically resurrecting deleted bindings. An
interrupted save remains a separately labelled candidate until its contents are
shown to match the committed envelope. See
[runtime compatibility](runtime-updates.md#integration-configuration-across-core-rollback).
Install and rollback validate candidate settings without contacting the device.
Missing packages leave connection and activity configuration intact, with
execution unavailable.

Package payloads are immutable by integration ID and manifest version. Any
payload change requires a new manifest version and matching APK `pkgver`; an
APK release suffix change such as `-r0` to `-r1` cannot replace an existing slot.

Keep official repository admission separate from sideloading. A sideload still
needs an explicitly trusted signing key. Inclusion in the curated catalog also
requires shared contract and burst/failure tests, package verification, review,
and hardware evidence for production support. See the
[testing and compatibility guide](development/testing.md) for those requirements.

An independent source repository must pin `couch-plugin` and `couch-sdk` to one
full Couch commit and call the shared admission harness. It must not carry a
private copy of the protocol. The staged schema-2 source-pin graph independently pins the shared tooling and
each integration repository at full commits. Schema-2 package provenance records
the source, SDK and tooling repositories and commits plus binary and manifest
hashes, so a package can be reconstructed without treating the monorepo as the
owner of every integration. Each integration repository owns `integration.json`,
`plugin.json`, `Cargo.lock`, its implementation and reusable-harness tests.

Before promoting a package to the production feed, set up protected signing
and publication, establish supported core/protocol versions,
and validate installation, upgrade, rollback, and failure recovery on the
remote. Do not silently convert existing built-in connections into plugins;
that needs an explicit configuration migration and device validation.

## Further reading

- [Web package management and repository trust](integration-management.md)
- [Package format and lifecycle](integration-packages.md)
- [Developer guide](development/index.md)
- [Core runtime updates](runtime-updates.md)
