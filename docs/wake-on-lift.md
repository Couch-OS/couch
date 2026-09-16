# Wake on lift

Couch uses the HA100's MiraMEMS DA218B/MIR3DA **accelerometer**, rather than a
gyroscope, to restore the display when the resting remote is picked up.
`remote.wake_on_lift` defaults to `true`, including for older configurations.
The webUI exposes **Remote settings → Wake when picked up**.

## Hardware and kernel

The sensor is on **I²C bus 2, address 0x27**. The stock device tree advertises
`gsensor_2` at 0x26, but the stock driver retries 0x27 and the stock boot log
records a successful probe there. Live reads confirm chip ID register 0x01
returns 0x13. No full bus scan or guessed GPIO assignment is necessary.

`CONFIG_COUCH_MOTION=y` builds the board adapter into the kernel. The patch is
`kernel/patches/ha100-motion.patch`; `kernel/motion/` also supports a temporary
external-module build against a matching kernel for reversible bring-up.
**Build kernels and kernel modules on a dedicated Linux build host only.** The adapter reserves address
0x27, validates the chip ID before writing, and configures ±2g and the vendor's
50Hz output rate. It preserves factory calibration, trim and engineering
registers. Unloading a diagnostic module restores the original range, output
rate and power registers. The kernel starts with the sensor suspended.

Root-only sysfs interface:

- `/sys/kernel/couch_motion/enabled`: write `1` to sample, `0` to suspend.
- `/sys/kernel/couch_motion/accel`: signed `x y z`, **1024 counts per g**; reading
  while disabled fails with `EAGAIN`.

## GUI behavior

`ui/couch-gui/src/motion.rs` samples at 10Hz on a background thread while the
screen is dimmed, off, or displaying the dock clock. Active use and disabling
wake-on-lift suspend the sensor. Missing hardware is retried every five seconds
without blocking rendering or disabling touch/button wake.

The detector learns a resting pose from six stable samples, then requires a
vector change of at least 210 counts for two consecutive samples. This supports
any resting orientation, rejects zero data and single-sample bumps, and wakes
once per standby period. Generation tags discard notifications from an earlier
standby session. Wake restores brightness, dismisses the dock clock, refreshes
lights after display-off, and schedules the existing panel verification.

This is **display-standby wake**, not deep SoC suspend wake. Couch currently
keeps the CPU available during display standby. A sensor interrupt capable of
waking `mem` suspend has not been traced or verified. Battery consumption and
thresholds need field measurement before claiming a low-power suspend solution.

## Validation

- Five tests cover noise, bumps, real lift patterns, invalid data, arbitrary
  resting orientation and stale notifications across standby sessions.
- Model regression test preserves an explicit opt-out and enables older configs.
- Live module initialization and 100 resting samples passed without restarting
  the GUI. Gravity measured 1017–1111 counts; maximum departure from the learned
  pose was 75 counts, below the 210-count threshold.
- A real-GUI mount-namespace fixture passed simulated lift from dim, display-off
  and dock-clock states, plus opt-out sensor suspension and physical button wake.
  Production configuration/Wi-Fi hashes were unchanged and its GUI restored.
- The built-in kernel compiled on a dedicated Linux build host and booted on the HA100 as
  `3.18.79-couch-normal-ge581fb141386`. The boot write was read back and verified;
  the independent recovery partition hash was unchanged.
- The user confirmed on September 9 that picking up the dimmed remote wakes it.
  Physical pickup from full display-off/dock clock and battery consumption still
  need field validation; those modes have passed simulated sensor tests.

After deployment, leave the remote still until dimmed, then pick it up. Repeat
from display-off and from the charging clock. Confirm table vibration does not
wake it, the webUI switch disables lift wake, and buttons/touch still work.
