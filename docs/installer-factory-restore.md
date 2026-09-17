# Factory firmware restore (HA100)

This is the owner-side recovery that returns a Sanytron HA100 to a clean,
bootable vendor factory state over the MediaTek download agent. Use it when the
device no longer boots a usable OS (for example after an interrupted install or
a partial stock restore) and before a from-Android install test.

It is distinct from two narrower flows:

- The **Couch install/restore** path (`couch_install`, the TUI, the Wi-Fi stage)
  only ever writes `recovery, userdata, logo, odmdtbo, boot`. It never writes
  `system`/`vendor` and assumes a working OS is already present.
- `stock_baseline_restore.py` writes only `userdata, odmdtbo, boot` to discard a
  disposable test install on top of an intact stock `system`.

Neither rewrites the actual Android OS. A device whose `system`/`vendor` is
missing or foreign (Couch occupies `system`) will not boot from either, which is
why a full factory restore exists.

## What it writes and preserves

`factory_restore.py` writes the full factory OS and boot chain, each read back
and hashed on-device after writing, in this order (boot chain last so an
interrupted flash leaves the old boot in place):

    system, vendor, secro, odmdtbo, logo, cache, userdata, recovery, lk2, lk, boot

It **never** writes the preloader — the factory preloader already on the device
is preserved, which also keeps the device re-flashable over the download agent
if a write is interrupted — and it **never** writes any calibration/identity
partition (`proinfo, nvram, nvdata, protect1, protect2`, and the other reserved
regions in `couch_install.NEVER_WRITE`). Calibration is verified against the
runtime baseline both before and after writing. `cache` is written as a fresh
empty filesystem.

`userdata` is optional and omitted by default. Writing it just copies gigabytes
of mostly-zero empty filesystem over the slow download agent; Android formats
`/data` on first boot, so a factory restore does not need to. Include it (via the
bundle builder's `--userdata-image`) only when you specifically want `/data`
pre-formatted to a known empty image.

The broad write set is unlocked only for a manifest whose `purpose` is
`factory-restore` (see `couch_install.allowed_write_targets` and
`FACTORY_WRITE_ORDER`); every other manifest keeps the narrow Couch write set.

## Inputs

1. **Pinned factory firmware.** `tools/release/ha100_factory_firmware.json` pins
   the vendor archive (`RS30_HAOS_HA100_V1.0.4_20260702`, fetched from the Drive
   source), its SP Flash `binpackage` member hashes, and the full-partition
   `restore_images` hashes.
   `tools/release/factory_firmware_pin.py --check <factory.zip>` confirms a
   local archive reproduces every pinned value. No workflow watches this pin for
   drift. No firmware bytes are stored in the repo.
2. **Full-partition bundle.** `tools/release/factory_bundle.py` turns the
   extracted `binpackage` into the installer's strict full-partition format:
   expand the Android-sparse images (`system, vendor, cache`), pad every image to
   its exact device partition size, and grow `cache` to a full empty ext4. It
   emits a `factory-restore` `release.json` with per-image sha256. `userdata` is
   included only when `--userdata-image` is given (a known-good full-size empty
   F2FS); by default it is left out.
3. **Runtime baseline + CID confirmation**, as for `stock_baseline_restore.py`.

## Running it

Run from a pinned installer checkout, against a freshly power-cycled preloader on
the selected USB bus/port (no reset is sent). `--confirm-factory-reset`
acknowledges that `/data` is wiped. Example:

    python3 factory_restore.py \
      --allow-private-flash --confirm-factory-reset \
      --manifest <factory-bundle>/release.json \
      --baseline <current-device-baseline.json> \
      --confirm-cid-sha256 <sha256 of the device CID> \
      --checkout <mtk checkout> \
      --loader <da>.bin --loader-sha256 <sha> \
      --preloader <preloader>.img --preloader-sha256 <sha> \
      --journal-dir <new journal dir outside the repo> \
      --lock-dir <usb lock dir> --bus 1 --ports 1 --boot-after-restore

A full flash writes several gigabytes at download-agent speed and takes about an
hour. The device reboots into factory Android; normal startup is not certified by
the operator and must be confirmed by watching the device boot.
