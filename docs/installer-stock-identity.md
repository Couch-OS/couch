# Known-device stock identity recapture

`tools/installer/capture_stock_identity.py` captures a fresh private identity
baseline after stock Android has run. Android can legitimately update `nvdata`;
comparing every calibration byte to the older Couch snapshot would then reject
the same physical device. This command preserves the prior baseline and verifies
CID, storage capacity and the complete reviewed partition layout before reading
fresh identity bytes. It never writes flash. DA exit is an explicit option.

This is **not public enrollment of an unknown remote**. It requires a retained,
trusted complete baseline for this unit, its explicitly confirmed CID SHA-256,
a SHA-256-pinned reviewed private `stock-baseline` manifest, the reviewed
mtkclient checkout, and pinned loader and same-device original board-data files.
The manifest is used only for its reviewed model/layout profile; stock OS image
files need not remain present. Matching GPT/MT6580 alone does not identify HA100.

Example (all paths and hashes must be supplied from private reviewed artifacts):

```sh
python3 tools/installer/capture_stock_identity.py \
  --baseline /private/retained-baseline.json \
  --confirm-cid-sha256 "$TRUSTED_CID_SHA256" \
  --stock-manifest /private/stock/manifest.json \
  --stock-manifest-sha256 "$REVIEWED_STOCK_MANIFEST_SHA256" \
  --checkout /private/mtkclient \
  --loader /private/loader.bin --loader-sha256 "$LOADER_SHA256" \
  --preloader /private/preloader.bin --preloader-sha256 "$BOARD_DATA_SHA256" \
  --backup-dir /private/new-stock-identity \
  --bus 1 --ports 2.1 --check-only
```

`--check-only` validates files without USB. Remove it only for the explicitly
planned read-only DA capture, then reboot the connected remote into preloader.
Add `--boot-after-capture` to request DA exit only after independently verified
capture. The HA100 may otherwise fail CDC driver reattachment while still in DA
mode. The tested exit powers the unit off; hold Power to start Android afterward.
An acknowledged exit does not verify OS startup. Failed captures never request
exit; failed exit or cleanup prevents publication of the new baseline.

Physical bus/port selection remains exact. DA upload changes RAM but no flash
write API is exposed. No Couch SSH or running Linux session is needed.

All five identity partitions are copied and independently reread from the device.
Only after successful capture and USB cleanup is `baseline.json` created inside
the new backup directory. It retains the canonical trusted CID only after the
observed DA CID matches the existing fixed encoding. It records new identity
hashes, the predecessor baseline hash, and source pins. An error leaves no usable
new baseline; retain partial backups for diagnosis and retry into a new directory.

Neither vendor Device ID nor MAC is decoded or inferred. Android startup and
identity behavior are not certified by capture; those require separate physical
checks. The public installer gate is unchanged. Use the new baseline explicitly
for subsequent private installation, retaining both old and fresh originals.

## Finalize original recovery in the same session

After the operator has physically verified stock Android startup,
`tools/installer/finalize_stock_recovery.py` combines fresh identity capture with
restoration of the original Android recovery. It requires all capture source,
profile and target pins plus `--allow-private-flash --stock-boot-confirmed`,
`--recovery /private/original-recovery.img` and `--recovery-sha256 HASH`.
Use `--check-only` first. `--boot-after-capture` remains explicit and only runs
after every verification succeeds. No resume or retry mode exists.

For the currently reviewed private original recovery, the exact full-partition
size is 16,777,216 bytes and SHA-256 is
`dd925ba4b671ac9d9539b4de0c23bc4e8a03bfa13c81c6278418cdcf49b4ae8d`.
Do not use the misleadingly named `android-p9-BACKUP.img`: that is a stock boot
image, not the original recovery.

The command verifies trusted CID/layout, captures and independently rereads all
five identity partitions, syncs the new backup directory, and checks existing
boot/odmdtbo against the pinned stock manifest. It then writes a journal before
constructing a writer whose only allowed image is recovery. Recovery is fully
read back; boot, odmdtbo and fresh identity hashes must remain unchanged.
Userdata is neither read nor written. A new `baseline.json` is published only
after successful DA exit (when selected) and USB cleanup. The prior baseline is
untouched. A failure retains the journal and identity snapshots for review;
never treat a partial journal as permission to retry automatically.

## Bootstrap stock Android into the private Linux RAM stage

`tools/installer/bootstrap_linux_stage.py` implements a separate development-only
bootstrap exception: back up the current stock boot and recovery plus all five
identity partitions, then write **only boot** with the reviewed RAM-stage image.
It does not back up or modify userdata and does not alter public installation
policy. Original recovery remains available as the retained recovery path.

Inputs include the trusted old baseline/CID, pinned stock manifest, pinned raw
original recovery file, and loader/board-data inputs from finalization. Explicit
`--allow-private-flash --stock-boot-confirmed` are required. Additionally supply
`--stage-image`, `--stage-sha256`, `--stage-metadata`, and
`--stage-metadata-sha256`. The metadata must match the existing builder's private
read-only RAM-stage schema, exact filename/16MiB size/image hash and declared
read-only recovery operation. No unpublished image is automatically selected or
trusted based only on its filename. Run `--check-only` before the physical trial.

Observed boot, recovery and odmdtbo must match stock pins before capture. The
command independently reads back saved identity/boot/recovery originals and
persists their hashes in a journal before allowing the boot-only writer. It
then verifies stage boot readback and unchanged recovery, odmdtbo and calibration.
Only after clean completion does it publish `baseline.json` and
`backup-receipt.json`; the receipt binds to the completed journal's hash.

Use a new private `--backup-dir` each time; resume and automatic retries are
unsupported. Optional `--boot-after-capture` requests DA exit only after all
checks pass. On the HA100 this may power off: hold Power to start the RAM stage.
Successful readback is not proof that the stage booted or Wi-Fi works. This
command neither restores its own saved boot nor exposes a subsequent installer
write service; those are separately reviewed operations.
