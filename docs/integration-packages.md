# Integration packages

The integration-capable development runtime provides a paired web
**Integrations** page for browsing, installing, updating, rolling back and
removing packages, plus persistent custom repositories with explicit key trust.
See [integration management](integration-management.md) for the operator flow.
The released `.170` runtime predates the host; these source capabilities require
a core update before they are available on a device.

Couch integrations are signed APKs containing one isolated plugin. Extraction
runs `apk` in a fresh private staging root with the configured trust keys,
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
directory. The binary's hello manifest must match before activation.

Every change to the installed payload, including the manifest or rebuilt
executable, **must bump
`manifest.version` and the matching APK `pkgver`**. Slots are immutable by
integration ID and manifest version. Publishing only `-r1` for different bytes
at the same manifest version is not an integration update and will be refused
as a conflicting slot; the packaging helper emits `-r0`.

Only regular files and directories are admitted. Links, devices, FIFOs, sockets,
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

# Inside the disposable Alpine packaging container:
install -Dm644 "$KEY_DIR/developer.rsa.pub" \
  /etc/apk/keys/developer.rsa.pub

tools/integrations/build-apk.sh echo 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-echo \
  clients/couch-echo/plugin.json "$KEY_DIR/developer.rsa" build/integrations
tools/integrations/build-apk.sh denon 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-denon \
  clients/couch-denon/plugin.json "$KEY_DIR/developer.rsa" build/integrations
tools/integrations/build-repository.sh "$KEY_DIR/developer.rsa" \
  build/integrations build/integration-repository
```

The corresponding `.rsa.pub` must sit beside the private key. `abuild -r`
creates and reads an intermediate APK index, so the disposable build container
needs that public key in its own `/etc/apk/keys`. Publish the resulting
repository directory over HTTPS; it contains `armv7/APKINDEX.tar.gz` and the
signed APKs. Keep the private key on the packaging host or mount it only into
the disposable container for packaging; never copy it to a device. Provision
only public keys on devices. The official public key is embedded in
`couch-confd`, delivered by the signed core runtime, and materialized in the
package store's private trust directories. No separate key-file copy is needed
for an official repository. The web page stores a custom key only after its
fingerprint is reviewed and confirmed. Manual CLI users may instead provision a
dedicated path such as `/opt/couch/integration-keys/custom/NAME` and select it
with `--keys-dir`. Neither method adds keys to Alpine's global `/etc/apk/keys`.

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

The paired web page persists custom repository URLs and keys after explicit
fingerprint confirmation. The CLI remains a separate per-invocation workflow:
repeat `--repository` and the custom `--keys-dir` for each manual install; it
does not automatically select a web-managed repository.

The public official feed bases are:

```text
https://packages.couch-os.dev/preview
https://packages.couch-os.dev/stable
```

Runtimes up to `.175.dev` read the official feeds only from
`dangerouslaser.github.io/couch-integrations`. Later runtimes try the address
above first and that one second, so they work before and after the feed
repository moves to the Couch-OS organization. Indexes are fetched with
redirects off, so the old address cannot simply forward: the feed must not
move, and the custom domain must not be attached, until remotes that use
integrations run a runtime with both addresses.

The installer appends `armv7`. Preview initially contains Denon. Stable serves
a valid signed empty index and has no installable packages until a
production-tier integration has validated hardware evidence.

The official public key can be inspected at
`https://packages.couch-os.dev/preview/couch-integrations.rsa.pub`.
Its PEM file SHA-256 is:

```text
80f3a73d86759cda103cb4f9a876cd4caee9d25c235c6d782b4be8a900b2696c
```

The integration-capable runtime carries that key inside `couch-confd`. For the
CLI's default trust selection, `Store::trust_keys` materializes it at
`/opt/couch/integrations/.official-keys/couch-integrations.rsa.pub` (under
`COUCH_INTEGRATIONS_DIR` when overridden). The historical default selector
`/opt/couch/integration-keys/official` now resolves to that embedded official
key; adding files to the historical directory does not extend official trust.
An explicit custom `--keys-dir` still uses the selected directory unchanged.
The web manager keeps separate key directories per repository and fingerprint
under `integrations/management/keys`. Keys do not cross repository trust scopes.

No manual key provisioning is needed for this official preview install:

```sh
/opt/couch/runtime/current/couch-confd integrations \
  install-repository couch-integration-denon \
  --repository https://packages.couch-os.dev/preview
```

The released `.170` runtime predates the package host and cannot run this
command. Install an integration-capable runtime first.

## Core rollback and saved configuration

External connections can be configured after the first integration-capable core
update. `config.json` contains an old-readable projection and a complete modern
`integration_config` extension in one atomic document; no second core update or
rollback-slot deletion is required. An old core sees external devices as
unconfigured and their external commands as disabled, while built-in devices
remain available. The complete extension survives a rollback without edits.

If the old core saves configuration, its rewrite drops the extension. The next
upgrade preserves those edits and offers a confirmed recovery export for
explicit restoration; it does not resurrect deleted devices. Packages and
private connection settings are retained independently. See
[core rollback and recovery](runtime-updates.md#integration-configuration-across-core-rollback)
for the confirmed/pending export distinction and whole-config import procedure.

## Native-code boundary

Plugins are trusted native code. On a production Linux device the host launches
each plugin as a separate process, drops root to UID/GID 65534, and on the
HA100 grants supplemental group 3003 for ordinary network sockets. The framed
protocol, capability checks, timeouts, and process retirement provide fault and
privilege separation. They are not a full sandbox: plugins share an
unprivileged UID, can reach the LAN, and have no separate mount or network
namespace. Admit packages only from a signing key you trust.
