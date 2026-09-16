# Boot BusyBox from source

Newly assembled boot, recovery, and installer RAM images use a static ARMv7
BusyBox built from the pinned upstream 1.37.0 source, the configuration here,
and a retained, signature-verified ARM Alpine compiler package closure. The
installed Alpine runtime keeps its own package-managed BusyBox.

The published .168 OTA preserves its verified official BusyBox 1.31 boot
payload; its installer uses the source-built 1.37 variant. Do not replace that
tested OTA component while applying this policy. Its variant-specific published
provenance remains the release record; a current 1.37 receipt describes a new
assembly or the installer, not the historical OTA payload.

On a Docker build host supporting ARMv7 execution (native or QEMU/binfmt):

```sh
curl -fL https://busybox.net/downloads/busybox-1.37.0.tar.bz2 -o /private/cache/busybox-1.37.0.tar.bz2
python3 tools/release/package_closure.py prepare /private/cache/busybox-toolchain --package build-base --package linux-headers
python3 tools/build-busybox.py build /private/cache/busybox-1.37.0.tar.bz2 /private/cache/busybox-toolchain build/busybox-source
python3 tools/build-busybox.py verify build/busybox-source --install build/busybox-armv7l
```

The source hash is checked before extraction. The compiler closure is verified
again before the build; retain it for exact rebuilds because package repositories
change. The ARM builder image is pinned separately. Compilation runs with no
network and saves its source, compiler details, installed package versions, ELF
report, applet list and checksummed receipt. The full configuration enables static
musl linking and the boot/recovery/installer applets listed here; `tc`, which these
paths do not use, is disabled.

`tools/build.sh` and `tools/build-recovery.sh` verify this receipt and recipe before
using the executable. Set `BUSYBOX_BUILD_DIR` to another verified build directory.
For RAM-stage packaging, supply this same verified executable as `--busybox`.
Keep previous private candidates intact until the new boot and recovery paths
have passed physical validation. ARM emulation checks do not prove kernel 3.18
or hardware compatibility.

New OTA ramdisk assembly uses only a verified 1.37 receipt. It never falls back
to `build/busybox`, a downloaded executable, or a prior release artifact. To
replay a historical release, reuse its published signed boot payload after
verifying its published manifest and signature; do not reconstruct that release
through the current build path. Historical payload reuse is intentionally
separate from producing a new OTA image.

The corresponding-source release includes the upstream source archive, this
configuration and builder recipe, and the compiler/package identifiers from the
receipt. No device backups are needed for this build.

SHA hardware acceleration is disabled: this ARMv7 Cortex-A7 has no ARMv8 crypto instructions, and the upstream default SHA-NI path is x86-specific. Portable SHA implementations are checked in the ARM smoke test.
