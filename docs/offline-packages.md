# Offline Alpine package closure

`tools/release/package_closure.py` prepares a **noninstallable** ARMv7 package cache on Linux. Run it on a dedicated Linux build host; it does not contact the remote or install anything into a device/rootfs. Docker runs as the invoking user, with no capabilities, a read-only filesystem and only a new output directory mounted writable. No home directory, keys, device backups, configuration, or Docker socket is mounted.

The default runtime roots match `tools/provision-alpine.sh`: `wpa_supplicant`, `openssh`, `iw`, `tzdata`, `hostapd`, and `dnsmasq`. The last two retain the optional recovery portal; they are not required to launch the normal on-device Wi-Fi setup. BusyBox/IP/DHCP and shared libraries resolve transitively. This is not a general Alpine development environment.

```sh
# In the checkout on the dedicated Linux build host; output must not exist.
python3 tools/release/package_closure.py prepare build/offline-armv7
python3 tools/release/package_closure.py verify build/offline-armv7 --authenticate
python3 -m unittest discover -s tools/release -v
```

The builder is Alpine 3.21.7, pinned to its image digest and Linux/amd64 platform. APK resolves **ARMv7** against an empty installed database, uses ARMv7 public signing keys from that builder, saves signed v3.21 main/community indexes, downloads the recursive closure and verifies package signatures. It then simulates installation without network access or maintainer scripts. `--authenticate` repeats signature verification using the pinned builder's keys, with Docker networking disabled and the cache mounted read-only. The builder image must already be cached for fully offline operation.

`closure.json` records exact APK filenames/versions, URLs, SHA-256 hashes, index/key hashes, root package requests, APK version and builder digest. Plain `verify` checks inventory integrity; it does not authenticate a publisher. The manifest itself is unsigned and must eventually be covered by the signed Couch release inventory. See Alpine's [package manager documentation](https://docs.alpinelinux.org/user-handbook/0.1a/Working/apk.html) for dependency and signature handling.

Optional repeated `--package name=version` arguments replace the default root set.

Validation on the dedicated Linux host resolved 26 packages, verified every signature, and passed the offline simulation with Docker networking disabled. A negative fixture omitted `libcrypto3` and correctly failed dependency resolution. Local tests reject tampering, missing/extra files, symlinks, unexpected package URLs, unpinned builders and option injection, and cover the retained archive: deterministic bytes, a restore/verify round trip, a package tampered with inside the archive, an archive that does not match the baseline pin, and that `--authenticate` still runs the offline solve after a restore.

`--architecture x86_64` prepares a separate host-tool closure for [userdata image creation](userdata-image.md); ARM runtime assembly explicitly rejects those tool packages.

## Re-resolving a version list is not reproducible

Preparation resolves the current contents of the versioned branch; it is **not** a promise that another online preparation will return identical versions. `requested` records only the explicit roots. Every transitive dependency — 131 of the 141 packages in the current ARM closure — resolves to whatever the mirror serves that day.

This was measured on the dedicated Linux build host on 2026-09-17. Re-preparing with byte-identical roots produced a `closure.json` of `27cf056d…` where `b51d36e9…` was pinned. The only difference was `libcrypto3` and `libssl3` moving `3.3.7-r0` → `3.3.7-r1`, **one day** after that baseline was pinned. Asking for the superseded revisions explicitly does not recover them either: `apk` reports `ERROR: unable to select packages`, because Alpine deletes superseded revisions from the mirror. Pinning more versions therefore cannot make a closure reproducible — the packages stop existing. The same mechanism lost the `1804ab4b…` closure that `ha100_os_baseline.json` still names, and it recurs every time OpenSSL or any other dependency ships a patch revision.

The durable release input is the **retained closure archive**, not the version list. A manifest hash, a source URL or a builder digest cannot reconstruct bytes that upstream deleted.

## Retain the closure archive

`archive` writes a verified closure directory as a single byte-deterministic archive: the verified manifest file set plus `closure.json`, sorted, with mode, ownership and timestamps normalized. The same closure always produces the same bytes, so the archive can be re-derived and compared rather than merely trusted.

```sh
# On the dedicated Linux build host, after prepare and verify --authenticate.
python3 tools/release/package_closure.py archive build/offline-armv7 \
  --archive couch-alpine321-armv7-closure.tar.gz
sha256sum couch-alpine321-armv7-closure.tar.gz
```

`restore` is the reverse, and it ends in exactly the `verify` a prepared directory takes — there is no second, weaker trust path. Bound the bytes first, the way the integrations feed snapshot is bound:

```sh
printf '%s  %s\n' PINNED_SHA256 couch-alpine321-armv7-closure.tar.gz | sha256sum -c -
python3 tools/release/package_closure.py restore REVIEW/closure \
  --archive couch-alpine321-armv7-closure.tar.gz --authenticate
```

Extraction refuses absolute, traversing, duplicate, non-regular or oversized entries before writing, then `verify` recomputes every hash in `closure.json` and re-derives the whole inventory. `--authenticate` behaves exactly as it does for a prepared directory: it still re-runs signature verification and the offline dependency solution inside the pinned builder with Docker networking disabled. The archive is not a way to skip authentication.

`tools/release/prepare_rootfs.py SPEC CLOSURE OUTPUT` accepts either a prepared closure directory or the retained archive. An archive is checked against the reviewed pin before extraction, restored into a temporary directory that is removed afterwards, and then assembled through the unchanged path.

### Two pins, and why both stay

`ha100_os_baseline.json` carries `package_closure_archive` and `package_closure_sha256`. They are not redundant:

- `package_closure_archive` (`file`, `size`, `sha256`) binds the exact bytes a build host received. A modified `.apk` inside the archive fails here, before anything is extracted.
- `package_closure_sha256` binds the reviewed inventory identity. It survives re-archiving or re-compression, and it is what `os_baseline.seed` writes the on-device capability marker against. Anyone who edits a package **and** rewrites `closure.json` to match produces a self-consistent archive that `verify` accepts; this pin is what refuses it.

### Where the archive is stored

Nothing in this repository publishes the archive. Storing it is a separate publishing decision, and the shape to follow already exists: the integrations feed distributes an immutable `feed-<commit>` release asset pinned by SHA-256 and checked with `sha256sum -c -` (see [integration release rollout](integration-release-rollout.md#produce-the-tested-set-receipt)). Adopting one means:

1. Prepare and `verify --authenticate` a closure on the dedicated Linux build host.
2. `package_closure.py archive` it and record the file name, byte size and SHA-256.
3. Publish those exact bytes as an immutable release asset on a commit- or version-addressed tag, never a mutable one.
4. Set `package_closure_archive` in `ha100_os_baseline.json` to that `{file, size, sha256}`, together with the matching `package_closure_sha256`, and review it as a baseline change.

This baseline now pins the retained closure `b51d36e9` (141 armv7 packages) together with its archive `ha100-closure-b51d36e9-141pkg.tar.gz`. It replaces `1804ab4b`, the 134-package closure the baseline named until now: several of that closure's package revisions were deleted from the Alpine mirrors and no copy of it survived anywhere, so it could not be restored, re-solved or re-downloaded, and no OS image could be assembled at all — precisely the failure the retained archive removes. `b51d36e9` was archived with `package_closure.py archive` from a verified directory, and restoring it reproduces that directory byte-for-byte.

Step 3 above is still open: the archive bytes are retained on the dedicated build host, not yet published as an immutable release asset, so a build host needs that file supplied out of band.

`null` in this field means archive input is **refused**; it never means any archive is accepted. A replacement closure needs a newly reviewed inventory, a new baseline pin and the applicable OS assembly and hardware validation; it does not inherit the previous closure's approval.

### Set-ID files

Staging refuses every set-user-ID and set-group-ID file, in the pinned base archive and again in the assembled rootfs, because such a program runs with privileges its caller does not have. One file is admitted by name, in `clean_stage.REVIEWED_SET_ID`, matched on its exact path, mode and content digest:

| Path | Mode | Package | Reviewed |
|---|---|---|---|
| `usr/libexec/dbus-daemon-launch-helper` | `04750` | `dbus-daemon-launch-helper-1.14.10-r4` | 2026-09-22 |

This is D-Bus's system-bus activation helper. When a client asks the system bus for a service that is not running, the helper starts it under that service's own user account, which is why upstream ships it set-uid root with a group-restricted mode. Couch does not use D-Bus activation: it starts `couch-bluetoothd` itself and nothing on the remote registers an activatable system service, so the helper is never invoked. It is admitted because excluding it is worse, not because it is needed — it arrives with the Bluetooth packages in the OS closure, and the [first-use install](bluetooth.md#system-packages-on-first-use) already puts this exact file on every remote where Bluetooth has been switched on, so refusing it in the image would leave the image's inventory different from every running remote's while changing nothing about what is installed.

The digest is pinned, so a future package revision of the same file fails the check and comes back for review. Any other set-ID file, and this path with a different mode or different bytes, still fails the build.

## Remaining assembly work

The clean staging specification and this closure can now be combined with `prepare_rootfs.py`; see [clean release staging](releases.md#assemble-offline-packages). It installs packages/scripts in an isolated ARM-compatible root, then normalizes and rescans the archive. Building and signing partition images remains a separate step. Preserve first-use SSH host-key generation and empty onboarding configuration. Do not use the old provisioning script's `--allow-untrusted` fallback for releases.

The clean stager explicitly allows the four reviewed extensionless recovery CGI destinations and requires executable modes. Vendor licensing/calibration handling, observed partition layouts and real recovery/boot validation remain release gates. Neither the staging tarball nor this APK cache is flashable.
