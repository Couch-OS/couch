# Couch installer

Desktop installer and temporary Linux installation service for the Sanytron
Astrion HA100. This repository consumes pinned OS payloads built by
[Couch](https://github.com/dangerouslaser/couch).

The source keeps the `tools/installer/` layout so existing build recipes,
workspace lockfiles and source references remain usable after extraction.
`VERSION` under that directory is the installer version; it is independent of
the OS version selected by `installer.json`.

## Develop and test

Use Python 3.12 or newer and Rust stable. Each Rust workspace retains its own
lockfile. On Linux:

```sh
python3 -m unittest discover -s tools/installer -p 'test_*.py'
(cd tools/installer/host && cargo fmt --check && cargo test --locked)
(cd tools/installer/tui && cargo fmt --check && cargo test --locked)
(cd tools/installer/linux_stage/probe && cargo test --locked --features private-install)
(cd tools/installer/linux_stage/storage && cargo test --locked)
```

macOS and Windows can build and test the desktop workspaces. RAM-service and
storage tests use Linux APIs. ARM service builds additionally need the
`armv7-unknown-linux-musleabihf` Rust target and an ARM musl C compiler for ring.
The native binary workflow builds Linux, Windows and universal macOS artifacts
and records compiler, source, version and binary hashes. It does not publish.

With Zig 0.15.2 installed, the included compiler wrapper supports ARM builds:

```sh
export ZIG=/path/to/zig
export CC_armv7_unknown_linux_musleabihf="$PWD/tools/installer/toolchain/arm-musl-cc.py"
(cd tools/installer/linux_stage/probe && cargo build --locked --release \
  --target armv7-unknown-linux-musleabihf --features private-install)
```

## Prepare installer metadata

```sh
python3 tools/installer/bump_version.py v0.1.1
python3 tools/installer/release_descriptor.py \
  --installer-repository dangerouslaser/couch-installer \
  --os-config /path/to/reviewed-os/installer.json \
  --source-commit INSTALLER_SOURCE_COMMIT \
  --output /path/to/new-assets/installer.json
python3 tools/installer/installer_launchers.py \
  --assets /path/to/new-assets --output /path/to/new-launchers --version v0.1.1
```

The asset directory must also hold the reviewed host/TUI binaries for Linux x64,
Windows x64 and universal macOS. Keep their build receipts, corresponding source
and notices. Schema 2 identifies installer and OS versions and source commits
separately and pins the exact OS archive. Existing schema-1 descriptors remain
supported. Installer release tags are `installer-v…`; OS payload tags remain
`v…` in the Couch repository. Metadata generation neither rebuilds nor publishes
the OS payload.

The standalone [source collector](tools/installer/source/README.md) collects this
exact Git commit and all four locked Cargo workspaces. It requires dependency
notices and audited Rust standard-library sources; OS source remains separate.

The neutral RAM image builder belongs to the installer. Complete OS images,
kernel integration and private full boot-image assembly belong to Couch. The
existing public OS archive still bundles the RAM image; changing that archive's
layout requires a separately versioned compatibility change.

Build a neutral RAM payload from reviewed binary and package inputs with
`python3 tools/installer/image/neutral_ramdisk.py --help`. It writes
`installer.cpio.gz` and a receipt without owner firmware or device access.

Keep owner firmware, enrollment records, credentials and backups outside Git.
Never write `preloader_*` or `lk`. Host tests and ARM builds do not establish
physical installation or recovery acceptance. Publishing and device validation
remain explicit release steps.
