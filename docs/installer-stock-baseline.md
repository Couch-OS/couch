# Original Android baseline

An installer baseline binds one remote's storage identity, partition layout,
calibration and saved originals. Keep it outside Git with the associated restore
instructions. It is private device evidence, not part of a public installer.

See [device recovery](device-recovery.md) for the partition map and recovery
constraints, and [the installer guide](installer.md) for the supported flow.

## Fresh Android enrollment

Start with working Android, enable USB debugging and authorize the connected
computer. Enrollment must bind the selected ADB serial to its physical USB port
and canonical eMMC CID before following that same port into download mode.

The model string and MT6580 chip identifier are insufficient model evidence.
The enrollment implementation checks the pinned official boot/overlay images
and fixed partition boundaries, then captures and independently verifies the
remote's calibration and original boot, recovery and overlay images. The vendor
Device ID and Wi-Fi/Bluetooth addresses are separate recorded identity; do not
substitute Android ID or a serial number for the vendor Device ID.

Only a completed, verified enrollment journal is a usable baseline. Preserve
partial captures for diagnosis, but never silently promote them or resume a
failed write session from them. See [identity handling](installer-stock-identity.md).

## Preparing a restoration test

Use a compatible original image set from the same remote and firmware. The
pinned official OTA does not include Android userdata or recovery. Its runtime
files can provide owner-local installation inputs; that does not make its full
images interchangeable with another Android release.

Do not run the OTA updater script: it writes bootloader partitions outside the
Couch installer policy. Never write `preloader_*` or `lk`.

Before restoration, read the current layout and compare the relevant partitions
with the retained originals. Record exactly which partitions need replacement.
Retain a working recovery path until Android boot and identity have been checked.
After Android runs, capture a new baseline: Android can legitimately update
NVRAM/nvdata, so old calibration hashes are not a substitute for current identity
validation.

The inspected stock Android fstab uses F2FS for userdata. A Couch ext4 image is
not an Android userdata image. If original userdata is unavailable, preparing an
empty compatible filesystem loses that Android data and needs an explicit
restoration plan. Use the firmware's compatible formatter/checker on disposable
regular files, and check the current encryption metadata before relying on an
unencrypted image. Never repair an original backup in place.

## Consistent backups

A running, writable filesystem is not a consistent raw backup. Prefer the
installer's unmounted download/staging environment. If using recovery, verify
that the actual backing filesystem and all aliases are read-only and that no
application or setup service can write to it; a recovery screen alone does not
prove this.

For a raw transfer, require the observed partition size and matching hashes
before transfer, on the saved file, and after transfer. Keep binary data separate
from logs, use private permissions, and preserve the verified original unchanged.
Do not use lazy unmount or an indefinite filesystem freeze to bypass active
writers. See [storage and backup policy](installer-storage-policy.md).

Local test receipts, machine paths, per-device hashes and session-specific backup
exceptions belong in the ignored scratchpad or private backup directory. They do
not change the public installer policy or establish validation for a later build.
