# Kernel build and provenance

The HA100 kernel source is maintained in the separate public
[`Couch-OS/couch-kernel`](https://github.com/Couch-OS/couch-kernel)
repository. The release source, configuration, compiler, container, and zImage
hashes are the source of truth in [`release-pin.json`](release-pin.json).
`docs/kernel-release-candidate.md` records the acceptance scope for that pin.
The stock kernel remains the independent recovery fallback.

## Configure a builder

Copy `local.env.example` to the ignored repository-root `local.env`. Set
`KTREE` to a clean kernel checkout. A local build is the public default:

```sh
KTREE=/work/couch-kernel kernel/build.sh normal
```

For an explicit remote build, configure all three local-only values:

```sh
KERNEL_BUILD_MODE=remote
KERNEL_REMOTE_HOST=builder.example.invalid
KERNEL_REMOTE_RECIPE_ROOT=/srv/couch/kernel-recipe
KTREE=/srv/couch-kernel/base
```

Then run `kernel/build.sh normal` or override a configured mode with
`kernel/build.sh --local normal` / `kernel/build.sh --remote normal`. The
wrapper transfers only the tracked `kernel/` recipe. The remote path must be an
absolute POSIX path; host names and filesystem layouts never belong in tracked
documentation.

The build records `KBUILD_BUILD_HOST`. New builds default to the generic
`couch-builder`. If reproducing a historical artifact whose hash includes a
different host identity, set `KBUILD_BUILD_HOST` in ignored `local.env`; changing
it changes the kernel binary hash.

Both profiles regenerate `.config` from the tracked baseline and profile
fragment, require a clean source tree, and record source status, toolchain,
container, configuration, and artifact SHA-256 values in `manifest.json`.
Normal builds additionally enforce the pinned source ancestry.

## Validate and package

```sh
python3 tools/release/kernel_provenance.py \
  --boot build/couch-board-init-fixed.img \
  --kernel-manifest build/board-init-manifest.json
python3 -m unittest discover -s kernel -p 'test_*.py'
```

The provenance check does not make a new installer image or attest an altered
ramdisk, DTB, recovery image, vendor input, or device validation. New OTA
ramdisks require a verified source-built BusyBox 1.37 receipt; see
[`tools/busybox/README.md`](../tools/busybox/README.md). To replay an older
release, verify and reuse its published signed payload instead of rebuilding it
with current inputs.
