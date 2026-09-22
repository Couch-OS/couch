# Installer review follow-ups (PR #74, 2026-09-12)

A high-effort multi-angle review of PR #74 confirmed ten correctness findings.
The release-critical one is fixed in this PR; the rest are tracked here because
they live in code paths that the hardware-validated install-from-Android flow
does not exercise (the firmware-watch workflow, the Wi-Fi debug tooling, and the
not-yet-validated Wi-Fi stock-restore feature).

## Fixed in this PR

- **Host/Python Wi-Fi ready timeout was shorter than the stage's new wait.**
  `network::ready` capped at 120 s and `wifi_benchmark.wait_ready` at 60 s, but
  the stage's worst case grew to ~128 s (transport 20 + interface 15 + settle 3
  + 90 s socket wait). Both budgets are now 180 s and the host reports the last
  stage status on timeout instead of a generic message.

## Fixed after the first review pass

- **#10 restore rebind gate** now binds a restore by immutable hardware identity
  (chip, CID encoding, eMMC CID, capacity) and the five calibration partitions,
  and no longer requires the mutable OS partitions or the layout map to match;
  each check is named so a mismatch is diagnosable. `SavedEnrollment::rebind_mode`.
- **#6/#8 firmware-watch** add `sys.path` for the `import` under `shell: python`
  and reject a non-ZIP Drive response before derivation, so the job runs and
  fails soft instead of dying on `ModuleNotFoundError`/`BadZipFile`.
- **#7 firmware-watch auto-PR** no longer writes a half-update that fails its own
  validation; it commits a `firmware-drift-report.md` drift notice (a green,
  reviewable PR) naming the three coupled pins a maintainer must update together.
- **Debug supervisor `killall` and `$BB wait`** (deferred items 2 and 3) are
  closed: the fixed `killall` runs on both ways out of the worker loop, so a
  `supplicant-socket-timeout` no longer leaves a supplicant holding the control
  socket into the next generation, and `wait` is the shell builtin it always
  was, so the worker is reaped and `worker_exit` reports what it did instead of
  BusyBox's 127 for a missing applet.

## Fixed

- **Couch restarts ignored the recovery flag (fixed, couch-installer PR "Reinstall
  without a saved enrollment").** The Reinstall/Restore retry path restarted Couch
  seconds after it came back, while init had the recovery flag armed, so the stage
  was written and the bootloader then started recovery. Every Couch restart now
  waits for the flag to read clear.
- **`PROBE_COUCH` does not tell recovery from a normal boot.** Normal Couch mounts
  the Couch filesystem at `/mnt/alpine` too; only the boot control block says which
  system starts next. The comment and README now say so; the recovery action's
  behaviour is unchanged.

## Deferred (not on the hardware-validated install path)

1. **Restore plan key and older stages.** The host sends `restore: true` only
   for a restore, so this release's matched stage accepts every plan. A host
   from this PR driving a *pre-PR* stage image would still have that stage reject
   an unknown key; a stage capability flag in the status JSON would let the host
   gate on it. Low risk while host and stage ship together.
2. **Stage debug `lifecycle` made mandatory without a `debug_protocol` bump.** A
   still-running pre-PR debug stage now errors in `lifecycle_summary` instead of
   degrading to read-only diagnostics.
3. **`debug_late_watch` can append a prior generation's record** into the next
   generation's bounded debug log after a retry. Debug-only log hygiene.
4. **firmware-watch.yml: `import firmware_restore` needs `sys.path`.** Under
   `shell: python` the step runs from a temp dir, so the import fails; the watcher
   never detects drift.
5. **firmware-watch.yml: the auto-PR edits only `archive.*`,** which
   `firmware_restore.load()` then rejects against the official runtime pin, so the
   generated PR fails its own validation.
8. **firmware-watch.yml: the fetched OTA is not checked for ZIP magic,** so a
   Drive HTML interstitial reaches `derive()` and raises `BadZipFile`, turning the
   "fail-soft" job red.
9. **`recover_native_bootstrap.recover` chain kwargs are reachable only from
   tests and `recover_debug_chain.py` only prints an admission JSON;**
   `docs/installer-bootstrap-recovery.md` still documents the 4-arg form.
10. **Wi-Fi stock-restore rebind gate is too strict for restore.**
    `SavedEnrollment::rebind` requires live boot/recovery/odmdtbo/logo to still
    equal the enrollment's saved originals. On a device running Couch that can
    never hold for an Android enrollment, so the merged Wi-Fi restore stops at
    "live hardware differs from retained enrollment" before any write. The
    restore path must compare only stable identity (CID, capacity, partition
    layout, the five calibration partitions) and not the mutable OS partitions it
    is about to overwrite. This is why the over-Wi-Fi stock restore is documented
    as not hardware validated; the USB download-agent stock restore is unaffected
    and was exercised on hardware on 2026-09-12.

## Hardware findings, 2026-09-12 Wi-Fi stock restore run

- **Stage TLS deadline (fixed, PR #80).** The connection deadline was a single
  whole-transaction budget (max 30 min) set once at connect. The 5.6 GB stock
  userdata at ~5 MiB/s over Wi-Fi plus backups/recovery/readback exceeded it and
  the read after "synced userdata" aborted as "stage JSON header interrupted".
  `DeadlineSocket` is now an idle timeout that resets on every byte, so a slow but
  progressing transfer never aborts while a stalled one still fails.
- **Wi-Fi write throughput (open).** The over-Wi-Fi write held ~5-7 MiB/s for the
  5.6 GB userdata. It is not per-chunk fsync (single `sync_all` at close in
  `install/block.rs`) and not O_DIRECT (the write path opens the block node with
  `direct=false`; O_DIRECT is only the readback). The limiter is elsewhere -
  candidate: the host/probe chunk-size and TLS framing (`stage.rs` CHUNK is 1 MiB,
  the probe's is 64 KiB), zlib decode, or buffered eMMC write throughput. Needs
  on-hardware profiling before any write-path change; do not change it blind.
- **Stock restore identity strictness.** `stock_baseline_restore.compare_baseline`
  requires all five calibration partitions to equal the baseline, but Android and
  Couch legitimately rewrite `nvdata` (and this device's `protect1`/`protect2`
  differed from an older baseline), so a restore that is safe by CID + layout was
  rejected as "Runtime identity baseline differs: nvdata". Same over-strict pattern
  as the enrollment rebind gate (fixed for restore). Recovery workaround: build a
  baseline from the device's current enrollment. A durable fix would bind the
  destructive restore by CID + capacity + layout and treat calibration drift as a
  reviewed, explicit condition rather than a hard mismatch.
- **Remote recovery limitation.** A remote left in a stale MediaTek preloader
  (after an aborted DA/Wi-Fi session) only recovers with a physical power cycle:
  software USB reset, port de/reauthorize and an mtkclient reset do not reboot a
  bare preloader, and the lab host cannot cut USB port power. Bootstrapping the
  installer's restore/reinstall path also requires a *running* Couch or Android;
  it cannot bootstrap a device stuck in the RAM stage.
- **`od` is not a required BusyBox applet.** The recovery action and the reinstall
  flag clear read the boot control block back with `od`. The current BusyBox config
  builds it (`CONFIG_OD=y`), but `tools/busybox/required-applets.txt` does not list
  it, so a future config could drop it silently. Adding it changes the BusyBox
  recipe inputs and needs a rebuilt, re-receipted BusyBox, so it waits for the next
  BusyBox build.
