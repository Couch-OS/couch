# Bluetooth and BLE

Status as of 2026-09-14: **Bluetooth does not work on Couch, on either kernel,
and it has never been exercised.** The bridge daemon, kernel config and image
package changes for the first spike are on this branch, see the
[implementation plan](#implementation-plan-branch-bluetooth-rebased-onto-dev-2026-09-14). This document records what exists, what is
missing, the intended path to a BLE HID remote with per-activity pairings, and
the staging checklist. Kernel-side tasks are mirrored in the kernel tree at
`Documentation/couch/bluetooth.md` on the `bluetooth` branch of
[dangerouslaser/couch-kernel](https://github.com/dangerouslaser/couch-kernel).

## Goal

Let the HA100 act as a Bluetooth Low Energy HID peripheral (keyboard plus
consumer-control remote) towards a TV or streaming box, keep one bond per
target device, and switch which device it is connected to when the user
switches activity. Simultaneous connections are not a goal.

## What exists today

| layer | state | evidence |
|---|---|---|
| radio | MT6580 CONSYS combo chip, Bluetooth 4.0 with LE | MediaTek datasheet; same block that provides Wi-Fi, which works |
| MediaTek transport | built in: `CONFIG_MTK_COMBO_BT=y`, `CONFIG_MTK_BTIF=y` | `kernel/couch-ha100.config` |
| `/dev/stpbt` | created by `connectivity/common/conn_soc/linux/pub/stp_chrdev_bt.c`; opening it powers the BT function on through WMT and pumps raw HCI packets over STP | kernel source |
| Linux Bluetooth core | **off**: `# CONFIG_BT is not set` | `kernel/couch-ha100.config` line 987 |
| HCI device for BlueZ | **none**: `/dev/stpbt` is a character device, not an `hci_dev` | kernel source |
| firmware | the CONSYS ROM patch is already linked into `/` for Wi-Fi by `stage2/hardware-init.sh`; the same WMT patch load covers Bluetooth | stage2 |
| Bluetooth MAC | recorded by the installer as `bluetooth_mac` in the identity backup; stock loads it from NVRAM, our kernel has no NVRAM path | `tools/installer` |
| userland stack | **none**: no BlueZ in the Alpine image, no Bluetooth code in the daemon, GUI or stage2 | repository grep |
| validation | "built in, never exercised" on both from-source and stock | `kernel/README.md` matrix |

Why the core is off: stock Android on MediaTek runs the Bluedroid stack in
userland straight over `/dev/stpbt` through `libbt-vendor`, so the vendor tree
never configured the kernel stack. Nothing in Couch replaces Bluedroid.

## The gap

BlueZ needs an `hci0` registered with the kernel Bluetooth core. The MediaTek
driver only offers a character device. Two ways to close the gap:

1. **Virtual HCI bridge (spike).** Enable `CONFIG_BT` and `CONFIG_BT_HCIVHCI`,
   then run a small daemon that shuttles packets between `/dev/vhci` and
   `/dev/stpbt`. About a hundred lines, no new kernel code, and it answers
   "does the radio work" in a day.
2. **In-kernel STP HCI driver (proper).** A driver that calls
   `mtk_wcn_wmt_func_on(WMTDRV_TYPE_BT)`, registers an `hci_dev`, and maps
   `hdev->send` to `mtk_wcn_stp_send_data(..., BT_TASK_INDX)` and the STP
   receive callback to `hci_recv_frame`. Replaces the daemon once the spike
   proves the radio.

## Kernel constraints to plan around

- Linux 3.18 has no separate LE toggle; enabling `CONFIG_BT` includes LE.
- The management `Add Advertising` command arrived in Linux 4.2, so the BlueZ
  `LEAdvertisingManager1` D-Bus API will not work here. Advertising, including
  directed advertising for device switching, must be driven through raw HCI
  or the older management commands.
- LE Secure Connections landed after 3.18. Pairing falls back to legacy LE
  pairing, which TVs and streaming boxes generally accept. Verify per target.
- The controller comes up without a programmed address unless we write the
  recorded `bluetooth_mac` with a vendor command after power-on.

## Multi-device switching design

BLE HID peripherals connect to one central at a time. The model that works
for multi-device keyboards applies here:

- BlueZ stores one bond per central; nothing extra is needed to keep several.
- Each activity references the bonded address it should talk to.
- On activity switch: disconnect the current central, then directed-advertise
  (or advertise with a whitelist) to the target's address until it connects.
- Pairing a new device is a GUI flow: undirected connectable advertising for a
  bounded time, then store the bond and offer it in the activity editor.

## Implementation plan (branch `bluetooth`, rebased onto `dev` 2026-09-14)

The first milestone is the spike: prove the radio through Linux's virtual HCI
device with BlueZ on top. Everything below it needs a kernel with `CONFIG_BT`
and `CONFIG_BT_HCIVHCI`. Since [boot image updates](runtime-updates.md#boot-image-updates)
that kernel reaches a remote on the Dev channel as the boot payload of a dev
build, no reinstall; only the BlueZ packages still wait for a new OS image and
are `apk add`ed over SSH on the development remote meanwhile.

**What is on this branch**

- `clients/couch-bt`: `couch-bt-bridge`, the daemon that shuttles H4 packets
  between `/dev/vhci` and `/dev/stpbt`. It reassembles the STP side's byte
  stream into whole frames (vhci requires one packet per write), creates the
  virtual controller explicitly (events written before it exists are
  refused), handles the STP whole-chip reset errnos (88 waits, 99 reopens),
  and can send the set-address vendor command first so `hci0` carries the
  owner's recorded `bluetooth_mac`. The opcode defaults to MediaTek's 0xFC1A
  and is a flag because the HA100 has not confirmed it. Host-testable: the
  framer and the pump are exercised with socket pairs standing in for the two
  devices. Static musl, one dependency (`libc`, for `poll`), like
  `couch-voice`.
- `kernel/couch-ha100.config`: `CONFIG_BT=y`, `CONFIG_BT_HCIVHCI=y`, every
  other Bluetooth profile and transport explicitly off. This changes the
  config hash `kernel/release-pin.json` pins, so a Bluetooth kernel is a new
  candidate through `docs/kernel-release-candidate.md`, not a drop-in. Built
  clean on Ollie 2026-09-14 from the pinned source commit (`ea122a39`, the
  kernel repository's `bluetooth` branch adds only documentation on top of
  it) into `~/couch-kernel/out-bluetooth`; `kernel/release-pin.json` on this
  branch pins that build (zImage `9dd8e84c…`, effective config `1568fe8c…`)
  so `prepare_public_boot.py` exports it. `CONFIG_BT` alone pulled no extra
  symbols in; `olddefconfig` kept the rest of the config byte for byte.
- `tools/provision-alpine.sh`: `bluez` and `bluez-deprecated` join the image
  package list, for `bluetoothd`, `btmgmt`, `hciconfig` and `hcitool`.

**Milestones**

1. *Spike kernel.* Dev build `.140.dev` carries the kernel above as a boot
   image payload: install the runtime, then the boot image the Updates page
   offers after the reboot, then over SSH `apk add bluez bluez-deprecated`,
   `scp` the `couch-bt-bridge-armv7` release asset to `/opt/couch/couch-bt-bridge`
   and run it. Acceptance: `/dev/vhci` and `/dev/stpbt` exist, the bridge
   creates `hci0`, `hciconfig hci0 up`, `btmgmt info` reports LE,
   `hcitool lescan` sees nearby advertisers, and the bridge log shows the
   set-address command answered (or names the opcode that must replace 0xFC1A).
   Done 2026-09-14 (`hci0` up, LE scan sees advertisers). The system service
   starts and stops `couch-bt-bridge` for the **Bluetooth** toggle in Settings
   and on the web UI's remote page (`Request::Bluetooth`, setting `bluetooth=`
   in `settings.conf`, off by default).

   **Wi-Fi and Bluetooth share one combo radio and STP transport.** The first
   spike that took Wi-Fi down (2026-09-14) did so because the bridge was not a
   singleton and started at boot racing Wi-Fi: two bridges on the un-guarded
   `/dev/stpbt` desynced STP, which hit its retry limit and reset the whole
   chip, dropping Wi-Fi. The fixes (all userspace): the bridge takes a
   whole-file lock so only one ever runs; it opens the devices non-blocking and
   backs off on a full transport (ENOSPC) instead of dying and wedging in an
   uninterruptible read; and it is no longer started from `stage2.sh` at boot,
   only by the toggle, by which time Wi-Fi is up. No kernel change was needed;
   stock Android runs both together on this chip. The bridge ships in the
   runtime bundle from .143 (updaters from .142 accept extra top-level
   `couch-*` executables), with a copy in the boot ramdisk as a fallback; the
   service prefers the runtime copy, so bridge fixes are runtime-only updates.
   The kernel still has to go through the candidate checks before promotion.

   Follow-up hardening: if a chip reset ever does fire, re-run DHCP on `wlan0`
   so Wi-Fi re-associates without a reboot.
2. *Power and coexistence.* BT idle current unplugged per
   `docs/ha100-power-validation.md`; Wi-Fi throughput with the BT function on.
3. *HID over GATT.* A second daemon (or the same one grown) that registers the
   HID service with BlueZ over D-Bus, with the keyboard and consumer-control
   report map, and drives advertising through raw HCI since 3.18 has no
   `Add Advertising` management command. First pairing with a real TV; record
   which pairing method each target accepts.
4. *Switching.* One bond per target, an activity references its bonded
   address, and an activity switch disconnects and directed-advertises to the
   new target. GUI: pair-new-device flow and per-activity target picker.
5. *Proper driver.* Replace the bridge with an in-kernel `hci_dev` over STP,
   per the kernel repository's task list, once the spike has proven the radio.

## Staging checklist

Kernel (couch-kernel `bluetooth` branch, built on Ollie):

- [x] `CONFIG_BT=y`, `CONFIG_BT_HCIVHCI=y` in `couch-ha100.config`; keep
      `BT_RFCOMM`, `BT_BNEP`, `BT_HIDP` off unless a profile needs them
      (on this branch; the candidate still needs building and re-pinning).
- [x] Build `normal` (Ollie `out-bluetooth`, 2026-09-14; pinned on this branch).
- [ ] First boot on the HA100 through the `.140.dev` boot image; confirm
      `/dev/vhci` and `/dev/stpbt` both appear and Wi-Fi, IR, display and
      keys behave as on the pinned kernel.
- [ ] Later: in-kernel STP HCI driver replacing the bridge daemon.

Userland (this repo):

- [x] Add `bluez` (and `bluez-deprecated` for `hciconfig`/`hcitool`) to the
      image package list (`tools/provision-alpine.sh`; takes effect in the next image).
- [x] Bridge daemon between `/dev/vhci` and `/dev/stpbt` (`clients/couch-bt`).
- [ ] Spike acceptance: `hciconfig hci0 up` succeeds, `btmgmt info` reports
      LE, `hcitool lescan` sees nearby advertisers.
- [ ] Program `bluetooth_mac` from the identity record at bring-up.
- [ ] HID-over-GATT peripheral: GATT HID service, report map for keyboard and
      consumer control, advertising driven through raw HCI.
- [ ] First pairing with a real TV or Apple TV; record which pairing method
      each target accepts.
- [ ] Bond store keyed per activity; disconnect-and-redirect on switch.
- [ ] GUI: pair-new-device flow and per-activity target picker.
- [ ] Power validation: BT idle current unplugged, per
      `docs/ha100-power-validation.md`.

## Open questions

- Does the CONSYS BT function power on cleanly alongside Wi-Fi under the
  built-in WMT driver, or does it need the sleep/wake handling stock uses?
- Which HCI vendor command programs the address on CONSYS_6580?
- Does each target (LG, Apple TV, Android TV, Fire TV) accept a BLE HID
  keyboard with legacy pairing? Record results here as they are tested.
