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
installed. Keep the private key outside the repository.

```sh
tools/integrations/build-apk.sh \
  YOUR_ID 0.1.0 \
  clients/target/armv7-unknown-linux-musleabihf/release/couch-plugin-YOUR_ID \
  clients/couch-YOUR_ID/plugin.json \
  /secure/path/integration.rsa \
  build/integrations
```

The helper builds a package named `couch-integration-YOUR_ID`, sets its
architecture to `armv7`, and adds no install scripts.

## Trust

Both sideload and repository installation require a valid APK signature from a
key already provisioned in the device's integration trust directory. Sideload
does not mean unsigned. Never commit a signing key or copy it into a package.

For a repository, collect signed APKs and create a signed Alpine index:

```sh
tools/integrations/build-repository.sh \
  /secure/path/integration.rsa \
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

The development CLI is exposed under `couch-confd integrations`. From the
outer root shell, invoke it through the Alpine chroot:

```sh
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
  install-sideload /path/inside/alpine/couch-integration-YOUR_ID-0.1.0-r0.apk
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations \
  install-repository couch-integration-YOUR_ID \
  --repository https://packages.example.invalid/couch
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations list
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations rollback YOUR_ID
chroot /mnt/alpine /opt/couch/runtime/current/couch-confd integrations remove YOUR_ID
```

The sideload path must be visible inside `/mnt/alpine`; copy the APK into that
filesystem first or use the corresponding path after entering the chroot.

Repository installation verifies the signed index, then verifies the package
again during admission. The store keeps immutable version slots and active and
previous slot hashes paired in one atomic state record. A candidate must pass
payload audit and protocol handshake before activation. Rollback swaps the two
hashes in that record.

Removing a package does not erase a user's connection record. Commands stop
until a compatible package is installed again.

## Distribution checklist

- Use a unique ID and a version that matches the embedded manifest.
- Publish source and license information required by your dependencies.
- Keep the signing key offline and distribute only its public half.
- Test admission, install, upgrade, rollback, and removal on a disposable root.
- State which Couch source or release the package was tested against.
- Do not describe a repository as public until it is actually hosted.

## Source references

- [`tools/integrations/build-apk.sh`](https://github.com/dangerouslaser/couch/blob/main/tools/integrations/build-apk.sh)
- [`tools/integrations/build-repository.sh`](https://github.com/dangerouslaser/couch/blob/main/tools/integrations/build-repository.sh)
- [`docs/integration-packages.md`](https://github.com/dangerouslaser/couch/blob/main/docs/integration-packages.md)
