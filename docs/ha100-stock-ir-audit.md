# HA100 stock IR driver audit

The original HA100 kernel was inspected offline after Couch completed PWM
transfers but an LG B4 did not respond. No device commands, GPIO changes or
transmissions were performed during this audit.

## Inputs and method

The privately backed-up `kernel.bin` has SHA256
`25d39655548070233670b2c3218284c5c0ccfede46285438cb7b43a76e814998`.
Its gzip payload expands to 18,972,672 bytes. Symbol recovery using
[vmlinux-to-elf](https://github.com/marin-m/vmlinux-to-elf), version 1.2.2.post2,
found 121,470 symbols with base address `0xc0008000`. LLVM objdump resolved the
IR call paths. Extracted binaries, reconstructed ELF and disassembly remain
outside Git.

The original vendor `consumerir.mt6580.so` hash is
`05658d1ca0c03a13f16f74eb554170cb597cb7b42d187b7981c10da6e1ced8c8`.
Its device paths are `/dev/irtx` and `/dev/ir-learning`; no GPIO/sysfs enable path
was evident. Android's IR service launches the standard HAL; board init changes
only `/dev/irtx` ownership and permissions.

## Findings

| Stock function | Address | Observed behavior |
|---|---|---|
| `dev_char_ioctl` | `0xc04f9cc8` | Solution query returns 1; LED-enable copies its argument but performs no action; carrier setting is unimplemented |
| `dev_char_write` | `0xc04f9e8c` | DMA copy/inversion, IRQ enable 0/1, 26 MHz selection, IRQ acknowledgement, PWM setup, delays, DMA free and PWM disable |
| `irtx_probe` | `0xc04fa264` | Reads channel/inversion and registers the character device; no GPIO, pinctrl or regulator setup |
| `mt_pwm_power_on_hal` | `0xc04d6e40` | Enables controller/channel clocks; no LED-power operation |

Stock `irtx_pwm_config` at `0xc1160c50` selects channel 0, memory mode,
no divider, block clock, idle/guard 0, stop bit 31, high/low duration 25 and one
waveform. The duration registers therefore target one-microsecond samples.
Couch deliberately pairs carrier-scaled Rust samples with corresponding duration
registers instead; stock and Couch buffers are not interchangeable.

## Conclusion and limits

No missing HA100-specific LED-enable sequence was found in these paths. GPIO8's
inherited PWM_A mux and the measured 26 MHz zero-sample rate already agree with
Couch's expected routing/configuration. IRQ enable differs intentionally because
Couch polls completion without an unhandled interrupt; there is no evidence that
IRQ masking suppresses PWM output.

This does not prove LED wiring, supply, optical polarity or emitted carrier.
A receiver/scope or a camera verified to detect a known IR emitter is the next
useful measurement. Do not guess GPIO/regulator changes from another board's
source. See [IR validation](ir.md) for actual transfer results and the failed
LG reception test.

## Android application and HAL follow-up

The following inspection used original images stored in a private audit workspace,
without mounting or accessing the remote. Read-only `debugfs` extraction, JADX
and Capstone Thumb disassembly were used; APKs, native libraries and reconstructed
source remain outside Git. The inspected images have SHA256:

- `system.img`: `39ed365ce914c6fccb2469ca03cfcb037b752bdcb87fcae1f102c882aa350ff8`
- `vendor.img`: `de47882c6c32d2b009c65b83217aec2ff2960e21a2b1ff03744f8f437da587a9`

### Factory application paths

`priv-app/TestApp/TestApp.apk` (SHA256
`66430fab48544f827138a11456113eb7a096be4202f9711d24e1b2b58897079d`)
contains `com.aiks360.rct.ui.manual.InfraredActivity` and
`InfraredActivity2`. Both obtain `consumer_ir`, check emitter availability and
call `ConsumerIrManager.transmit(38000, pattern)` directly. No additional
emitter-enable operation appears in these recovered methods.

The second activity sends one frame on a fresh dpad key-down, ignoring Android
key repeats. Its six 71-duration fixtures contain a complete NEC frame followed
by approximately 39.7–39.8 ms silence and one NEC ditto. They decode to address
`0x01`, inverse `0xfe`, with commands `46/16/47/15/55/40` hex for
up/down/left/right/OK/back. These are not the LG address `0x04` candidates.
The first activity's differently labelled send/on/off controls use the same
array, so their labels do not establish discrete power commands. A separate
menu-key fixture contains much shorter, cycle-count-looking durations passed
straight to the microsecond API; it is not a trustworthy timing reference.

`launcher_ha100`'s `GPIOUtils` controls `/sys/class/leds/red/brightness` and
`button-backlight/brightness` from charging/backlight code. Its packaged native
libraries are Bugly components. Targeted searches of the launcher, FactoryMode,
EngineerMode and the extracted factory/custom/HID JNI libraries found no
alternate transmitter enable path. The HID library references `/dev/uhid`,
not an IR peripheral. Some JADX classes failed decompilation, so this is a
bounded negative finding, not proof that every OEM path was recovered.

The backed-up `SanytronRemote.apk` also uses `ConsumerIrManager`, but contains
Kodi/WebOS/room configuration code and Kotlin 2.2 metadata. It appears to be a
later application prototype; do not treat it as independent factory evidence.

### Actual vendor HAL timing

The HAL's transmit implementation begins at ELF virtual address `0xac8` and is
**Thumb code**. Disassembling this region as ARM gives misleading instructions.
The recovered solution-1 path establishes:

| Setting or operation | Evidence | Behavior |
|---|---|---|
| `irtx.hal.mode` | `0xc1c–0xc46` | Defaults to 0; positive values retain input durations |
| `irtx.hal.duty` | `0xc4e–0xc96` | Defaults to 25, but is only logged in this solution-1 path |
| Carrier period | `0xca0–0xcbc` | Round `1,000,000 / carrier_hz` to integer microseconds |
| Mode-0 duration conversion | `0xd16–0xd50` | Round every mark/space to a whole number of those periods |
| Actual carrier high time | `0xece–0xee8` | Round floating period times hardcoded `0.33`; literal at `0x10e0` |
| Carrier synthesis | `0xf28–0xf9e` | Start carrier phase again for each mark; pack sample bits into words |

At a requested 38 kHz, this produces 26-microsecond periods and 9-microsecond
high intervals: approximately 38.46 kHz and 34.6% duty, before driver inversion.
No overrides of these properties were found in the inspected system/vendor
property files or `etc/init/*.rc` files. The vendor IR service stanza starts
the standard HAL as system/system without a separate enable action. Boot image
properties were outside this follow-up's scope.

### Actionable comparison and remaining limits

Retain Couch's matched sample-buffer/kernel timing configuration. Its current
three-samples-per-carrier geometry differs from this stock implementation;
comments describing it as the stock HAL's exact geometry should be corrected.
Changing an Android duty property would not change this solution-1 waveform,
and Couch does not consume those Android properties.

When a verified optical receiver or scope is available, compare carrier,
polarity and mark/space timing first. A controlled stock-timing fixture must
pair one-microsecond samples with duration registers 25; never feed that buffer
to Couch's carrier-scaled register configuration. Separately compare an LG
candidate frame alone with the same frame followed by an NEC ditto. Repeating
full commands at one-second intervals does not reproduce the factory fixture.
Neither difference currently establishes the cause of failed LG reception.

## Does the remote have a usable onboard IR receiver?

The `/dev/ir-learning` reference is executable generic HAL functionality, but
the original HA100 kernel does **not** enable its corresponding driver. The
stock ELF's embedded configuration, recovered with `scripts/extract-ikconfig`,
contains `# CONFIG_MTK_IR_LEARNING_SUPPORT is not set`. None of its 121,470
recovered symbols or strings identify the learning driver. The backed-up
vendor `lib/modules` contains connectivity modules only, and this driver's
Kconfig option is a boolean rather than a loadable-module option.

The HAL installs a Thumb callback at `0x1269` in its device structure at offset
`0x60`. Its implementation at `0x1268` opens `/dev/ir-learning` read/write,
queries sample rate using ioctl `0x80016b02`, then captures data using
`0x80016b01`; a positive capture return becomes the output length. This matches
the generic MediaTek `drivers/misc/mediatek/ir_learning/mt_irlearning.{c,h}`
source in the existing kernel checkout. That source:

- Matches device-tree compatible `mediatek,irlearning-spi` and requires
  `spi_clock`, with optional inversion configuration.
- Registers SPI bus 0, chip-select 1 and configures approximately 1 MHz sampling.
- Captures a fixed 256 KiB buffer through `spi_sync`; plain `read()` returns no
  data. At 1 MHz the capture spans approximately 2.1 seconds.
- Uses the buffer for both SPI receive and transmit, so its capture ioctl is
  an active bus transaction, not a passive software-only probe.

No learning call was found in the inspected factory IR activities or launcher.
There is no verified HA100 receiver pin assignment or populated receiver
component in this evidence. The HAL callback therefore cannot establish that
the physical remote can receive IR, and cannot provide an immediate substitute
for an external optical measurement. Enabling the generic driver or selecting
SPI pins speculatively would be unjustified. A board schematic, component
identification or documented factory learning feature is needed before pursuing
this path. No receiver ioctl, pin change or device operation was attempted.

## Current shipping software comparison (September 10, 2026)

The original `system.img` identifies itself as `HAOS_V1.0.3_20260327` in
`/build.prop`. Sanytron's [IR guide](https://hub.sanytron.com/support/astrion/infrared)
places the first user-facing IR release at software V1.2.0 on June 6, 2026.
That warranted checking the shipping software, rather than assuming the earlier
factory image represented the complete working IR implementation.

The original launcher's `OtaHelper`, `ICloudBusiness` and
`ServerCallbackAdapter` expose separate anonymous update checks for model codes
`aiks_ha_remote_ha100.app` and `aiks_ha_remote_ha100.fw`. Querying those exact
channels returned **app 1.4.2 (version code 73)** and **firmware 2026020212**.
The latter is an older February build with charging release notes, not a new
IR kernel release. Sanytron's [firmware guide](https://hub.sanytron.com/support/astrion/firmware)
also distinguishes software features and underlying firmware versions.

Downloaded artifacts stayed in the private audit workspace, outside Git; neither was installed:

| Artifact | Bytes | SHA256 |
|---|---:|---|
| App 1.4.2 APK | 44,850,104 | `858fbbf79d864a3434f8ebd132906ad7f2db54c8275e2992436331709a0c80bf` |
| Firmware 2026020212 ZIP | 518,937,104 | `e9d6cd18a43c9195caac613a6168c43cf9b95758a7348931b17ae3aec0aa863a` |

Each file's MD5 matches the update response's `signature` field (not its separate
`checkSum` field). This confirms matching downloaded bytes, **not cryptographic
signer verification**. The model/channel provenance comes from the backed-up
manufacturer launcher; its metadata endpoint uses HTTP. Treat the artifacts as
analysis inputs, not newly approved restore images. Private metadata, source,
and extracted images live under the existing `ir-app-audit/latest-ota` directory
in the private audit workspace. No device-identity data was sent in the update checks.

### Comparison results

- Firmware's `consumerir.mt6580.so` is byte-identical to the earlier audited HAL
  (`05658d1c…6ced8c8`). Its IR service still runs the standard executable as
  system/system, without a separate emitter-enable action.
- Symbol recovery again found 121,470 kernel symbols. Although the compressed
  kernel hashes differ, the compared symbol spans are **byte-identical at the
  same addresses**: `dev_char_ioctl` (452 bytes), `dev_char_write` (984),
  `irtx_probe` (856), `mt_pwm_power_on_hal` (260), `irtx_pwm_config` (64), and
  the GPIO mode/output helpers (68/80). This is a targeted comparison, not a
  claim that every kernel byte is identical.
- Firmware DT overlay fragment 26 still supplies `pwm_ch = 0` and
  `pwm_data_invert = 0` for `mt_irtx_pwm`; it supplies no additional IR pinctrl
  setup.
- App 1.4.2's recovered `InfraredManagerK` calls Android
  `ConsumerIrManager.transmit(frequency, durations)`. Its plaintext format
  explicitly carries frequency followed by durations. Its NEC generator uses
  67 alternating durations: 9000/4500 leader, 560 marks, 560/1690 spaces,
  LSB-first address/complement/command/complement, and a final 560 mark.
  No extra GPIO/JNI/UART enable path or mandatory ditto precedes that call.
  The older Java IR manager also uses `ConsumerIrManager` directly.
- JADX reported 93 failures across the APK; the inspected NEC transmit methods
  recovered successfully. The Broadlink conversion method did not fully
  decompile, so this audit makes no claim about its complete implementation.

This closes the proposed “new shipping IR implementation” lead without finding
a missing kernel operation. It supports retaining the existing measured timing
and focusing the next test on actual optical emission and modulation. No
transmission, pin change, reboot, restore or flash was performed in this audit.

## Initialization follow-up (September 10, 2026)

This follow-up examines setup outside the app/HAL transmit path. Offline
extractions remain in the private audit workspace; none of the
Android executables were run on the remote.

### Android startup

- Extracted the original boot ramdisk's regular configuration files and
  inspected its symlink targets. `default.prop` points to
  `/system/etc/prop.default`; `ueventd` points to the Android `init` binary.
- Reviewed 124 `.rc` files across the ramdisk, system and vendor configuration,
  including hardware imports, project setup, normal/factory/meta boot paths,
  service declarations and hardware writes.
- `init.project.rc` only changes `/dev/irtx` permissions and ownership. Its
  button-backlight entries likewise grant permissions; they do not turn on a
  supply. The IR HAL service has no extra enable command.
- All 60 vendor initialization `.rc` files are byte-identical between the
  original image and the downloaded firmware. Their boot ramdisk gzip streams
  are also identical, SHA256
  `b31c11677944a12c87e28fa769239e658a4a4e17bc4fd57f5d67c06631474f57`.
- Targeted static string/import inspection of Android `init`, the lights and
  power HALs, and `spm_loader` found no IR-specific GPIO/regulator path.
  The lights HAL references ordinary LED brightness/trigger interfaces.
  `spm_loader` references `/dev/spm` and imports open/read/close, so its kernel
  read-side effects require separate examination; it was not executed.
- The shipping app's `GPIOUtils.ctrlCharge()` writes the `red/brightness`
  endpoint with inverted boolean values. `ChargeBaseManager` selects zero at
  100% battery and one below 100%. Its `ctrlKeyBacklight()` writes ordinary
  boolean values to `button-backlight/brightness`. These names and values
  establish application behavior, not the physical pin mapping; the custom
  stock LED implementation must be decoded before treating either as a GPIO.

These are bounded negative findings. A string search is not proof that a
binary cannot compute a path or call another library. Optional imports absent
from the backed-up filesystem, kernel initialization, bootloader setup and
actual board wiring must not be inferred from the lack of an Android script.

### Missing stock board setup: custom LED pinctrl

The original kernel's `get_cust_led_dtsi()` at `0xc04f4ae8` performs board
initialization that is absent from Couch's generic LED fallback. It finds
`mediatek,kpd_btn_light`, resolves its platform device, acquires its pinctrl
handle, and looks up the named states below. The stock appended DTB plus
device overlay supplies the physical mappings; these are not inferred from
the LED `data` fields.

| Stock state pair (low/high) | Pin | Purpose indicated by DT name |
|---|---|---|
| `btn_light_ldo_3v3_low/high` | GPIO17 | 3.3 V enable |
| `btn_light_gpio_low/high` | GPIO58 | Button lighting |
| `chrg_gpio_low/high` | GPIO61 | Charging control |
| `stdby_gpio_low/high` | GPIO14 | Standby control |

Each state selects GPIO mode, its output level, and `slew-rate = 1`. The
`default` state is empty. Stock initialization selects GPIO17 high, waits
2 ms, selects GPIO58 high for normal boot (low for other boot modes), waits
2 ms, selects GPIO61 high, waits 2 ms, then selects GPIO14 high. The three
ARM delay calls load `0x26666220`, corresponding to `udelay(2000)` with the
stock `CONFIG_HZ=300` delay multiplier.

The parser replaces LED `data=4` with `mt_set_kpd_button_backlight()`
(`0xc04f4954`) and `data=2` with `mt_set_sub_chgr_backlight()`
(`0xc04f4998`). Those callbacks select GPIO58 and GPIO61 high for positive
brightness, low otherwise. **The values 4 and 2 are not physical GPIOs in
the stock implementation.** Couch's fallback incorrectly treated them that
way; its later touchscreen guard prevents button brightness from driving
GPIO4, but does not restore this missing initialization or callback mapping.

Read-only inspection of the running Couch kernel found GPIO17, GPIO58 and
GPIO61 in GPIO mode as outputs held low. GPIO14 was an input, with DIN high.
Thus the named 3.3 V enable and all three other controls differ from stock
normal startup. This is a concrete initialization defect, unlike the earlier
negative app/IR-driver searches. GPIO8 remains the separately selected PWM_A
signal; no evidence calls for changing the transmitter pin.

The next fix should restore the validated named pinctrl states, delays and
LED callbacks, replacing the invalid numeric-pin fallback. It must preserve
touchscreen reset ownership and fail safely if required states are missing.
Then repeat the optical test. The 3.3 V rail's electrical connection to the
IR emitter is still unproven, so this audit does not establish an IR fix.
No pin writes, builds, flashes or transmissions were performed in this audit.

### Restoring the stock board controls

Kernel `d1cecbeb` implements the recovered sequence through the existing named
pinctrl states. Initialization runs from LED probe; the pinctrl handle and board-device reference
are retained until teardown. A mutex serializes initialization, runtime
selection and cleanup. Unknown LED name/selector combinations fail rather
than becoming raw pins or function pointers. LCD brightness remains on its
existing path, and button lighting no longer touches touchscreen reset GPIO4.

The implementation and host regression tests are preserved in
[`ha100-board-led-init.patch`](../kernel/patches/ha100-board-led-init.patch).
Tests cover normal/recovery startup order and accepted/rejected selector
combinations. The normal kernel built successfully on a dedicated Linux build host; the packaging
and boot-health Python suite also passed (eight tests). Image SHA256:
`2e98a376893c62b1ec1eab01881e427749c5cffe9e358939ec795ad9bd0889b0`.
The p8 write passed readback verification. Fresh pre-change p8 backups are
retained on the device and in the private audit workspace; p9 is preserved.

The first device boot exposed a validation error before any board pin changed:
`pinctrl_lookup_state("default")` returns `-ENODEV` because the DT default state
is empty. Stock tolerates this absent state and never selects it. Requiring it
prevented LED probe. The correction validates all eight functional states,
while skipping the unused empty default; missing functional states still fail.
Correction `ea122a39` also adds acquisition failure diagnostics and a regression
check for the optional default state. It built successfully on a dedicated Linux build host and its
image passed p8 readback verification, SHA256
`dbe414f9e137fb8df28153680eb50b8549557cb506f8edc86182fdd2ad995725`.

Device validation passed on `3.18.79-couch-normal-gea122a39f434`:

- GPIO17, GPIO14 and GPIO61 are output-high after boot. Button brightness
  0/255 drives GPIO58 low/high while GPIO4, GPIO17, GPIO14 and GPIO61 stay high.
- GUI heartbeats advance, USB serial and Wi-Fi SSH work, and local boot-health
  verification clears the BCB. The recovery partition and GUI binary hashes
  are unchanged.
- With telemetry off, the same LG B4 NEC Volume Up command (`0x04`, `0x02`,
  one ditto) completed and the user confirmed **it worked**. No waveform or
  transmitter-pin change accompanied this board initialization fix.

This confirms the initialization change resolves the tested IR failure. It
does not isolate which board control electrically enables the emitter or
validate other appliances, hold/repeat behavior, or power-management scenarios.
Kernel source is published on `Couch-OS/couch-kernel` branch
`couch-irtx-bringup`; the Couch repository retains the source patches and audit.
