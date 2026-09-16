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

Preparation resolves the current contents of the versioned branch; it is **not** a promise that another online preparation will return identical versions. Preserve the entire resulting cache as the pinned release input. Optional repeated `--package name=version` arguments replace the default root set. Upstream mirrors may remove older versions; hashes cannot restore missing bytes.

Validation on the dedicated Linux host resolved 26 packages, verified every signature, and passed the offline simulation with Docker networking disabled. A negative fixture omitted `libcrypto3` and correctly failed dependency resolution. Local tests reject tampering, missing/extra files, symlinks, unexpected package URLs, unpinned builders and option injection.

`--architecture x86_64` prepares a separate host-tool closure for [userdata image creation](userdata-image.md); ARM runtime assembly explicitly rejects those tool packages.

## Remaining assembly work

The clean staging specification and this closure can now be combined with `prepare_rootfs.py`; see [clean release staging](releases.md#assemble-offline-packages). It installs packages/scripts in an isolated ARM-compatible root, then normalizes and rescans the archive. Building and signing partition images remains a separate step. Preserve first-use SSH host-key generation and empty onboarding configuration. Do not use the old provisioning script's `--allow-untrusted` fallback for releases.

The clean stager explicitly allows the four reviewed extensionless recovery CGI destinations and requires executable modes. Vendor licensing/calibration handling, observed partition layouts and real recovery/boot validation remain release gates. Neither the staging tarball nor this APK cache is flashable.
