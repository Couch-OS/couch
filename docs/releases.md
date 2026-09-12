# Clean release staging

`tools/release/clean_stage.py` assembles a deterministic **noninstallable** rootfs staging archive. It takes a hash-pinned clean Alpine ARMv7 minirootfs and an explicit list of Couch artifacts. It never copies a running remote's rootfs, extracts archive links onto the host, or includes vendor files automatically.

The independent staging manifest is not an installer release manifest. It records source commit, input hashes, normalized timestamps, selected artifacts, and output hash. `installable` stays false: offline packages, reviewed vendor inputs, partition images, release signatures, and hardware recovery validation remain required.

Example input (replace the source commit and verify every input hash):

```json
{
  "schema": 1,
  "source_commit": "FULL_40_CHARACTER_GIT_COMMIT",
  "source_date_epoch": 1788912000,
  "alpine": {
    "path": "build/alpine-minirootfs-3.21.7-armv7.tar.gz",
    "version": "3.21.7",
    "architecture": "armv7",
    "sha256": "762c2e1e8d3e6f02e75754de8e4e5766f4c1b280d42f3d640fb30f810e786ee8"
  },
  "artifacts": []
}
```

The recorded base hash matches Alpine's [published checksum](https://dl-cdn.alpinelinux.org/alpine/v3.21/releases/armv7/alpine-minirootfs-3.21.7-armv7.tar.gz.sha256). Hash matching alone is not publisher authentication. The source commit is an inventory field; reproducible source builds and provenance still need release-pipeline enforcement.

Each artifact specifies `source` relative to the checkout, `destination` inside the image, `sha256`, and mode `493` (0755) or `420` (0644). Only supported `/opt/couch` executable/script names and web assets are accepted. Do not point the tool at device backups, a mounted userdata partition, or a sanitized copy of a developer rootfs.

```sh
python3 tools/release/clean_stage.py build/release-staging-input.json build/clean-stage
python3 -m unittest discover -s tools/release -v
```

The output directory must be new. Private-key paths, saved networks, private Couch configuration, password hashes, path traversal, special devices, and files below archive symlinks are rejected. The base BusyBox must be ARM32 ELF. The archive gets a minimal empty Couch configuration, hostname, and key-only SSH settings; root has no usable password. Existing enrollment-gated `couch-system` generates host keys on first authorized use. No keys or enrollment are created by staging.

Archive ownership and timestamps are normalized. Absolute symlinks are retained as image-root links because Alpine uses them extensively; no host extraction occurs. Downstream image assembly must likewise avoid following links outside the image. Fixed Python/zlib and input artifacts are needed for reproducibility across build hosts. Filename/content checks reduce accidental secret inclusion; they cannot prove arbitrary supplied binaries contain no embedded private data.

Validation includes traversal/link attacks, private state and keys, wrong hashes/version, empty onboarding configuration, reproducible output, and the existing official 3.21.7 ARMv7 minirootfs (525 entries after generated directories/files). This is not yet a bootable release: do not flash its tarball.

## Assemble offline packages

On Ollie, `tools/release/prepare_rootfs.py SPEC CLOSURE NEW_OUTPUT` combines the same staging specification with a verified `package_closure.py` cache. It runs authenticated APK installation and its ARM maintainer scripts in an isolated container using Ollie's existing ARM binfmt emulator. No host binfmt registration, loop mount, device access or privileged container is used. The container has only the filesystem/chroot capabilities required for a disposable tmpfs root, no network, read-only inputs, and one new writable output directory.

The result is a normalized `rootfs-staging.tar.gz` plus a noninstallable `staging.json` recording base/artifact/package hashes and builder digest. APK installation and ARM executable checks are logged separately. The archive is rescanned for private state, host keys, password credentials, special nodes, link traversal and modified Couch defaults. Package-created numeric ownership is retained; archive order/timestamps are normalized. Failed assembly leaves diagnostics without a successful staging manifest.

Extensionless recovery scripts are explicitly limited to `save`, `setpw`, `scan`, and `enroll` under `opt/couch/www/cgi-bin/`; they require mode 0755 and an input hash like other artifacts. There is no general extensionless-file exception.

Validation on Ollie installed 19 additional packages over the pinned base, executed dnsmasq's pre-install and BusyBox's trigger, and ran ARM Wi-Fi/SSH/iw version checks. Two independent builds produced identical normalized archives (1277 entries without optional artifacts). A separate fixture included all four real CGI scripts. The version-only Wi-Fi check reports missing `/dev/urandom` because no runtime device tree is mounted; it does not test networking. This validates package assembly, not remote boot or a complete Couch runtime. Reviewed vendor files, complete executable/script inventory, partition images, signatures and physical recovery validation remain required.

## Prepare a raw userdata filesystem

`prepare_ext4.py` now turns packaged staging into an exact-size regular ext4 file using explicit observed geometry and a pinned offline filesystem-tool cache. It validates the filesystem without loop mounts or host rootfs extraction. See [userdata image preparation](userdata-image.md) for commands, reproducibility evidence and the remaining noninstallable release gates.

## Inventory the actual Couch payload

`runtime_inventory.py` now prepares the explicit GUI/daemon/script/portal/notice artifact list, verifies embedded web assets and reports vendor/boot provenance gaps. See [runtime payload inventory](runtime-payload.md). Its clean Couch staging spec replaces the earlier fixture-only payload; `clean_runtime_ready` does not mean the complete release is installable.

## Private vendor candidate preparation

For personal-device validation, a separate [private offline workflow](runtime-payload.md#private-offline-vendor-recovery)
can verify original Android image inputs, recover missing static property
contexts, overlay the reviewed files, and build a private userdata image. It
requires an explicit private-bundle flag; ordinary clean staging still rejects
vendor inputs. No backup, firmware, credentials or private image belongs in
Git or a public release. The workflow records provenance without granting
redistribution rights, and keeps signing, source/build review and physical
boot/recovery validation outstanding.
