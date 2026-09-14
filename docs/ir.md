# Infrared

Couch transmits per-device codes through the HA100's onboard blaster. It does
not provide IR learning. `clients/couch-ir/` contains the Rust protocol encoders,
waveform generator and `/dev/irtx` transport.

## Driver status

**LG B4 Volume Up worked on-device on September 10, 2026.** Kernel `ea122a39`
restores the stock board supply/control initialization and LED callbacks that
Couch omitted. The user confirmed a successful NEC `0x04 / 0x02` command with
one ditto after this change; the same command had failed before it. GPIO17's
named 3.3 V enable and GPIO14's standby control now remain output-high. Button
lighting correctly controls GPIO58 without touching touchscreen reset GPIO4.
See the [board initialization fix](ha100-stock-ir-audit.md#restoring-the-stock-board-controls)
and [`ha100-board-led-init.patch`](../kernel/patches/ha100-board-led-init.patch).
IR telemetry is disabled. Subsequent user tests confirmed physical-button volume
up, held repeats, power off/on, and commands before and after side-button sleep
while undocked. This validates the tested LG B4 commands, not every catalog code
or appliance. IR volume down remains unverified; its earlier test used webOS.

The remaining entries in this section record earlier bring-up results.

Kernel `9b699dde` is live and its guarded completion path passed two separate
zero-word writes (2,136 and 2,149 µs) and one 281 µs carrier-burst write (2,154 µs).
Longer all-zero transfers also passed: 68 ms requested, 976-byte write returned
in 69,482 µs; 500 ms requested, 7,132-byte write returned in 501,744 µs.
GUI heartbeats advanced and no TX errors were logged. These establish bounded
DMA completion and repeated-write cleanup; **optical emission and actual target
control remain unvalidated**.

A subsequent single Volume Up test against an LG B4 OLED produced **no observed
TV response**, although the API returned `sent: true` and the GUI remained
responsive. The command was NEC address `0x04`, command `0x02`, with zero repeats,
from the bundled LG C1 record; C9, OLED65C8PUA and MR21GC records agree. The user
confirmed the original remote works from the same position. This is a failed
end-to-end test, not evidence of working optical output; emitter routing, carrier
timing and target compatibility remain under investigation.

After correcting the first NEC repeat gap, a later user-observed test sent one
LG Volume Up frame plus one ditto, followed by three separately requested presses
with the same repeat pattern. The user reported **no TV response**. All writes
completed and the GUI heartbeat continued advancing. Full-frame first-completion
times were about 68 ms and ditto times about 13 ms. The corrected repeat path is
therefore tested at the controller but has not resolved appliance reception.
Further command retries alone cannot distinguish missing optical emission from
an invalid emitted signal; use a known-sensitive IR receiver/camera or scope.

The driver requests `/dev/irtx` mode 0600; runtime mdev sets 0660 (observed 10:61).
The effective DT selects PWM channel 0, inversion 0. Read-only GPIO inspection
confirmed GPIO8 mode 2, the MT6580 `PWM_A` function. Current IR probe and vendor
HAL do not set that mux or enable a separate LED GPIO, so the configuration is
inherited from boot. The stock overlay adds no IR pinctrl state. Pinmux matching
PWM_A does not by itself prove the board's LED connection or power.

Previous candidates reached `sent_waves=1` while interrupt enable/status remained
zero, then timed out. The [MediaTek PWM API](https://android.googlesource.com/kernel/mediatek/+/android-mtk-3.18/drivers/misc/mediatek/pwm/mt_pwm.c)
provides the sent-wave counter independently. The fix polls exactly one completed
wave, requires a zero baseline before or during the current transfer, and waits
the full computed waveform duration after configuration returns. A stale count
cannot complete the next frame. No shared IRQ is enabled. Error diagnostics read
only the powered channel and shared controller before acknowledgement/disable.
`kernel/test-irtx-completion.c` tests the actual helper's stale-count and duration
guards on Ollie.

An earlier driver revision hung during transmission. Compilation and successful
probe do **not** validate LED output, carrier frequency, completion interrupts,
or display coexistence. Physical validation remains pending; see below.

## Couch's waveform ABI

Couch explicitly pairs its Rust generator with its custom kernel driver.
Compatibility with an arbitrary stock MediaTek driver is not established.

| Operation | Value | Behavior |
|---|---|---|
| Set carrier | `0x40045200`, pointer to u32 | 10–100 kHz, stored per open descriptor |
| Get solution | `0x80045201`, pointer to u32 | returns 1, PWM-only |
| Write | little-endian u32 words | waveform followed by one duration word |

Each sample bit lasts `round(26 MHz / (3 × carrier))` reference clocks. At
38 kHz this is 228 clocks, approximately 8.77 µs. A mark uses one high sample
and two low samples, about 33% duty. `HDURATION` and `LDURATION` each contain
sample clocks minus one. Words play least-significant bit first.

The final u32 is the declared frame duration in microseconds. The driver
**subtracts four bytes before allocating/copying DMA data**, then programs
`BUF0_SIZE = waveform_words - 1`. The register itself counts words minus one;
it does not remove a trailer. Only waveform bytes are inverted when requested.

Writes are serialized; carrier settings on another open descriptor cannot
change the current frame. Both declared duration and computed waveform length
are bounded to about one second, with up to one padded word. Completion polls
have a computed timeout plus 50 ms scheduling slack. The channel is disabled
and drained before DMA memory is freed. The trailer never controls a timeout.

## Source evidence and correction

The [older MediaTek PWM driver](https://github.com/SoCXin/MT6737/blob/89eaf42ad72e6fcde9b1179c7be30e05968fe63c/linux/kernel/drivers/misc/mediatek/irtx/mt6735/mt_irtx_pwm.c)
uses one-microsecond samples, does not implement carrier selection and clocks
all supplied words. The [later consumer-IR HAL](https://github.com/StyxProject/vendor_mediatek_opensource/blob/856297f9007ca310ae33d8d54881789a15135926/hardware/consumerir/src/consumerir.c)
generates carrier-scaled samples and a duration trailer. These are useful
references, but **not a verified compatible pair**. Earlier documentation
incorrectly treated their buffer formats as interchangeable and assumed the
register's minus-one encoding removed the trailer. The Couch driver corrects
that mismatch; optical measurement is still required.

## The protocols

Pure encoders in `src/proto.rs`, each a function from address/command to a
carrier and a mark/space table, checked against the published specs (San
Bergmans' [sbprojects.net/knowledge/ir](https://www.sbprojects.net/knowledge/ir/)
for NEC/RC5/RC6/SIRC; the common LIRC `SAMSUNG32` timings for Samsung):

| protocol | carrier | codes |
|---|---|---|
| `nec` | 38 kHz | 8-bit address + 8-bit command, address auto-complemented |
| `nec-ext` | 38 kHz | 16-bit address + 8-bit command |
| `rc5` | 36 kHz | 5-bit address + 6-bit command, bi-phase, toggle bit |
| `rc6` | 36 kHz | mode 0: 8-bit address + 8-bit command, double-width toggle |
| `sony12/15/20` | 40 kHz | 7-bit command + 5/8-bit address (+8-bit extended) |
| `samsung` | 38 kHz | 8-bit address (sent twice) + 8-bit command |
| `raw` | caller's | a literal microsecond table |

Repeats are per-protocol: NEC sends its short 9 ms/2.25 ms/560 µs "ditto" frame
every 110 ms; the others resend the whole frame (SIRC every 45 ms, the RCs and
Samsung ~107–114 ms). `tx.rs` handles held keys with `--repeats N`.

One carrier note: the brief lists "NEC/Sony 38 kHz", but the SIRC specification's
carrier is **40 kHz**, and that is what a Sony set expects, so the Sony encoders
use 40 kHz. The `--dry-run` output always prints the carrier, so this is never
hidden; override it with `--carrier` if a particular set proves otherwise.

## Codesets

`couch-model`'s `Integration::Ir { codeset }` names *which* remote's codes a
device answers to. The codes themselves live in a small text table
(`src/codeset.rs`), one button per line:

```text
# lg-tv.codeset
power   nec   0x04 0x08
volup   nec   0x04 0x02
voldn   nec   0x04 0x03
```

Button names are free text, but the intended vocabulary is `couch-model`'s
`Action.command` (`on`/`off`/`volume:up`/…). The convention — documented here,
not baked into a shared type — is that `codeset` resolves to
`/opt/couch/ir/<codeset>.codeset`. **`couch-model` is unchanged**: it already
carries the one field this needs, and adding an IR-code table to a crate that
also builds for wasm would be the wrong place for it. `couch-ir` deliberately
does not depend on `couch-model`, the same separation `couch-voice` keeps.

## Building

```sh
cd clients
cargo build --release --target armv7-unknown-linux-musleabihf -p couch-ir
cargo test -p couch-ir          # host, no device needed
```

A static musl binary, `rust-lld`-linked, no cross sysroot — the same toolchain
constraints `docs/voice.md` describes. The release binary is ~342 KB. The only
dependency is `libc`, for `open`/`ioctl`/`write`/`close`.

## What is verified, and how

Proven by `cargo test -p couch-ir` (32 tests, host):

* **Every protocol encoder**, against exact spec tables — NEC's worked example
  (address 0x00, command 0x16), NEC extended, Samsung's symmetric leader and
  doubled address, Sony 12-bit, and the fully hand-derived RC5 and RC6 all-zero
  frames (checked against their canonical totals, 24003 µs and 52·444 µs).
* **The PWM buffer generation**, both solutions, against hand-computed buffers
  where the geometry is small enough to check bit by bit, plus word-boundary
  crossing and the little-endian serialisation.
* **The ioctl numbers**, against the vendor macros.
* **Repeat handling and solution selection**, by driving a recording fake
  through the real `transmit` path — NEC sends the ditto frame, resend protocols
  resend the whole frame, a single press never sleeps.

Not verifiable off-device, and needing the blaster:

* That `/dev/irtx` accepts the `write()` and the ioctls return what the source
  says (a wrong ioctl number would surface as `ENOTTY`, which `couch-ir` reports
  by name).
* That the emitted light actually switches a real television.
* That the carrier/duty are within a receiver's tolerance.

## Testing on the device

`couch-ir` prints the whole timing table with `--dry-run` before it will send
anything, precisely so this can be checked in stages.

**1. Confirm the encoding with no hardware** (any machine):

```sh
couch-ir send nec 0x04 0x08 --dry-run
```

Prints the carrier, the 67-entry NEC table, and the PWM buffer size. Diff that
table against a known-good NEC capture for the code, or against a logic-analyser
trace of the real send in step 3.

**2. Validate registration without sending.** After a coordinated candidate
boot, inspect `/dev/irtx`, the driver probe log and normal display/touch/wake
behavior. An ioctl-only query can confirm solution 1 without writing a frame.
Do not use a power command as a driver-presence check.

**3. Coordinate one measured transmission.** Keep recovery access available,
choose a verified command for the actual target, and capture the blaster with a
receiver or oscilloscope. Compare carrier, duty and mark/space timing with the
dry run. Check the kernel log and display afterward. Do not retry a timed-out
frame automatically; it may already have emitted light.

**4. Validate repeats and coexistence.** Only after a single frame succeeds,
check held-key cadence, simultaneous display activity, wake/sleep and separate
callers with different carrier frequencies. The Couch driver polls completion;
it does not retain the vendor driver's fixed 100 ms post-send sleep.

## Bounded bringup probe

Build the diagnostic independently of the GUI:

```sh
cd clients
cargo build -p couch-ir --release --target armv7-unknown-linux-musleabihf --example irtx_probe
cargo run -p couch-ir --example irtx_probe -- --pulse --dry-run
```

The ARM executable is `clients/target/armv7-unknown-linux-musleabihf/release/examples/irtx_probe`.
On the device, its default operation only opens `/dev/irtx`, queries solution 1
and sets 38 kHz on that descriptor. `--zero` and `--pulse` are explicit write
operations; `--dry-run` opens nothing for any mode.

- `--zero`: writes `[0x00000000, 281]`, one zero waveform word and a duration trailer.
- `--pulse`: writes `[0x49249249, 281]`, eleven high samples spaced three ticks
  apart, ending low. This is a short carrier burst, not a television command.
- `--zero-us 68000` or `--zero-us 500000`: allocates enough all-zero sample
  words for at least that duration, plus the separate trailer. Inputs are bounded
  to 1–1,000,000 µs. These exercise longer DMA without intentional carrier marks;
  no repeat loop is built in. Add `--dry-run` to inspect lengths without hardware.
- `--carrier 36000`, `--carrier 38000` (default), or `--carrier 40000` changes
  the queried descriptor's carrier and computes sample clocks, buffer length
  and padded duration consistently. The accepted range matches the driver:
  10,000–100,000 Hz. For example, `--zero-us 68000 --carrier 36000 --dry-run`
  verifies the payload without opening the device. These zero-wave checks do
  not measure the emitted optical carrier.
- The two original probes send exactly eight bytes in **one syscall with no retry**, even on EINTR
  or a short write. At 228 clocks per sample the actual waveform lasts about
  281 µs. Logs flush before the write and report return value, errno and elapsed
  time afterward. A successful return proves driver completion, not optical output.

Run registration-only checks before zero-waveform, pulse and finally a verified
real target command. Collect kernel messages, elapsed time, advancing GUI
heartbeat and display/wake state between stages. Stop after an error.

Before the first physical test, retain a verified normal boot image and recovery
partition backup. The boot-health process eventually clears the recovery BCB;
wait for that process to finish before arming recovery for a risky write, and
verify its value. Restore normal boot intent after the controlled test. A shell
`timeout` cannot recover a CPU wedged in MMIO; watchdog/physical recovery must
remain available. USB enumeration alone does not prove a responsive kernel.

The historical failure was a four-byte zero waveform on `15b9bb34`, followed by
watchdog reboot. Corrected clock ordering did not eliminate it. No captured
trace proves its exact cause, so do not attribute it solely to the ABI mismatch.
The candidate's minimum valid frame is eight bytes including the trailer.

## Read-only pinmux check

```sh
cat /sys/class/misc/mtgpio/pin | sed -n '1p;/^ *8:/p'
```

The first digit after `8:` is mode; observed `8:21001110` confirms mode 2.
The header documents the remaining fields. This attribute's show callback reads
GPIO state; **writing** the same attribute can change pins and is not a query.
Do not substitute GPIO numbers from generic PWM test code for HA100 wiring.

## Output-path investigation

A real LG B4 OLED Volume Up command (NEC address 0x04, command 0x02) returned transport
success but produced no visible response. The original remote worked from the
same position. A phone camera showed neither remote, so that comparison did not
establish whether Couch emitted IR. Do not equate `sent=true` with target reception.

The source and live mux/config agree on PWM_A, non-inverted samples, little-endian
LSB-first waveform bits and 228 reference clocks per sample at nominal 38 kHz.
However, successful write duration does not verify the PWM clock: the conservative
minimum-time guard can conceal hardware completing earlier than expected.

Diagnostic kernel `b3c10e0e` logs one `TX timing` record after each successful
write, separating `first_complete_us` from `guard_complete_us` and reporting
`expected_us`, `setup_us`, carrier and DMA size. The first counter observation
requires the existing zero baseline; a stale count is ignored. Safety guards,
IRQ state and DMA cleanup are unchanged. Counter timing is sampled at roughly
0.5–1 ms intervals; setup time is reported separately.

After coordinated candidate boot and recovery setup, run individually:

```sh
irtx_probe --zero-us 68000 --carrier 38000
irtx_probe --zero-us 500000 --carrier 38000
```

Capture both probe output and `TX timing` kernel lines. At the intended 26 MHz,
first completion should track approximately 68.2 ms and 500.1 ms. A 66 MHz source
would instead imply approximately 26.9 ms and 197.0 ms, while guard completion still
waits the expected duration. If first completion does not scale with payload
length, investigate counter semantics before attributing it to a clock ratio.
These all-zero transfers emit no intentional carrier marks and cannot establish
optical carrier, polarity or LED power. Scope/receiver evidence or a responding
target is still needed before declaring IR control working.

## Measured completion timing

On diagnostic kernel `b3c10e0e`, individually issued zero transfers produced:

| DMA bytes | Expected µs | Setup µs | First completion µs | Guard completion µs | Write return µs |
|---|---:|---:|---:|---:|---:|
| 972 | 68,190 | 22 | 68,905 | 68,905 | 72,190 |
| 7,128 | 500,057 | 25 | 500,554 | 500,554 | 503,986 |

GUI heartbeats advanced through both transfers. The first counter observation
scales with waveform length at the intended 26 MHz rate; it does not show the
previously considered 66 MHz acceleration. These measurements validate all-zero
sample timing, not optical carrier modulation or LED power.

The original HA100 Android IR service launches the standard consumer-IR HAL.
Its board init file only changes `/dev/irtx` permissions/ownership; the extracted
HAL has no GPIO/sysfs power path evident. The stock kernel contains a board-specific
MT6580 PWM IR driver. Comparing its actual probe/write/ioctl machine code with
Couch is the remaining software-only route to finding omitted output setup.
The generic donor driver and matching pinmux alone do not establish LED wiring.

The completed [stock-driver audit](ha100-stock-ir-audit.md) found no omitted
GPIO, pinctrl or regulator enable operation in the original IR paths. Optical
measurement remains necessary; no speculative output-pin changes are justified.

## Remaining hardware checks

- Confirm PWM0 reaches the IR LED and its idle polarity is correct.
- Measure 36/38/40 kHz carrier and approximately 33% duty.
- Confirm completion status and bounded timeout behavior without a bus hang.
- Verify display PWM, touch and wake remain stable during and after sending.
- Provision verified LG toggle/discrete codes before enabling IR power actions;
  a default preference is not a bundled, validated code database.

## Offline library and imports

The configuration server embeds a pinned, source-attributed library. Browse it
through the web UI by brand, device type and model, assign source commands to
remote functions, then save a named local codeset. Existing IR devices can be
reassigned without replacing their room or device identity. LG power setup
uses the same picker but only accepts `power`, `power-on` and `power-off`.
Importing, browsing and saving never transmit. An explicit installed-command
Test request sends one press with no retry; a successful driver write is not
proof that an appliance received the signal.

Authenticated endpoints are `GET /api/ir/catalog`, `GET /api/ir/catalog/:id`,
`POST /api/ir/import` (`format`, `name`, `text`), and
`GET/PUT /api/ir/codesets/:id` (`text` on PUT). `GET /api/ir/codesets` lists local
sets. `POST /api/ir/codesets/:id/test` takes an exact installed `command` name.
Catalog responses distinguish supported commands and unsupported reasons,
include source/license details, and report `physically_verified: false`.
`blaster_available` means `/dev/irtx` is registered; it does not mean optical
output or appliance compatibility has been tested.

Local files live beside configuration in `ir/<id>.codeset` (normally
`/opt/couch/ir`). IDs use lowercase letters, digits, hyphens and underscores,
at most 64 characters. Writes are atomic; traversal and symlink reads are
rejected. A codeset is at most 256 KiB with 256 unique command names.
Existing `<button> <protocol> <address> <command>` lines remain supported.
Raw captures use `<button> raw <carrier-hz> <comma-separated-microseconds>`:
20–60 kHz, 1–1024 positive alternating mark/space durations, at most 500 ms.
The transmitter uses 33% duty cycle; incompatible imported raw duty cycles
are rejected rather than changed silently.

Flipper `.ir` Version 1 imports use four-byte little-endian addresses/commands.
NEC, compatible NECext, RC5/RC6, SIRC12/15/20 and Samsung32 map to Couch
encoders; other parsed protocols remain visible but cannot be assigned.
Flipper NECext carries a 16-bit command: conversion requires its second byte
to be the first byte's inverse, because Couch generates that inverse itself.
See the [official Flipper format](https://github.com/flipperdevices/flipperzero-firmware/blob/dev/documentation/file_formats/InfraredFileFormats.md)
and [NEC decoder](https://github.com/flipperdevices/flipperzero-firmware/blob/dev/lib/infrared/encoder_decoder/nec/infrared_decoder_nec.c).
The combined 5,477-model catalog's source licenses and refresh procedures are
recorded in [catalog provenance](../daemon/couch-confd/assets/ir/README.md).
User imports are local data; importing does not grant redistribution rights.

The larger Flipper Devices source uses MIT, independently of the older
Flipper-IRDB CC0 boundary. Detailed commands decompress on demand; the full
database is not expanded into command tables at daemon startup. Both indexes
are deserialized into typed records that borrow from the embedded JSON rather
than kept as a `serde_json::Value` tree, and the browse reply is rendered once
on first use, so `GET /api/ir/catalog` copies that body and splices in
`blaster_available` rather than walking 5,477 codesets per request. SIRC20
imports preserve all 13 address bits (5-bit device plus 8-bit extension).


### Carrier and coexistence controller checks

On kernel `9b699dde`, two separate one-word zero transfers completed in
2136/2149 µs, and the 281 µs carrier-pattern probe completed in 2154 µs.
The 38 kHz long-zero probes completed in 69482 µs (68 ms requested) and
501744 µs (500 ms requested). Separate 68 ms zero probes completed in
69713 µs at 36 kHz and 69328 µs at 40 kHz. Each used one write with no retry;
GUI heartbeats advanced afterward and no transmit errors were logged.

The read-only GPIO dump reported GPIO8 as mode 2 (PWM_A); no mux writes were
needed. These checks verify carrier-dependent programming, bounded DMA and
repeat-transfer completion. They do not measure optical carrier frequency or
prove reception by a television. Real-device command validation remains pending.

### Next decisive optical test

The September 10 [shipping-software comparison](ha100-stock-ir-audit.md#current-shipping-software-comparison-september-10-2026)
found the same IR HAL and byte-identical relevant kernel paths in the currently
served official firmware. The current launcher still calls the standard Android
IR service with ordinary NEC timing. It provides no evidence for a new GPIO,
polarity change or alternate transport.

The earlier camera comparison needs a positive control. A functioning hybrid
Bluetooth/IR remote does not prove the tested button emitted IR. LG documents
both RF/Bluetooth and IR in its [Magic Remote diagram](https://www.lg.com/us/support/pdf/remotes/2016-Magic-Remote-MR600.pdf);
that is a reason not to assume the B4's OEM Volume button is an optical control,
not a claim that this older diagram establishes the B4's exact routing.
[Sony's camera-test instructions](https://www.sony.com/electronics/support/articles/00223964)
likewise require a working IR comparison and note that some cameras filter IR.

1. First verify a camera can actually see an emitter using a known IR-only
   remote, or a specifically verified IR button. Try the front camera if the
   rear camera is filtered. Do not interpret darkness from both remotes.
2. Once that control flashes visibly, coordinate one Couch transmission with
   the emitter at the same camera/distance/angle. A camera can distinguish
   visible emission from no detected emission; it cannot validate 38 kHz or
   decode NEC. No automatic repeat loop is needed.
3. If Couch flashes, use a demodulating IR receiver to capture the leader and
   data envelope. A photodiode/scope measurement is needed to resolve optical
   carrier frequency, duty and polarity directly. Compare with a known working
   LG IR capture before changing code bytes or waveform timing.
4. If Couch does not flash under the verified camera setup, inspect emitter
   routing, physical LED supply and transistor polarity with board evidence.
   A scope trace at PWM_A and then at the emitter distinguishes missing drive
   from a downstream hardware/output-path issue. Do not guess GPIO changes.

These are proposed coordinated checks; they were not performed during the
read-only software audit.

### Positive-control camera result (September 10)

The user subsequently confirmed that the camera **clearly sees the LG remote's
Power-button IR flash**, but sees **no flash from Couch** during three coordinated
LG Volume Up full-frame-plus-ditto attempts. This corrects the earlier inconclusive
camera comparison: the camera now has a working optical positive control.
It strongly prioritizes the output/emitter path over trying more LG command
codes, while not measuring carrier frequency or proving that arbitrarily weak
emission is absent.

Read-only GPIO inspection before and after the subsequent GUI restart still
reported GPIO8 `21001110`: mode 2 (PWM_A), input low, output latch low,
output direction, and input sensing enabled. The low static input after a
completed transfer is expected and does not show the level during transmission.
No GPIO register was changed.

### Opt-in output telemetry candidate

Kernel source `4ee1dd92` adds an `output_telemetry` boolean parameter, default
**off**. The patch is preserved as
[`ha100-irtx-output-telemetry.patch`](../kernel/patches/ha100-irtx-output-telemetry.patch).
It does not modify carrier samples, GPIO configuration, IRQ masks, DMA cleanup,
or the existing deadline/duration guards.

When explicitly enabled for a coordinated transmission, it records the DMA
address, first four words, nonzero-byte count and one-bit count. While the
channel is powered, it reads the programmed buffer address, control/duration
registers, enable/clock/IRQ state and shared 3D-LCM output configuration.
It samples GPIO8 DIN before logging: at most 256 reads with one-microsecond
spacing and a 500-microsecond wall-time cutoff, without disabling interrupts
or preemption. Scheduling can delay the observation; the measured interval is
included. This is pad-feedback evidence, not a measurement of LED current or
a calibrated carrier-frequency test.

After root review and candidate boot, verify the parameter exists and defaults
to `N`, then enable it only for the agreed measurement:

```sh
cat /sys/module/couch_irtx/parameters/output_telemetry
printf 1 > /sys/module/couch_irtx/parameters/output_telemetry
# Perform only the separately coordinated transmit/capture.
printf 0 > /sys/module/couch_irtx/parameters/output_telemetry
```

Do not replace this with direct PWM MMIO reads while clocks are gated; those
can wedge the bus. The parameter only controls observations; writing it does
not transmit. There is no runtime switch to a stock one-microsecond waveform
ABI in this candidate.

Read-only effective-DT checks showed no ordinary LED PWM0 user: LCD backlight
uses `CUST_BLS_PWM` through `disp_bls_set_backlight`. The initial interpretation
of red/button LED `data=2/4` as physical pins was wrong: stock replaces those
values with board callbacks. The subsequent
[initialization audit](ha100-stock-ir-audit.md#missing-stock-board-setup-custom-led-pinctrl)
identified omitted 3.3 V/standby setup and actual GPIO61/GPIO58 callbacks.
This remains a possible shared-power issue despite no generic PWM0 conflict.

Built on Ollie with the normal profile from a clean source tree. Existing IR
completion-guard tests passed. Local `build/couch-irtx-output.img` preserves
the previous timing candidate's DTB/ramdisk, is 7,946,240 bytes, and has SHA256
`355a004bd5491569ed435eb3d1fcaf3dccdfee4f0c9fb154a90e28c5196f34bf`.
Build success is not physical validation; flashing and the measured send are
separate coordinated steps.

Deployed this candidate on 2026-09-10 after verifying that its boot parameters,
DTB and ramdisk exactly match the running image. The p8 write passed readback
verification; the device reports `3.18.79-couch-normal-g4ee1dd92f1b1`.
USB serial and Wi-Fi SSH work, GUI heartbeats advance, and the boot-health
check cleared the recovery BCB. The recovery partition hash is unchanged.
Fresh full p8/p9 backups are retained on the device and Ollie under
`irtx-output-20260910`. Telemetry remains disabled (`N`) pending the coordinated
camera test. No transmission or optical validation has occurred on this kernel
yet; successful boot does not establish that IR output works.

The first coordinated send on this kernel completed, but the user again saw
no IR flash. PWM0 was enabled with its 26 MHz clock selected, nonzero DMA
waveform data, a matching buffer address, and `3DLCM=0`. These register values
do not establish physical emission. GPIO capture was **invalid**: all sample
counts were zero because bare pin `8` triggered the vendor wrapper's
`HARDCODE` stack dumps before the sampling loop. Those warnings consumed
roughly 51–53 ms and distorted the diagnostic repeat timing.

Correction `ce7c9ee7` supplies `8 | 0x80000000` to the four GPIO getter calls;
`mt_gpio_pin_decrypt()` strips that required API flag before accessing GPIO8.
It changes no hardware configuration. See
[`ha100-irtx-pad-api.patch`](../kernel/patches/ha100-irtx-pad-api.patch).
The corrected kernel was built on Ollie; eight build/boot-health tests and
the IR completion-guard test passed. Its image SHA256 is
`214b495b78900c840c0b5aa1eaca8451d17c0a3edf91b2e5b30496d519e74bbf`.
Do not interpret the first capture's zero counts as a pin held low.

The corrected kernel booted and passed a second coordinated LG Volume Up test
at the transport level. The user observed **neither an IR flash nor a volume
change**. GPIO8 now returned valid samples: full frame 71 high / 151 low,
35 transitions in 501 microseconds; repeat 69 high / 129 low, 31 transitions
in 500 microseconds, with zero getter errors. The full frame completed at
68,354 microseconds against 67,629 expected; repeat at 12,535 against 12,067.
No hardcoded-pin warnings occurred, and telemetry was disabled after the send.

This demonstrates toggling in GPIO8's input feedback during PWM output, not
LED current or a calibrated carrier measurement. The controller's buffer
address matched the nonzero waveform allocation and `3DLCM` remained zero.
Further work should establish the actual board route, LED supply/enable and
drive polarity, or obtain an optical positive control using the original
Android stack. Do not call the LED defective or change unrelated GPIOs based
solely on this result; the GPIO8-to-emitter board connection is still unproven.

### Confirmed device controls after board initialization fix

With kernel `ea122a39f434`, the user confirmed the LG B4 in Bryan’s Office
responds to the physical remote controls through per-device IR assignments:
NEC address `0x04`, command `0x02` for Volume Up (single and held repeats), and
command `0x08` for Power toggle (both off and on from TV standby). Volume Down
was still using webOS during its successful test, so IR Volume Down remains
unverified. Remote sleep/wake validation is separate from TV standby.

The undocked check exposed an unrelated GUI idle bug: the room-list
`light-shown` flag prevented dimming, including underneath a TV screen. The hold was
removed; the live GUI logged dimming after 30 seconds and brightness fell from
255 to 42. Side-button sleep/wake and IR before/after sleep were subsequently
confirmed by the user while undocked.
