# Offline Rust installer storage policy

`tools/installer/linux_stage/storage` is an independent library workspace. It is
**not linked into the RAM probe** and contains **no production block writer**.
Its tests use temporary regular files on Linux; nothing here authorizes flashing.

## API and transaction phases

`Plan::new` validates expected CID, capacity, the complete nonoverlapping partition
layout and exact image sizes. Targets are an enum: recovery, userdata, logo,
odmdtbo and boot, in that order. Neither a network-supplied device path nor a
write offset is accepted. Each image has a full SHA-256 and a pinned inventory
of one-MiB chunk hashes; the last chunk may be smaller. The incoming chunk hash
must match before its bytes are written. Image sizes require 4096-byte alignment
for the current direct-read implementation.

The caller supplies an exact framed `Read` stream, a `Storage` backend and a
`Journal`. Stream framing must expose EOF at the declared image boundary and
enforce transport deadlines; a persistent raw TCP socket is not an image stream.

For each target:

1. Compare observed CID/full layout and reject mounted or held targets.
2. Ask the computer to durably journal and acknowledge `Writing(target)`.
3. Open the target and append only verified sequential chunks.
4. Check incoming full hash and exact stream length; fsync and close the writer.
5. Acknowledge `Synced(target)`, recheck device/mount state, then reopen read-only
   with `O_DIRECT` and hash the entire target using an aligned buffer.
6. Acknowledge `Verified(target)` only when that independent hash matches.

`Complete` follows all targets and a final observation check. Every error aborts
the backend and poisons this transaction object; there is no automatic retry or
resume. Host journaling errors prevent progress. The computer must bind every
phase to its reviewed plan/device/session and retain the durable journal; device
RAM state alone cannot provide crash recovery.

## Integration gates

The `Storage` trait is a contract, not independent evidence of identity. A real
backend must itself read CID and validate both GPT copies, bind opened block
descriptors to the expected major/minor and size, inspect mount namespaces,
aliases/holders and prevent competing mounts/writers. It must preserve and
independently check calibration before/after installation. Those observations
must not be assertions copied from an untrusted network request.

The Linux direct-reader verifies `O_RDONLY | O_DIRECT`, uses 4096-byte-aligned
memory and fails closed on unsupported direct I/O or short reads. It never
falls back to cached reads. The writer must be closed before the separate
read-only descriptor is opened. `O_DIRECT` bypasses the Linux page cache; it does
not prove power-loss durability inside the eMMC controller. Sync, subsequent
boot checks and a physical recovery strategy remain separate requirements.

## Validation

Run `cargo test --locked` in the storage workspace on Linux with an ext4
scratch filesystem. The tests exercise aligned `O_DIRECT` reads after close,
post-close corruption detection, rejection of buffered descriptors, mounted/CID
mismatch, bad chunks, truncated/extra streams, journal failure and refusal to
retry. Non-Linux hosts can compile the policy crate, but its Linux direct-I/O
tests are gated out; a zero-test host run does not validate readback behavior.
HA100 kernel/eMMC direct I/O and real block integration require physical testing.
