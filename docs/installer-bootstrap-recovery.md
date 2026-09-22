# Recovering a failed native bootstrap

`couch-installer/tools/installer/recover_native_bootstrap.py` admits a narrow
recovery case: a native Couch reinstall (with or without a saved Android
enrollment) saved all nine current originals, verified its temporary boot write,
then failed before leaving the bootstrap phase. Its command-line entry point is
**offline only** and never discovers or opens a USB device.

```sh
python3 couch-installer/tools/installer/recover_native_bootstrap.py \
  --session /private/retained/native-session \
  --expected-temporary-boot-sha256 RECORDED_VERIFIED_TEMPORARY_BOOT_SHA256
```

Admission verifies the private `current-couch-snapshot.json`, the pinned official
partition profile, all original file sizes and hashes, contiguous native journal
sequence, retained-device binding (`retained_enrollment_bound`, or
`couch_device_bound` with `android_enrollment: none` on both the binding and the
snapshot, for a reinstall without a saved enrollment), OriginalsSaved snapshot
digest, and matching temporary-write admission/readback checkpoints. A later
installation phase, missing readback, changed evidence, symlinked original, or
temporary boot masquerading as its own original stops admission. The old session
is never resumed or modified. This is not Android saved-enrollment import.

The `recover(proof, reader, writer_factory, new_directory)` library entry point
is for an explicitly authorized operator wrapper that already owns an exclusive,
pinned `ExactUsbBackend` session. It performs no discovery, reconnect or restart.
The writer factory takes `(release, bundle, binding)` and must construct the
existing `ConnectedMtkWriter` on the **same connected MTK instance** as the reader.
The manifest supplied to that writer permits only `boot`.

Before writing, the helper rechecks source evidence and freshly observed chip,
canonical CID digest, capacity, exact partition map, current temporary boot hash
and the eight retained non-boot partition hashes. The writer repeats those live
checks. It restores the saved original boot once, independently reads it back,
rechecks all eight retained partitions, then publishes `verified.json` in a new
private recovery directory. A failed or ambiguous write never retries and never
publishes success. The wrapper may request a restart only after the successful
return; normal OS startup still requires physical observation.

The six synthetic recovery fixtures and existing writer protocol tests perform
no hardware operations. Read-only admission of saved real session evidence is
also distinct from live restoration. Keep the original backups and all recovery
records; do not start a fresh installer capture while a temporary installer boot
would be mistaken for the rollback baseline.
