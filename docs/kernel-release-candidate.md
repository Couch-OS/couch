# Kernel candidate and remaining hardware checks

`kernel/release-pin.json` pins source commit `81d180fc19ec` and the exact
normal-profile zImage, effective configuration, compiler and container hashes
used in the September 16 `.167.dev` battery reporting test. It contains no
device identity or private firmware. This selects the tested kernel for release
staging; it does not make an installer image production-ready.

Normal builds enforce the source ancestry in `kernel/source_policy.py`.
Later descendants may be developed, but release verification still requires
the exact pinned candidate and hashes until separately reviewed and validated.

Verify an existing local candidate before staging:

```sh
python3 tools/release/kernel_provenance.py \
  --boot build/couch-board-init-fixed.img \
  --kernel-manifest build/board-init-manifest.json
python3 -m unittest discover -s tools/release -p 'test_kernel_provenance.py'
```

The verifier checks the clean normal build manifest and extracts the zImage from
the actual Android boot payload. Runtime inventory also refuses a normal boot
candidate whose kernel differs from the pin. Recovery retains its independent
stock-kernel validation path. Both reports remain `installable: false`; neither
changes USB write gates, release signatures, private/vendor input requirements,
or boot/recovery tests. A matching kernel does not attest a new ramdisk or DTB.

## Validation scope

The `.167.dev` candidate booted through the existing OTA activation path with
the new kernel, healthy GUI/system service and advancing cached battery gauge
samples. Its four Bluetooth backport modules match the new kernel's vermagic.
See [the battery audit](ha100-battery-gauge.md) for the reporting checks and
remaining calibration questions.

Confirmed: LG B4 volume up with one repeat while awake and USB docked; button
backlight changes affect GPIO58 without changing touch-reset GPIO4 or board
GPIO17/14/61; advancing GUI heartbeat, recovery BCB clearance and USB serial.

Owner acceptance on `.167.dev` confirmed undocked display standby/wake with
keys, IR before and after wake, Bluetooth with the existing bond after reboot,
and dock/undock status. These checks establish the OTA kernel/boot update
behavior; they do not claim quantitative standby savings or battery
calibration.

Clean installer payload boot/recovery validation remains a separate full
installer gate. It does not limit this OTA alpha's eligibility. Preserve the
working boot image and separate recovery before any further kernel experiments.

Output telemetry remains compiled into this exact tested binary but defaults
off. Leave `couch_irtx.output_telemetry` disabled for normal use. Removing compiled
diagnostics changes the binary and requires another validation round; retain
that as a later optimization, not an untested replacement for this candidate.

## Remaining bring-up assumptions

1. **Charging semantics need a focused audit.** Stock `red` is a GPIO61 charging
   control callback, not a literal GPIO2 LED. The stock launcher writes it on
   charge-level changes; Couch currently has no equivalent userspace writer.
   Verify charging/full-charge behavior against the kernel charging controller
   before adding a replacement policy; do not infer polarity from the label.
2. **Board header provenance remains partial.** The original bring-up seeded
   some HA100 headers from the donor board. Audit active users against effective
   DT and stock code before enabling additional peripherals; unused donor
   definitions are not verified hardware mappings.
3. **Panel identification retains a GPIO4 maker-ID fallback.** Its current path
   reads the pin; it does not drive touch reset. The selected panel works, so
   this is a lower-priority cold-probe/variant audit, not evidence for changing
   the live reset sequence.
4. **Keep numeric DT callback selectors separate from pins.** HA100 LED dispatch
   now validates both name and selector, and has no raw-GPIO fallback. The LCM
   and display-PWM callbacks are assigned internally, unlike untrusted numeric
   LED data. Do not generalize the original fallback to other integrations.
