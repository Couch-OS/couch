Title: Packaging and signing
Description: Build, sign, publish, install, and roll back an integration APK.
Order: 5

# Packaging and signing

Couch integration packages are signed, ARMv7 Alpine APKs with one manifest and
one executable. The tooling and commands described here are a **developer
preview**. Do not assume they exist on a released device until its release notes
say so.

## Package layout

An APK contains exactly:

```text
/usr/lib/couch/integrations/ID/
  manifest.json
  bin/couch-plugin-ID
```

The ID in the directory, manifest, package name, and binary name must match.
The manifest's executable path is relative to the integration directory.
Every installed payload change, including a rebuilt executable or changed
manifest, **must bump `manifest.version` and the matching APK `pkgver`**.
Couch stores immutable `ID/VERSION` slots and rejects different bytes under an
existing version. Publishing only an APK revision change such as `-r0` to `-r1`
cannot replace that slot or serve as an integration update. The helper emits
`-r0` for each new manifest version.

Candidates are unpacked into a private staging root and audited before
activation. Couch rejects links, devices, FIFOs, sockets, set-id or
world-writable files, files outside the payload, and packages over the file or
byte limits. Package scripts and triggers are not allowed.

## Build the ARM binary

```sh
(cd clients && cargo build --release \
  --target armv7-unknown-linux-musleabihf \
  -p couch-YOUR_ID --bin couch-plugin-YOUR_ID)
```

Run Cargo from `clients/` so it loads that workspace's ARM linker
configuration.

## Create a signed APK

Run Couch's packaging helper in Alpine with `abuild` and `abuild-sign`
installed. The ARM binary can be cross-compiled on Linux or macOS, but the APK
helper itself needs an Alpine packaging shell or container. Create a dedicated
development key outside the source checkout:

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
```

`abuild -r` creates and reads an intermediate APK index, so that disposable
build container must trust the matching public key. Its `/etc/apk/keys` is
build-only. Keep the private key on the packaging host or mount it only into
that container for packaging. Never copy it to a device. Device trust remains
separate in Couch's repository-scoped directories or an explicitly selected CLI
key directory; do not put an integration key in the device's global Alpine
`/etc/apk/keys`.

```sh
tools/integrations/build-apk.sh \
  YOUR_ID 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-YOUR_ID \
  clients/couch-YOUR_ID/plugin.json \
  "$KEY_DIR/developer.rsa" \
  build/integrations
```

The helper builds a package named `couch-integration-YOUR_ID`, sets its
architecture to `armv7`, and adds no install scripts.

## Trust

Both sideload and repository installation require a valid native APK signature.
Sideload does not mean unsigned. The integration-capable runtime embeds the
official public key in `couch-confd`, which arrives through the signed core
update. Official web or CLI installation needs no separate key-file transfer.
The CLI's historical default `/opt/couch/integration-keys/official` is now a
selector for the embedded key, materialized in the private package store at
`/opt/couch/integrations/.official-keys` (`COUCH_INTEGRATIONS_DIR` overrides the
store root). Files added to the historical directory do not add official trust.

For a custom feed, use the paired web **Integrations** page to enter its HTTPS
base URL and public PEM key, compare the displayed SHA-256 fingerprint with the
owner's published value, then select **Trust repository**. The URL and key are
persisted only after confirmation, with a trust directory scoped to that
repository and fingerprint. Couch does not automatically fetch or trust a
custom feed's key. Removing its repository record leaves installed packages and
saved connection settings intact.

For a manual custom or developer CLI install, provision only the public key in
a dedicated path such as `/opt/couch/integration-keys/custom/my-feed` and pass
`--keys-dir` explicitly. That option still reads the selected custom directory;
it does not inherit official or unrelated system keys. Never commit a private
signing key or copy it to a device or package.

For a repository, collect signed APKs and create a signed Alpine index:

```sh
tools/integrations/build-repository.sh \
  "$KEY_DIR/developer.rsa" \
  build/integrations \
  build/repository
```

Host that directory over HTTPS and register its public key through the paired
web page, or select a manually provisioned custom key directory in the CLI.

## Manage packages in the paired web UI

After the first integration-capable core update, open **Integrations** in the
paired configuration web UI. **Refresh packages** verifies signed indexes from
the built-in Stable and Preview sources and any confirmed custom repositories.
Choose **Install**, **Update to …**, **Restore previous version**, or
**Remove package** as appropriate. Operations report progress and failure;
removal retains saved connections and credentials. The page distinguishes a
verified previous-version fallback from a missing or invalid package. Downloads
and installation follow explicit user actions; package updates are not automatic.

An older retained core does not force a second upgrade before integrations can
be enabled. The configuration file contains an old-readable projection and a
complete modern extension. Rollback keeps built-in devices usable; external
controls require the capable core. If the old core saves edits, re-upgrade keeps
those edits and offers an explicitly restorable recovery export rather than
automatically reintroducing deleted bindings. The recovery import replaces the
whole configuration, so export the current house first.

The following SSH CLI remains supported for development, signed sideloads and
manual repair. Web repository registration and CLI `--repository` selection are
separate workflows.

## Install and operate on a remote

The normal HA100 SSH server runs inside Alpine, so an SSH session sees
`/opt/couch` and `/opt/couch/integration-keys` directly. The USB serial and
early recovery shells run in the outer initramfs, where the same files are
under `/mnt/alpine`. Detect which shell you have before copying files or adding
`chroot`:

```sh
if [ -f /etc/alpine-release ]; then
  echo "Alpine shell"
elif [ -f /mnt/alpine/etc/alpine-release ]; then
  echo "outer initramfs shell"
fi
```

These commands require an integration-capable Couch runtime. The published
`v0.1.0-alpha.20260916.171.dev` prerelease is protocol-v1-capable; the older
`.170` release predates the host. Treat the following probe, rather than a
release-number assumption, as the compatibility check on a particular remote:

```sh
/opt/couch/runtime/current/couch-confd \
  --supports-integration-protocol=1
```

The development CLI is exposed under `couch-confd integrations`. In the normal
Alpine SSH session, name the exact trust directory for that package source:

```sh
/opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/developer \
  install-sideload /tmp/couch-integration-YOUR_ID-0.1.0-r0.apk
/opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/my-feed \
  install-repository couch-integration-YOUR_ID \
  --repository https://packages.example.invalid/couch
/opt/couch/runtime/current/couch-confd integrations list
/opt/couch/runtime/current/couch-confd integrations rollback YOUR_ID
/opt/couch/runtime/current/couch-confd integrations remove YOUR_ID
```

For example, copy a development package and its **public** key from the build
host into the normal Alpine SSH environment, then admit it through Couch:

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
`chroot /mnt/alpine` and use `/mnt/alpine/opt/...` or `/mnt/alpine/tmp/...`
when manipulating files outside the chroot. Command arguments after `chroot`
remain Alpine paths such as `/opt/couch/integration-keys/custom/developer` and
`/tmp/package.apk`.

Installing with `apk add` directly is not equivalent: it bypasses Couch's
payload audit, protocol handshake, immutable slot store, activation record,
and rollback path.

Custom repositories can be saved in the paired web UI as described above. A
manual CLI installation still supplies its URL and dedicated key directory on
each invocation:

```sh
/opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/acme-lab \
  install-repository couch-integration-YOUR_ID \
  --repository https://packages.example.invalid/couch
```

Repository installation verifies the signed index, then verifies the package
again during admission. The store keeps immutable version slots and active and
previous slot hashes paired in one atomic state record. A candidate must pass
payload audit and protocol handshake before activation. Rollback swaps the two
hashes in that record.

Removing a package does not erase a user's connection record. Commands stop
until a compatible package is installed again.

An admitted integration is trusted native code. The host starts it in a
separate process and drops root to the unprivileged integration identity; on
the HA100 it grants only the supplemental network group needed to open normal
Internet sockets. Protocol framing, deadlines, capability checks, and process
retirement contain failures. This is privilege separation, not a complete
sandbox: integrations share an unprivileged UID, can reach the LAN, and do not
run in separate mount or network namespaces. Install code only from a feed
whose signing key you trust.

## Distribution checklist

- Use a unique ID and a version that matches the embedded manifest; bump it for every payload change.
- Publish source and license information required by your dependencies.
- Keep the signing key offline and distribute only its public half.
- Test admission, install, upgrade, rollback, and removal on a disposable root.
- State which Couch source or release the package was tested against.
- Verify the signed index, public key, and package URLs after each deployment.

## Hosting a feed on GitHub

An APK feed is static files: a signed `APKINDEX.tar.gz` and its signed APKs
under an architecture directory. The public
[`dangerouslaser/couch-integrations`](https://github.com/dangerouslaser/couch-integrations)
repository holds publication policy, the immutable source graph, and the Pages
build and deployment workflow. The staged schema-2 source-pin graph pins the
shared Couch tooling and every selected integration repository independently at
full commits. Each integration repository owns `integration.json`, `plugin.json`,
its lock file, implementation, and reusable-harness tests. Couch remains the
single owner of the SDK, framed protocol, admission harness, and APK tooling;
integration repositories pin `couch-plugin` and `couch-sdk` to the same full
Couch commit and reuse `couch-plugin::testing` instead of copying the protocol. GitHub
Pages serves this layout:

```text
preview/armv7/APKINDEX.tar.gz
preview/armv7/couch-integration-YOUR_ID-0.1.0-r0.apk
stable/armv7/APKINDEX.tar.gz
```

The feed base URLs are:

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

The installer adds `armv7` when it fetches the index. `preview` initially
contains the Denon integration. `stable` serves a valid signed empty index and
contains no packages until an integration has production-tier hardware
evidence; preview hosting does not make Denon stable.

The official public key is
[`couch-integrations.rsa.pub`](https://packages.couch-os.dev/preview/couch-integrations.rsa.pub).
Its PEM file SHA-256 is:

```text
80f3a73d86759cda103cb4f9a876cd4caee9d25c235c6d782b4be8a900b2696c
```

The integration-capable runtime already embeds that public key. Inspect the
published fingerprint through a trusted source when auditing it; do not add it
to Alpine's global key store. The complete manual preview install command is:

```sh
/opt/couch/runtime/current/couch-confd integrations \
  install-repository couch-integration-denon \
  --repository https://packages.couch-os.dev/preview
```

The `.170` runtime predates the package host; publishing the feed alone does
not make that command available there. Confirm protocol support on the target
runtime with the probe above before attempting an install.
GitHub Packages does not offer a native APK registry among its
[supported formats](https://docs.github.com/en/packages/learn-github-packages/introduction-to-github-packages).
Pages is suitable for an initial public feed within its
[limits](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits):
1 GB published size and a soft 100 GB monthly bandwidth limit. Larger feeds
can keep source and CI on GitHub and move static delivery to object storage.

Publishing needs a trusted post-merge or approved release workflow, protected
signing-key access, and Pages deployment permissions. Never expose the signing
key to pull-request builds. Require admission before publication, publish only
eligible catalog tiers, retain prior immutable package versions for rollback,
record the integration repository and commit, SDK repository and commit,
tooling repository and commit, and binary and manifest hashes in schema-2
provenance. Deploy the complete index and package set together. Archives and release receipts can also live in GitHub Releases.

The core update supplies the official trust key and the paired web page retains
confirmed custom repositories. The CLI does not persist its `--repository`
argument, so manual installations still name the preview or stable URL.
Integration releases can then ship independently of the core runtime. Official
key rotation requires a reviewed core trust update and coordinated feed signing;
it cannot be authorized by files added to the old default key directory.
Changing hosting does not remove the first runtime update needed to install the
plugin host and package manager.

## Source references

- [`tools/integrations/build-apk.sh`](https://github.com/Couch-OS/couch/blob/main/tools/integrations/build-apk.sh)
- [`tools/integrations/build-repository.sh`](https://github.com/Couch-OS/couch/blob/main/tools/integrations/build-repository.sh)
- [`docs/integration-packages.md`](https://github.com/Couch-OS/couch/blob/main/docs/integration-packages.md)
