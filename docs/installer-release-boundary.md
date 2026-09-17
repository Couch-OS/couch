# Installer release boundary

The installer is developed in
[Couch-OS/couch-installer](https://github.com/Couch-OS/couch-installer) with an
independent build version in its `VERSION` file. Couch consumes it as a git
submodule at `couch-installer/`, so installer files appear in a Couch checkout
under `couch-installer/tools/installer/`. It consumes pinned Couch OS payloads.
Its native host, terminal UI, dependency preparation, transport bridge and Linux
installation service build without the Couch application workspaces.

## Ownership

The installer owns desktop binaries, launchers, host dependency pins, Android
enrollment and restoration, the temporary installation service, and its storage
writer. Canonical firmware and host dependency pins live in
`couch-installer/tools/installer/pins/`; Couch image builders locate those same
pins through `tools/release/installer_pins.py`. The canonical neutral RAM builder
is `couch-installer/tools/installer/image/neutral_ramdisk.py`. It consumes
explicit binary and APK-closure inputs without importing Couch's release tooling.
Couch builds `installer.cpio.gz` with `tools/release/prepare_public_ramdisk.py`.
Private kernel/boot assembly stays in Couch.

Couch owns the installed OS, application updates, boot and recovery images,
kernel integration, and OS image production. The existing public OS archive
still contains `installer.cpio.gz` alongside the installed images. Building that
complete archive continues to require Couch's kernel and image assembly tooling
and an initialized `couch-installer` submodule. The independent desktop
installer can reuse an existing archive unchanged. Separately publishing the RAM
environment is a later packaging migration, not an assumption made by this
interface.

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

Installer releases are built from Couch-OS/couch-installer commits. Set the next
build version without changing any published command, and commit the change to
that repository:

```sh
python3 couch-installer/tools/installer/bump_version.py v0.1.1
python3 couch-installer/tools/installer/bump_version.py --check
```

Build from the exact reviewed installer commit and retain each platform's binary
receipt. Create a new descriptor selecting an already reviewed OS payload:

```sh
python3 couch-installer/tools/installer/release_descriptor.py \
  --installer-repository Couch-OS/couch-installer \
  --os-config /path/to/reviewed-os/installer.json \
  --source-commit INSTALLER_SOURCE_COMMIT \
  --output /path/to/new-assets/installer.json
python3 couch-installer/tools/installer/installer_launchers.py \
  --assets /path/to/new-assets \
  --output /path/to/new-launchers \
  --version v0.1.1
```

The descriptor builder defaults to `couch-installer/tools/installer/VERSION`. The
assets directory also needs the six platform host/TUI files listed in
[native launcher packaging](installer-native-launchers.md). Preserve separate
installer and OS source receipts and corresponding source. A source export or
descriptor alone does not establish binary provenance or physical acceptance.
These commands create local candidates; they do not publish anything.

Collect installer-only corresponding source with
`couch-installer/tools/installer/source/corresponding_source.py`, which requires
all four Cargo workspaces and their lockfiles plus dependency notices and an
audited Rust source component. Its
[source guide](https://github.com/Couch-OS/couch-installer/blob/dev/tools/installer/source/README.md)
describes the commands and the remaining native toolchain receipt checks. Keep
the selected OS payload's source archive separately; an installer-scoped archive
cannot satisfy the full Couch OS source contract.

After publication and acceptance, update the public commands using
`tools/release/bump_release.py installer-v0.1.1 --repository Couch-OS/couch-installer`.
The historical `tools/release/current-release.txt` path remains the published tag
pointer for the website, with the repository in
`tools/release/installer-repository.txt`. Update the site's consumption of both
pointers before moving published commands. Runtime promotion no longer rewrites
them. Until an independent
installer is actually published, the existing commands continue to reference
the previous published release.

## The installer submodule

Each Couch commit pins one commit of
[Couch-OS/couch-installer](https://github.com/Couch-OS/couch-installer) through
the `couch-installer` gitlink. Couch's release tooling reads installer files from
the submodule: pins through `tools/release/installer_pins.py`, the neutral RAM
builder through `tools/release/prepare_public_ramdisk.py`, and installer source
for full corresponding-source collection. Clone with
`git clone --recurse-submodules`, or initialize the submodule after cloning or
pulling:

```sh
git submodule update --init couch-installer
```

Every git worktree needs its own init. `tools/release` tests and image assembly
need the initialized submodule.

Installer changes are made in Couch-OS/couch-installer. Pull requests go to its
`dev` branch and are gated by its `admission` check; installer CI and the daily
firmware pin watch run there. Its
[README](https://github.com/Couch-OS/couch-installer/blob/dev/README.md) lists
the build and test commands. The installer-related workflows left in Couch are
`public-installer-package.yml`, `runtime-os-compatibility.yml` and
`release-pointers.yml`.

To bump the pin, check out a commit that is on the installer repository's `dev`
or `main`, stage the gitlink and open a Couch pull request:

```sh
git -C couch-installer fetch origin
git -C couch-installer checkout INSTALLER_COMMIT
git add couch-installer
```

Keep the installer host, temporary service and storage writer together. Their
wire protocol and fixtures need coordinated changes. Full OS image assembly and
physical installation acceptance remain separate from a successful installer
build.
