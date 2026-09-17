# HA100 battery gauge and power management

The displayed percentage is an uncalibrated software estimate. A short run of
approximately one percentage point lost per minute does not establish the same
rate of physical battery depletion: the kernel deliberately steps its displayed
percentage down while it catches up with the internal estimate. Real excess
consumption remains possible because display standby does not suspend the system.

## Scope and source provenance

This review covers the kernel pinned at `08fd6f4d2efa7ff9ab4812dad931d95bc4735c7b`
and the Couch UI and startup policy. Battery/power source directories at commit
`d417106e` are unchanged relative to this pin. Build dependency records confirm
the `mt6580/x15cm_s90_kr` custom battery headers are used.

The relevant code is in the separate [kernel repository][kernel]:

| Component | Source relative to the kernel root |
|---|---|
| Percentage smoothing, status, exported properties | `drivers/power/mediatek/battery_common.c` |
| Software gauge, battery tables and estimated current | `drivers/power/mediatek/battery_meter.c` |
| Charge state machine, current selection and termination | `drivers/power/mediatek/linear_charging.c` |
| PMIC ADC access | `drivers/misc/mediatek/power/mt6580/battery_meter_hal.c` |
| PMIC charger programming | `drivers/misc/mediatek/power/mt6580/charging_hw_pmic.c` |
| Board capacity, voltage/resistance/thermistor tables and charge defaults | `drivers/misc/mediatek/include/mt-plat/mt6580/x15cm_s90_kr/cust_battery_meter.h`, `cust_battery_meter_table.h`, `cust_charging.h` |

`CONFIG_MTK_SMART_BATTERY=y`, `CONFIG_MTK_HAFG_20` is unset, and external charger
options are unset. The Makefiles select the legacy meter/common code, linear
charging, and MT6580 PMIC implementation. The board header defines
`SOC_BY_SW_FG`; the similarly named unset Kconfig symbols do not select the
algorithm in this build.

The three custom headers are byte-identical to the Wiko K300 headers at upstream
`521b3081`. They were copied during HA100 bring-up, not calibrated for the remote.
Device-tree overrides are supported (`BATTERY_DTS_SUPPORT` is defined in
`mach/upmu_sw.h`). The saved stock kernel and board-init Couch image have the
same appended DTB, whose `bat_meter` and `bat_comm` nodes contain no calibration
properties. The saved keypad overlay has no battery calibration properties.
These artifacts do not substitute for reading the effective tree on another
user's remote.

## Baseline driver findings

The following describes the pinned kernel before the reporting changes below.

### Battery temperature is forced to 25°C

The tracked and effective kernel configurations enable
`CONFIG_MTK_DISABLE_POWER_ON_OFF_VOLTAGE_LIMITATION`. The board meter header
turns that into `FIXED_TBAT_25`, and `force_get_tbat()` immediately returns 25
at compile time. A device-tree value cannot override that early return.

This affects both the exported temperature and the temperature supplied to
the battery model and software charging-temperature checks. The normal
overtemperature path therefore does not receive the actual thermistor
temperature. `health=Good` is also assigned unconditionally by `battery_update()`;
it does not establish thermal condition or remaining cell capacity.

The archived stock kernel configuration contains the same option. That does
not prove the unavailable stock driver implements it identically. The PMIC
charger code still programs watchdog and voltage protections; this finding
does not establish that every hardware protection is absent. Its initialization
also writes `PMIC_RG_BATON_HT_EN=0`, so an independent battery-temperature cutoff
must be verified rather than assumed.

The thermistor ADC fields can still change while `batt_temp` remains 250.
Validate the actual thermistor and divider against an independent temperature
measurement before enabling their use in charge control. Do not use a constant
25°C reading as evidence that charging is thermally validated.

### The capacity and battery model come from another product

The board profile has `Q_MAX_POS_25=2535` mAh; Sanytron specifies a
[2000 mAh HA100 battery][charging-guide]. The official
[Astrion Smart Remote V1.0.0 datasheet][datasheet], linked from Sanytron’s
[datasheet library][datasheet-library], also specifies 2000 mAh and 5 V / 2 A
input. It does not give the cell’s maximum charge voltage, thermistor curve,
sense resistor, or gauge calibration. The input rating is not the cell charge
voltage. The live remote exported
`charge_counter=2535000`, confirming the 2535 mAh nominal setting is active.
The voltage-versus-discharge and internal-resistance curves are also inherited
from the phone profile.

The capacity default is about 27% larger than the advertised capacity. A larger
capacity alone would tend to make estimated depletion slower, so it does not by
itself explain fast percentage loss. Incorrect voltage and resistance curves,
initialization state, temperature, and cell aging can also affect the estimator.
There is no measured error bound for this build; changing only Qmax to 2000
would not validate the rest of the model.

### Percentage and current are not independent measurements

`system::battery()` in [the GUI](../ui/couch-gui/src/system.rs) reads kernel
`capacity` directly. The kernel exports `BMT_status.UI_SOC`, a presentation
value derived from its internal `SOC`.

In `mt_battery_Sync_UI_Percentage_to_Real()` (`battery_common.c:1925`), when
UI_SOC exceeds SOC, the normal path decrements it one point at a time. The
header's `SYNC_TO_REAL_TRACKING_TIME` is 60 seconds and `BAT_TASK_PERIOD` is 10.
The counter is compared before incrementing, so sustained catch-up decrements
normally span seven periodic calls, approximately 70 seconds, rather than an
exact wall-clock 60 seconds. Charger events and other tracking paths can change
the timing. A report of roughly one point per minute is compatible with this
policy; proving that explanation requires the internal and displayed SOC values
from the affected remote.

The MT6580 HAL's hardware fuel-gauge current and coulomb functions are stubs.
The selected software gauge instead estimates current in `oam_run()` from
the difference between modeled open-circuit voltage and measured battery
voltage, divided by modeled internal resistance. It integrates that current
in software. `current_now` exposes this estimate, so agreement between its
integral and the percentage is not independent validation.

| Reading | Meaning in this driver |
|---|---|
| `capacity` | Smoothed UI_SOC, percent; not raw SOC. |
| `current_now` | Modeled battery current, µA; positive is modeled discharge and negative modeled charging. |
| `BatteryAverageCurrent` | Averaged charging-sense ADC estimate, mA, using the configured sense resistance; input is set to zero without a charger. Not discharge current. |
| `charge_counter` | Constant QMAX25 × 1000; not accumulated or remaining charge. |
| `current_max`, `voltage_max` | Hard-coded 3,000,000 µA and 5,000,000 µV; not effective battery charge settings. |
| `batt_vol`, `BatterySenseVoltage` | Averaged battery voltage in µV and mV respectively. ADC accuracy is not independently calibrated here. |
| `batt_temp` | Tenths of °C, but fixed at 250 in this build. |
| `health` | Set to `Good`; not a capacity or thermal diagnosis. |

The upstream [power-supply ABI][power-supply] gives `charge_counter` a different
meaning. Generic tools that integrate this field or trust these maximum fields
will produce misleading results on this vendor driver. The generic
`FG_g_fg_dbg_*` sysfs attributes are also unsuitable without checking their
updater: `update_fg_dbg_tool_value()` is compiled under `SOC_BY_HW_FG`, while
this build uses the software gauge.

If a healthy, genuinely 2000 mAh cell physically lost 1% of its charge every
minute at a sustained rate, that would imply approximately 1.2 A average battery
current and 100 minutes of runtime. That arithmetic cannot be applied directly
to a short interval of this gauge's displayed percentage.

### Charging status does not prove net charging or termination

The driver normally reports `Charging` when a charger is detected, a battery is
present, and the charge state is not `CHR_ERROR`. `mt_battery_update_EM()` reports
`Full` when UI_SOC is 100 with a charger and no error; it does not require a
fresh measurement of zero charge current. The reviewed GUI treated both as a true
`charging` boolean, including for its dock-clock policy.

`cust_charging.h` enables `HIGH_BATTERY_VOLTAGE_SUPPORT`, despite the similarly
named unset CONFIG option. Its default linear-charger target is 4.350 V, with
a 200 mA full-current threshold and 4.110 V recharge threshold. The normal
top-off path checks the averaged charging current over six qualifying calls
before entering full state; it also has a top-off timeout. Numeric policy can
be overridden by the effective device tree. Confirm the cell's rated charge
voltage and the active settings against the actual HA100 hardware before
changing them. A 2000 mAh specification alone does not identify the voltage rating.

There is also a documented difference from stock userspace's GPIO61 policy at
100%; its electrical role remains unresolved. See
[charging and standby validation](ha100-power-validation.md). Neither a `Full`
status nor that GPIO difference alone establishes overcharging.

### Display standby leaves substantial work running

[The GUI](../ui/couch-gui/src/main.rs) blanks the panel with `FBIOBLANK`; it does
not request system suspend. In the reviewed release,
[startup](../stage2/gui-start.sh) enforced a three-core minimum through `/proc/hps/num_base_perf_serv`, including during
display standby. The current keypad loop waits for input with a one-second
timeout; the older 40 ms screen-off polling description no longer applies.
Lift detection samples acceleration every 100 ms while armed in standby.
Networking and enabled integrations remain running.

Default settings are 100% brightness, key lights enabled, dim after 30 seconds,
and screen off after five minutes. Activity keep-awake, setup, pairing and
other explicit holds can extend awake time. An earlier room-view idle hold was
fixed, so the affected user's version matters. These are credible sources of
real consumption and user-to-user differences, but their individual current
costs have not been measured in this review.

## Reporting and standby corrections

The Couch changes incorporate [PR #180](https://github.com/dangerouslaser/couch/pull/180)
and retain its author’s commit: battery icons, the default-off percentage setting
on the remote and web UI, and persistent settings across GUI restarts.
[Battery parsing](../ui/couch-gui/src/battery.rs) additionally rejects malformed
or out-of-range capacity instead of clamping it into a plausible reading.
Capacity availability, charging status and supply presence are independent.
The dock clock follows the USB/AC/wireless `online` nodes (with a status fallback
on older kernels without readable supply nodes); only reported `Charging` earns
the charging icon. Its caption distinguishes Full, Charging, Plugged in and an
unavailable gauge. Percentage remains the kernel estimate, not a recalibration.

[Panel standby](../ui/couch-gui/src/panel.rs) releases the saved HPS minimum to
one core only after `FBIOBLANK` succeeds. It restores the active minimum before
wake and on normal teardown; startup restores three cores on each supervised
GUI restart. HPS can still add cores for background demand. Unknown or missing
HPS interfaces are left alone and failed writes can be retried. This removes a
known standby floor; its current saving and wake responsiveness need device
measurement. It does not introduce system suspend or change active brightness.

The separate [kernel reporting change][kernel-fix] is guarded by
`CONFIG_COUCH_HA100` and `SOC_BY_SW_FG`. It:

- Removes the modeled `current_now` and placeholder current/voltage maxima and
  `charge_counter` from standard battery/USB properties. The software current
  estimate remains available with explicit provenance in diagnostics.
- Returns no data for forced/test `batt_temp` and for capacity before gauge
  initialization or without a cell; reports health as Unknown.
- Reports Discharging when unplugged, Not charging for a charge error/hold, and
  Full from the algorithm’s full flag only while not recharging. UI_SOC of 100
  alone no longer overrides the exported status. The inherited charge algorithm
  can itself mark full at 100 on insertion; this is still algorithm state, not
  an independent physical charge-completion measurement.
- Adds read-only `battery/couch_gauge`, a cached, versioned snapshot of SOC,
  UI_SOC, charge state, effective model constants, raw thermistor inputs and
  explicitly labeled software estimates. Reading it does not recalculate or
  reset the gauge. `sequence` and `sampled_boottime_seconds` identify freshness.
  The full units and fields are in the [kernel ABI documentation][gauge-abi].

The diagnostics script accepts both kernels, filters snapshot fields and
handles readable sysfs files that return ENODATA without aborting. Compare
`power.gauge.soc_percent` and `ui_soc_percent` to identify smoothing catch-up.
`temperature_fixed=1` means `algorithm_temperature_c` is not a measurement;
`calibration=unverified` applies to the whole inherited profile. Standard
properties removed by the new kernel correctly appear as unavailable.

Charge voltage, current limits, thermistor conversion, temperature policy,
Qmax/OCV/resistance tables and GPIO61 policy remain unchanged. The release pin
selects merged kernel `81d180fc19ec` for `.167.dev`, with all four Bluetooth
backport modules rebuilt for that exact kernel.

## Validation and next measurements

The `.167.dev` candidate booted the HA100 through the existing OTA activation
path with a healthy GUI and system service. The gauge reported `ready=1` with
an advancing sequence, Discharging while unplugged and health Unknown; the
standard modeled-current and forced-temperature exports were unavailable.
This checks reporting and startup, not battery calibration, discharge accuracy
or charge termination. Undocked runtime and the full standby/wake, IR and
Bluetooth acceptance round remain to be measured on this kernel.

A read-only snapshot from the development remote running
`3.18.79-couch-normal-g08fd6f4d2efa` reported Charging, 97%, 4.365 V,
`current_now=-497100` µA and `BatteryAverageCurrent=679` mA. Its reported
temperature was the forced 25°C. This confirms the live ABI/profile behavior,
not physical current accuracy, safe temperature, charge termination, or an
undocked discharge rate. Session evidence is kept outside Git in `scratchpad/`.

For a useful comparison:

1. Record the affected Couch/kernel version, elapsed time and percentage range,
   time since reboot/undocking, actual screen state, brightness, idle settings,
   and enabled Wi-Fi/Bluetooth/integrations.
2. Take the existing [allowlisted snapshot](../tools/diagnostics/ha100-power-snapshot.sh)
   at docked, just-undocked, dimmed and screen-off transitions. Record elapsed
   time and `uptime`/snapshot sequence separately. During a supervised 15–30
   minute interval, collect a few timestamped snapshots in a fixed state, without rebooting or changing load.
   Do not deliberately discharge to cutoff to calibrate the percentage.
3. With a validated build of the reporting kernel, capture `couch_gauge` in
   those snapshots. A decreasing gap between UI_SOC and SOC at a nearly fixed
   step interval supports model catch-up; it does not exclude real load. On the
   old kernel, missing gauge fields cannot establish this explanation.
4. Compare real runtime and independently measured battery current for screen
   on, dim and off at matched settings. USB input power while charging includes
   both system load and charging losses; it is not undocked battery current.
5. Validate the cell rating, thermistor circuit and stock calibration before
   replacing the inherited profile or removing forced-temperature behavior.
   Measure screen-off core count, current and key/lift/touch/IR wake with the
   relaxed floor before promotion. Validate wake paths before system suspend.

The reported user’s drain remains unconfirmed. These reporting and standby
changes do not establish a percentage error bound or calibrated runtime. Model
correction and real excess consumption need to be distinguished by the
measurements above.

[kernel]: https://github.com/Couch-OS/couch-kernel/tree/08fd6f4d2efa7ff9ab4812dad931d95bc4735c7b
[charging-guide]: https://hub.sanytron.com/support/astrion/charging
[power-supply]: https://docs.kernel.org/power/power_supply_class.html
[datasheet-library]: https://hub.sanytron.com/support/datasheets
[datasheet]: https://drive.google.com/file/d/1H4W41LSZb488SfHwW4lbxWRVGYaCHsDN/view
[kernel-fix]: https://github.com/Couch-OS/couch-kernel/pull/4
[gauge-abi]: https://github.com/Couch-OS/couch-kernel/blob/254b66cf66ebfa6e4de362a8995db9b32675a58a/Documentation/ABI/testing/sysfs-class-power-couch-gauge
