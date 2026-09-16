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
selected build. It does not upgrade Alpine, the recovery image or the stable
update bootstrap; use the OS installer for those. It preserves `config.json`,
saved Wi-Fi, SSH enrollment, and per-device data.

## Installation and recovery

The root system service verifies an Ed25519 publisher signature, model/version,
archive SHA-256, and each allowlisted file's size/hash/mode. Archives cannot
contain links, special files, configuration, or partition images. The file
list is closed: the required binaries and scripts, `fbcon`, web assets and
licence texts, and (from .142 on) further top-level `couch-*` executables.
What a bundle may actually contain is decided by the *oldest deployed* updater,
not this one - see [compatibility floor](#compatibility-floor). Download and
extraction limits are enforced. Files are staged under
`/opt/couch/runtime/slots/<archive-sha256>` while the running version remains
active. The user separately confirms **Install & restart**.

Activation rechecks staged files, journals the prior slot, switches a symlink,
clears the bootloader control block and reboots. The clear matters: init arms
`boot-recovery` at every boot and clears it only after health checks that begin
90 seconds in, so an **Install & restart** pressed before then used to reboot
into recovery with the update applied (the first boot image update, .140.dev,
did exactly that). The next boot arms the flag again before anything can hang,
so nothing is lost by clearing it for a deliberate restart; the Power menu's
restart does the same. The stable `runtime-boot.sh` requires a live GUI heartbeat and system
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
The system service prunes runtime slots once, before it serves its first
request: the active slot, the one kept for rollback (`runtime/previous`, written
by the bootstrap when a candidate is accepted) and a staged candidate are kept,
and every other slot directory is removed. Nothing is pruned while a candidate
still awaits its boot confirmation, and only names that are slot ids are
considered, so the private staging temporary survives. Do not manually remove
the active or previous slot.

## Compatibility floor

A runtime bundle is refused **before it is downloaded**, on the signed manifest
alone, by any updater whose allowlist does not know every file name in it. And a
check offers only the single newest release on the channel: there is no fallback
to an older one when the newest is unusable. Together those two facts turn one
unknown name into a permanent stop. One published bundle carrying one unknown
name strands every remote whose updater predates that name - not only on that
release, but on every release after it, until someone reinstalls the full OS.

So the rule is not "the updater in this checkout accepts it". It is:

> A published runtime bundle may contain only names the **oldest deployed
> updater** accepts.

That updater is the one in the OS image the public installer writes, today
**v0.1.0-alpha.20260910.24**. A remote installed this morning runs .24's runtime
and .24's updater until it updates itself, so .24 is the floor.

### What the floor accepts

Transcribed from `git show v0.1.0-alpha.20260910.24:daemon/couch-updates/src/staging.rs`:

- the sixteen `REQUIRED` names: `couch-gui`, `couch-confd`, `couch-system`,
  `couch-sonos`, `couch-coreelec`, `couch-wmt-properties.so`, `stage2.sh`,
  `hardware-init.sh`, `gui-start.sh`, `system.sh`, `confd.sh`, `setup-mode.sh`,
  `portal.sh`, `station.sh`, `wifi-conf.sh`, `build.json` - all of them, or the
  bundle is incomplete;
- `fbcon`;
- `www/` files ending `.html`, `.css`, `.js`, `.svg`, `.png` or `.woff2`, and
  exactly `www/cgi-bin/{save,scan,enroll,setpw}`;
- `licenses/*.txt`;
- with every file at mode 0644 or 0755, scripts, `couch-*` and the CGI at 0755,
  no file over 64 MiB and no bundle over 128 MiB.

Note what is **absent**: any `couch-*` wildcard. This checkout's updater accepts
a further top-level `couch-*` executable - that arrived in .142 - and the
floor's does not, so **a new binary cannot enter the runtime bundle**. Note also
what is **present**: `licenses/*.txt`, which the floor has taken since the first
signed update. Licence texts have never been what a floor updater refuses.

### Where a new binary goes instead

The boot ramdisk's `/extra`, delivered by the signed
[boot payload](#boot-image-updates) and so only to images that take it. The
whole Bluetooth stack lives there - `couch-bt-bridge`, `couch-bt-hid` and
`couch-bluetoothd` - beside the kernel that gives it `/dev/vhci`.
`tools/release/prepare_boot_candidates.py` puts them in the ramdisk from
`runtime_inventory.BOOT_EXTRA`; `couch_system::bluetooth::base()` prefers a
runtime copy if one is ever there, then `/extra` (copied into `/tmp`, which is
bind-mounted into the Alpine root), then Alpine's own `bluetoothd`.

### Checking a bundle before publishing

`tools/release/update_floor.py` holds the floor's rules, once, with the reason
they cannot move. It prints the bundle contents and every reason the floor would
refuse them, and exits non-zero on a refusal:

```sh
python3 tools/release/update_floor.py --inventory                  # this checkout's lists
python3 tools/release/update_floor.py --tree CLEAN_RUNTIME         # a staged clean tree
python3 tools/release/update_floor.py --manifest couch-VERSION-ha100-update.json
```

It is not the only gate. `couch_updates::bundle` runs the same check in Rust
(`staging::check_floor`) and **refuses to sign** a bundle that fails it, so a
promotion cannot produce an unpublishable release by accident. CI runs the
`--inventory` form and the unit tests in
`tools/release/test_update_floor.py`, which assert that nothing in
`runtime_inventory.RUNTIME` is a name the floor refuses.

### When the floor can move

Only when both are true: the installer's OS image has been rebuilt with a newer
runtime, **and** every remote installed from the older image has updated past
it. Rebuilding that image is the real fix and a known task - see
[installer](installer.md#what-version-a-fresh-install-runs). Until then, moving
the constant in `update_floor.py` and `staging.rs` breaks remotes in the field,
which is exactly what the constant exists to prevent.

### What this cost, twice

- **.141.dev** could not be installed anywhere: `couch-bt-bridge` had been added
  to the runtime bundle. Fixed by moving the bridge into the boot ramdisk, and
  by teaching the updater the `couch-*` wildcard so that a *future* release
  could add one.
- **.160.dev, .163.dev and .164.dev** repeated it: the wildcard made the bundles
  installable by a current remote, so the binaries came back into the runtime -
  `couch-bt-bridge`, `couch-bt-hid`, then `couch-bluetoothd` - and a remote on
  the floor refuses all three. These are published, non-draft prereleases, and
  .164.dev sorts above .148, so `discover()` offers them and nothing else. The
  floor's updater has no dev channel: to it `alpha.20260913.164.dev` is an
  ordinary `alpha.*` prerelease, so an Alpha remote is offered it and refuses
  it, with no fallback to .148. (A Stable remote is offered nothing at all,
  because every release so far is a prerelease.) That is the report this section
  exists to answer: see
  [the installer FAQ](installer.md#the-web-ui-reports-an-older-version-than-the-installer-i-used).

A released `.dev` prerelease is visible to every remote whose updater predates
the Dev channel. Until the floor moves past .142, a `.dev` tag is not private.

## Boot image updates

A release may also carry `couch-VERSION-ha100-boot.tar.gz` with its own signed
`couch-VERSION-ha100-boot.json` (`kind: "boot"`). The archive holds exactly the
owner-neutral boot payload `tools/release/prepare_public_boot.py` exports: the
source-built `zImage` and the clean `boot.cpio.gz`, no Android header, no device
tree, no stock bytes. A check selects the newest runtime and its matching signed boot manifest as
one update. Both manifests must name the same release and OS baseline. A release
with both payloads downloads and verifies both, then offers one **Install &
restart** action. Releases with software alone use the same action. Older
updaters still use their original two-stage flow to install the release that
introduces this updater; subsequent updates use the combined flow.

The selection is journaled in `/opt/couch/updates/transaction.json` before either
payload is staged. Only a fully staged selection becomes ready, including after
a system-service restart. An interrupted or failed download remains an error;
check and download again. Activation rechecks both slots and their baseline
before writing boot, then switches the runtime slot and restarts once. The boot
stage marker survives until the runtime switch completes, allowing an interrupted
activation to retry without replacing the original boot backup. The runtime
bootstrap still accepts or rejects the candidate using its existing health gate.
This coordinates two writes; it does not make a single boot partition atomic or
add automatic boot-image rollback. Power loss during flashing still requires the
existing recovery procedure, and runtime health rollback still restores only the
runtime slot.

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
version, the kernel commit the manifest notes named, and the digests written;
the Updates page and the remote's **Settings → Updates** show it as the
installed boot image.

What protects the boot after that is the existing one: init arms `boot-recovery`
before anything can hang and clears it only after a healthy GUI, so a kernel
that boots but never gets there lands in recovery on its own. A kernel that
dies before init cannot arm anything and loops on the bad image; that needs the
physical route, hold **Back** while powering on, and then the saved image, see
[restoring the previous boot image](device-recovery.md#restoring-the-previous-boot-image).
The boot payload is not a runtime slot: nothing rolls back to `previous.img` on
its own, and the recovery partition is never written by an update. While the
saved image is there the Updates page offers to write it back
(`POST /api/updates/boot-rollback` with `{"confirm":true}`, the system service's
`BootRollback`). That path parses `previous.img`, checks its kernel and ramdisk
against the digests in `previous.json` and refuses if either is missing or does
not verify, then writes and reads the partition back exactly as an install does.
It does not restart: use the Power menu. The saved image is consumed once it is
back, because the image it replaced is no longer installed, so the next check
offers that boot payload again.

### Status and updates started by older versions

Both UIs describe a combined update as software and kernel with one restart.
`Status.kind` is `combined`, `runtime`, or `boot`; `steps` is 1 for an offered
update and 0 otherwise. `guidance` supplies the shared explanation.
`boot_release` identifies the verified boot payload (or the original OS build),
and `boot_behind` compares that label with the installed runtime. An older label
alone does not mean an update is incomplete.

Older updaters leave `/opt/couch/updates/pending-boot.json` after installing a
runtime with a boot companion. The new updater retains support for finishing
those updates: if no newer runtime is offered, it checks the installed release's
signed boot manifest against the actual partition. Different bytes produce a
boot-only offer. Identical bytes update the installed boot record and clear the
pending note without downloading, flashing, or restarting. This handles releases
that republish unchanged kernel and ramdisk bytes under a new version; previously
the check offered nothing but left the remote saying **Update unfinished**.

Opening **Settings > Updates** with `boot_pending` set requests one automatic
check, subject to the normal automatic-check setting and six-hour limit. **Check
for updates** remains available for a manual retry. Neither check downloads or
installs a payload.

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
