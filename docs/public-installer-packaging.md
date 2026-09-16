# Public installer packaging

`tools/release/package_public_installer.py` combines six reviewed neutral inputs
into a deterministic gzip/USTAR archive and emits `installer.json` for the native
host and immutable launcher generator. It neither discovers owner files nor
publishes assets. Only these seven flat regular archive members are permitted:

- `userdata.ext4`: compact vendor-free filesystem from `prepare_public_userdata.py`.
- `installer.cpio.gz`: neutral installer RAM stage from `prepare_public_ramdisk.py`.
- `boot.cpio.gz`, `recovery.cpio.gz`, `zImage`: neutral boot inputs from `prepare_public_boot.py`.
- `logo.bgra`: Couch's own frame from `prepare_public_logo.py`.
- `manifest.json`: version, exact Couch source commit and six file size/SHA-256 pins.

The packager requires the original builder receipts (`image.json`, `ramdisk.json`,
`boot.json`, `logo.json`) and an exact-source build attestation produced by the
release build. This attestation is trusted build evidence, not a new signature or
proof that an arbitrary supplied binary was compiled from its claimed source.
It must describe the actual frozen build; never generate it by inspecting a later
checkout's HEAD or by relabeling old candidate outputs.

The userdata receipt must also carry the `fresh_core` binding produced by
`fresh_os.py` and retained by `prepare_public_userdata.py --fresh-core`. Its
source commit, rootfs digest, bootstrap/package baseline, official integration
key and same-source verified integration set must agree with this build.
Older userdata without that evidence is refused even if a newly written build
attestation hashes it correctly. See the [fresh rootfs binding
workflow](integration-release-rollout.md#bind-the-fresh-rootfs-before-image-assembly).

```json
{
  "schema": 1,
  "kind": "couch-public-os-build",
  "source_commit": "<40 lowercase hexadecimal characters>",
  "source_archive_sha256": "<SHA-256 of the exact Couch source archive used by the build>",
  "complete": true,
  "private_inputs": false,
  "files": {
    "userdata.ext4": {"size": 123, "sha256": "<digest>"},
    "installer.cpio.gz": {"size": 123, "sha256": "<digest>"},
    "boot.cpio.gz": {"size": 123, "sha256": "<digest>"},
    "recovery.cpio.gz": {"size": 123, "sha256": "<digest>"},
    "zImage": {"size": 123, "sha256": "<digest>"},
    "logo.bgra": {"size": 1536000, "sha256": "<digest>"}
  },
  "builder_receipts": {
    "userdata": {"sha256": "<image.json digest>"},
    "ramdisk": {"sha256": "<ramdisk.json digest>"},
    "boot": {"sha256": "<boot.json digest>"},
    "logo": {"sha256": "<logo.json digest>"}
  }
}
```

`--source-archive` is the exact Couch Git archive used as build input:
`git archive --format=tar.gz COMMIT`. Its fixed-size global PAX commit comment must
match `source_commit`; the packager validates the 512-byte header and bounds the
metadata length before reading it. This is distinct from the complete
corresponding-source bundle, which also contains Cargo, Alpine, kernel, BusyBox
and compiler sources and is separately assembled and verified before publication.

The component kernel source commit remains separate from the Couch source commit.
The boot receipt must match the reviewed kernel source and zImage pin. The logo
receipt must match Couch's canonical artwork, and the userdata/RAM receipts must
identify vendor-free inputs targeting the compiled owner-OTA inventory. Actual
source archive, receipt and payload bytes are independently rehashed; input files
stay open through packaging and are rehashed while being written to the tar.
Links, special files, mixed receipts, extra inventory names and private builder
kinds are rejected. Existing output directories/files are preserved on collisions. Binary-mode input
reads work on Windows; the compressed archive is flushed to disk before its
exclusive final link, and descriptor/receipt files are flushed before return.

```sh
python3 tools/release/package_public_installer.py \
  --attestation BUILD/public-os-build.json --source-archive BUILD/couch-source.tar.gz \
  --userdata BUILD/public-userdata --ramdisk BUILD/public-ramdisk \
  --boot BUILD/public-boot --logo BUILD/public-logo \
  --version v0.1.0-alpha.1 --output NEW_PUBLIC_ASSETS
```

Outputs are `couch-VERSION-ha100-public-inputs.tar.gz`, `installer.json`, and
`package.json` (build/receipt hashes, distinct kernel source and unpublished
status). The compressed limit is 1 GiB, compatible with the launcher; the native
uncompressed limit is 2 GiB. Tar timestamps, IDs, modes and ordering are fixed,
with no directory/link/PAX records or extra compressed streams.

Before publication, use the integrated native host's offline admission command:

```sh
couch-installer-host verify-public NEW_PUBLIC_ASSETS/installer.json \
  NEW_PUBLIC_ASSETS/couch-v0.1.0-alpha.1-ha100-public-inputs.tar.gz NEW_VERIFICATION_DIR
```

This performs no downloads or device access. Place the actual release host/TUI
platform binaries alongside `installer.json`, then run `installer_launchers.py`
to bind their hashes. Source/notices publication and signing remain separate
release steps. Each desktop artifact needs its actual compiler/standard-library
source receipt; the old ARM candidate's Rust receipt cannot stand in for every
new host platform. Owner OTA headers/DTB, vendor firmware/libraries, DA, original
partitions and calibration never enter this public payload.

Validation: `python3 -m unittest discover -s tools/release -p
'test_package_public_installer.py'`. Set `COUCH_NATIVE_PUBLIC_HOST` to the reviewed
native host executable to also exercise real offline archive admission. Fixtures
contain synthetic public bytes and do not read any remote or owner data.
