# Runtime updates

The web UI's **Updates** page checks the project's GitHub releases for signed
HA100 application bundles. Three channels: **Stable** ignores prereleases;
**Alpha** accepts `alpha.*` prereleases and stable versions, but not dev builds;
**Dev** accepts everything Alpha does plus builds cut from the `dev` branch,
whose tags carry a trailing `.dev` identifier (`v0.1.0-alpha.20260914.7.dev`).
A dev tag keeps the `alpha.<date>.<n>` core of the alpha it follows, so all
three streams sort in one order and a remote on Dev also picks up the next
promoted alpha. See [development flow](development-flow.md) for how the
branches and channels fit. Checks are triggered by opening the paired web
UI and limited to once per six hours during a service session. Downloads and
installation always require the user's choice.

The same operations are available on the remote itself under **Settings →
Updates** (hold Menu on the home screen): the installed build, the channel
(left/right switches Stable and Alpha), **Check for updates** with a one-glance
value (up to date, a version available, checking, downloading, ready, failed),
**Download & verify** once a version is offered, and **Install & restart** once
the candidate is staged, which needs OK twice within six seconds. The GUI talks
to the root system service over the same control socket the web UI's daemon
uses, on a worker thread, and re-reads the status every two seconds while the
section is open. Nothing is downloaded or installed without a press.

This updater replaces Couch applications, Sonos CLI, services, and their
runtime scripts, including the CoreELEC client, and it can write a signed
[boot image](#boot-image-updates) (kernel and boot ramdisk) published with the
installed build. It does not upgrade Alpine, the recovery image or the stable
update bootstrap; use the OS installer for those. It preserves `config.json`,
saved Wi-Fi, SSH enrollment, and per-device data.

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

The rollback also clears the bootloader control block before it reboots. Init
arms `boot-recovery` there at every boot and clears it only after watching a
healthy GUI, which begins 90 seconds in, so when this gate gives up at 90 seconds
the flag is still armed and the reboot would otherwise land in recovery rather
than in the runtime just selected. The next boot arms the flag again before its
own checks, so a previous runtime that is itself broken is still caught. Images
built before this change (release .115 and earlier) have the older bootstrap:
their rollback still switches the slot correctly but the reboot enters recovery,
and [leaving recovery](device-recovery.md#leaving-recovery-after-a-rejected-runtime-candidate)
takes one command on the recovery serial shell. `runtime-boot.sh` is the stable
bootstrap and is not carried by runtime updates, so the fix ships only with a new
full OS image.
An interrupted pointer preparation can be retried after boot clears its pending
journal; the updater reclaims only its stale temporary symlink.
Completed slots are retained; automatic slot garbage collection is not yet
implemented. Do not manually remove the active or previous slot.

## Boot image updates

A release may also carry `couch-VERSION-ha100-boot.tar.gz` with its own signed
`couch-VERSION-ha100-boot.json` (`kind: "boot"`). The archive holds exactly the
owner-neutral boot payload `tools/release/prepare_public_boot.py` exports: the
source-built `zImage` and the clean `boot.cpio.gz`, no Android header, no device
tree, no stock bytes. A check offers it only when no newer runtime is available
on the channel, only for the release whose runtime is installed (the updater
that understands it is the one that runtime shipped), and only while the boot
partition does not already carry that kernel and ramdisk byte for byte. So a
release with both assets installs in two steps: the runtime first, then, after
its reboot, the boot image the same Updates page now offers.

Download and verification stage the two files under `/opt/couch/boot/slots/<sha256>`
with the same digest checks as runtime slots and the same OS baseline gate.
**Install & restart** then does on the remote what the installer does on the
host: it reads the whole 16 MiB `boot` partition, keeps that image's header
page (lk's load addresses and command line) and its appended device tree,
splices the new zImage and ramdisk in, recomputes the Android image ID, saves
the previous partition contents to `/opt/couch/boot/previous.img` (with
`previous.json` naming what was replaced), writes the new image, drops the page
cache and reads the partition back before it counts as applied. A readback
mismatch writes the saved image back and reports it. Nothing is written when the
partition does not parse as an Android boot image, when the staged files differ
from their manifest, or when the baseline differs. `installed.json` records the
version and digests written.

What protects the boot after that is the existing one: init arms `boot-recovery`
before anything can hang and clears it only after a healthy GUI, so a kernel
that boots but never gets there lands in recovery on its own. A kernel that
dies before init cannot arm anything and loops on the bad image; that needs the
physical route, hold **Back** while powering on, and then the saved image, see
[restoring the previous boot image](device-recovery.md#restoring-the-previous-boot-image).
The boot payload is not a runtime slot: there is no automatic rollback to
`previous.img`, and the recovery partition is never written by an update.

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
release in `dangerouslaser/couch`; mark alpha tags as prereleases.

A boot payload is signed from the public boot directory and the clean runtime of
the same version (for the OS baseline it is bound to):

```sh
cargo run --manifest-path daemon/Cargo.toml -p couch-updates -- \
  boot PUBLIC_BOOT_DIR CLEAN_RUNTIME v0.1.0-alpha.1 /private/path/runtime.seed NEW_OUTPUT
```

It emits `couch-VERSION-ha100-boot.tar.gz` and `couch-VERSION-ha100-boot.json`;
attach both to the same release as the runtime pair. The manifest notes carry
the kernel commit from the payload's `boot.json`. Only publish a boot payload
whose kernel has booted the HA100 from a flashed image; the updater checks
signatures and digests, not whether a kernel works. Publishing is
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

Physical acceptance on the development HA100, 2026-09-13, on the .24 full OS
image: the signed .115 runtime installed through the paired web UI and passed
health acceptance (rebooted 07:30:35, serving again 07:32:49). A deliberately
failing candidate (.116, whose `gui-start.sh` never started the GUI, published
for the test and then deleted) was downloaded, verified and activated; the gate
rejected it 75 seconds into its boot and rolled the slot back to .115, but the
remote came up in recovery because the recovery flag was still armed, which is
the interaction the paragraph above describes and the current bootstrap fixes.
The slot state in recovery was exactly as designed (`current` on .115, journal
cleared, the rejected slot retained). Still outstanding: a rejected candidate
returning directly to the previous runtime on an image carrying the fixed
bootstrap, and an interrupted candidate boot confirming recovery selects base.

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
The same applies to the stable bootstrap: the 2026-09-13 change that clears the
recovery flag on rollback updated `runtime_boot_sha256` and kept
`ha100-alpine321-ffmpeg612-runtimeboot2`, because nothing a runtime bundle relies
on changed; only where a rejected candidate's reboot lands.

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
