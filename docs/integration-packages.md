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

## Why it works this way

An integration is an Alpine package, and yet Couch never runs `apk add` on the
remote's own system to install one. Both halves are deliberate.

The package format is Alpine's because the hard parts are already solved
there: a signed package, a signed index of what a repository offers, version
comparison, and a verifier (`apk`) that is already in the OS image. Couch
writes none of that; it calls `apk verify`, `apk fetch` and `apk version`, and
anyone can build and host a repository with stock Alpine tools.

The package is not installed into the OS, because:

- **The OS is outside Couch's updates and rollback.** The Alpine root comes
  from the OS image the installer writes over USB. Couch updates itself in
  runtime slots under `/opt/couch/runtime` and can step back to the previous
  slot; nothing does that for `/usr` or Alpine's package database. A package
  installed there would be a change no Couch update carries and no rollback
  undoes.
- **Install scripts and triggers run as root.** Repositories added by the
  owner exist, so packages are third-party code. `apk add` would run whatever
  a package's scripts say, as root, on the real system. Couch unpacks with
  `--no-scripts` and `--no-network` into an empty private root made for that
  one package, and keeps only the payload directory from it.
- **Trust has to stay separate.** The feed's key must not be able to sign an
  OS package, and Alpine's keys must not be able to sign an integration. Each
  repository has its own key directory, passed with `--keys-dir`; nothing is
  ever added to `/etc/apk/keys`, and keys do not cross repositories.
- **Versions need slots.** Couch keeps each version in its own immutable
  directory (`slots/ID/VERSION`) with an active and a previous selection,
  rechecks the selected tree's hash every time it is used, falls back to the
  previous version when the active one fails, and rolls back on request. `apk`
  keeps one installed version and has no way back.
- **Dependencies could move the OS.** A real `apk add` resolves dependencies,
  and may install or upgrade system libraries to satisfy them. The private
  root has no repositories and no network, so a package that depends on
  anything cannot be unpacked at all.

This has a price. Couch carries code that a plain `apk add` would not need
(the archive preflight, the payload audit, slots, the handshake before
activation), and every integration has to be a self-contained static program:
it cannot lean on a shared library from the OS.

Bluetooth is the one place Couch does run a plain `apk add` on the OS, and the
contrast is the point. Switching Bluetooth on for the first time adds `dbus`,
`bluez` and `bluez-deprecated`: official Alpine packages, from the image's own
Alpine repositories, checked with the Alpine keys the image shipped with. They
are OS packages, so the OS is where they belong. Even so, the step first runs
`apk add --simulate` and goes ahead only if the plan is nothing but new
installs; if it would upgrade or remove anything the image shipped with, it
stops.

## Isolation between packages

Every package used to run as the same user, 65534. Each installed package now
gets a user of its own: a number between 60000 and 64999, written down in
`uids.json` at the root of the package store the first time Couch sees that
package, and never given to a second one. Removing a package keeps its row, so
a package installed a year later cannot end up as the user an old one ran as.
The group is the same number as the user.

On the remote's kernel, this is what that buys between two *different*
packages:

- Neither can attach a debugger to the other, read `/proc/<pid>/mem` or
  `/proc/<pid>/environ`, look at its open files or its memory map, or copy
  memory out of it another way.
- Neither can send the other a signal.
- Neither can read a file the other owns, nor a file only root can read -
  which is where Couch keeps connection settings and keys.
- Neither can write a core file. A package that crashes leaves no copy of what
  it held in memory on the disk.
- Neither can gain privileges by running something else: `no_new_privs` is
  set, as it always was.

Most of that rests on one thing the package itself does: `couch_plugin::serve`
makes the process undumpable as its first act, which hands `/proc/<pid>` to
root. The host cannot do it on the package's behalf - `execve` puts the flag
back. Packages published before this SDK were not built with that call and
stay dumpable. That is accepted: they hold no key of their own, and a read
across two different users is refused by the kernel regardless.

What this does **not** do:

- It does not hide that other processes exist. Any package can list `/proc`
  and see every process on the remote, with its command line and its user.
- It does not restrict the network. A package can reach anything on the LAN,
  and anything on the Internet, exactly as before.
- It does not separate two connections of the *same* package. Both children
  are that package's user and can read each other.
- It is not a defence against a package that means harm. It is separation
  between packages that are merely independent of one another.

Nothing on disk is given to these users, and no package file changes owner or
mode, so a core rolled back to a release that predates all this runs every
package as it did before, under the one user, and ignores `uids.json`.

Not done, and worth doing later: mounting `/proc` with `hidepid=2` so that a
package sees only its own processes, a small seccomp deny-list covering the
calls a device integration never needs, and per-user firewall rules so that a
package can only reach the device it is configured for.

## Building and installing

Build the supplied plugin binaries with:

```sh
(cd clients && cargo build --release --target armv7-unknown-linux-musleabihf \
  -p couch-echo --bin couch-plugin-echo)
(cd clients && cargo build --release --target armv7-unknown-linux-musleabihf \
  -p couch-sonos --bin couch-plugin-sonos)
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
tools/integrations/build-apk.sh sonos 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-sonos \
  clients/couch-sonos/plugin.json "$KEY_DIR/developer.rsa" build/integrations
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
Sonos entries and is never a production feed. Do not use its key for a release.
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
repository moves to the Couch-OS organization. Runtimes from before
[feed metadata](#feed-metadata) fetch indexes with redirects off, so for them
the old address cannot simply forward: the feed must not move, and the custom
domain must not be attached, until remotes that use integrations run a runtime
with both addresses. Later runtimes follow [redirects](#redirects) to HTTPS
addresses, and still try both addresses.

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

## Feed metadata

A signed index proves who made it. It does not say when, and it has no order.
Someone between a remote and a feed could serve an old, validly signed index
for ever, or an older one than the remote has already seen, and so hide a
package that was fixed. A remote would also learn that a package needs a newer
Couch only after downloading and unpacking it.

So a feed publishes two more files beside each index:

```text
CHANNEL/armv7/feed.json       what the feed offers right now
CHANNEL/armv7/feed.json.sig   a signature over exactly those bytes
```

`feed.json` carries a `sequence` number that only goes up, the times it was
`issued` and `expires` (30 days; the feed signs again at least weekly), the
size and SHA-256 of the one `APKINDEX.tar.gz` it describes, and for every
package its file name, size, SHA-256 and the protocol versions from its
manifest. The signature is RSA PKCS#1 v1.5 with SHA-256, made with the same
key that signs the index and the packages:

```sh
openssl dgst -sha256 -sign KEY -out feed.json.sig feed.json
openssl dgst -sha256 -verify PUB -signature feed.json.sig feed.json
```

On every refresh, and again just before an install, the remote downloads the
index, then the two files, and checks, in this order:

1. The signature is valid for that repository's trusted key (the key the owner
   confirmed, or the built-in official key). Metadata that is there but
   unsigned, or signed by anyone else, is refused.
2. It is a format this Couch reads (`schema` 1). A later format counts as no
   metadata; see below for what that means.
3. For an official repository, `channel` is the channel the repository is
   (preview metadata cannot be served as stable).
4. `sequence` is not lower than the highest one this remote has accepted from
   that repository. The same number again is fine. A lower one is refused:
   "The package feed is older than one this remote has already seen".
5. It has not expired.
6. The downloaded index is byte for byte the one the metadata names.

A repository that fails any of these offers nothing on that refresh, and the
Integrations page says which repository and why. The other repositories still
load. A feed being republished at the moment of a refresh can fail check 6
once; the next refresh reads a consistent pair.

**The clock.** A remote can start with its clock unset. If the clock reads
more than a day before the metadata was issued, it is not believed: the expiry
check is skipped, the log says so, and everything else still applies. A clock
later than `expires` is the expiry case, and refuses.

**When metadata is required.** What a remote remembers is kept per repository
in `integrations/management/feed-state.json`: the highest sequence accepted,
and that metadata has been seen. A repository that has never published
metadata works as it always did. Once a remote has accepted metadata from a
repository, that repository must keep providing it: missing metadata, or a
format this Couch cannot read, is then refused. Removing a repository and
adding it again starts over. The record belongs to the repository, not to an
address, so the official feed's previous address cannot serve something older
than the current one did. The official repositories follow the same rule for
now, because the official feed does not publish metadata yet; a later Couch
will require it of them from the start.

**Refusing before the download.** A package whose `min_core_protocol_version`
is higher than this Couch speaks is listed as not installable, "Needs a newer
Couch". The Integrations page shows the reason and disables Install or Update
for it, and the remote refuses the request without downloading anything. A
connection waiting for its package (see
[built-in integrations that became packages](integration-migration.md)) keeps
waiting with that reason instead of trying an install that cannot work.

**After the download.** The file `apk fetch` produced must have the name, size
and SHA-256 the metadata promised, before `apk` is asked to open it. `apk`
still verifies the package's signature afterwards.

### Redirects

The index and the metadata may be redirected up to three times, and only to an
`https://` address. A redirect to plain HTTP, or to anything else, is a failed
download. Following a redirect cannot make a bad feed acceptable: what arrives
is checked against the key, the sequence and the hash whatever address served
it. Plain HTTP is still refused, because it would show everyone on the network
which packages a remote asks for.

The package itself is downloaded by `apk fetch`, which does its own HTTP.
Couch does not change that. In Alpine 3.21 (`apk-tools` 2.14.6, its bundled
`libfetch/http.c`, confirmed by running it against a redirecting server):

- 301, 302 and 303 are followed, for the index `apk` reads and for the package;
  307 and 308 are not, and fail the download.
- At most four redirects per file (`MAX_REDIRECT 5` counts requests).
- The scheme is not checked. A redirect from HTTPS to plain HTTP is followed
  without a warning, and so is one to another host.

So a feed that redirects package downloads must use 301, 302 or 303. A
redirect cannot get another package installed: `apk` verifies the package
against the repository's key, and with feed metadata the file must also match
the size and hash the feed signed.

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
