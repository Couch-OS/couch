# HA100 kernel review

Research and read-only inspection: 2026-09-08. Scope: a responsive universal
remote running Couch, with predictable wake, reliable IR/network control, and
useful battery standby. Recommendations below are not benchmarked improvements.
No kernel, firmware, partition, governor, or sleep-state changes were applied.

## Assessment

Keep the working MediaTek 3.18 base for the next development cycle. Prioritize
measurement, driver parity, and power policy over a wholesale kernel migration.
The current upstream [MT6580 device tree](https://raw.githubusercontent.com/torvalds/linux/master/arch/arm/boot/dts/mediatek/mt6580.dtsi)
contains CPU, timer, interrupt-controller, and UART descriptions with dummy
clocks; it does not provide this remote's complete board support. A mainline
port would be a separate hardware-enablement project.

Sanytron documents a 2000 mAh battery and local IR transmission, including
operation without a network. Those are useful product requirements for Couch,
although vendor battery-life claims are not measurements of this Linux build.
See the [charging guide](https://hub.sanytron.com/support/astrion/charging) and
[IR guide](https://hub.sanytron.com/support/astrion/infrared).

## Verified environment and baseline

`ssh ollie` connects as `bryan`. The remote is physically enumerated on Ollie
as MediaTek USB `0e8d:201c`, with serial at `/dev/ttyACM0`. Ollie's existing
`~/couch-kernel/serrun.py` successfully queried its shell.

| Item | Observed |
|---|---|
| Running remote | `3.18.79 #4 SMP PREEMPT`, ARMv7, built September 8 |
| Kernel checkout | `/home/bryan/couch-kernel/base`, clean `couch-ha100` at `7a0e5e8f` |
| Base commit | `521b3081`, Wiko K300 ALPS sources |
| Connectivity donor | `/home/bryan/couch-kernel/donor`, `0d4632f00cf848ef142fc4f1846f31abc3f72252` |
| Build output | `/home/bryan/couch-kernel/out` |
| Config provenance | Output `.config` exactly matches `kernel/couch-ha100.config` by SHA-256 |
| CPUs | `0-2` online; HPS floor `3`; governor `interactive` |
| Frequencies | Advertised approximately 604.5 MHz–1.3 GHz; sampled requested frequency 1.001 GHz |
| Memory | `MemAvailable: 903416 kB`; no active swap in that snapshot |
| Power | `freeze mem` exposed; autosleep `off`; USB online; battery `Full`, 100% |
| Tracing | Only `nop` available; `tracing_on=1` does not establish which events are enabled |
| Inputs | Matrix keypad, MediaTek keypad, ACCDET; no touchscreen input registered |

Config SHA-256:
`d35dd9d585ba30253508dadd07bc9c62040e21f2d0481b32922fd3cf2c0382bb`.
Installed `couch-kbuild` image ID:
`sha256:0555bb78c532853ec90a6c503095c070245f97d86ec2215072c5b45e8a345ef9`.

The checkout has an `origin` pointing to the upstream Wiko repository, but
`couch-ha100` has no tracking branch. This is more precise than saying there
is no Git remote. I did not establish that the custom commit is backed up
elsewhere.

## 1. Separate diagnostic and normal-use builds

The current configuration combines expensive debugging with limited profiling.
It enables `PROVE_LOCKING`, `LOCKDEP`, `DEBUG_LOCKDEP`, `DEBUG_SPINLOCK`,
`DEBUG_MUTEXES`, `DEBUG_PREEMPT`, and `DMA_API_DEBUG`. Ollie's
`base/lib/Kconfig.debug` explicitly describes overhead for lockdep debugging
and performance degradation from DMA API debugging.

Create two explicit configuration fragments, with separate output directories:

- **Diagnostic:** retain locking/DMA checks for driver bring-up; add
  `CONFIG_FUNCTION_TRACER`, `CONFIG_FUNCTION_GRAPH_TRACER`,
  `CONFIG_IRQSOFF_TRACER`, `CONFIG_PREEMPT_TRACER`, and `CONFIG_PM_DEBUG` where
  Kconfig dependencies permit. Run tracing only around a reproduction.
- **Normal use:** disable the above lock-validation/debug options and
  `CONFIG_DEBUG_LOCK_ALLOC`; let Kconfig resolve derived symbols. Evaluate
  disabling `CONFIG_MTK_FTRACE_DEFAULT_ENABLE` and unused MTK debug facilities
  separately. Preserve diagnostics needed for recovery, symbols, and boot logs.

Retain `CONFIG_PREEMPT=y`, high-resolution timers, CPU idle, and `HZ=300`
initially. They are already enabled. Treat 1000 Hz, PREEMPT_RT, compiler changes,
and permanent maximum frequency as separate experiments only if traces justify
them. Removing DWARF debug information mainly affects build artifacts; it is
not equivalent to removing runtime lock checking.

Use the [ftrace documentation](https://docs.kernel.org/trace/ftrace.html), with
this kernel's `/sys/kernel/debug/tracing` path and its own Kconfig as authority
for available facilities. Compare the same workload before claiming a speedup.

## 2. Remeasure the keypad before rewriting it

Live sysfs identifies `mt_gpio_kpd` as bound to `matrix-keypad`, compatible
`gpio-matrix-keypad`. In the actual source:

- `drivers/input/keyboard/matrix_keypad.c:174` schedules delayed work from the IRQ.
- `matrix_keypad_scan` at line 116 performs column scanning in that worker.
- IRQ disable/re-enable operations still occur under spinlocks and merit tracing.

The README's 46–62 ms hard-IRQ measurements are historical evidence from the
earlier setup, not a newly verified property of `#4`. The current driver does
not directly scan the matrix inside its hard-IRQ handler. Do not assume the
entire scan must be moved to a worker; it already is.

Capture physical presses and held keys, with IRQ entry/exit, workqueue execution,
scheduler wakeup, evdev delivery, and GUI presentation timestamps. Trace the
MediaTek EINT mask/unmask path if the delay remains. Synthetic evdev injection
tests GUI handling but bypasses the GPIO/IRQ path.

Keep the existing three-core workaround until this comparison is complete.
Then test an adaptive core floor: enough capacity during interaction and voice,
less while idle. Avoid pinning the UI to a core HPS can offline. Measure frame
tails and first-command latency, not just average FPS.

**Change made 2026-09-15 (kernel branch `couch-keypad-debounce`, commit
`08fd6f4d`, app PR pinning it; awaiting hardware acceptance).** Two findings
drove it. First, the keypad node `mt_gpio_kpd` exists only in the `odmdtbo`
overlay the bootloader applies; the partition on the dev remote is byte-identical
to the factory overlay, so the earlier `tools/dtbpatch.py` 50 → 8 ms debounce
change was undone by installer/restore writes and every remote runs
`debounce-delay-ms = 50` (20 fast DOWN taps deliver 8 presses, because
debounce applies to press and release). Second, `irq-mt-eic.c` emulates
dual-edge triggering by flipping the EINT polarity after every interrupt and
has no `irq_disable`, so `disable_irq_nosync` is lazy: the row EINT stays
unmasked until the scan's column strobes edge the held row again, `handle_level_irq`
then masks it and marks it pending, and the driver's `enable_irq` after the
scan replays that edge through the software resend tasklet. A held key therefore
rescanned every ~85 ms. The fix lives in `drivers/input/keyboard/matrix_keypad.c`
under the existing `CONFIG_COUCH_HA100` + `mt_gpio_kpd` guard (stock and rescue
device trees unchanged): debounce clamped to 8 ms, column settle clamped to
50 µs, and while any key is down the driver keeps the row IRQs off and polls the
matrix every 20 ms, re-enabling the IRQs only when a scan finds every key up.
Stop cancels the pending poll; suspend leaves the row IRQs enabled for wake
(the lazy disable would otherwise leave a strobed row masked). Acceptance:
`tools/keypad/keytap --seconds 10` then 20 fast taps → 20 presses; a held key
with `kworker/0:1` idle; a second key while one is held reported within ~20 ms;
wake on key press. Open question to measure: whether the per-scan cost itself
(the README's 46–62 ms) was the interrupt storm or the GPIO path; if scans are
still expensive, raise `COUCH_HA100_HOLD_POLL_MS` (all three values are
`#define`s at the top of the driver).

## 3. Implement wake capability before enabling system sleep

The live matrix-keypad DT node has **no `linux,wakeup` property**, and its device
has no `power/wakeup` attribute. This tree's DT parser specifically reads
`linux,wakeup` (`matrix_keypad.c:428`). Thus matrix-key wake is not currently
configured through this driver's normal wake mechanism. That does not prove
all hardware wake paths are impossible; it identifies a concrete missing step.

Add the appropriate board-specific wake declaration and verify MediaTek EINT
wake routing. Use a reviewed image-specific DT change or board-specific driver
change; the shared `odmdtbo` partition also affects rescue boot. Do not assume
that adding a modern `wakeup-source` property alone will work with this parser.

`CONFIG_SUSPEND` and autosleep support are already built, but autosleep is off.
`CONFIG_PM_RUNTIME` and `CONFIG_PM_DEBUG` are off. Runtime PM is a separate
driver-integration experiment, not a prerequisite switch for system suspend.

The live `battery suspend wakelock` was continuously active while USB was
online. `drivers/power/mediatek/battery_common.c:3008` takes this lock when a
charger is detected. This is consistent with charging behavior, not evidence
of an unexplained leak. Preserve charger supervision and measure on battery
as well as USB/dock; do not remove the lock merely to force sleep.

Use staged `pm_test` checks before actual suspend, then repeated wake cycles
with a verified recovery/wake path. Measure input wake, display recovery,
WiFi reconnection, first network command, and microphone recovery. Follow the
[kernel suspend-debugging procedure](https://docs.kernel.org/power/basic-pm-debugging.html),
checking interfaces against this older tree. USB serial may disappear in sleep.

Near-term userspace improvement: replace the GUI's 40 ms screen-off polling
sleep with event-driven waiting and deadlines. That removes avoidable polling
latency and wakeups, but does not itself provide suspend-to-RAM.

## 4. Port IR as a core universal-remote feature

IR is disabled in the current config and the platform driver directory is
missing. Keep the tested `couch-ir` userspace ABI for the initial port.

The [MT6735 PWM donor](https://github.com/SoCXin/MT6737/blob/89eaf42ad72e6fcde9b1179c7be30e05968fe63c/linux/kernel/drivers/misc/mediatek/irtx/mt6735/mt_irtx_pwm.c)
matches the documented `mediatek,irtx-pwm`, `pwm_ch`, and `pwm_data_invert`
binding. It still requires MT6580 clock, PWM, pinmux, and DMA integration.
Source inspection found issues worth fixing during the port:

- Lines 168–169 sleep for an estimated duration plus a fixed 100 ms. Use verified
  transfer completion with a bounded timeout if supported, retaining required
  protocol spacing in userspace. Do not simply delete the delay.
- `copy_from_user` failure returns its positive residual byte count instead of
  `-EFAULT`; PWM configuration errors are overwritten by a success count.
- DMA memory is freed before PWM is disabled. Establish completion/quiescence
  before freeing, including error and timeout paths.
- Audit zero/oversized/unaligned writes, rounded word lengths, and serialization
  of the shared PWM state. These are donor-code concerns, not defects in a
  currently installed Couch IR driver.

Acceptance: real target responds, measured carrier/pulse timing agrees with
the protocol, holds repeat correctly, and local volume/power work without WiFi.
Do not block the rendering thread on transmission or network reconnection.

## 5. Port touch without importing another panel's firmware policy

The [tlsc6x donor](https://github.com/danascape/linux-daria-mt6877/tree/fourteen-qpr1/drivers/input/touchscreen/mediatek/tlsc6x)
exists on branch `fourteen-qpr1`; its Makefile uses `CONFIG_TOUCHSCREEN_TLSC6X`.
It is a source reference, not a drop-in driver for this 3.18 board.

Specific hazard: `tlsc6x_main.c:1930` calls `tlsc6x_tp_dect`; in
`tlsc6x_comp.c:1415`, detection calls `tlsc6x_do_update_ifneed()`. That routine
passes bundled firmware to the update controller. Disabling only
`TPD_AUTO_UPGRADE_PATH` does **not** remove this detection-time path.

Initially disable automatic updates, burn/config-write facilities, and APK
debug interfaces. Identify the actual controller/config, preserve existing
firmware, and adapt IRQ/reset/regulator wiring and coordinates. Test taps,
dragging, simultaneous keypad use, and repeated display/suspend resumes.
Gate any later firmware update on a separately verified matching image.

A [vendor-source request](https://forum.sanytron.com/t/request-for-the-gpl-kernel-source-for-the-astrion-ha100-linux-3-18-79/316)
for this exact board/build was present with one post and no reply at inspection.
Obtaining that source remains valuable; public donor drivers do not establish
the exact HA100 electrical configuration.

## 6. Fix display state ownership before pursuing GPU acceleration

Existing measurements in `README.md` report roughly 430 ms display wake and
110 ms backlight writes, plus cached-brightness divergence and recovery
workarounds. These were not remeasured in this review. They justify tracing
LCM initialization, command-queue waits, and backlight restore on `#4`.

Fix requested-versus-applied brightness handling in the driver and restore the
requested level after resume. Validate the actual PWM state and visible image,
then remove userspace retry workarounds only when no longer needed. Shorten
panel initialization delays only with panel evidence and cold/warm wake tests.
The existing software-rendered UI is a reasonable baseline while this work
proceeds.

## 7. Make builds and recovery reproducible

`kernel/build.sh` copies the tracked config only when output `.config` is absent.
An edited tracked config can therefore be silently ignored on an incremental
build. Current hashes match, but future builds should explicitly merge fragments
or reject unexplained drift. Archive the effective config beside every image.

Pin the compiler checkout and container inputs; record source/donor commits,
dirty state, compiler version, container ID, and hashes of zImage, DTB, ramdisk,
and packed image. Preserve `vmlinux` and `System.map` for the matching build.
Use supported `KBUILD_BUILD_*` settings to stabilize identification; see
[reproducible kernel builds](https://docs.kernel.org/kbuild/reproducible-builds.html).
Keep GCC 4.9/Python 2 initially so driver experiments change one variable.

Back up the custom kernel branch to a repository under project control.
Bring Ollie's useful serial helpers into the versioned workflow, with an
explicit host/port option and one serial-session owner.

Replace unconditional 90-second BCB clearing with local health evidence from
essential services. Internet or Home Assistant availability should not be a boot
requirement for an IR remote. A failure before init arms the BCB is another gap:
arm recovery before booting a trial image, after readback verification, through
a verified one-shot boot mechanism. Without that mechanism, document the limit
and retain manual recovery. Do not change this sequence experimentally during
an unrelated driver test.

## Maintenance and validation order

Linux 3.18 is absent from the [maintained upstream releases](https://www.kernel.org/category/releases.html).
The [archive includes 3.18.140](https://www.kernel.org/pub/linux/kernel/v3.x/),
but a version bump alone would neither integrate MediaTek fixes nor make the
kernel maintained. Audit the existing vendor backports before selecting stable,
network, filesystem, and driver fixes for separate tested commits.

Trim unused phone features only after checking shared dependencies. Keep the
PMIC, charging, thermal, watchdog, and WiFi infrastructure. Bluetooth remains a
requirements decision; GPS/FM/modem/camera support are candidates for inspection,
not a bulk disable list. Memory pressure was not evident in the live snapshot,
so swap/zram tuning is not the first performance task.

Recommended sequence:

1. Preserve source and artifact provenance; make config selection explicit.
2. Build diagnostic and normal-use variants; establish current physical-input
   and display baselines, with tracing disabled for final timing comparisons.
3. Port IR and touch independently, each with functional acceptance tests.
4. Fix measured keypad/display stalls; retest the three-core workaround.
5. Establish keypad wake and repeated suspend/resume; tune active/idle policy.
6. Audit unused drivers and maintenance backports in small batches.

Record p50/p95/p99 press-to-feedback and press-to-command latency, maximum
frame time during holds, IR repeat cadence, wake-to-visible and wake-to-network
times, and battery current in active/dim/off/suspend states. Include cold boot,
WiFi outage, USB/dock attachment, and repeated wake cycles. Improvements remain
hypotheses until these comparisons are made.
