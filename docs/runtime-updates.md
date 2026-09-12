# Runtime updates

The web UI's **Updates** page checks the project's GitHub releases for signed
HA100 application bundles. Stable ignores prereleases; Alpha accepts `alpha.*`
prereleases and stable versions. Checks are triggered by opening the paired web
UI and limited to once per six hours during a service session. Downloads and
installation always require the user's choice. There is no remote GUI indicator.

This first updater replaces Couch applications, Sonos CLI, services, and their
runtime scripts, including the CoreELEC client. It does not upgrade Alpine, the kernel, boot/recovery images,
or the stable update bootstrap; use the OS installer for those. It preserves
`config.json`, saved Wi-Fi, SSH enrollment, and per-device data.

## Installation and recovery

The root system service verifies an Ed25519 publisher signature, model/version,
archive SHA-256, and each allowlisted file's size/hash/mode. Archives cannot
contain links, special files, configuration, or partition images. Download and
extraction limits are enforced. Files are staged under
`/opt/couch/runtime/slots/<archive-sha256>` while the running version remains
active. The user separately confirms **Install & restart**.

Activation rechecks staged files, journals the prior slot, switches a symlink,
and reboots. The stable `runtime-boot.sh` requires a live GUI heartbeat and system
service for five consecutive checks within 90 seconds. Missing or invalid
heartbeats reset the streak, and each system health command has a two-second
timeout. A failed or interrupted candidate
boot returns to the previous slot (or the base runtime). The recovery boot always
uses the base runtime. On this device's BusyBox, rollback removes the current
symlink before replacing it; interruption in that interval selects the base.
An interrupted pointer preparation can be retried after boot clears its pending
journal; the updater reclaims only its stale temporary symlink.
Completed slots are retained; automatic slot garbage collection is not yet
implemented. Do not manually remove the active or previous slot.

## Publishing

Build the current web bundle, ARM daemon/GUI and Sonos client, then assemble the
clean runtime. Generate a durable signing seed **outside Git** with:

```sh
cargo run --manifest-path daemon/Cargo.toml -p couch-updates -- keygen /private/path/runtime.seed
```

This writes a new mode-0600 seed and prints its public key. Embed that public key
as `/opt/couch/update-key.pub` in OS images, alongside a versioned `build.json`.
Keep the seed backed up securely; losing it prevents updates to installed images
trusting that key. The key cannot be replaced by an application bundle.

Create a signed release pair from a clean `/opt/couch` tree:

```sh
cargo run --manifest-path daemon/Cargo.toml -p couch-updates -- \
  CLEAN_RUNTIME v0.1.0-alpha.1 /private/path/runtime.seed NEW_OUTPUT
```

The publisher emits `couch-VERSION-ha100-runtime.tar.gz` and
`couch-VERSION-ha100-update.json`. Attach both to the matching versioned GitHub
release in `dangerouslaser/couch`; mark alpha tags as prereleases. Publishing is
separate from packaging. The client examines the most recent 100 releases and
requires GitHub's SHA-256 asset digest on the manifest as well as its publisher
signature. Existing installer assets alone are not runtime updates.

An OS image must include its public trust key before runtime updates can work.
Browser fixtures and host staging/rollback tests do not replace physical
update-and-rollback acceptance on the remote.

## Validation

Run `cargo test --locked -p couch-updates` in `daemon/`, and on Linux run
`python3 tools/tests/test_runtime_update_boot.py`. The boot fixtures use temporary
runtime slots, process/heartbeat fixtures and an intercepted reboot command; they
never access device partitions. They cover healthy acceptance, failed or hung
health checks, intermittent heartbeats, interrupted activation and rollback to
the base or previous runtime.

Physical acceptance still requires installing a signed candidate through the
paired web UI, observing GUI/system health acceptance, and intentionally failing
a candidate health check to verify reboot into the previous runtime. Repeat with
an interrupted candidate boot and confirm recovery still selects the base runtime.

Update acceptance requires advancing GUI heartbeats from the same process; a recently frozen GUI or repeated process restarts cannot satisfy the boot-health gate.

## Full OS compatibility

Application versions do not identify the Alpine packages or stable boot scripts
beneath them. New signed manifests therefore carry `required_os_baseline`, a
capability ID such as `ha100-alpine321-ffmpeg612-runtimeboot2`. The updater checks
`/opt/couch/os-baseline.json` before any payload download or staging, then checks
it again immediately before activation. A missing, malformed, symlinked or
mismatched marker requires a fresh full OS installation. The marker cannot be
included in a runtime bundle or changed through its allowlist.

The full OS builder (`tools/release/prepare_rootfs.py`) generates this marker
only after checking the reviewed complete FFmpeg APK closure, installed ARM
FFmpeg and the stable boot script against `tools/release/ha100_os_baseline.json`.
The pin records build inputs; its capability ID stays independent of application
release versions. Changing the supported OS capabilities requires a reviewed pin
and ID change. Updating a reviewed package pin without changing capabilities may
retain the ID after compatibility validation.

The runtime publisher now requires that generated marker at the root of its
`CLEAN_RUNTIME` input. It signs the ID without shipping the marker. A clean runtime
export must preserve the marker from the matching newly built full OS; do not
invent it or copy it onto an older installed image. Existing immutable candidate
22/23 fixtures predate this marker and need a fresh full OS build for new updates.

Legacy manifests with no requirement retain their original signing bytes and
policy: the optional field is omitted during serialization. Old updater binaries
reject manifests containing the new unknown field, so they cannot silently skip
the requirement. New manifests must be published with the updated release tool;
release notes alone are never an OS compatibility check.
