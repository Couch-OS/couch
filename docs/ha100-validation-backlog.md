# HA100 validation backlog

This guide keeps the durable hardware findings and next checks for the HA100
without recording a developer workstation, home directory, device address, or
private artifact location. Use a dedicated Linux build host and set any device
or peer addresses through environment variables in the commands below.

## Established baseline

- The supported development baseline is the vendor MediaTek Linux 3.18 tree.
  A mainline MT6580 port would be a separate hardware-enablement project; the
  upstream device tree does not describe the HA100 board's clocks, display,
  PMIC, storage, or pin control.
- Preserve the exact source, effective configuration, compiler/container
  inputs, and hashes for every kernel candidate. The active release record is
  [kernel candidate and remaining hardware checks](kernel-release-candidate.md),
  and the pinned values live in `kernel/release-pin.json`.
- The matrix keypad already defers scanning to workqueue context. Measure the
  real GPIO/IRQ path before rewriting it or changing the three-core workaround.
  The acceptance procedure is in [the keypad guide](../tools/keypad/README.md).
- The current board description does not prove matrix-key wake support. Verify
  the board-specific wake declaration and MediaTek EINT routing before enabling
  whole-system suspend. A charger wakelock while USB is online is expected
  charger supervision and is not evidence of a sleep leak.
- IR and display changes require physical acceptance. Keep experimental IR
  disabled until carrier timing, target response, repeat behavior, and wake
  recovery pass. Keep the tested software-rendered GUI path while display
  ownership and resume timing are measured.

## Ordered work

1. **Provenance and build repeatability.** Make profile/config selection
   explicit, archive the effective config beside each image, and retain source,
   compiler, container, DTB, ramdisk, and image hashes. Keep kernel recovery
   and rollback artifacts available during every trial.
2. **Input timing.** Capture physical key presses with IRQ entry, deferred scan,
   scheduler, evdev, and GUI timestamps. Report p50/p95/p99 press-to-feedback
   latency, frame-time tails, held-key behavior, and wake latency. Synthetic
   evdev injection does not validate the GPIO/IRQ path.
3. **Wake and standby.** Stage `pm_test` checks, then repeat input-wake and
   display/network recovery cycles on battery and while docked. Record battery
   current, temperature, capacity, CPU floor, online CPUs, and GUI heartbeat at
   active, dim, off, and wake transitions. Do not claim standby savings from a
   single snapshot.
4. **IR.** Port or enable the board driver only behind a separately measured
   candidate. Validate local volume and power control without Wi-Fi, carrier and
   pulse timing, repeat cadence, serialization, and timeout/error behavior.
5. **Display and board mappings.** Trace panel initialization, PWM enable
   state, backlight restoration, and GPIO ownership across cold and warm wake.
   Treat donor board headers and numeric callback selectors as unverified until
   matched to the effective device tree and stock behavior.
6. **Peripheral backports.** Evaluate Bluetooth, touch, and other backports in
   isolated builds with their own acceptance gates. Do not import another
   panel's firmware-update policy or treat a public donor driver as proof of
   HA100 electrical compatibility.

## Related procedures

- [Kernel candidate checks](kernel-release-candidate.md)
- [Battery, charging, and standby validation](ha100-power-validation.md)
- [Runtime validation](ha100-runtime-validation.md)
- [Keypad acceptance tools](../tools/keypad/README.md)
- [Kernel backports research](kernel-backports-research.md)
- [Device recovery](device-recovery.md)

The detailed September 2026 session records are retained in the local ignored
cleanup archive under `scratchpad/cleanup-2026-09-16/archive/`. They contain
machine-specific observations and private artifact locations and are not part
of the public documentation surface.
