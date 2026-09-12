# Device partition layout and recovery

This is a developer reference for the Sanytron Astrion HA100. Use the live
partition table and the device's verified backups before planning a write;
partition numbers below are reference observations, not a substitute for
checking the connected device.

## Partition roles

| Partition | Reference node | Role |
| --- | --- | --- |
| `boot` | p8 | Normal Couch kernel and initramfs; temporarily holds the RAM installer during installation |
| `recovery` | p9 | Independent rescue kernel and recovery initramfs after Couch installation |
| `para` | p10 | Bootloader control block (BCB) |
| `odmdtbo` | p12 | Device-tree overlay shared by normal and recovery boots |
| `expdb` | p13 | Crash dumps and boot diagnostics |
| `vendor` | p14 | Android vendor partition |
| `system` | p21 | Android system partition |
| `userdata` | p23 | Couch's Alpine filesystem, with Couch under `/opt/couch` |

Slot contents depend on installation progress. Do not assume a device still has
Android or a working Couch recovery image from its partition names alone.
Never write `preloader_*` or `lk`. Keep original images, calibration, credentials
and per-device identity files outside Git.

## Recovering a failed boot

Normal init writes `boot-recovery` into the first 512 bytes of the BCB. Health
checks begin after 90 seconds and clear the marker only after observing a
rendered GUI frame and three advancing heartbeats from the same process. If
the checks fail, init reboots with recovery still selected. A reboot while the
marker is armed also selects recovery. This cannot protect a failure before
init runs. The `ENV_v1` area at offset 128 KiB in `para` must be preserved.

Unclaimed normal boot sessions reboot after 15 minutes. `touch /tmp/stay` keeps
a diagnostic session alive; normal runtime startup claims the session itself.

On the HA100, hold the physical **Back** button during power-on to enter
recovery. A working Couch recovery image provides a USB serial shell. Verify
that access before changing boot images, including with userdata unavailable.
Keep the rescue kernel independent of experimental normal kernels.

If neither slot boots, use the reviewed MTK download-mode recovery workflow
with that device's verified originals. Do not improvise raw writes from the
reference partition numbers. The [installer workflow](installer-wifi-wizard.md)
describes receipt-verified restart and Android restore; the
[stock baseline guide](installer-stock-baseline.md) covers the retained inputs.
Backups of an existing Couch installation are not Android restoration images.

A bad `odmdtbo` affects both boot paths. Validate its container, device tree and
size before writing, then verify the full readback. Preserve its original image
alongside the other device backups.
