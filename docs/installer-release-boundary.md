# Installer release boundary

The installer is developed in `tools/installer/` with an independent build
version in `VERSION`. It consumes pinned Couch OS payloads. Its native host,
terminal UI, dependency preparation, transport bridge and Linux installation
service can be built without the application workspaces. The code remains in
this repository during migration. A repository export can now include the
standalone README, ignore and line-ending rules, and installer CI workflows.

## Ownership

The installer owns desktop binaries, launchers, host dependency pins, Android
enrollment and restoration, the temporary installation service, and its storage
writer. Canonical firmware and host dependency pins live in
`tools/installer/pins/`; Couch image builders consume those same pins. The
canonical neutral RAM builder lives in `tools/installer/image/neutral_ramdisk.py`.
It consumes explicit binary and APK-closure inputs without importing Couch's
release tooling. The old `tools/release/prepare_public_ramdisk.py` command remains
a compatibility entry point. Private kernel/boot assembly stays in Couch.

Couch owns the installed OS, application updates, boot and recovery images,
kernel integration, and OS image production. The existing public OS archive
still contains `installer.cpio.gz` alongside the installed images. Building that
complete archive continues to require Couch's kernel and image assembly tooling.
The independent desktop installer can reuse an existing archive unchanged.
Separately publishing the RAM environment is a later packaging migration, not
an assumption made by this interface.

## Release identities and compatibility

New installer releases use `installer-v…` tags, independently of Couch's `v…`
runtime tags. The installer version is not the version of the installed OS.
An installer-only fix can select the same OS archive and source revision as its
predecessor. Runtime releases do not require an installer build.

Installer downloads may use exactly `dangerouslaser/couch` or
`Couch-OS/couch-installer`; OS payload downloads remain in
`dangerouslaser/couch`. Select the installer location with the descriptor
builder's `--installer-repository` option. The host rejects other repositories,
domains and mutable download locations. Existing published URLs stay valid.

Schema 2 of `installer.json` records:

- `installer`: its `version`, exact `source_commit`, and immutable `release_url`;
- `os`: its `version`, exact `source_commit`, and `installation_protocol`;
- `payload`: the OS archive's immutable URL, byte size, SHA-256 and format;
- the existing `kind` and supported device `model`.

Protocol 1 describes the currently supported installation transaction. The host
rejects unsupported protocol values before opening a device. The archive's
schema-1 manifest must match the **OS** identity and all six file pins; it need
not match the installer identity. The protocol declaration is release metadata
bound to the selected archive, not a new hardware capability negotiation.

Existing schema-1 descriptors remain accepted with their shared version/source
identity and implicit protocol 1. Old hosts do not understand schema 2, so new
launchers must always pin the matching new host and TUI binaries. Schema 2 does
not change on-device writes or the existing wire protocol.

## Preparing an independent installer release

Set the next build version without changing any published command:

```sh
python3 tools/installer/bump_version.py v0.1.1
python3 tools/installer/bump_version.py --check
```

Build from the exact reviewed source revision and retain each platform's binary
receipt. Create a new descriptor selecting an already reviewed OS payload:

```sh
python3 tools/installer/release_descriptor.py \
  --installer-repository Couch-OS/couch-installer \
  --os-config /path/to/reviewed-os/installer.json \
  --source-commit INSTALLER_SOURCE_COMMIT \
  --output /path/to/new-assets/installer.json
python3 tools/installer/installer_launchers.py \
  --assets /path/to/new-assets \
  --output /path/to/new-launchers \
  --version v0.1.1
```

The descriptor builder defaults to `tools/installer/VERSION`. The assets
directory also needs the six platform host/TUI files listed in
[native launcher packaging](installer-native-launchers.md). Preserve separate
installer and OS source receipts and corresponding source. A source export or
descriptor alone does not establish binary provenance or physical acceptance.
These commands create local candidates; they do not publish anything.

Collect installer-only corresponding source with
`tools/installer/source/corresponding_source.py`, which requires all four Cargo
workspaces and their lockfiles plus dependency notices and an audited Rust source
component. Its [source guide](../tools/installer/source/README.md) describes the
commands and the remaining native toolchain receipt checks. Keep the selected
OS payload's source archive separately; an installer-scoped archive cannot
satisfy the full Couch OS source contract.

After publication and acceptance, update the public commands using
`tools/release/bump_release.py installer-v0.1.1 --repository Couch-OS/couch-installer`.
The historical `tools/release/current-release.txt` path remains the published tag
pointer for the website, with the repository in
`tools/release/installer-repository.txt`. Update the site's consumption of both
pointers before moving published commands. Runtime promotion no longer rewrites
them. Until an independent
installer is actually published, the existing commands continue to reference
the previous published release.

## Repository extraction

Prepare an independent repository tree with its own README, ignore rules and
installer-only workflows:

```sh
python3 tools/installer/export_source.py . /tmp/couch-installer-repository --repository
git -C /tmp/couch-installer-repository init -b dev
```

This creates no GitHub repository, pushes no source and changes no public
commands. Review the exported file manifest before committing the candidate.
The retained `tools/installer` layout permits existing compiler and source
recipes to work unchanged. The standalone workflow builds desktop artifacts and
tests the host, TUI, Linux RAM service and storage workspace; it does not publish.
For Windows acceptance in the new repository, explicitly choose
`Couch-OS/couch-installer` as the workflow's `installer_repository` input.
Cross-repository artifact reads can use the optional `INSTALLER_ARTIFACT_TOKEN`
secret; same-repository validation uses the normal workflow token.

Verify the source boundary by exporting to a new directory outside the checkout:

```sh
python3 tools/installer/export_source.py . /tmp/couch-installer-source
cd /tmp/couch-installer-source/tools/installer
(cd host && cargo test --locked)
(cd tui && cargo test --locked)
(cd linux_stage/probe && cargo test --locked --features private-install)
(cd linux_stage/storage && cargo test --locked)
```

The export retains the `tools/installer` layout but contains no Couch
application workspaces or `tools/release` implementation. CI runs an isolated
build from this export in addition to the desktop platform matrix.
By default it selects Git-tracked installer files and the project license,
excluding build output and private state filenames. During local development,
`--include-new-source` also includes non-ignored untracked installer files and
marks the export receipt accordingly; review those additions before using it.
Neither mode is a substitute for the exact-commit corresponding-source collector.
The RAM service tests need Linux; macOS can run the desktop tests and cross-build
the service with the ARMv7 Rust target and an ARM musl C compiler.

Keep the installer host, temporary service and storage writer together. Their
wire protocol and fixtures need coordinated changes. After the standalone source
and artifacts have been published with reviewed immutable pins, Couch can replace
its in-tree installer source with those pinned inputs. Until that cutover, the
in-tree source stays canonical. Full OS image assembly and physical installation
acceptance remain separate from a successful isolated desktop build.
