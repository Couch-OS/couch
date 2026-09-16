# Dual-booting stock Android and Couch (design exploration)

> **Status, 2026-09-12.** Exploration only. Nothing here is implemented or
> hardware-validated. It records what the HA100 already gives us, the options
> for a "try Couch without giving up Android" layout, a recommendation, and the
> hardware checks that have to pass before any of it is built. The second half
> covers a self-sufficient rescue image that can fetch and install Couch from
> GitHub, which turns out to be the same piece of work.

## Why

Today an install replaces Android's boot, recovery, userdata, logo and odmdtbo.
Going back means the desktop installer, the saved originals and a slow restore.
A remote that boots stock Android by default, and Couch on request, lowers the
cost of trying Couch to one small partition write and a held button, and makes
"give up" a menu item instead of a recovery procedure.

## What the device already gives us

These are facts from the repo, the device backup and the pinned factory
firmware, not assumptions.

- **lk has exactly two boot slots** (`boot`, `recovery`) and picks one from
  three inputs: the BCB in `para` (`boot-recovery` in the first 512 bytes
  selects `recovery`), the physical **Back** key held at power-on (selects
  `recovery`), and the lk boot menu (Volume Up held at power-on; Volume Up
  moves, Volume Down selects). Nothing else is needed from the bootloader and
  nothing here writes `lk` or `preloader_*`; the whole safety model depends on
  never doing that.
- **Boot images are unsigned.** The stock image has no signature block, so lk
  boots anything we pack (`tools/bootimg.py`). No unlock, no `seccfg` change.
- **The reverse layout has already run on hardware.** The first bring-up commit
  (`87fef87`, 2 Sep 2026) booted Couch from the `recovery` slot with stock
  Android untouched in `boot`; init cleared the BCB as its first act so the next
  reboot returned to Android, and the lk menu was the backstop. That is the
  layout proposed below. The opposite arrangement (Android's `boot.img` in the
  recovery slot, `tools/swap-slots.sh`) was scripted but there is no record it
  was ever booted, and it is the one with an open question (lk tags a recovery
  boot with `boot_mode=2`, which Android's vendor init scripts may react to).
- **Couch's kernel is self-contained.** The from-source `3.18.79` kernel carries
  its device-tree changes in its own appended DTB (`tools/dtbpatch.py`); the
  `odmdtbo` Couch writes today is the official OTA's copy, not a Couch-specific
  one. It has `CONFIG_F2FS_FS=y`, `CONFIG_BLK_DEV_LOOP=y`,
  `CONFIG_EFI_PARTITION=y` and `CONFIG_KEXEC` off.
- **Android's `/data` is plaintext by default.** The stock `fstab.mt6580` (read
  from the vendor image backup) mounts `userdata` as F2FS with
  `encryptable=.../metadata`, not `forceencrypt`. Encryption only exists if the
  owner turned it on in Settings, which is detectable (the F2FS superblock is
  absent and `metadata` carries a crypto footer).
- **Couch can take its Wi-Fi blobs from the stock `vendor` partition.**
  `stage2/hardware-init.sh` already falls back to mounting `/dev/mmcblk0p14`
  read-only when the private bundle is absent. Under dual boot `vendor` is
  always present, so the per-owner vendor assembly the installer does today is
  unnecessary for a trial install.
- **A 16 MiB image with Wi-Fi exists.** The installer's RAM stage packs the
  stock kernel, WMT loader/launcher with Bionic, firmware, `wpa_supplicant` with
  musl, BusyBox and a static rustls service under the 16 MiB boot limit
  (`tools/release/prepare_wifi_ramdisk.py`, `tools/installer/wifi-stage`).
- **On-device GitHub fetching exists.** `daemon/couch-updates` already lists
  GitHub releases over `ureq`/rustls, checks GitHub's asset digest, verifies the
  Ed25519 publisher signature and SHA-256, and stages/activates with rollback.
- **The setup hotspot exists.** `stage2/portal.sh` brings up `ap0` (the second
  wiphy the MediaTek driver registers when `A` is written to `/dev/wmtWifi`),
  hostapd, dnsmasq and a captive portal for entering Wi-Fi credentials from a
  phone.

Partition map (observed, 8 GB eMMC; `userdata` runs to the end of the user
area):

| partition | offset | size | dual-boot role |
| --- | --- | --- | --- |
| `boot` (p8) | 0x1d80000 | 16 MiB | **stock Android boot.img, untouched** |
| `recovery` (p9) | 0x2d80000 | 16 MiB | **Couch kernel + initramfs** (replaces stock recovery) |
| `para` (p10) | 0x3d80000 | 512 KiB | BCB: which slot boots next |
| `logo` (p11) | 0x3e00000 | 8 MiB | stock (lk shows it for both OSes) |
| `odmdtbo` (p12) | 0x4600000 | 16 MiB | stock, untouched |
| `expdb` (p13) | 0x5600000 | 10 MiB | Couch boot markers, boot-attempt counter |
| `vendor` (p14) | 0x6000000 | 288 MiB | stock; Couch reads WMT blobs from it |
| `system` (p21) | 0x1d800000 | 1224 MiB | stock, untouched |
| `cache` (p22) | 0x6a000000 | 112 MiB | stock |
| `userdata` (p23) | 0x71000000 | 5631 MiB | Android `/data`; Couch lives inside or beside it (see storage) |

## Slot arrangement: Android in `boot`, Couch in `recovery`

Recommended. Stock Android's boot path stays byte-for-byte stock, which is the
point of a trial. Couch occupies the recovery slot, entered by:

1. **The BCB.** Couch writes `boot-recovery` into `para` and leaves it there
   while Couch is the chosen OS. Stock Android never clears the BCB (only
   Android's recovery does, and it is gone), so once chosen Couch stays the
   default across reboots and power cycles.
2. **The Back key at power-on.** The zero-software path from Android into Couch.
   This is the line in the install guide: "hold Back while turning it on".
3. **`adb reboot recovery`.** Android 8.1's init writes `boot-recovery` into
   misc for a recovery reboot, so this lands in Couch. Useful for us; not a user
   path. There is no non-root Android app path: writing `para` needs root.

Leaving Couch for Android is a Couch GUI action ("Switch to Android"): the
system service clears the BCB and reboots. Because the BCB is cleared, Android
is then the default until the user holds Back again or Couch re-arms it.

This inverts today's BCB semantics, and the health check has to invert with it:

- Today `initramfs/init` arms `boot-recovery` first thing and clears it after a
  healthy GUI, so a broken Couch lands in the rescue image.
- In the trial layout `boot-recovery` *is* Couch. Init must instead keep a
  **boot-attempt counter in `expdb`** (next to the existing markers): increment
  at entry, clear after the same health check. Three consecutive uncleared
  attempts clear the BCB and reboot, so a Couch that repeatedly fails to reach a
  healthy GUI falls back to Android on its own. A kernel that hangs before init
  runs is not covered by this; the lk menu's Normal Boot is the manual escape,
  and that path must be verified to override an armed BCB (see checks).
- There is no separate rescue slot in this layout. The rescue role folds into
  the Couch image itself (second half of this document).

Two interactions with stock Android need a decision, not a fix:

- **Factory reset from Android Settings** reboots into "recovery" with
  `recovery\n--wipe_data` in the BCB, which is now Couch. Couch's init should
  recognise that command and either perform the wipe of Android's `/data`
  (mkfs.f2fs; trivial if Couch has its own partition, self-destructive if Couch
  lives inside `/data`) or refuse, clear the BCB and explain on screen. Refusing
  is the safe first version.
- **Sanytron OTA updates** apply through stock recovery, which is displaced.
  While dual-booted, Android cannot update. The pinned OTA is a full-image
  package that would also overwrite `recovery`, so this is a feature: an
  applied OTA would silently remove Couch. Document it.

Rejected alternatives:

- *Couch in `boot`, Android's `boot.img` in `recovery`* (the `swap-slots.sh`
  design). Keeps Couch as the default but puts stock Android on the slot lk
  tags as a recovery boot, which is unverified, and loses both Android's
  recovery and Couch's rescue image. Worth keeping as the "committed" layout
  later, once a user decides Couch is their daily OS and wants power-on to mean
  Couch without the BCB doing the work.
- *A kexec chooser in `boot`* that loads either kernel from files. Unlimited
  "slots" and keeps stock recovery intact, but `CONFIG_KEXEC` is off, MediaTek
  3.18 kernels are not known to survive kexec (lk-initialised panel, WMT, GIC
  and reserved-memory state), and it would need lk's exact cmdline and DTB
  reproduced for the second kernel. High effort, uncertain outcome. Not
  pursued.
- *Modifying lk* for a third slot or a real OS menu. Never write `lk`.

## Storage: where Couch's rootfs goes

Couch's rootfs is a compact ext4 image, today written raw over `userdata`. Under
dual boot Android needs `userdata` back. Two viable placements.

### Option A: a loop-mounted image inside Android's `/data` (recommended for the trial)

Couch's ext4 image becomes a preallocated regular file, e.g.
`/data/couch/rootfs.ext4`, on Android's F2FS. Couch's init mounts `userdata` as
F2FS, loop-mounts the image and continues exactly as today from `/mnt/alpine`.

Why first:

- **No partition map change.** No GPT rewrite, no shrink, nothing new for lk or
  the Android kernel to accept. Stock `userdata` stays exactly as the user has
  it, apps and pairings included. Uninstall is "delete one file and put stock
  recovery back".
- **Android reformatting is not required.** Shrinking `userdata` (option B)
  means either an F2FS shrink, which needs a recent `resize.f2fs` and validation
  on this 3.18-era F2FS, or a reformat that costs the user their Android setup
  and leans on the 5.9 GB backup/restore path. Neither is trial-friendly.
- **The image pipeline is unchanged.** `prepare_ext4.py` already produces
  exactly this file; only its target size changes.
- **The write is a file copy, not a raw partition write.** The Couch image can
  do it itself on first boot (from the host over the existing TLS transport, or
  from GitHub; see the rescue section). The only raw write in a trial install
  is the 16 MiB `recovery` slot.

Costs and risks:

- Loop over F2FS on a 3.18 kernel means buffered double caching and one more
  layer on every write. Couch's rootfs is mostly read, and there is ~980 MB of
  RAM. Measure GUI start and config-write latency on hardware before accepting.
- The F2FS on-disk format must be one both kernels agree on. Couch's kernel is
  the same 3.18.79 lineage as stock, so it must simply never mount with newer
  features; Android's `fsck.f2fs` runs on every Android boot and must stay
  clean after Couch has written. Clean unmount on every Couch reboot is
  mandatory, and `switch to Android` must sync first.
- If the owner has encrypted `/data`, Couch cannot read it. Detect (no F2FS
  magic at 1024/5120, crypto footer in `metadata`) and refuse with an on-screen
  message; the install path must check this before writing the recovery slot.
- Android factory reset removes Couch. Acceptable: the Couch image then finds
  no rootfs and offers to reinstall (rescue mode).
- The file must be preallocated (fallocate) at install, never grown lazily, so
  Android running low on space cannot corrupt Couch.

### Option B: carve a `couch` partition from the tail of `userdata`

Shrink `userdata` in place and append a `couch` GPT entry (1 to 1.5 GiB, sized
from the measured compact image; Android keeps ~4 GB). Both GPT copies are
rewritten with new CRCs; the original GPT is saved beside the other originals.
`userdata` keeps its start offset, so nothing else moves. lk reads the GPT at
boot rather than a compiled-in table, the kernel parses GPT, and Android's
`by-name` links come from partition names, so a new `couch` partition should be
invisible to Android. Whether lk really accepts a modified GPT on this unit
is check 6 below; the fallback if it does not is the factory restore, which
never writes the GPT and therefore cannot fix it, so this must be tested with
download mode and the saved GPT ready.

This is the better long-term shape: raw partition writes with the existing
O_DIRECT readback, no loop layer, an uninstall that is a GPT restore plus a
`userdata` reformat, and a "commit to Couch" that is a GPT edit growing `couch`
over a deleted `userdata`. Its cost is the Android `/data` shrink or reformat
described above and a new GPT writer with its own review, in an installer whose
core invariant today is "the observed layout must equal the pinned layout;
repartitioning is unsupported".

Recommendation: build the trial on option A, keep the GPT untouched, and revisit
option B only if the loop path fails its performance or robustness checks or
when the committed layout is designed.

Not viable: `cache` (112 MiB, and Android wants it), `system` (stock Android
lives there), anything requiring `lk` or `preloader` changes.

## What changes, by component

- `initramfs/init`: mount F2FS `userdata`, loop-mount the rootfs file (or mount
  the `couch` partition); boot-attempt counter in `expdb`; clear-not-arm BCB
  logic; recognise `--wipe_data`; enter rescue mode when no rootfs is found.
- `daemon/couch-system`: a `switch-to-android` command (sync, clear the BCB,
  reboot) and a `set-default` command (arm or clear the BCB). The GUI and web
  UI get the buttons. The `Run {command:"reboot"}` path is the model.
- `stage2/hardware-init.sh`: prefer `/dev/mmcblk0p14` for vendor blobs when
  dual-booted; the private bundle becomes optional.
- Installer: a "Trial install" mode whose write set is `recovery` only, plus a
  file placed into `/data` (by the Couch image itself on first boot, or by the
  stage over the existing TLS transport). `couch_install.WRITE_ORDER` and the
  partition-profile checks stay as they are; `logo`, `odmdtbo` and `userdata`
  are not written in trial mode. Android's original `recovery` is saved as it is
  today; the userdata backup is skipped because `/data` is not touched.
- Release tooling: the rootfs image gets a size target for the trial file rather
  than the partition size; the recovery-slot image is the normal Couch boot
  image plus rescue mode.
- Docs: `device-recovery.md` partition roles, the installer guide, and this
  document promoted from exploration to design once the checks pass.

The existing full install (Couch in `boot`, rescue in `recovery`, rootfs over
`userdata`) remains the "committed" path and is unchanged.

## A rescue image with Wi-Fi that installs Couch from GitHub

Feasible, and mostly assembled from parts that exist. In the trial layout there
is no spare slot for a separate rescue image, so this is a **mode of the Couch
boot image**, entered when init finds no usable rootfs (fresh trial install,
Android factory reset, corrupt image) or when the user asks for it. The stock
Android boot slot is the fallback for a Couch image that itself fails, which is
a better fallback than today's rescue image.

What it needs, and where it comes from:

| need | source | status |
| --- | --- | --- |
| Wi-Fi radio | WMT loader/launcher, Bionic libs, firmware from the stock `vendor` partition at run time | works today in stage2 via the p14 fallback; nothing vendor-owned ships in the image |
| supplicant, DHCP | `wpa_supplicant` + musl from the Alpine closure, BusyBox `udhcpc` | already packed by `prepare_wifi_ramdisk.py` |
| credentials | saved `/opt/couch/networks.conf` when a rootfs exists; otherwise the setup hotspot and captive portal from `stage2/portal.sh`, so a phone supplies the SSID/password and presses Install | hostapd, dnsmasq and httpd must move into the ramdisk (they live in the Alpine rootfs today); ~1.5 MB |
| fetch and verify | `couch-updates`: GitHub releases API, asset digest, Ed25519 publisher key baked into the image, SHA-256, bounded download | exists for runtime bundles; needs an "OS image" asset kind and a streaming write instead of tar extraction |
| write | fallocate + write + fsync + independent readback of the rootfs file (option A), or the RAM stage's verified partition writer (option B) | the stage's storage policy crate is the model; journal in `expdb` instead of on a host |
| screen | `fbcon` or the installer's progress renderer (`tools/installer/display`) | exists |
| size | stock kernel ~7 MB, BusyBox ~1 MB, supplicant + libs ~2.5 MB, hostapd/dnsmasq ~1.5 MB, static rustls fetcher ~3 to 4 MB, gzip | fits the 16 MiB slot with a few MiB to spare; the RAM stage proves the shape |

What it deliberately cannot do: change the partition map, write `lk`,
`preloader` or calibration, or fetch anything not signed by the pinned key.
It also cannot bootstrap itself onto a stock device: the first write of the
16 MiB image into the recovery slot still needs the desktop installer's
download-agent path, or `dd` from rooted Android (the 2 Sep bring-up did the
latter; whether `adb root` works on an unmodified retail unit is unverified).
After that first write the remote is self-sufficient: reinstall, upgrade the
OS, or recover from a wiped `/data`, all from the device with a phone.

This also fixes a gap in the current design: the rescue image today depends on
the Couch rootfs on `userdata` for its network, which is exactly the thing a
rescue is for.

## Hardware checks before building anything

Ordered so that each one costs at most a power cycle and none needs the
download agent. Keep the full device backup at hand.

1. Stock Android boots from `boot` with Couch in `recovery` and the BCB
   cleared; Android's own settings and pairings survive. (Expected: stock.)
2. Back held at power-on boots the `recovery` slot (documented; re-verify on the
   from-source kernel).
3. With `boot-recovery` armed, the lk menu's Normal Boot still boots Android.
   This is the manual escape from a Couch that hangs before init; if the menu
   does not override the BCB, the trial layout needs a different escape.
4. `adb reboot recovery` from stock Android lands in Couch.
5. Android Settings factory reset lands in Couch with `--wipe_data` in the BCB.
6. (Option B only) lk, the preloader and Android accept a GPT with `userdata`
   shortened and a `couch` entry appended. Do this from download mode with the
   original GPT saved.
7. Couch mounts Android's F2FS `/data` read-write, loop-mounts a preallocated
   ext4 file, runs the GUI from it, reboots to Android, and Android's boot-time
   `fsck.f2fs` stays clean. Measure GUI start and config-write latency against
   the raw-partition install.
8. Couch's Wi-Fi comes up from `vendor` (p14) blobs on the from-source kernel
   with the private bundle absent.
9. Couch's kernel runs correctly with the device's own `odmdtbo` rather than
   the OTA's copy the installer writes today (the factory and OTA copies
   already differ: 34960 vs 37120 bytes).
10. Whether `adb root` is available on stock retail firmware.

## Rough sequencing

1. Checks 1 to 5 and 7 to 9 on the bench with hand-built images (a day or two).
   These decide the design; nothing below starts without them.
2. Init and system-service changes: F2FS + loop mount, boot-attempt counter, BCB
   inversion, switch/default commands and GUI buttons.
3. Trial install mode in the installer: recovery-only write set, rootfs file
   placement, encryption detection, uninstall.
4. Rescue mode in the Couch image: hotspot portal, GitHub fetch and verified
   file write, `expdb` journal. This can ship after the trial mode; the trial
   works without it as long as the host places the rootfs file.
5. Committed layout and option B if wanted.

## Risks worth naming

- The lk boot menu not overriding an armed BCB (check 3) removes the manual
  escape; the mitigation is the boot-attempt counter plus download mode.
- F2FS cross-kernel corruption: same lineage, but Couch writing to Android's
  filesystem is new territory. Clean unmounts and Android's fsck are the
  evidence; a corrupted `/data` costs the user their Android setup, which is
  exactly what the trial promises not to do.
- Trial users who like Couch will run it long-term on the loop path; make sure
  the performance measurement in check 7 is on the GUI paths users feel.
- Displacing stock recovery blocks Android OTAs and turns factory reset into a
  Couch boot; both need to be in the trial guide.
