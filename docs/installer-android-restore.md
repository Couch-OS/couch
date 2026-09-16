# Restore stock Android over Wi-Fi

This describes the native installer's **Restore stock Android** mode: it returns a
Couch remote to stock Android over the authenticated Wi-Fi RAM stage instead of
the slow USB download agent. **Nothing here is hardware validated.** The pieces
below pass host and fixture tests only; a physical stock-to-Android restore, its
first boot, and re-enrollment remain separate acceptance records. Read the
[installer guide](installer.md), the [Wi-Fi stage](installer-linux-usb-stage.md),
the [stock baseline notes](installer-stock-baseline.md) and the
[wire protocol](../tools/installer/linux_stage/PROTOCOL.md) first.

## What it does

Restore is selectable at the first menu, next to **Reinstall existing Couch**. It
shares the reinstall bootstrap path:

- The device currently runs **Couch** (same USB CID-bound restart as reinstall).
- A saved **Android enrollment** is imported and re-bound to the live hardware.
  The importer already requires `original_os: Android` and structurally Android
  boot/overlay originals, so Couch backups can never be imported as Android originals.

It then backs up the current Couch partitions first (YOLO may skip only the
current Couch userdata, as it does today), and writes the Android restore set in
the fixed order **recovery → userdata → logo → odmdtbo → boot**, boot last, with
chunked pinned-hash transfer, fsync+close and O_DIRECT readback after every
target, journalling each phase. There is no automatic retry. After the verified
restore the device should boot stock Android.

## The restore image set

| Target | Source | Pinning |
| --- | --- | --- |
| `recovery` | retained Android enrollment | enrollment record SHA-256, re-hashed |
| `logo` | retained Android enrollment | enrollment record SHA-256, re-hashed |
| `odmdtbo` | retained Android enrollment | enrollment record SHA-256, re-hashed |
| `boot` | retained Android enrollment | enrollment record SHA-256, re-hashed (ANDROID! header checked) |
| `userdata` | freshly generated full F2FS image | structural F2FS + owner build receipt |

`recovery`, `logo`, `boot` and `odmdtbo` are the **device's own** saved Android
originals (the official OTA contains no Android userdata or recovery, and the
device's installed overlay can differ from the OTA by one byte — the keypad
debounce change — so the device's own overlay is what a strict stock baseline
restores). The host re-hashes each against the imported enrollment record before
any write; `tools/installer/host/src/android_restore.rs` performs this and refuses
a non-Android enrollment.

`userdata` is a **full-partition raw F2FS image** written with no expansion and no
network personalization (contrast the compact ext4 userdata a normal Couch install
grows and personalizes on the device). Because `make_f2fs` writes fresh UUID/time
fields, a stock userdata image has **no reproducible content hash**, so it is
verified two ways: structurally (exact partition size, both F2FS superblocks at
offsets 1024 and 5120, and rejection of Android-sparse or ext4 images), and by an
owner-side build receipt that pins the exact bytes, the formatter and the source
archive. The stage-side `restore` plan flag (see PROTOCOL.md) forbids compact
userdata and any network/vendor personalization for a restore.

## Firmware pin and the vendor OTA

The restore never redistributes firmware. `tools/release/ha100_firmware_restore.json`
pins the vendor Android OTA (Google Drive file id and CDN URL), its archive hash
and size, the `boot`/`odmdtbo` member hashes, and the `make_f2fs`/`fsck.f2fs`
hashes. The installer always fetches the OTA **from the vendor source at install
time** (the same `public_inputs::official` download used today, which already
points at `ha100_official_runtime.json`) and trusts only these repo-pinned hashes;
it derives `boot`, `odmdtbo` and the formatter from the verified archive.

`tools/release/firmware_restore.py` cross-checks the restore pin against the
official runtime pin and can `--check`/`--derive` the values from a supplied OTA.

### Keeping the pin current

`.github/workflows/firmware-watch.yml` runs daily (and on manual dispatch). It
downloads the Drive file without credentials — handling Drive's large-file
confirm-token interstitial and failing **soft** on a quota error rather than
spamming pull requests — hashes it, and if the hash differs from the pin it opens
a review pull request updating the pins, with the newly derived metadata in the
body. It never auto-merges and never uploads the firmware as an artifact unless a
maintainer opts in on a manual run (redistribution is not authorized by default).

**Drive fetch vs `--local-ota` — the trade-off.** The Drive confirm-token flow is
fragile: Google can change the interstitial markup and rate-limits anonymous
downloads of a 500 MB file, so a hard dependency on it at install time would make
installs flaky. The chosen design keeps the robust, already-reviewed CDN download
as the installer's fetch path (pinned by hash), records the Drive id for the
watcher, and lets the watcher tolerate Drive quota/markup failures without
blocking. An `--local-ota <zip>` path is the documented fallback for an owner who
already has the archive: it is verified against the same pin before anything is
derived, exactly like `--local-payload` for the Couch OS package. Never trust an
unpinned download from either source.

If the Drive share turns out to be the **owner's** Drive rather than the vendor's,
a Drive API service account would allow cheap change detection via file metadata
(`modifiedTime`/`md5Checksum`) without downloading 500 MB daily; the watcher could
then fetch the full file only when metadata changes. The share's ownership is
currently unknown, so the watcher downloads and hashes.

## Preparing the stock userdata image (owner side, offline)

No Android userdata backup exists for this device, so a fresh empty stock
filesystem is generated with the **firmware's own** `make_f2fs`, exactly as
recorded in the stock-baseline session notes. This runs offline, never touches a
device, and is not part of the installer:

1. Prepare owner inputs from the pinned OTA, which reconstructs the system image
   that carries `make_f2fs`/`fsck.f2fs`:
   `python3 tools/release/prepare_official_inputs.py official-fw.zip /private/owner-inputs`
   (or the native `couch-installer-host prepare-official …`).
2. Under QEMU ARM in a non-root, network-disabled, read-only-root container (see
   `scratchpad/session-notes/installer-stock-baseline-session-history-2026-09-10.md`),
   run the pinned `make_f2fs -t 0 -l userdata <regular-file> <sectors>` against a
   disposable regular file sized to the userdata partition (5 905 055 744 bytes),
   then check it with the pinned `fsck.f2fs`. Verify the formatter/checker hashes
   against `ha100_firmware_restore.json` (`firmware_restore.py` prints them).
3. Record a `couch-stock-userdata` receipt beside the image:

   ```json
   {
     "schema": 1,
     "kind": "couch-stock-userdata",
     "model": "sanytron-ha100",
     "filesystem": "f2fs",
     "size": 5905055744,
     "sha256": "<sha256 of the generated image>",
     "formatter_sha256": "31b59330aa8f705630a7ae56f0f6fb9bc3b00202c1ecfaacf9077dcf8ebfd8c0",
     "source_archive_sha256": "e9d6cd18a43c9195caac613a6168c43cf9b95758a7348931b17ae3aec0aa863a",
     "reproducible": false
   }
   ```

The installer prompts for this image and its receipt during a restore and verifies
both (`android_restore::verify_stock_userdata`) before any write. Current on-device
encryption metadata must be understood before relying on a plaintext image; see the
stock baseline notes.

## Write ordering and safety

Restore reuses the existing transaction driver and storage policy unchanged apart
from the `restore` flag: back up current Couch partitions first (calibration and
identity always; current Couch userdata unless YOLO), then write
recovery → userdata → logo → odmdtbo → **boot last**, with full O_DIRECT readback
and a durable journal checkpoint before each acknowledgement. The original boot
backed up here is the **current Couch** boot; the Android boot written last is the
saved original. Bootloaders (`preloader_*`, `lk`, `lk2`), the GPT, `para`/BCB and
the five calibration/identity partitions are never written. Any error preserves
the journal and originals; there is no automatic retry or restore.

The 5.9 GB userdata image is mostly zero. The host's backup chunks skip all-zero
runs when saving, but the restore **write** streams and the device reads back and
hashes the complete image, so no zero-run is skipped on the verified write. At
~20 MiB/s over Wi-Fi the userdata write is roughly five minutes.

## After restoring: re-enroll from Android

A restore writes Android but does **not** produce a new Couch enrollment. Android
can legitimately update its own NVRAM/nvdata, so the pre-restore calibration is not
a substitute for current identity. Before any future Couch install on this device:

1. Confirm stock Android boots, and that its screen, Wi-Fi and reported identity
   work.
2. Re-enroll from Android: enable USB debugging and run the installer's fresh
   enrollment (or `tools/installer/capture_stock_identity.py` /
   `tools/installer/enroll_android.py`) to capture a fresh post-Android baseline,
   retaining the Device ID / MAC values from Android settings.

## What is and is not validated

Host/unit tests cover: the firmware restore pin and its change detection
(`tools/release/test_firmware_restore.py`), structural F2FS checks, receipt and
originals verification, the Couch-as-Android refusal, and the restore plan rules
(full-partition images, no network/vendor) in `couch-installer-host`. The stage's
`restore` flag parse/enforcement is exercised in the Linux probe build.

Not validated here: any physical restore, first stock-Android boot, the QEMU
`make_f2fs` image on real hardware, and re-enrollment. The stage probe cannot be
built or run on the Mac. Treat every command above as reviewed offline only.
