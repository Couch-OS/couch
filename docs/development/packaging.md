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
mkdir -p "$KEY_DIR"
openssl genrsa -out "$KEY_DIR/developer.rsa" 4096
openssl rsa -in "$KEY_DIR/developer.rsa" -pubout \
  -out "$KEY_DIR/developer.rsa.pub"
chmod 600 "$KEY_DIR/developer.rsa"
```

Keep the private key on the packaging host. Only the `.rsa.pub` file belongs
in a device trust directory.

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

Both sideload and repository installation require a valid APK signature from a
key already provisioned in the selected integration trust directory. Sideload
does not mean unsigned. Keep each feed's public keys under a dedicated path
such as `/opt/couch/integration-keys/custom/my-feed`; do not add integration
keys to Alpine's global `/etc/apk/keys`. An integration-capable runtime defaults
to `/opt/couch/integration-keys/official`. Pass `--keys-dir` explicitly for a
custom or developer feed so it cannot inherit official or unrelated system
trust. Never commit a private signing key or copy it to a device or package.

For a repository, collect signed APKs and create a signed Alpine index:

```sh
tools/integrations/build-repository.sh \
  "$KEY_DIR/developer.rsa" \
  build/integrations \
  build/repository
```

Host that directory over HTTPS after provisioning the matching public key on
the Couch device.

## Install and operate on a remote

The SSH root shell on an HA100 is the outer initramfs, while `couch-confd` and
Alpine's `apk` run inside the mounted Alpine system. Enter that environment for
package operations; running the bare command from the outer shell will not have
the required APK tooling.

These commands require an integration-capable Couch runtime. The current `.170`
device release predates this host and cannot install integration APKs. After an
eligible runtime is installed, this probe exits successfully:

```sh
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd \
  --supports-integration-protocol=1
```

The development CLI is exposed under `couch-confd integrations`. From the
outer root shell, invoke it through the Alpine chroot and name the exact trust
directory for that package source:

```sh
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/developer \
  install-sideload /path/inside/alpine/couch-integration-YOUR_ID-0.1.0-r0.apk
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
  --keys-dir /opt/couch/integration-keys/custom/my-feed \
  install-repository couch-integration-YOUR_ID \
  --repository https://packages.example.invalid/couch
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations list
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations rollback YOUR_ID
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations remove YOUR_ID
```

The sideload path must be visible inside `/mnt/alpine`; copy the APK into that
filesystem first or use the corresponding path after entering the chroot.

For example, copy a development package and its **public** key from the build
host, then admit the package through Couch:

```sh
ssh root@couch.local \
  'mkdir -p /mnt/alpine/opt/couch/integration-keys/custom/developer'
scp "$KEY_DIR/developer.rsa.pub" \
  root@couch.local:/mnt/alpine/opt/couch/integration-keys/custom/developer/
scp build/integrations/couch-integration-YOUR_ID-0.1.0-r0.apk \
  root@couch.local:/mnt/alpine/tmp/
ssh root@couch.local \
  'chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
    --keys-dir /opt/couch/integration-keys/custom/developer \
    install-sideload /tmp/couch-integration-YOUR_ID-0.1.0-r0.apk'
```

The paths given to `scp` and the outer SSH shell include `/mnt/alpine`. Paths
given after `chroot /mnt/alpine` do not. Installing with `apk add` directly is
not equivalent: it bypasses Couch's payload audit, protocol handshake,
immutable slot store, activation record, and rollback path.

Custom repositories use the same CLI. Couch does not currently save repository
URLs or expose repository management in the web UI, so every repository
installation supplies the URL and its dedicated key directory explicitly:

```sh
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
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

- Use a unique ID and a version that matches the embedded manifest.
- Publish source and license information required by your dependencies.
- Keep the signing key offline and distribute only its public half.
- Test admission, install, upgrade, rollback, and removal on a disposable root.
- State which Couch source or release the package was tested against.
- Do not describe a repository as public until it is actually hosted.

## Hosting a feed on GitHub

An APK feed is static files: a signed `APKINDEX.tar.gz` and its signed APKs
under an architecture directory. The public
[`dangerouslaser/couch-integrations`](https://github.com/dangerouslaser/couch-integrations)
repository is the home for reviewed sources, catalog, and build workflow.
GitHub Pages will serve this layout once feed publishing is enabled:

```text
preview/armv7/APKINDEX.tar.gz
preview/armv7/couch-integration-YOUR_ID-0.1.0-r0.apk
stable/armv7/APKINDEX.tar.gz
```

The proposed preview URL is
`https://dangerouslaser.github.io/couch-integrations/preview`; the installer
adds `armv7`. It is not a published feed until that URL serves a signed index
and the matching official public key ships on Couch. The initial `stable` feed
is deliberately empty; a preview Denon package does not become stable merely
because it is hosted.
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
and deploy the complete index and package set together. Archives and release
receipts can also live in GitHub Releases.

Provision an official public trust key on the remote once. The current CLI does
not persist the repository URL, so each install still names the preview or
stable URL. Future integration releases can then ship independently of the
core runtime. Key rotation must overlap trusted old and new keys before
removing the old key. Changing hosting does not remove the initial runtime
update needed to install the plugin host and package manager.

## Source references

- [`tools/integrations/build-apk.sh`](https://github.com/dangerouslaser/couch/blob/main/tools/integrations/build-apk.sh)
- [`tools/integrations/build-repository.sh`](https://github.com/dangerouslaser/couch/blob/main/tools/integrations/build-repository.sh)
- [`docs/integration-packages.md`](https://github.com/dangerouslaser/couch/blob/main/docs/integration-packages.md)
