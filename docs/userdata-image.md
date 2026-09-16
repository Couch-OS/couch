# Raw userdata image preparation

`tools/release/prepare_ext4.py` builds a **noninstallable raw ext4 file**, using packaged clean staging and a separate verified x86_64 filesystem-tool cache. Run it on a dedicated Linux build host. It does not open a device, mount a loop device, extract a rootfs onto the host, or change host tools/kernel settings.

```sh
python3 tools/release/package_closure.py prepare build/image-tools \
  --architecture x86_64 --package e2fsprogs --package e2fsprogs-extra --authenticate
python3 tools/release/prepare_ext4.py build/packaged-stage build/image-tools \
  tools/release/ha100_userdata_geometry.json build/userdata-image
```

Inputs and outputs must be ordinary files/directories; the output directory must be new. `packaged-stage` must contain `staging.json` and its hash-matching `rootfs-staging.tar.gz` from [package assembly](releases.md#assemble-offline-packages). The archive is rescanned before use. The tool cache pins all eight dependencies, including e2fsprogs 1.47.1-r1, by SHA-256 and verifies Alpine signatures again inside the builder. Preserve this cache to reproduce a build when mirrors replace packages.

## Geometry and reproducibility

The checked-in geometry records the observed userdata offset (1,895,825,408 bytes), size (5,905,055,744 bytes) and SHA-256 of the private, validated layout evidence. It contains no storage CID, calibration, device ID, credentials or backups. These values came from one validated HA100 capture; they are not permission to assume another unit's layout. The eventual installer must compare the complete observed partition layout before any write.

Alternative geometry requires explicit aligned byte values, partition `userdata`, an evidence hash and source `observed` or `fixture`. Unknown fields are rejected to prevent accidentally copying private baseline identifiers. The evidence hash records provenance; this helper does not independently authenticate a hardware observation. Fixtures remain explicitly labeled in the output inventory.

A digest-pinned Alpine container runs offline with read-only inputs, a disposable tmpfs tree, and one writable output directory. Only file ownership/access capabilities are added; there is no privileged mode or chroot capability for image creation. Tool packages are installed into temporary storage with scripts disabled. The ARM payload is never executed by this step.

The image has 4096-byte blocks, a deterministic UUID/hash seed derived from content and size, fixed timestamps via `SOURCE_DATE_EPOCH`/`E2FSPROGS_FAKE_TIME`, explicit inode/reserve settings, fully initialized tables/journal, and an explicit ext4 feature list compatible with the older kernel. New `64bit`, `metadata_csum`, and `orphan_file` defaults are excluded. The private builder configuration avoids host `mke2fs.conf` differences. See the [upstream mke2fs manual source](https://kernel.googlesource.com/pub/scm/fs/ext2/e2fsprogs/+/22f5c951fa3b7dd44b9eb0bac45c58be5a3887f8/misc/mke2fs.8.in) for these options.

`userdata.ext4` is exactly the supplied partition size. A host filesystem may store zero regions as holes; reading the file still returns full raw ext4 bytes, not Android sparse-image encoding. SHA-256 is streamed, so hashing does not allocate the entire image in RAM. `image.json` records the source/closure hashes, geometry, UUID and complete image hash. Installation remains false.

## Validation and remaining work

Both a 64 MiB fixture and two full-size observed-geometry builds passed `e2fsck -fn`, superblock size/feature checks, and repeated-build byte equality. The full-size fixture hash was `dfa0285ba3ad20163e1ea4a004466b100050224e003ab1637ac8b10ce4d3af03`; direct readback confirmed the empty onboarding configuration. These images contain the clean Alpine packages and real recovery CGI fixture, not a complete reviewed Couch runtime.

Geometry validation, raw magic/length/features, tampering, architecture separation and private-state checks have local tests (`python3 -m unittest discover -s tools/release -v`). Full output hashes are expected to change when payloads or pinned tools change.

Reviewed vendor/runtime artifacts, the complete boot/recovery image set, a signed release manifest, and physical recovery/boot validation are still required. No image produced by this helper should be flashed while these gates remain open.
