# Bluetooth and BLE

Status as of 2026-09-12: **Bluetooth does not work on Couch, on either kernel,
and it has never been exercised.** This document records what exists, what is
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

## Staging checklist

Kernel (couch-kernel `bluetooth` branch, built on Ollie):

- [ ] `CONFIG_BT=y`, `CONFIG_BT_HCIVHCI=y` in `couch-ha100.config`; keep
      `BT_RFCOMM`, `BT_BNEP`, `BT_HIDP` off unless a profile needs them.
- [ ] Build `normal`, confirm `/dev/vhci` and `/dev/stpbt` both appear.
- [ ] Later: in-kernel STP HCI driver replacing the bridge daemon.

Userland (this repo):

- [ ] Add `bluez` (and `bluez-deprecated` for `hciconfig`/`hcitool`) to the
      image package list.
- [ ] Bridge daemon between `/dev/vhci` and `/dev/stpbt`.
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
