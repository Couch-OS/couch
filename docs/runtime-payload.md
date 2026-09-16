# Clean runtime payload inventory

`tools/release/runtime_inventory.py BASE_SPEC NEW_OUTPUT` audits explicit local build artifacts and emits `payload-inventory.json`. When the clean Couch portion passes, it also emits a ready-to-use `staging-input.json` for [offline package assembly](releases.md#assemble-offline-packages). It never contacts the remote, reads personal configuration, executes vendor extraction, or copies a vendor tree into the image.

```sh
tools/build-wmt-properties.sh
tools/build-release.sh   # couch-gui, couch-confd, couch-system, couch-sonos, couch-coreelec, couch-bt-bridge, couch-bt-hid, couch-bluetoothd
python3 tools/release/runtime_inventory.py build/alpine-staging-input.json build/runtime-payload
python3 tools/release/prepare_rootfs.py build/runtime-payload/staging-input.json \
  build/offline-armv7 build/packaged-runtime
```

`BASE_SPEC` supplies the pinned clean Alpine input and timestamp. Its artifact list is replaced by the explicit runtime list. Run on the checkout holding the ARM build artifacts; on the release host that is the checkout `tools/build-release.sh` just ran in ([releases.md](releases.md#build-host)). The inventory's Git commit describes the audited checkout, **not** an attestation that existing binaries were built from that commit. Tracked payload changes are represented only by a cleanliness flag and diff hash; their contents are not copied into the report.

## Runtime files

| Role | Files and checks |
|---|---|
| GUI, configuration server, boot console | `couch-gui` (built with `tools/build-gui.sh`), `couch-confd` (`tools/build-webui.sh`), `couch-system`, `fbcon`; little-endian ARM32 ELF, no dynamic-loader/library requirement. The GUI and daemon builds compile in the Sonos developer key from `build/sonos-api-key` when present ([sonos.md](sonos.md#release-builds)) |
| CoreELEC control | `couch-coreelec`, built in the clients workspace; static ARM32 ELF installed at `/opt/couch/couch-coreelec` |
| Sonos LAN control | `couch-sonos`, built with `tools/build-sonos.sh`; static ARM32 ELF installed at `/opt/couch/couch-sonos` |
| Bluetooth HID daemon | `couch-bt-hid`, built in the clients workspace (`-p couch-bt-hid`); static ARM32 ELF. The BLE HID peripheral: registers the HID GATT app with bluetoothd (zbus) and advertises over raw HCI. Started, with dbus + bluetoothd + the bridge, by the system service's Bluetooth toggle (`couch_system::bluetooth`, all under one `chroot /mnt/alpine`). Needs `dbus` and `bluez` in the image. A copy also rides in the boot ramdisk (`/extra/couch-bt-hid`) with the bridge, and the system service copies both into shared `/tmp` when the runtime lacks them, so a runtime bundle can stay within the strict allowlist of updaters before .142 |
| Bluetooth bridge | `couch-bt-bridge`, built in the clients workspace (`-p couch-bt`); static ARM32 ELF at `/opt/couch/couch-bt-bridge`. In the runtime bundle from .143 (updaters from .142 accept extra top-level `couch-*` executables); a copy also rides in the boot ramdisk (`/extra/couch-bt-bridge`) as a fallback for the first Bluetooth-capable image. The system service starts it only on the Bluetooth toggle, looking beside itself (the runtime slot) first, then `/extra`, then `/opt/couch`; a whole-file lock keeps it a singleton |
| Patched bluetoothd | `couch-bluetoothd`, Alpine 3.21's BlueZ 5.79 `bluetoothd` with the patch in `third_party/bluez` (bonded devices' CCC values survive a restart), built by `third_party/bluez/build.sh` in the pinned `alpine:3.21` arm/v7 container, which `tools/build-release.sh` runs when the patch, script or README changed (output `build/bluez/`). A musl ARM32 ELF linked against the Alpine root's glib, dbus and libudev, so it runs inside the chroot, never from the initramfs; the inventory checks that instead of static linking. The system service starts it in place of `/usr/lib/bluetooth/bluetoothd` when the runtime carries it. GPL-2.0-or-later: `licenses/BlueZ-GPL-2.0.txt` rides along, and a release carrying it publishes the `bluez` source component ([corresponding-source.md](corresponding-source.md)) |
| Runtime scripts | `stage2.sh`, `runtime-boot.sh`, `hardware-init.sh`, `gui-start.sh`, `system.sh`, `confd.sh`, `setup-mode.sh`, `portal.sh`, `wifi-conf.sh`, `station.sh` |
| Recovery portal | `www/index.html` and exactly `cgi-bin/{save,setpw,scan,enroll}` |
| Readable notices | Lato/Inter OFL, Lucide ISC, IRDB and BlueZ (GPL-2.0, LGPL-2.1, BSD-2-Clause) notices under `/opt/couch/licenses` |
| GUI resources | Lato fonts, Slint source/assets and build-policy hashes; fonts/images are embedded by Slint, and the raw Lucide alpha catalog is verified inside the GUI binary |
| Configuration webUI | Every current `web/couch-web/dist` file must occur verbatim in the daemon binary, including HTML, JavaScript and WASM; a placeholder or stale bundle fails |

This payload is described in four places: the lists in this script, the destination allowlist in `tools/release/clean_stage.py`, the `REQUIRED` names the updater enforces in `daemon/couch-updates/src/staging.rs`, and the build order in `tools/build-release.sh`. `tools/release/test_runtime_payload_lists.py` compares them, so drift between them fails CI instead of a device.

Provider clients are Rust libraries linked into GUI/daemon binaries. Sonos and CoreELEC also ship standalone CLIs; see [Sonos LAN client](sonos.md) for browser setup, remote controls and diagnostic commands. The offline Alpine closure includes `openssh-client-default` for optional CoreELEC OS controls. Package assembly checks the actual ARM client version and strict batch/host-key options with `ssh -G`, which does not contact a device. The diagnostic WMT script is omitted. No settings, network profiles, user configuration, enrollment keys, host keys, properties snapshot, device identity or calibration enters the generated artifact list. The clean stager still generates empty onboarding configuration.

Both existing Lato font files matched upstream Google Fonts binaries; the previously missing OFL notice is now included. The inventory also records offline, locked Cargo package/license metadata for the UI, daemon, and clients workspaces on the ARM target, including build dependencies. This is a declared-license inventory, not automatic approval of a license choice or a binary-reachability analysis. In particular, Slint 1.17.1 declares GPL/Slint license alternatives; the release needs an explicit reviewed route and complete dependency notices.

## Vendor and boot inputs remain separate

The vendor report hashes expected WMT binaries, stock recovery modules, firmware patches, Bionic loader/libraries and SELinux property contexts. It records the entire existing firmware/context directory contents for review. Their presence does not establish redistribution permission; vendor files remain rejected by clean artifact staging.

The original local bundle lacked (subsequently recovered in a private verified bundle; see below):

- `vendor/etc/selinux/nonplat_property_contexts`
- `system/etc/selinux/plat_property_contexts`
- `system/etc/ld.config.txt`

Source inputs are the original `vendor.img` and `system.img` backups. Before accepting an extraction, record their verified hashes/origin, exact extracted-file hashes and the applicable redistribution/source/notice evidence. A mixed Android/vendor bundle must not be assigned one invented blanket license. Review its library dependencies and firmware selection; the historical extractor includes more than just the Wi-Fi files.

The development extractor now processes `SYSTEM_DIRS`, retains nested `vendor/etc/selinux`, and creates `system/etc` before dumping its linker configuration. A fixture exercises the actual shell with fake Docker/debugfs and confirms all three paths. This does **not** make the old extractor a release tool: it still has a live-device fallback, a mutable builder tag, and incomplete-input warnings. The inventory does not invoke it.

Boot/recovery candidate images are inspected in memory for Android v0/zImage structure, complete newc bounds, private or appended ramdisk payloads, and current init/BusyBox/fbcon/boot-health members. Their hashes and source-file hashes are recorded. No image is repacked or flashed. Still required: the selected kernel's matching source/config/compiler manifest, DTB/header provenance, reproducible BusyBox/fbcon sources, correctly sized boot and recovery images, and corresponding-source/notice review. Existing normal/diagnostic kernel output manifests are candidates, not proof of the latest deployed kernel; select and verify the exact intended artifacts on Ollie.

## Validation and release status

The audit found 21 clean runtime artifacts, verified all 2,087 current web assets in the daemon, and inventoried 470 GUI/179 daemon dependency records. On Ollie this payload assembled into a 1,301-entry rootfs and a full-size userdata ext4 file that passed `e2fsck`. The resulting archive contains the actual Couch GUI/daemon/scripts, replacing the earlier Alpine+CGI-only fixture; vendor inputs are still absent.

The inventory reports `clean_runtime_ready` separately from `payload_complete` and `installable`; the latter remain false. Clean source-to-binary attestations, vendor completeness/provenance/rights, project/dependency license review, a signed complete release inventory, and physical boot/recovery validation remain gates. The user's existing source changes were preserved, and no physical device interaction occurred.

## Private offline vendor recovery

`private_vendor.py` is a separate, deliberately private workflow. It verifies
both complete original image hashes, reads raw ext4 with host `debugfs` in
read-only mode, checks every retained file against its original image, and
extracts only the three missing property/linker configuration files. Its fixed
33-file allowlist excludes NVRAM, calibration, property snapshots and personal
configuration. It refuses extra files, symlinks, mismatches and existing output
directories. It does not execute a vendor binary, mount an image, contact a
device, or fall back to ADB.

```sh
python3 tools/release/private_vendor.py PRIVATE_EXISTING_BUNDLE PRIVATE_NEW_BUNDLE \
  --system-image PRIVATE_BACKUPS/system.img --system-sha256 ORIGINAL_SYSTEM_SHA256 \
  --vendor-image PRIVATE_BACKUPS/vendor.img --vendor-sha256 ORIGINAL_VENDOR_SHA256
python3 tools/release/audit_vendor_elf.py PRIVATE_NEW_BUNDLE PRIVATE_ELF_AUDIT.json
python3 tools/release/prepare_private_rootfs.py CLEAN_PACKAGED_ROOTFS \
  PRIVATE_NEW_BUNDLE PRIVATE_ROOTFS
python3 tools/release/prepare_ext4.py PRIVATE_ROOTFS OFFLINE_E2FSPROGS \
  tools/release/ha100_userdata_geometry.json PRIVATE_IMAGE \
  --private-vendor-bundle PRIVATE_NEW_BUNDLE
```

The private overlay validates the original clean archive first, accepts only
exact vendor paths and hashes from verified provenance, and preserves all
clean configuration/credential checks. The default image-builder path rejects
private staging; the explicit flag and matching bundle provenance are required.
All resulting manifests retain `private_only: true`, `installable: false`, and
`redistribution_authorized: false`. These outputs must remain outside Git and
public release artifacts.

Offline validation on Ollie recovered the missing three files and verified all
30 retained files against the original system/vendor backups. The latest
runtime package contains 23 explicit Couch artifacts, including the IR catalog's
MIT and CC0 notices. Its private vendor overlay contains 1,348 entries; a raw
5,905,055,744-byte userdata image passed `e2fsck -fn`. Independent repeats
produced byte-identical normalized archives and raw ext4 images. The 32 release
tests cover source mismatches, forbidden files, private/default-path separation,
hash checks, reproducibility and dependency-closure classification. These are private candidate
artifacts, not clean-HEAD build attestations or boot validation.

The ELF audit finds no missing **WMT loader/launcher transitive** dependency
names. Bionic supplies `ld-android.so` through the linker, as documented in
[Android O's linker implementation](https://android.googlesource.com/platform/bionic/+/refs/heads/oreo-r4-release/linker/dlfcn.cpp).
The additional bundled `libutils.so`, outside that WMT dependency closure,
references absent `libvndksupport.so`; resolve or remove that unused-library
branch before claiming the entire historical bundle is dependency-complete.
Static dependency-name checks do not validate dynamic namespaces, symbol
versions, firmware behavior or physical Wi-Fi startup.

The normal runtime also requires `build/couch-wmt-properties.so`, built with
`tools/build-wmt-properties.sh`. It uses the same narrow detected-chip property
bridge as the RAM installer; only the Android WMT launcher receives LD_PRELOAD.
A missing radio must not be interpreted as missing saved Wi-Fi credentials.
