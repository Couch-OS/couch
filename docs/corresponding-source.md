# Release source archives

`tools/release/corresponding_source.py` collects public source bytes for a selected
release. Its final assembly refuses missing components, changed hashes and
inventory-only receipts. Run collections on a build machine with several GB of
free space. Source downloads are network reads; APKBUILD shell code is never
executed by the collector.

The archive contains:

- Public Couch files exported from an exact Git commit, including independent
  workspace locks and build recipes. Dirty working files, generated targets,
  session notes and unrelated experiments are excluded.
- Complete locked Cargo dependency sources from all five application workspaces
  and the installer TUI, native host, RAM probe and storage crates. Vendor sources
  retain upstream license, copyright and notice files. Packages omitting workspace
  notices receive additional upstream texts at their published Git commits. Offline Cargo metadata
  resolution verifies every workspace against the generated source replacement.
- The exact aports recipe for each origin represented in the pinned ARM APK
  closure, every local patch/script, and upstream source archives checked against
  APKBUILD SHA512 values. Original Git recipe tar files preserve safe symlink
  aliases; the review copy flattens these aliases to regular files.
- Compiled normal kernel source at its recorded Git commit, the matching configuration, public
  build recipes and compiler/container receipt. A current kernel output directory
  is not accepted unless it matches the selected release pin. Dirty kernel builds
  require separate patch review; this collector refuses them.
- Rebuilt BusyBox source, full configuration, build recipe and toolchain records.
  An opaque downloaded binary is insufficient.
- The selected Rust standard-library source component and its copyright/license
  texts, identified alongside the ARM library hash and Rust compiler version.

No Android images, firmware, vendor library binaries, user configuration,
calibration or signing credentials belong in this archive. The owner-local
vendor extraction process remains separate. The stock recovery kernel retained
from an owner backup is not covered by the compiled normal kernel source.
Stock/vendor-derived boot and recovery images must be assembled locally and
are excluded from hosted assets. The project uses GPL-3.0-or-later;
the release selects Slint's GPL alternative. Other components retain their own
licenses. Generated `NOTICES.md` lists declared licenses and points to retained
upstream texts; it does not assign one license to the combined archive or grant
rights to excluded vendor material.

## Collecting a release

Use a fresh output directory for each frozen release commit. The collector reads
Git objects, so an uncommitted change cannot silently become published source.
If the installer host crate is not present in that commit, Cargo collection fails
instead of skipping it.

```sh
python3 tools/release/corresponding_source.py project \
  --repo . --commit FULL_RELEASE_COMMIT --output /source/release
python3 tools/release/corresponding_source.py cargo --output /source/release
python3 tools/release/corresponding_source.py cargo-notices \
  --output /source/release --cache /cache/cargo-notice-repositories \
  --supplements tools/release/cargo-license-supplements.json
python3 tools/release/corresponding_source.py alpine \
  --closure /inputs/arm-packages --metadata /inputs/apk-source-metadata.json \
  --aports /cache/aports.git --cache /cache/distfiles --output /source/release
```

The notice collector uses each crate’s `.cargo_vcs_info.json` commit and GitHub
repository/homepage metadata. Missing repository metadata requires a reviewed
`--repository-overrides` JSON mapping from `name-version` to repository URL. It
never changes the checksummed vendor tree. Final assembly refuses uncovered
packages or an incomplete notice collection. A small reviewed supplement manifest
covers exact crate versions whose publication repository contains no full notice,
or whose published Git commit is unavailable. Each entry pins the published
Cargo.toml hash, Git identity and SPDX declaration, preserves that metadata, and
adds the unmodified standard MIT template from a pinned SPDX revision. It never
fills in copyright placeholders or presents standard terms as recovered upstream
attribution. Changed package identities require a fresh review; these entries do
not silently apply to dependency upgrades. `cargo-notices.json` distinguishes
recovered notices from supplements and retains the reason and source URLs.
See [Cargo licensing metadata](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields)
and the [SPDX MIT text](https://spdx.org/licenses/MIT.html).

Create the aports cache with `git clone --filter=blob:none --bare
https://github.com/alpinelinux/aports.git /cache/aports.git`. Recipe commits come
from the APK's actual `.PKGINFO`; the collector compares those fields with the
reviewed metadata file and validates every APK against its pinned closure. It
tries Alpine's v3.21 distfiles mirror. For an expired mirror entry, provide
`--source-overrides overrides.json`: keys are `ORIGIN-COMMIT/FILENAME`, values are
public HTTPS URLs. Every replacement still must match the original recipe's
SHA512. Overrides cannot change expected hashes. `--offline` requires a previously
verified download cache. A failed collection records missing origins and stays
incomplete; fix the missing source and rerun against the same immutable inputs.

Prepare kernel and Rust receipts using `collect_external_sources.py kernel` or
`rust-stdlib` (see `--help`). BusyBox's audited source export can use the same
receipt schema. Then import each source-only component:

```sh
python3 tools/release/corresponding_source.py external \
  --directory /inputs/busybox-source --receipt /inputs/busybox-source/receipt.json \
  --output /source/release
```

Each external receipt needs `schema: 1`, `kind: "couch-external-source"`, a
`component` of `kernel`, `busybox` or `rust-stdlib`, and a SHA256 `files` mapping.
`source_archive`, `configuration`, `build_recipe` and `toolchain_receipt` name
actual files in that mapping. `binary_sha256` identifies the corresponding binary
without including it. Additional provenance fields are retained. Exact source,
configuration and build records must be reviewed before importing this receipt.
The Rust component is library source, not a standalone compiler checkout; retain
its bootstrap instructions and use the matching compiler checkout when rebuilding.

```sh
python3 tools/release/corresponding_source.py assemble \
  --output /source/release --archive /release/couch-source-COMMIT.tar.gz
```

The deterministic tar/gzip contains only explicitly verified source files,
component manifests and notices. Its adjacent JSON records the archive SHA256.
Assembly does not overwrite an earlier archive. Verify the emitted tar stream
without extraction using `corresponding_source.py verify-archive --archive FILE`;
it checks every member against the source manifest and refuses duplicates or
unsafe member types. `complete: true` means all
required source components and checksummed bytes were collected; it is not a
claim of physical-device validation, bit-identical application builds or vendor
redistribution permission. Bind the archive hash and source commit to the signed
release inventory, and compare all final binaries' build receipts with this
commit and their actual toolchain versions before publication.

After extracting, use the retained directory layout. From `couch/`, for example:

```sh
cargo --config ../cargo-config/vendor.toml test \
  --manifest-path clients/Cargo.toml --locked --offline
```

## Validation and references

Run `python3 -m unittest discover -s tools/release -p test_corresponding_source.py`.
Fixtures exercise traversal/binary exclusions, dirty-tree isolation, concatenated
APK metadata, checksum failures, recipe aliases and incomplete assembly refusal.
No device or private package input is used by tests.

- [Cargo vendor and multi-workspace synchronization](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html)
- [Cargo configuration path resolution](https://doc.rust-lang.org/cargo/reference/config.html#config-relative-paths)
- [Pinned Alpine recipes](https://github.com/alpinelinux/aports)
- [Rust compiler and standard-library builds](https://rustc-dev-guide.rust-lang.org/building/how-to-build-and-run.html)
