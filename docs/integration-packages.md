# Integration packages

Couch integrations are signed APKs containing one isolated plugin. `apk` is
used only in a fresh private staging root with the configured trust keys,
`--no-network`, and `--no-scripts`; Couch never mutates Alpine's package
database or permits package scripts/triggers.

Each APK installs exactly this payload:

```text
/usr/lib/couch/integrations/ID/
  manifest.json
  bin/couch-plugin-ID
```

`manifest.json` is the protocol-v1 `couch-plugin::Manifest`. Its ID must match
the directory and its executable must be a normal relative path below that
directory. The binary's hello manifest must match before activation. Only
regular files and directories are admitted. Links, devices, FIFOs, sockets,
set-id/world-writable files, excess files/bytes, and paths outside the payload
are rejected.

Build the supplied plugin binaries with:

```sh
(cd clients && cargo build --release --target armv7-unknown-linux-musleabihf \
  -p couch-echo --bin couch-plugin-echo)
(cd clients && cargo build --release --target armv7-unknown-linux-musleabihf \
  -p couch-denon --bin couch-plugin-denon)
```

Build an APK with the reusable script in an Alpine `abuild` environment. It
requires `abuild`, `abuild-sign`, `readelf`, and `jq`; it refuses a non-ARM
binary and a manifest whose ID, version, or executable does not match the
requested package, then produces an `armv7` APK with no scripts:

```sh
tools/integrations/build-apk.sh echo 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-echo \
  clients/couch-echo/plugin.json DEV_KEY.rsa build/integrations
tools/integrations/build-apk.sh denon 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-denon \
  clients/couch-denon/plugin.json DEV_KEY.rsa build/integrations
tools/integrations/build-repository.sh DEV_KEY.rsa build/integrations build/integration-repository
```

The corresponding `DEV_KEY.rsa.pub` must sit beside the private key. The build
environment needs that public key in its APK key directory to check packages
while `abuild` makes its local index. Publish the resulting repository directory
over HTTPS; it contains `armv7/APKINDEX.tar.gz` and the two signed APKs.
Provision only the public key in Couch's integration trust directory.

On a Docker-capable Linux build host, `tools/integrations/smoke.sh OUTPUT_DIR`
reads **every** entry in `integrations/catalog.json`, then performs the package
flow with a newly generated ephemeral key for every declared binary/manifest.
It leaves `packages.tsv`, packages, a signed `test-repository`, public test
key, and `lifecycle.log` in `OUTPUT_DIR`; its container is removed afterwards.
The test repository deliberately includes both `test-only` Echo and `preview`
Denon entries and is never a production feed. Do not use its key for a release.
The harness sideloads and repository-installs each entry, verifies an empty
trust store rejects a valid signed first package without state, and records the
archive-integrity rejection of an appended-byte package. It requires Docker
support for `linux/arm/v7` execution (QEMU/binfmt on an x86 build host), an
ARMv7 binary for every catalog entry, and the ARMv7 `couch-confd` binary.

`couch-confd integrations` provides:

```text
install-sideload SIGNED.apk
install-repository NAME --repository URL
list
rollback ID
remove ID
```

Sideloading is explicit but not unsigned: both flows require native APK
signature verification against configured keys. Repository fetch validates the
signed index and admission repeats APK verification. The store defaults to
`/opt/couch/integrations`, or `COUCH_INTEGRATIONS_DIR`, with immutable
`slots/ID/VERSION` and an atomic `state/ID` JSON selection holding active and
previous version/hash receipts. Candidates are unpacked, audited and
handshake-checked before activation. Slot files and state are synced before
rename. Every resolution rechecks the selected tree hash; an incompatible or
tampered active plugin falls back to a verified previous slot without erasing
the active record. Installation and rollback also verify that the candidate
accepts saved settings for every existing connection. `rollback` swaps the two
selections.

Before `apk` extracts anything, Couch accepts only bounded Alpine APK v2
concatenated gzip/tar streams. It rejects archive links, special entries,
escaping names, excessive members, and excessive compressed or decompressed
sizes. Native `apk` then verifies the package signature inside a fresh private
root before Couch audits the extracted tree.
