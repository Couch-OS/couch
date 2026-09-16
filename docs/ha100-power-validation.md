# HA100 charging and standby validation

This is a validation plan for kernel `ea122a39`, not a new charging policy.
The September 10 IR fix restored stock GPIO17 supply, GPIO14 standby and GPIO61
charging-related initialization. Their electrical relationships are not fully
established; do not experiment with their polarity based on their names.

See the [battery gauge review](ha100-battery-gauge.md) before interpreting these
readings. The pinned build forces battery temperature to 25°C, assigns health
`Good`, and uses an inherited phone battery model. Those fields cannot validate
thermal behavior or cell health. The separate reporting fix withholds the fixed
temperature, reports Unknown health, and exposes labeled cached gauge state; it
does not change charging policy or validate the inherited model.

## Take one read-only snapshot

```sh
REMOTE_IP=remote.local
COUCH_SSH_KEY=/path/to/couch-key
ssh -i "$COUCH_SSH_KEY" -o IdentitiesOnly=yes root@"$REMOTE_IP" sh -s \
  < tools/diagnostics/ha100-power-snapshot.sh
```

The script reads an allowlist of battery fields, GPIO14/17/58/61, LED brightness,
IR telemetry enablement, GUI heartbeat freshness, uptime and CPU/HPS state.
When present, it also reads the kernel’s cached `couch_gauge` snapshot. It contains no delays,
transmissions, reboots, process signals or device writes. It does not report
serial numbers, network configuration, credentials or process command lines.
Missing properties and readable sysfs nodes returning ENODATA appear as
`unavailable`; the remainder of the snapshot is still collected. An optional
filesystem-root argument supports offline regression fixtures.
Capture each transition separately; do not leave an unattended polling loop.

Legacy battery units: `batt_temp` is tenths of a degree Celsius, `batt_vol` is
microvolts, `BatterySenseVoltage` and `ChargerVoltage` are millivolts, and
`current_now` is microamps. On the reviewed software-gauge build it is a model
estimate: positive means modeled discharge, negative modeled charging. It is
not an independent measurement of battery current. `BatteryAverageCurrent`
comes from `BMT_status.ICharging` in milliamps and estimates charging current
from sense ADCs; its input becomes zero without a charger. These two fields
are not interchangeable.

## Observed docked snapshot

On September 10, the tested kernel reported USB online, Full, 100%, Good,
25°C, 4.227 V and `BatteryAverageCurrent=0`. `current_now` was 165700 µA.
GPIO14/17/61 were GPIO-mode outputs high; GPIO58 was low. The GUI heartbeat was
one second old, LCD brightness 42, and IR output telemetry was disabled.

This confirms the reported full/docked state, not a measured charge-termination
cycle or thermal trend. The exact docked IR volume test was confirmed separately.
The reported 25°C and Good are not independent physical measurements; the
reviewed driver forces those values.

**LED brightness caches can differ from hardware:** `red/brightness` read zero
while GPIO61 was high from board initialization. Read actual GPIO output for
this comparison. The LED-class initial cached value does not prove a low pin.

## Stock charging-control evidence

The stock kernel maps LED `data=2` to `mt_set_sub_chgr_backlight`: positive
brightness selects `chrg_gpio_high` (GPIO61); zero selects low. The latest
stock launcher `GPIOUtils.ctrlCharge(z)` writes `!z ? 1 : 0` to `red/brightness`.
`ChargeBaseManager.handleChargeGPIO(level)` calls it with true at 100%, false
otherwise, on reported charging/level changes. Stock therefore requests GPIO61
low at 100% and high below 100%.

Couch currently has no equivalent userspace writer. Its full/docked GPIO61-high
state differs from that stock policy, while the kernel reports Full and
zero averaged charging current. Full is partly derived from displayed SOC,
not an independent charge-termination measurement. Establish what the external
charging circuit controls before implementing a policy; this observation alone does not prove
overcharging or justify forcing the pin low. Preserve kernel PMIC protections.

## Physical checks still needed

| Transition | Observation and snapshot |
|---|---|
| Awake, docked | Normal UI volume command controls the TV; heartbeat fresh; record battery/pins. |
| Allow display to dim | Record GPIO58/backlight change; GPIO17/14/61 remain consistent; no frozen GUI. |
| Undock, then wait for standby | Confirm USB online clears; record capacity, temperature and board pins when SSH is available. |
| Lift or tap to wake | Confirm display/touch responsiveness; send one normal UI IR command and observe the TV. |
| Redock below full | Confirm supply online/status change and stable temperature; record before/after snapshots. |
| Reach full naturally | Record last charging and first Full snapshots, GPIO61, averaged current and temperature; do not force or accelerate full charge. |

A single snapshot cannot prove a charging trend. Record readings at meaningful
transitions and actual physical results separately. Missing network access
while undocked is not by itself proof of a frozen device. Complete these checks
before removing diagnostics or replacing the already tested kernel binary.
Temperature checks above require an independent measurement until the
thermistor path is validated and the fixed-temperature behavior is corrected.

## Room-view idle regression

The room device list uses `light-shown`, which remains true underneath TV and
media screens. It must not hold the idle timer active: ordinary browsing should
dim and sleep even when that list is open. Pairing, setup, keyboard input, voice
recording, and activities explicitly configured to stay awake retain their holds.
Validate on an undocked remote by opening a room, then a TV, leaving it untouched
for the configured dim interval, and checking both panel brightness and the
`standby: dim` log. Repeat after returning to the room list.

## Side power button

The side PMIC power button (`KEY_POWER`, 116) controls the remote’s display
standby globally. The faceplate power button (F2, 60) retains TV/activity power
control. A side-button press sleeps an active or dimmed remote, or wakes a sleeping
one; release and repeat events never reach device mappings. Explicit sleep
overrides dock-clock and activity keep-awake behavior. Lift wake is inhibited for
two seconds while the remote is set down, then resumes normally. This uses the
existing panel standby path, not whole-system suspend or shutdown.

Device validation: press side power while viewing the TV, confirm a dark screen
and no TV power change, then press it again to wake. Check that the keypad power
button still controls the TV. Repeat with a keep-awake activity and while docked.

## Confirmed September 10 device checks

After removing the room-list idle hold, the live GUI logged dimming at 30 seconds
and the backlight reading dropped from 255 to 42. The user then confirmed side
power sleep/wake and IR commands before and after sleeping the undocked remote.
TV IR power off/on and held Volume Up repeats were also confirmed. These checks
do not validate the docked/full-charge policy; that investigation remains open.

## CPU minimum during display standby

The GUI releases `/proc/hps/num_base_perf_serv` to 1 after successful panel
power-down and restores the saved active minimum before unblanking. The startup
supervisor reinstates 3 before every GUI launch, including after a crash while
asleep. This is an HPS minimum, not a forced online count; load can keep extra
cores online. Display dimming and the dock clock keep the active floor.

Compare `cpu.active_floor` and `cpu.online` in snapshots while active, dimmed,
asleep and awake again. Verify side-button, key, touch and lift wake, followed
by ordinary IR control, and a GUI restart during standby. Test with Bluetooth
and Wi-Fi enabled as normally used. A failed/unsupported floor write must not
prevent panel wake. Measure physical power before claiming a runtime improvement.
