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
KEY_DIR="$HOME/.local/share/couch-integration-signing"
umask 077
mkdir -p "$KEY_DIR"
openssl genrsa -out "$KEY_DIR/developer.rsa" 4096
openssl rsa -in "$KEY_DIR/developer.rsa" -pubout \
  -out "$KEY_DIR/developer.rsa.pub"
chmod 600 "$KEY_DIR/developer.rsa"

tools/integrations/build-apk.sh echo 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-echo \
  clients/couch-echo/plugin.json "$KEY_DIR/developer.rsa" build/integrations
tools/integrations/build-apk.sh denon 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-denon \
  clients/couch-denon/plugin.json "$KEY_DIR/developer.rsa" build/integrations
tools/integrations/build-repository.sh "$KEY_DIR/developer.rsa" \
  build/integrations build/integration-repository
```

The corresponding `.rsa.pub` must sit beside the private key. The build
environment needs that public key in its APK key directory to check packages
while `abuild` makes its local index. Publish the resulting repository directory
over HTTPS; it contains `armv7/APKINDEX.tar.gz` and the signed APKs. Keep the
private key on the packaging host. Provision only the public key in a dedicated
Couch directory under `/opt/couch/integration-keys`. Integration-capable
runtimes use `/opt/couch/integration-keys/official` by default; custom feeds use
`/opt/couch/integration-keys/custom/NAME`. Do not add integration keys to
Alpine's global `/etc/apk/keys`.

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

## Install from a development host

The normal HA100 SSH server runs inside Alpine: `/opt/couch` in that session is
the directory an outer initramfs or USB serial shell sees as
`/mnt/alpine/opt/couch`. Detect the shell before choosing paths:

```sh
if [ -f /etc/alpine-release ]; then
  echo "Alpine shell"
elif [ -f /mnt/alpine/etc/alpine-release ]; then
  echo "outer initramfs shell"
fi
```

These commands require an integration-capable runtime; the current `.170`
device release predates the plugin host. From the normal Alpine SSH session,
this probe exits successfully once a suitable runtime is installed:

```sh
/opt/couch/runtime/current/couch-confd \
  --supports-integration-protocol=1
```

Create a development trust directory and copy only the public key and APK from
the build host:

```sh
ssh root@couch.local \
  'test -f /etc/alpine-release && \
   mkdir -p /opt/couch/integration-keys/custom/developer'
scp "$KEY_DIR/developer.rsa.pub" \
  root@couch.local:/opt/couch/integration-keys/custom/developer/
scp build/integrations/couch-integration-YOUR_ID-0.1.0-r0.apk \
  root@couch.local:/tmp/
ssh root@couch.local \
  '/opt/couch/runtime/current/couch-confd integrations \
    --keys-dir /opt/couch/integration-keys/custom/developer \
    install-sideload /tmp/couch-integration-YOUR_ID-0.1.0-r0.apk'
```

From an outer initramfs or serial shell, prefix the runtime command with
`chroot /mnt/alpine`; use `/mnt/alpine/...` when manipulating files outside
the chroot and Alpine paths such as `/opt/...` and `/tmp/...` in arguments to
the chrooted command. Do not run `apk add` against an integration package.
Direct installation bypasses Couch's audit, handshake, immutable slots,
activation record, saved-settings compatibility check, and rollback.

## Install from a custom repository

A custom repository is selected for one installation by passing both its
dedicated trust directory and base URL:

```sh
/opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/acme-lab \
  install-repository couch-integration-YOUR_ID \
  --repository https://packages.example.invalid/couch
```

There is no persistent repository configuration or repository-management UI
yet. Repeat `--repository` for every repository install. A proposed official
preview base is `https://dangerouslaser.github.io/couch-integrations/preview`,
using the default `/opt/couch/integration-keys/official`; it is not a public
feed until the signed index is deployed and the matching key is provisioned.
The initial stable feed is intentionally empty.

## Native-code boundary

Plugins are trusted native code. On a production Linux device the host launches
each plugin as a separate process, drops root to UID/GID 65534, and on the
HA100 grants supplemental group 3003 for ordinary network sockets. The framed
protocol, capability checks, timeouts, and process retirement provide fault and
privilege separation. They are not a full sandbox: plugins share an
unprivileged UID, can reach the LAN, and have no separate mount or network
namespace. Admit packages only from a signing key you trust.
