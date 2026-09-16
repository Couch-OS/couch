# Integration release rollout

The first integration-capable core and the first public integration package are
two independently signed releases. A core runtime supplies the protocol host,
package manager, user interfaces and official feed key. An integration feed
supplies versioned APKs. Do not copy an APK into a core runtime or installer OS
image to make the two releases appear atomic.

The initial pilot is the core contract tested at Couch commit
`b9eb59fd0a180fd3ae2d7b2ed27a61920cb5f6cb` with Denon `0.1.1` from the
official preview feed. The machine-readable identity is
`tools/release/tested-integrations.json`. It pins:

- protocol version 1 and the core files whose native host/adapter behavior was
  tested;
- the immutable preview feed release snapshot, official public key and Denon
  APK bytes;
- the independent Denon source commit and its Couch SDK/tooling commits;
- preview evidence: signed package lifecycle, read-only receiver status and
  input enumeration. It does not claim full command parity or a validated
  receiver model/firmware pair.

The core's tested commit contains the SDK/tooling commit used to build the
package. A later release candidate may use a descendant Couch commit only while
the pinned protocol and native host/adapter paths are unchanged. The verifier
fails if one of those paths changes. Files outside those pinned contract paths
can advance without relabelling the protocol as newly tested.

The release checkout needs complete Git history for these ancestry and path
checks. Preserve the tested commit through a merge commit or fast-forward. A
squash or rebase that removes it from the candidate's ancestry requires a new
tested commit and renewed evidence; do not weaken the ancestry check.

## Produce the tested-set receipt

Download the immutable feed release archive attached to source commit
`33ecd15083e3aa001f97fcb71b6a5640ca737712`, verify its SHA-256, and extract it
into a new review directory:

```sh
curl -fL -o couch-integrations-preview-armv7.tar.gz \
  https://github.com/dangerouslaser/couch-integrations/releases/download/feed-33ecd15083e3aa001f97fcb71b6a5640ca737712/couch-integrations-preview-armv7.tar.gz
printf '%s  %s\n' \
  e8de74d5ed36bab161a30107e6683e04bd2f2009d70ef2c8d4a4ac497034132f \
  couch-integrations-preview-armv7.tar.gz | sha256sum -c -
mkdir REVIEW
tar --no-same-owner -xzf couch-integrations-preview-armv7.tar.gz -C REVIEW
```

The four verifier inputs are below `REVIEW/preview` without renaming or
editing them:

```text
couch-integrations.rsa.pub
APKINDEX.tar.gz
couch-integration-denon-0.1.1-r0.apk
couch-integration-denon-0.1.1-r0.provenance.json
```

Then verify the exact public bytes and write a new candidate receipt:

```sh
python3 tools/release/verify_integration_set.py \
  tools/release/tested-integrations.json \
  --public-key REVIEW/preview/couch-integrations.rsa.pub \
  --index REVIEW/preview/armv7/APKINDEX.tar.gz \
  --package denon=REVIEW/preview/armv7/couch-integration-denon-0.1.1-r0.apk \
  --provenance denon=REVIEW/preview/armv7/couch-integration-denon-0.1.1-r0.provenance.json \
  --output NEW_RECEIPTS/tested-integrations.json
```

This is an offline verifier: it does not download, sign, publish, install or
contact a device. It verifies the embedded core key, source ancestry, unchanged
contract paths, catalog preview status, artifact hashes, provenance fields and
the package's exact signed-index entry. The receipt names the candidate Couch
commit and has `artifact_bytes_verified: true`. Generate it after the candidate
commit is frozen; a receipt from another commit is refused.

Pass that receipt into the normal runtime inventory:

```sh
python3 tools/release/runtime_inventory.py \
  --integration-set tools/release/tested-integrations.json \
  --integration-receipt NEW_RECEIPTS/tested-integrations.json \
  build/alpine-staging-input.json NEW_RUNTIME_INVENTORY
```

`payload-inventory.json` then records the verified version set as release
provenance. The set is metadata, not a new `/opt/couch` file. The runtime
contents remain governed by `runtime_inventory.RUNTIME` and the oldest deployed
updater's allowlist. Run `update_floor.py` on both the inventory and staged tree
before signing as usual.

The live Preview index can change when another package is published. The
commit-addressed release archive keeps this tested snapshot reproducible. To
adopt a later feed snapshot, review its new immutable archive, advance the
tested-set manifest deliberately and regenerate the receipt. Never loosen the
verifier to accept an unreviewed mutable index.

## Existing-device pilot

1. Build, sign and publish a normal floor-compatible core runtime from the
   final candidate. The integration host is inside existing files such as
   `couch-confd`; it adds no runtime filename. Keep the Denon APK out of the
   runtime archive.
2. Validate the signed runtime update and rollback on an HA100. Updating the
   core preserves a legacy-readable configuration projection and does not
   install a package or migrate an existing connection automatically.
3. The tester explicitly updates the core, confirms
   `couch-confd --supports-integration-protocol=1`, opens **Integrations**, and
   explicitly installs Denon `0.1.1` from Preview.
4. Exercise package install, removal/reinstall and rollback separately from core
   runtime rollback. Retain the preview label until exact model/firmware and
   command behavior have physical evidence.

The released `.170` runtime predates the host. Publishing the Denon APK alone
does not enable it, and no feed operation should silently install or migrate an
existing user.

### Local unsigned runtime candidate

A macOS development checkout can produce a concrete runtime-only candidate
before the dedicated Linux release-host rebuild. Use a clean final checkout and
freshly build every one of the five Cargo executables; do not copy an older
`target/` directory or mix binaries from different source revisions. Provision
the pinned Zig toolchain and Sonos build input through the existing ignored
`build/` paths without printing or copying their contents into a receipt.

For a runtime-only candidate, build the same first five targets as
`tools/build-release.sh`:

```sh
TARGET=armv7-unknown-linux-musleabihf tools/build-gui.sh
TARGET=armv7-unknown-linux-musleabihf tools/build-webui.sh
(cd daemon && cargo build --locked --release \
  --target armv7-unknown-linux-musleabihf -p couch-system)
TARGET=armv7-unknown-linux-musleabihf tools/build-sonos.sh
(cd clients && cargo build --locked --release \
  --target armv7-unknown-linux-musleabihf -p couch-coreelec)
```

Build `couch-wmt-properties.so` and `fbcon` from their unchanged sources with
the documented cross compilers, or reuse only bytes independently verified as
the published `.170` inputs after confirming their source and build recipes are
unchanged. The runtime publisher requires the WMT helper. `runtime_inventory.py`
also inventories `fbcon`, although the runtime publisher does not select that
optional file unless its release tree explicitly names it.

The Bluetooth binaries, patched BlueZ, kernel and initramfs are boot-payload
inputs. Their source paths are unchanged from `.170` and they are not selected
by a runtime-only publisher invocation, so do not run the BlueZ portion merely
to prepare this core pilot. If any boot payload is included, return to the full
`tools/build-release.sh` and boot-candidate workflow instead.

Run runtime inventory and the compatibility-floor checks against these exact
outputs. The inventory records the five binary hashes and candidate source
commit but remains an inventory, not source-to-binary attestation. This local
candidate is suitable for host inspection and separately authorized private
testing. It is not a publishable release: the dedicated Linux host must rebuild
the frozen commit, produce release provenance/corresponding source, and use the
protected publisher signing process. An unsigned archive cannot be installed by
the production updater.

## Fresh-install candidate

A fresh install currently boots the older `.24` software embedded in the public
OS image and then asks the owner to update. A pilot that starts with the host
available requires a new full OS build from the final integration-capable core:

1. On the dedicated Linux release host, build the web bundle and all ARM runtime
   artifacts with `tools/build-wmt-properties.sh` and
   `tools/build-release.sh`. Run the tested-set verifier and runtime inventory
   in that same frozen checkout.
2. Assemble the pinned offline Alpine package closure and rootfs, then produce
   the owner-neutral userdata, installer RAM stage, boot/recovery payloads and
   logo through the existing public installer workflow. The exact-source build
   attestation must name the final Couch commit and the actual new file hashes.
3. Package and admit the public installer inputs with
   `package_public_installer.py` and the native host's `verify-public` command.
   Reusing unchanged host/TUI executables is valid only with their original
   source/build receipts; it does not permit reusing the old userdata/runtime
   identity.
4. Perform a complete physical fresh-install and recovery acceptance. Confirm
   that the first boot already supports protocol 1, but leave Preview/Denon
   installation as a separate owner action.

The full OS image may contain the core host and embedded public key. It must not
contain the Denon APK, an installed package slot, connection settings or an
automatic migration instruction.

## Candidate state and remaining gates

The last published `.170` release provides complete public installer, runtime,
boot, source and checksum assets, but its runtime does not contain the
integration host. Its runtime inventory and this checkout's runtime filename
inventory are identical, which is why a new core OTA can stay within the `.24`
compatibility floor.

A full candidate needs:

- a final clean source commit and fresh release-host web/ARM builds;
- a feed-byte verification receipt generated at that final commit;
- runtime inventory, floor checks, signed runtime manifest/archive and complete
  corresponding source/notices;
- a rebuilt public OS/userdata input set if fresh installs should start on the
  capable core, plus exact-source build attestations and installer admission;
- host and HA100 runtime update/rollback acceptance;
- a physical full install/recovery trial for the rebuilt OS image;
- Denon pilot evidence kept at its current read-only/lifecycle scope until full
  command and receiver model/firmware validation is performed.

Protected build or signing material stays on the release host and outside Git.
Packaging a candidate is not authorization to publish it.
