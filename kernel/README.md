# The from-source kernel

Couch boots a custom ARMv7 kernel built on Ollie. The current release-staging
candidate is `ea122a39`, pinned with exact artifact hashes in
[`release-pin.json`](release-pin.json). It restores stock board supply and LED
pinctrl initialization; LG B4 IR volume control is confirmed while awake and USB
docked. Unplugged standby/charging and complete installer validation remain open.
See [candidate validation](../docs/kernel-release-candidate.md).

The stock kernel remains the independent recovery-slot fallback. Historical
bring-up information below is not a current hardware validation matrix.

## Where the tree is

The source tree is not in this repo - it is a 1GB kernel checkout and belongs
in its own git history. It lives on the build box (Ollie, `~/couch-kernel/`):

```
~/couch-kernel/base     the tree: parthibx24/android_kernel_wiko_k300 @ 521b3081
                        plus our delta, committed on branch couch-ha100 (ea122a39)
~/couch-kernel/donor    LCM-MTK/android_kernel_mediatek_mt6580, the tree the
                        CONSYS_6580 connectivity driver was grafted from
~/couch-kernel/out      build output (O=), disposable
```

The full branch is published at
[dangerouslaser/couch-kernel](https://github.com/dangerouslaser/couch-kernel)
on `couch-ha100`; the `couch` remote on Ollie points there and `origin` remains
upstream. A second Git copy lives on the Mac at `~/Projects/couch-kernel/base`
(`couch-published`, verified at `e581fb14`). This copy is for source backup,
not compilation. A full Git bundle is also backed up outside this repository.
The `patches/` directory preserves later hardware fixes as reviewable deltas.

What the delta on top of wiko_k300 is (one commit, 541 files, ~19MB):

| area | what |
|---|---|
| `drivers/misc/mediatek/connectivity/` | replaced with the CONSYS_6580 tree from the donor: wmt core, chip detect, wlan gen2, gps. Built in, because the vendor's `.ko` blobs are pinned to Sanytron's exact build and can never load into ours |
| `lcm/st7701s_wvga_dsi_vdo_boe_tn_tianxian/` | the panel this unit ships, adapted from wiko's `st7701s_wvga_dsi_vdo_tn_boe` |
| `include/mt-plat/mt6580/x15cm_s90_kr/`, `mach/mt6580/x15cm_s90_kr/` | the project's cust GPIO/EINT/keypad headers, seeded from `k300` |
| `leds/mt6580/leds.c` | stock values 2/4 select charging/button callbacks, not pins. Restored named pinctrl initialization and validated dispatch replace the incorrect early GPIO fallback |
| `misc/mediatek/Makefile` | cameraisp gated on `MTK_IMGSENSOR`, which this config turns off |

`couch-ha100.config` here is the `.config` that built `#4`: `config-stock.txt`
from the device backup, adjusted for this tree, with the connectivity stack
built in.

## IR transfer validation

The normal source checkout on Ollie now contains the tested board initialization
fix. The isolated `irtx-worktree` remains available for reviewed experiments.
The normal build recipe requires this baseline as an ancestor; release staging
requires the exact pin and effective artifact/configuration hashes.

```sh
python3 tools/release/kernel_provenance.py \
  --boot build/couch-board-init-fixed.img \
  --kernel-manifest build/board-init-manifest.json
```

This verifies kernel provenance without enabling the physical installer. See
[IR](../docs/ir.md) for current device tests and the independent recovery setup.

## Building

**All kernel compilation runs on Ollie**, an i9-14900K Linux server. From the
Mac, `kernel/build.sh` transfers the recipe and invokes the build over
`ssh ollie`; it never starts a local Docker VM. The kernel source remains in
Ollie's independent Git repository. Transfer and commit source edits there
before building; the wrapper transfers only this repository's build recipe.

```sh
kernel/build.sh diagnostic  # tracing enabled; Ollie ~/couch-kernel/out-diagnostic
kernel/build.sh normal      # reduced debug overhead; Ollie ~/couch-kernel/out-normal
scp ollie:couch-kernel/out-normal/arch/arm/boot/zImage build/zImage-normal
python3 kernel/pack.py build/linux-recovery-KNOWNGOOD.img build/zImage-normal build/couch-test.img
python3 -m unittest discover -s kernel -p 'test_*.py'
```

For initial container setup, run `docker build -t couch-kbuild kernel/` on
Ollie from a checkout of this recipe. The Dockerfile pins the compiler commit.
Both profiles regenerate `.config` from the tracked baseline and fragment on
every invocation, then verify the requested fragment survived `olddefconfig`.
Outputs are separate. `manifest.json` records source commit/status, effective
configuration, container/compiler identity, and artifact SHA-256 hashes.
`KTREE`, `KOUT`, `KIMAGE`, and `JOBS` overrides refer to paths/settings on Ollie.

`pack.py` swaps only the zImage into the known-good image: the appended DTB,
ramdisk, load addresses, page size and cmdline are carried over verbatim, so
the kernel is the single variable under test. It asserts the zImage magic and
the DTB boundary rather than trusting the sizes.

The container is amd64 (the AOSP `arm-eabi-4.9` prebuilt is an x86_64 binary)
and Ubuntu 20.04 (MediaTek's `DrvGen.py`, which the ALPS Makefiles run to
generate the cust headers, is Python 2). A full build takes a few minutes on
Ollie.

## Flashing and testing

`tools/flash-linux.sh build/couch-test.img` writes the image to the `boot` slot
(p8) from a running Couch or its recovery, over ssh, with a checksummed
readback. It refuses anything that is not a boot image and anything that is
not a block device: an earlier flash "succeeded" by writing a regular file
named `/dev/block/mmcblk0p8` into RAM.

Init writes `boot-recovery` into the BCB and clears it 90 seconds later. This
can direct a subsequent reboot into recovery after a failure that occurs
**after init arms it**; it does not protect failures before init or prove GUI
health. Preserve a verified p8 backup and the independent p9 recovery image,
and verify serial access through Ollie before flashing. Do not modify the
shared vendor overlay or bootloader partitions for kernel experiments.

Boot markers land in `expdb` (p13) from sector 13000; `tools/markers.sh` reads
them. The serial shell on `/dev/ttyGS0` is the debug channel when there is no
network.

## What works, what does not

| | from-source `#4` | stock `#7` |
|---|---|---|
| display (`mtkfb`, ST7701s) | yes | yes |
| keypad, mic key, volume | yes | yes |
| USB serial shell | yes | yes |
| WiFi (CONSYS_6580, built in) | yes, lease in ~19s | yes, vendor modules |
| touchscreen (`tlsc6x`) | **no** - not in any public tree, not yet ported | yes |
| IR transmit (`mt_irtx`) | **no** - `irtx/mt6580/` absent from the tree | yes |
| keypad backlight LEDs | GPIO case patched; `X15_S90_LEDS` proper fix pending | yes |
| Bluetooth, GPS, FM | built in, never exercised; see [Bluetooth](../docs/bluetooth.md) | never exercised |

Three traps that each looked like a hang, all handled in `initramfs/init` and
`stage2/stage2.sh` now, documented here because they will come back with the
next driver:

- `/dev/ttyGS0`'s major is dynamic and moves when drivers are added (233 on
  stock, 240 here). init reads it from `/proc/devices`.
- The wlan driver opens its RAM code by the hardcoded path
  `/etc/firmware/WIFI_RAM_CODE_6580`, and the wmt driver opens its patches by
  bare filename in `/`. stage2 links both.
- The wlan driver logs through `pr_debug`. With `CONFIG_DYNAMIC_DEBUG` that is
  compiled in but off: `echo 'file *gen2* +p' > /sys/kernel/debug/dynamic_debug/control`
  makes it talk, and `echo 0 > /dev/wmtWifi; echo 1 > /dev/wmtWifi` re-runs
  the probe without a reboot.

## Current validation and remaining work

Touch is now provided by `couch_tlsc6x` with bounded report retries and no
firmware-update path. The keypad EINT mux and production panel setup are live.
Apply the mail patches in `patches/series` order on top of `7a0e5e8f`.

The normal profile includes the board pinctrl fix and guarded PWM IR driver.
Optical LG B4 control has been validated awake/docked. Output telemetry defaults
off; retain the exact tested binary until remaining power-state checks pass.

The boot-health gate requires advancing local GUI heartbeats before clearing
the recovery BCB. It does not protect failures before init arms the BCB.

See [Slint performance](../docs/slint-performance.md) and
[GPU experiments](../docs/gpu-acceleration.md) for current optimization work.
Unplugged suspend, thermal/battery measurements, and IR following unplugged standby remain
unvalidated. Kernel source is committed on Ollie and backed up separately; this
repository and kernel have separate configured GitHub publishing remotes.
