# Couch

A Linux OS for the Sanytron Astrion HA100 universal remote.

[Project site](https://dangerouslaser.github.io/couch/) ·
[Kernel source](https://github.com/dangerouslaser/couch-kernel)

Couch combines an Alpine userland, a Slint device interface, Rust integration
clients, and a browser configuration UI. First boot configures Wi-Fi on the
remote; the hotspot portal remains an explicit recovery option.

**Status, September 10, 2026.** The remote boots from `boot` with our custom
`3.18.79-couch-normal-gea122a39f434` kernel. Display, keypad, touchscreen,
Wi-Fi and accelerometer support are present; the user has confirmed touch wake
and physical lift wake. The independent recovery slot retains a stock-kernel
Couch rescue image. Android is no longer installed; its images are backed up
outside this repository.

The built-in IR transmitter is working: LG B4 power off/on, volume up and held
repeats were confirmed, including commands before and after undocked remote
sleep. IR commands attach directly to existing devices in the web UI. The side
power button sleeps/wakes Couch; the keypad power button controls devices.

The GUI supports rooms, devices, scenes, media control screens and activity
start/stop sequences. Rust clients cover Kodi, Philips Hue, Home Assistant,
LG webOS, Denon, Android/Google TV, experimental Apple TV and IR. Setup and subsystem details are under `docs/`; the USB
installer is experimental and does not yet flash hardware.

The HA100 is a MediaTek MT6580 (quad Cortex-A7, ARMv7, 1GB RAM, 480x800 touchscreen)
shipping Android 8.1. This boots a non-Android userland on it and gives you a root
shell over the USB cable.

## The approach, and why

Mainline Linux knows about the MT6580 — `arch/arm/boot/dts/mediatek/mt6580.dtsi` is
in Torvalds' tree — but it is a 108-line skeleton: four CPUs, GIC, a timer, two UARTs,
and *dummy* fixed clocks. No clock controller, no pinctrl, no MMC, no display. Every
peripheral that matters here is out-of-tree vendor code: the display is `mtkfb`, WiFi
is the in-SoC `CONSYS_MT6735` block, and the IR blaster is a `mt_irtx` char driver at
major 243. So this started on the **stock 3.18.79 vendor kernel, byte-for-byte**,
replacing only the ramdisk - and once everything else worked, rebuilt that
kernel from MediaTek's public ALPS sources so it could be read, profiled and
changed. The stock kernel is still what the recovery image boots, and still
the independent rescue kernel. The custom kernel now supports touch and IR.
`docs/frankenkernel.md` records the bring-up plan and
`kernel/README.md` the working reference.

The slot layout has been swapped since bring-up:

| partition | holds |
|---|---|
| `boot` (p8) | Couch: our kernel + `initramfs/init` |
| `recovery` (p9) | Couch recovery: stock kernel + `recovery/init` - USB shell, WiFi, sshd, no UI |
| `para` (p10) | the BCB; `boot-recovery` in its first 512 bytes makes lk boot p9 |
| `expdb` (p13) | MTK crash dumps; our boot markers and kernel-log snapshots from sector 13000 |
| `system`, `vendor` (p21, p14) | Android's, still present, only a fallback source of blobs |
| `userdata` (p23) | the Alpine rootfs, Couch itself under `/opt/couch` |

Once init starts, it arms the BCB and clears it 90 seconds later. A reboot
while that marker is armed selects the recovery slot, with a USB shell and
SSH on an available network. This cannot protect a failure before init runs;
verify USB recovery access before flashing. Android is gone from the device; `android-p9-BACKUP.img`
in the backup puts it back on p9 if it is ever wanted.

Nothing is signed. The stock boot image has 8.7MB of trailing zeros where a signature
block would be, so `lk` boots unsigned images and a plain `dd` from rooted Android is
enough — no bootloader unlock, no fastboot, no BootROM exploit.

## Usage

```sh
tools/build.sh                 # stock kernel + initramfs -> build/linux-recovery.img
kernel/build.sh                # our kernel, in Docker -> zImage (see kernel/README.md)
python3 kernel/pack.py ...     # swap that zImage into a known-good image
tools/build-recovery.sh        # stock kernel + recovery/init -> build/couch-recovery.img
COUCH_IP=... tools/flash-linux.sh build/couch-test.img   # to p8, over ssh, verified
tools/markers.sh               # offline debug: progress markers + kernel logs
python3 tools/sercmd.py 'uname -a'                       # over the USB serial shell
```

`stage2/stage2.sh` and everything under `/opt/couch` live on the rootfs and
are pushed over ssh (`scp`, then check the md5) or, with no network, with
`tools/push.py` over serial. Neither needs a reflash.

The adb-era tools (`tools/backup.sh`, `flash.sh`, `boot-android.sh`,
`swap-slots.sh`, `provision-alpine.sh`) need Android running on the device
and are kept for the record; `boot-android.sh` now refuses unless told
Android has been put back on p9.

### Local build cache maintenance

Kernel builds run on Ollie. Rust host debug caches can grow by many gigabytes
across the independent workspaces. After a completed development milestone, run
`tools/clean-build-cache.sh` to inspect them, then
`tools/clean-build-cache.sh --apply` to remove regenerable debug compilation caches.
The script refuses cleanup during a Rust build and preserves standalone debug
executables, release/ARM builds, the active browser-preview cache, toolchains,
and everything under `build/` (including private device and recovery backups).
The next debug build will take longer. Do not delete `build/` wholesale.

## Recovering a bad boot

* After init starts, it writes `boot-recovery` to the BCB and clears it after
  90s. A subsequent reboot while armed selects recovery. Failures before init
  require the boot menu or download-mode recovery. Only the first 512 bytes of
  `para` are touched - an `ENV_v1` block lives at offset 128K and must survive.
* Unclaimed sessions self-reboot after 15 minutes. `touch /tmp/stay` keeps one
  alive; `stage2.sh` does it once it reaches the GUI, so only a boot that fails
  before that falls back.
* Enter recovery using the physical **Back** button during power-on (confirmed
  on the HA100). Do not use the previously documented Volume Up entry sequence.
  The Couch recovery screen provides the USB serial shell.
* If both slots are bad, MTK download mode over USB (mtkclient) rewrites a
  partition from the backup. **Never write `preloader_*` or `lk`.**

## Getting anything onto the screen

There is no `CONFIG_VT`, so no kernel fbcon and no console. `src/fbcon.c` draws
init's stdout with the kernel's own 8x16 font. Four separate bugs each produce an
identical blank screen, so fixing them one at a time looks like no progress:

1. **The panel switches its own backlight off when idle.** An early build blinked
   the backlight as a heartbeat and was accidentally keeping the screen alive.
   init now re-asserts brightness every 5s, and fbcon does after every frame.
2. **Alpha.** mtkfb composites ARGB8888. Colours built as `0x00RRGGBB` have alpha
   0, so they are perfectly correct in memory, readable back from `/dev/fb0`, and
   completely transparent on the glass.
3. **Write size.** mtkfb transfers to the panel per `write()`. A single 1.5MB
   write lands in memory and never reaches the display; 4096-byte chunks do.
   This is why plain `dd` displayed all along and fbcon never did.
4. **The font.** Extracting glyphs from `lib/fonts/font_8x16.c` with a naive
   `0x..` regex also matches the hex inside each per-glyph comment
   (`/* 65 0x41 'A' */`), shifting every glyph by one byte. Strip comments first,
   then assert that space is blank before writing the header.

One more trap, and the driver is telling the truth here: `fb_var_screeninfo`
reports `red=0/8 green=8/8 blue=16/8`, and the panel really does read the low
byte as red. Write `0xAARRGGBB` and red and blue come out swapped.

This was documented backwards for a while. The original check used a pale blue,
where a red/blue swap is ambiguous by eye. Verify with labelled bars of
saturated primaries instead - `COUCH_BARS=1 couch-gui` draws them - and read the
result back with `tools/screenshot.sh` rather than describing it.

`tools/sercmd.py 'cat /tmp/fbcon.geom'` dumps what the driver reported.

## Four things that will waste your day

1. **No `CONFIG_VT`.** There is no virtual terminal, so `exec >/dev/console` fails and
   takes init with it — the kernel panics with no output at all. Never redirect to
   the console. It also means a successful boot looks identical to a hung one: lk's
   logo simply stays on screen because nothing overwrites the framebuffer.
2. **No `CONFIG_DEVTMPFS`.** Nothing populates `/dev`. Device nodes are baked into the
   cpio by `tools/mkcpio.py`; `mdev -s` fills in the rest.
3. **`f_acm/instances` defaults to 0.** Setting `functions=acm` then gives a gadget
   advertising a configuration with no endpoints. The host sits at `CONNECTED` and
   never reaches `CONFIGURED`, and the kernel logs `cfg 1 speeds:` with nothing after
   it. Write `1` to `instances` *before* `functions`.
4. **The MediaTek preloader exposes its own CDC port ~6s into boot**, which is easy to
   mistake for your gadget. It disappears seconds later. `tools/boot.sh` waits 25s.

macOS has no RNDIS or EEM driver, so CDC-ACM is the only usable gadget of the ones
this kernel offers.

## What works

| | |
|---|---|
| RAM | 937MB of 979MB free (Android leaves far less) |
| CPU | 4 cores, after onlining `cpu1-3` (only cpu0 comes up by default) |
| Display | `mtkfb`, 480x800x32 at `/dev/fb0`, writable at 13MB/s |
| Input | `mt_gpio_kpd`, `mtk-kpd` (buttons); `mtk-tpd` touchscreen on the stock kernel only |
| IR | `/dev/irtx`, `mt_irtx` - stock kernel only, until it is ported |
| Watchdog | kicked by kernel threads `wdtk-0..3`; userspace need not |
| WiFi | works on both kernels; a lease ~19s after power-on on ours |
| Userland | Alpine 3.21.7 armv7 on `userdata`, with working `apk` |

## The Alpine userland

Alpine armv7 lives on `userdata` (p23), mounted at `/mnt/alpine`, with Couch
under `/opt/couch`. It started life on the 112MB `cache` partition and was
installed *from Android* over adb, which was far faster than pushing it down
a serial line and needed no network. That route is gone with Android; today
the rootfs is maintained in place over ssh. For the record, the install was:

```sh
adb shell 'umount /cache; mke2fs -t ext4 -b 4096 -L alpine -F /dev/block/mmcblk0p22'
adb shell 'mkdir -p /data/local/tmp/alp && mount -t ext4 /dev/block/mmcblk0p22 /data/local/tmp/alp'
adb push alpine-minirootfs-3.21.7-armv7.tar.gz /data/local/tmp/alpine.tgz
adb shell 'cd /data/local/tmp/alp && /data/local/tmp/busybox tar xzf /data/local/tmp/alpine.tgz'
```

Use busybox's `tar`, not Android's toybox, so symlinks and permissions survive.

init chroots into it rather than `switch_root`-ing into OpenRC, because OpenRC expects
a devtmpfs this kernel does not have. The initramfs stays PID 1 and supervises.

## WiFi

Works. `tools/provision-alpine.sh` bakes `wpa_supplicant` and its dependencies
into the base rootfs, because without them there is no network, and without a
network `apk` cannot fetch them.

Five things had to be right, and four of them fail silently:

1. **Do not restore Android's property area before running `wmt_loader`.**
   It reads `persist.mtk.wcn.combo.chipid`; if that is already set it assumes
   detection has happened and exits without ever opening `/dev/wmtdetect`. The
   snapshot that was meant to help WiFi was the thing preventing it. With no
   properties it reads the id from hardware (`ioctl` returns `0x6580`) and hands
   it to the driver. Restore the properties afterwards.
2. **`stpwmt` (major 190) is registered only after successful detection**, so
   waiting for the node before running the loader waits for something only the
   loader can produce. There is no devtmpfs, so `mknod` it afterwards.
3. **Use Alpine's `wpa_supplicant`, not `/vendor/bin/hw/wpa_supplicant`.** The
   vendor build logs solely to Android's logd and exits silently with no output
   at all when logd is not running - `-f` is not compiled in either.
4. **Emit every stored network, not just the first.** The first entry in
   Android's `WifiConfigStore.xml` is not necessarily the one in range.
5. **Run `udhcpc` inside the chroot.** busybox udhcpc does nothing without
   `/usr/share/udhcpc/default.script`; from the initramfs it takes the lease and
   never applies it, which looks exactly like DHCP failing.

Credentials are read from Android's own store on the device at runtime, so they
never live in this repo or the boot image. userdata is **f2fs**, not ext4.

### The SELinux context files are not optional

Couch carries MediaTek's blobs so it does not need Android's partitions. Getting
that to work needed one non-obvious file set: `plat_property_contexts` and
`nonplat_property_contexts` under `/system/etc/selinux` and
`/vendor/etc/selinux`.

bionic's property system maps property names to SELinux contexts using them.
Without them property access misbehaves, and `wmt_launcher` sits in a 300ms
`nanosleep` poll loop and never issues its STP-mode ioctl. The kernel then
reports

```
stp_init(717): WMT-CORE: no hif info!
stp_init fail (-1)
stp_deinit: gMtkWmtCtx.p_ic_ops is NULL
opfunc_func_on: func(3) pwr_on fail(-3)
```

which is misleading twice over: `p_ic_ops is NULL` is a consequence of the
failed init, not the cause, and the `-3` is `opfunc_pwr_on` failing - not the
"unsupported chip id" `-3` returned by the ops lookup in the same driver. The
chip id handover works fine; `SET_CHIP_ID` with `0x6580` lands, as
`cmd (1074034433),arg(25984)` in dmesg confirms.

With the six context files in the bundle: `HIF info added`, `STP mode success!`,
and `wlan0` appears. A clean boot reaches an address in ~35s.

On our own kernel the connectivity stack is built in rather than loaded as the
vendor's modules, and three more things had to be right, each of which failed
with nothing in the log. They are handled in `stage2.sh` and written up in
`kernel/README.md`: the wlan driver wants its RAM code at
`/etc/firmware/WIFI_RAM_CODE_6580` and the wmt driver wants its patches by
bare name in `/`; the driver logs only through `pr_debug`, so its errors are
compiled in but switched off until dynamic debug is opened for it; and with
no NVRAM record it invents a new MAC every boot, so stage2 writes one carrying
a MAC derived from the eMMC CID. A clean boot now reaches an address in ~19s.

Trap worth naming: the vendor `wmt_launcher` writes nothing to stdout and only
logs to Android's logd, so it looks healthy while doing nothing. `strace` is
what shows the poll loop - borrow Android's from `/system/xbin/strace` by
mounting `mmcblk0p21` read-only at a side path, leaving the bundle under test.

Writing `/dev/wmtWifi` while the chip is half-initialised panics the kernel in
`wmt_drv` (`__list_add` on an uninitialised mutex) and hard-resets the device.
Get the order right and it is fine.

## Setup portal

With no known network, the remote hosts its own. `stage2` falls back to
`portal.sh`, which brings up an AP, a wildcard-DNS captive portal and a small web
UI for choosing a network and enrolling an SSH key.

It is deliberately not an open AP. The AP runs **WPA2 with a random passphrase
generated per boot and shown on the device's own screen**, so joining it requires
physically looking at the remote. That is what makes enrolling an SSH key through
a web page acceptable: an open setup network would let anyone in radio range take
permanent root, and the exposure would not be brief - a device whose WiFi password
changed would sit in setup mode indefinitely.

On top of that: enrolling a key needs a **physical button press** on the remote
(read from evdev), the portal **times out** after ten minutes, and the shipped
image **contains no authorised keys at all**, so a fresh device trusts nobody.

Because a radio cannot scan while in AP mode, the network list is captured just
before switching and cached for the portal to serve.

## Reading the vendor source

`pwr_on fail(-3)` is not a mystery once you have the source. MediaTek's MT6580
kernel tree is public ([Mysteryagr/MT6580-Kernel-3.18](https://github.com/Mysteryagr/MT6580-Kernel-3.18),
180MB) and contains the exact driver our binary module was built from -
`connectivity/wlan/gen2/common/{wlan_lib,wlan_oid,wlan_p2p,wlan_bow}.c` matches
the paths embedded in `wlan_drv.ko`, and `connectivity/common/conn_soc` is the
SoC-connectivity variant the vendor ships as `common_main`.

In `conn_soc/core/wmt_core.c`, the ops lookup is a switch on the chip id:

```c
    case 0x6580:
        p_ops = &wmt_ic_ops_soc;
        break;
    default:
        p_ops = (P_WMT_IC_OPS) NULL;
    }
    if (NULL == p_ops) {
        WMT_ERR_FUNC("unsupported chip id (hw_code): 0x%x\n", chipid);
        return -3;
    }
```

So **-3 means "unsupported chip id"**: 0x6580 *is* supported, and the failure is
that the driver had no valid chip id when power-on ran. That is also the earlier
`gMtkWmtCtx.p_ic_ops is NULL` line - the same cause seen from the other end.

`wmt_loader` hands the id over by ioctl on `/dev/wmtdetect`; a working run shows

```
ioctl(_IOC_READ,  0x77, 0x3) = 25984     # 0x6580, read from hardware
ioctl(_IOC_WRITE, 0x77, 0x1, 0x6580) = 0 # handed to the driver
```

Anything that stops the loader reaching those ioctls leaves the driver with no
chip id and power-on fails with -3 - which is why restoring Android's property
area before the loader breaks WiFi (see above): the loader sees
`persist.mtk.wcn.combo.chipid` already set, assumes detection happened, and
exits without ever opening `/dev/wmtdetect`.

Having the source also makes a **rebuild** attractive: the tree includes four
MT6580 defconfigs, and this device's exact config is in the backup as
`config-stock.txt` (3867 lines). Rebuilding the vendor 3.18 kernel with
`CONFIG_VT` and `CONFIG_DEVTMPFS` enabled would retire fbcon and the hand-made
device nodes, while keeping the WiFi driver that only exists downstream. It
needs a gcc-4.9-era ARM cross toolchain, which Docker can supply.

### The rebuild, and what it still lacks

The rebuild happened. `kernel/README.md` is the reference; in short: the
`wiko_k300` `alps-3.18.79` tree is the base, the CONSYS_6580 connectivity
driver is grafted in from another MT6580 tree and built in, the panel driver
is adapted from wiko's ST7701s variant, and `config-stock.txt` is the config.
It boots this device with the display, keypad, USB serial and WiFi, and runs
the whole of Couch. Serial was the thing that made it possible to see anything
at all; the display worked before the console did.

Two drivers are still absent from every public MT6580 tree checked
([Mysteryagr](https://github.com/Mysteryagr/MT6580-Kernel-3.18),
[parthibx24/k80](https://github.com/parthibx24/android_kernel_mediatek_k80),
[LCM-MTK](https://github.com/LCM-MTK/android_kernel_mediatek_mt6580)):

| needed | status |
| --- | --- |
| `tlsc6x` (touchscreen) | absent; `danascape/linux-daria-mt6877` carries it wired into `tpd` and is the porting source |
| `CONFIG_MTK_IRTX_PWM_SUPPORT` (IR transmit) | `irtx/mt6580/` does not exist in any tree; `mt_irtx_pwm.c` from other MTK trees is the porting source |

So the stock kernel remains the one with touch and IR, and it is the kernel in
the recovery image, which must never share a kernel with the experiment.

The alternative unblock is still a GPL source request. A kernel panic leaked
the vendor's build path, `/home/felix/ha100fw/alps/out/target/product/x15cm_s90_kr/...`,
so Sanytron build from a full MediaTek ALPS tree and distribute a GPL-2.0
kernel; they are obliged to provide its source. Ask for the source of the
shipped build (`3.18.79 #7`, `RS30_HAOS_HA100_V1.0.4`).

Mainline is a different road: [u-boot-mt6580](https://github.com/predefine-mt6580/u-boot-mt6580)
has active MT6580 work (clocks, eMMC, display PWM), but `mt76` does not cover
MT6580's in-SoC CONSYS, so a mainline kernel means no WiFi at all.

## The keypad debounces in the device tree

Rapid taps were being swallowed: pressing DOWN 20 times as fast as possible put
only **8** presses into userspace, and the app handled all 8. Nothing in LVGL or
the read loop was dropping anything - the events never left the driver.

The keypad is a `gpio-matrix-keypad`, and its device tree node says:

```
debounce-delay-ms = 0x32     /* 50ms */
linux,no-autorepeat
```

50ms debounce applies to press *and* release, so a tap cycle costs ~100ms and
tapping is capped near 10/second. `linux,no-autorepeat` separately means holding
a key produces one event and nothing more, so the usual gesture for scrolling a
list does nothing. Android synthesises key repeat in its input framework rather
than relying on the driver, which is why it felt better; couch-gui does the same.

Debounce cannot be worked around from userspace - you cannot recover events the
kernel never generated. It lives in **`odmdtbo`**, the device-tree overlay lk
applies at boot, so `tools/dtbpatch.py` walks the flattened tree and patches the
value in place:

```sh
python3 tools/dtbpatch.py …          # 50 -> 8, one u32, size unchanged
dd if=patched.img of=/dev/mmcblk0p12 # from Linux; no Android needed
```

Measured after: the same three-tap test gives 3 presses, 3 mapped keys, 3 focus
moves. The 20-tap test went from 8 to 15.

## The keypad interrupt handler runs for 46-62ms with interrupts off

Measured with ftrace (`irq_handler_entry`/`exit`, `sched_switch`) on a real
press: the `matrix-keypad` row interrupt handler runs 46ms when it lands on
the idle task and 62ms when it lands on the scan worker, in hard-IRQ context
with interrupts disabled, so for that long nothing on the one online core
runs - not the tick, not the GUI's 5ms sleep timer, not the display's
command-queue completion. While a key is held the line re-fires every ~85ms
(the generic `gpio-matrix-keypad` driver re-enables the row EINTs after each
8ms rescan and MediaTek's edge emulation fires again), so a hold spends about
60% of the core inside that handler.

That one fact explains everything else that was measured: an isolated press
reaches userspace ~92ms after the kernel stamps it (5ms for a press during
a hold, when the rows are polled and the EINTs stay masked; 2-3ms with three
cores online, when the GUI runs elsewhere); a mic hold once starved the
capture thread to a 0.1s recording; `FBIOPAN_DISPLAY` looked like a kernel
busy-wait while a key was down, because the hard-IRQ time is charged to the
task it interrupted; and `kworker/0:1` takes ~22% of the core rescanning.

Nothing in the overlay's keypad node is unusual: `gpio-matrix-keypad`, four
row EINTs (6, 9, 11, 12) as plain `bias-disable` EINT pins, six column GPIOs,
`col-scan-delay-us = 200`, `debounce-delay-ms = 8`. The cost is inside the
scan the driver runs from hard-IRQ context: `gpio-matrix-keypad` activates
each column GPIO in turn and reads the rows with a `col-scan-delay-us` (200us)
settle between them, and on this SoC the column GPIO and row EINT operations
go through the pmic/EINT chip over a slow bus - six columns times the settle,
inside the handler, with interrupts off. Confirmed by a second capture (IRQ 268
again 46/62ms). This kernel ships only the `nop` tracer at runtime, so a
`function_graph` breakdown would need a kernel rebuild and is not worth it.

The interrupt is delivered only to CPU 0 (every one of its interrupts is
serviced there and the kernel refuses to move it), so the way out that works
without touching the driver is to make sure the UI has another core to run on:
`stage2.sh` holds a three-core hotplug floor (`/proc/hps/num_base_perf_serv`),
and the scheduler migrates the runnable UI off the frozen core by itself.
Measured on a held-key scroll: one core, 81 frames in 5s with a 45.9ms worst
frame and ~92ms to the first press; three cores, 283 frames, 5.6ms worst,
2.7ms. Two cores was not enough (a 50ms frame still leaked through), and
pinning the UI off CPU 0 with `sched_setaffinity` was worse: the hotplug
daemon can take the pinned core away and the UI then freezes for as long as it
is gone, 4.8s in one frame. Keep a core available; never force the UI onto one.
The cost is two cores' idle power, one line in `stage2.sh` to dial back.

## Standby

Nothing dimmed the panel before: init rewrote 255 to both backlights every
five seconds (a bring-up habit from when the panel seemed to switch itself off;
it does not - an unattended level holds for as long as it is left). `stage2.sh`
now stops that loop, and `couch-gui` owns brightness with two idle timers:

| after | panel | keys | first key press |
|---|---|---|---|
| 30s idle | backlight 40 | off | acts as normal |
| 120s idle | powered down: LCM, backlight PWM and touch suspended (`FBIOBLANK`) | off | wakes only, not acted on |

Only a key wakes the panel; a touch on a dimmed or dark panel is ignored (and
the controller is suspended while it is off). The mic key is the exception on
a dark panel: holding it means "talk", so it wakes and records. A pairing PIN
on screen, a recording, or first-run setup hold the panel awake.

**A resumed panel shows nothing until it is re-presented.** After the
display driver suspends and resumes the LCM (`FBIOBLANK`), the backlight
comes up and the panel scans - but the overlay it scans was configured by the
last `FBIOPAN_DISPLAY`, and Android re-presents after every resume where
nothing here did. Measured on a real wake: framebuffer full of the UI, driver
reporting `State=Alive`, backlight at 255, panel black; running the GUI with
`COUCH_VSYNC=pan` for five seconds brought the picture up and it stayed.
`Panel::present` issues one zero-offset pan after every unblank and once at
startup (whoever resumed the panel before the GUI started did not re-present
it either). It blocks ~17ms, longer while a key is held, which a wake pays
once.

**The panel can be asleep without the GUI having asked.** A second real
failure: the GUI had only dimmed, then a key woke it, wrote 255, and the
screen stayed black. The display driver's status (`/sys/kernel/debug/mtkfb`)
read `State=Sleep` - the LCM suspended - and the PWM still held the dim level.
Nothing in the GUI had blanked it and a two-minute watch of a dimmed idle
panel never reproduced it, so a watcher now logs every `Alive`/`Sleep`
transition with the driver's own line naming the caller (`lcm_suspend` is
logged with the calling pid and comm). Whatever the cause, the GUI no longer
trusts its own `blanked` flag: every wake asks the driver and unblanks if the
panel is asleep for any reason before setting the backlight, and the
one-second tick does the same in every state but Off, re-asserting the
backlight level forced past the LED layer. A panel that goes dark on its own
comes back within a second. The unblank on a live panel is a 10-20ms no-op.

**The backlight write can be lost, and then it sticks.** The LED layer drops a
write that matches the value the node already holds (10ms, no driver call,
against 110ms and a PWM change otherwise). Once, on a real wake from dim, the
display driver silently lost the write of 255 - the node said 255, the PWM
log showed the dim level still applied - and from then on every 255 was
deduplicated away: a dark remote that answered keys. `Panel::set_backlight`
writes a neighbouring value first when the node already holds the target, and
a second after every wake the GUI re-asserts full brightness the same forced
way while the screen is idle. Verified by forcing the PWM to the dim level
behind the LED node (`echo backlight:40 > /sys/kernel/debug/dispsys`) and
probing the applied level with a later write's `old =`: recovered. The
driver's log line is rate-limited (`dmesg` reports suppressed messages), so a
missing line is not a missing write - probe with `old =` instead. While off, nothing is rasterised or copied; Slint's
state keeps advancing and the first frame after waking catches up, with the
whole picture pushed from RAM because the panel was re-initialised. Input is
polled every 40ms while off. `COUCH_DIM_S` and `COUCH_OFF_S` shorten the
timers for testing; every transition is logged as `couch-gui: standby: ...`.

Measured: powering down takes ~570ms, waking ~430ms of panel re-init. A
backlight write takes 110ms - it goes through the display's command queue -
and that matters: an animation started right after one took its start time
from before the block and ran two frames instead of ten. `wake()` refreshes
Slint's clock after the blocking calls and before the key is dispatched. The
rule generalises: anything that blocks the loop and then starts an animation
must call `update_timers_and_animations` in between (the page slide learned
this first).

Two more facts from the same investigation, for whoever profiles this next:
`FBIOPAN_DISPLAY` busy-waits in the kernel for as long as any key is held
(the keypad rescans every 8ms while a key is down), so the GUI paces with a
timed sleep - see `docs/slint-notes.md`; and the touchscreen's interrupt thread
(`irq/261-tlsc6x_`, SCHED_FIFO 50) costs about 1.5ms per interrupt at ~100
interrupts a second while the glass is touched.

Take this one seriously though: `odmdtbo` is shared with Android, so unlike the
recovery-slot work a bad overlay affects both systems and recovery means
BootROM rather than a power cycle. Validate the FDT before writing (full node
walk, balanced nesting, expected value), confirm the image size is unchanged so
MediaTek's container header stays valid, and verify the partition readback
hashes to the patched image. Keep `odmdtbo.img` from `tools/backup.sh`.

## Distribution

The image is meant to be reusable, so it carries nothing device- or
owner-specific:

* **no SSH keys** - enrolment happens through the portal or over USB
* **no WiFi credentials** - read from Android's store on the device at runtime,
  or entered on the remote and saved to the Alpine partition
* **no Android property snapshot** - it contained this unit's serial number, and
  it broke WiFi detection anyway

`root` is unlocked with `*` rather than `!` in `/etc/shadow`: sshd refuses to
complete authentication for a locked account even with a valid key, but `*`
leaves it with no password at all, so key auth works and password auth cannot.

## Roadmap

1. ~~Alpine rootfs~~ - done.
2. ~~WiFi~~ - done.
3. ~~Captive portal~~ - done.
4. ~~Move to the `boot` slot~~ - done; the recovery slot is now Couch's own rescue image.
5. ~~A kernel built from source~~ - boots with display, keypad, serial and WiFi.
6. ~~Touch on the custom kernel~~ — done. Continue IR validation, keypad
   interrupt-latency work, true SoC suspend, and hotplug tuning.
7. ~~Put the kernel branch on a remote and on a second machine~~ — done.
   Full kernel history is published at [dangerouslaser/couch-kernel](https://github.com/dangerouslaser/couch-kernel),
   branch `couch-ha100`. Ollie holds the build checkout; the Mac holds an
   independent Git copy in `~/Projects/couch-kernel/base` (`couch-published`).
   Both copies were verified at `e581fb14`; all kernel builds still run on Ollie.

The remote's original control app was Android/Kotlin
(`~/Projects/sanytron`, "Sanytron Remote"); the Slint GUI in `ui/couch-gui`
replaced it against the raw framebuffer and evdev.

## Layout

This repo:

```
initramfs/init      what PID 1 does: markers, BCB arming, USB gadget, shell, heartbeat
recovery/init       the recovery slot's PID 1: shell first, then stage2 without a UI
stage2/             the pushable stage on the rootfs: vendor blobs, WiFi, DHCP, sshd, portal, GUI
kernel/             building our kernel: Dockerfile, config, build and pack scripts, README
ui/couch-gui        the Slint GUI (docs/slint-notes.md, keyboard.md, settings.md)
clients/            couch-ir (IR transmit), couch-voice
daemon/             couch-confd
web/                the config web UI (docs/webui.md)
src/fbcon.c         init's stdout on the panel, since the kernel has no console
tools/              build, pack, flash, serial, markers, dtbpatch; the adb-era ones need Android
docs/               design notes per subsystem; frankenkernel.md is the kernel plan
build/              gitignored: images, extracted kernels, the known-good image
```

Outside it:

```
~/Projects/sanytron-device-backup   all 23 partitions of this unit (1.7GB): per-unit
                                    calibration, never committed or shared; also
                                    android-p9-BACKUP.img and config-stock.txt
~/Projects/sanytron                 the original Android control app, superseded
Ollie ~/couch-kernel/base           the kernel source tree, branch couch-ha100 (kernel/README.md)
```
