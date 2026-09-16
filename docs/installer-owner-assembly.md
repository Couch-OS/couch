# Owner-local public installer assembly

Public build inputs are separated from the owner's Android bytes. The native host
verifies the official OTA and extracts its pinned bootstrap and 33-file runtime
inventory into its private session directory. No preloader or lk partition is a
write target; the preloader file supplies the existing DRAM initialization input.

Release builders produce these neutral artifacts without contacting a device:

- `tools/release/prepare_public_boot.py`: the pinned source-built zImage and clean
  normal/recovery gzip/newc ramdisks. No Android header, owner DTB, or stock kernel.
- `tools/release/prepare_public_ramdisk.py`: Alpine tools, source-built BusyBox,
  installer probe/display, WMT property bridge and scripts. No proprietary files,
  device identifiers, or Wi-Fi credentials.
- `tools/release/prepare_public_logo.py`: the canonical Couch480x800 BGRA
  frame (`logo.bgra`), without a logo partition header or charging images.
- `tools/release/prepare_public_userdata.py`: clean packaged Couch/Alpine staging,
  checked and compacted with the pinned offline e2fsprogs closure. It rejects the
  proprietary inventory's paths and content hashes. Filesystem growth occurs on
  the remote before the owner overlay; unused partition space is not transferred.
  Its required `--fresh-core` receipt binds the actual rootfs to the reviewed
  bootstrap and exact integration-capable core; see the
  [binding workflow](integration-release-rollout.md#bind-the-fresh-rootfs-before-image-assembly).

These builders prepare artifacts, not a published/signed release. Runtime source,
package/source closure receipts, public manifest signing and platform launcher
admission remain separate release gates. The host must admit each neutral input
against the release manifest before calling `host::assembly` and persist outputs
through its private session journal; assembly performs no device writes itself.

`assembly::owner_ramdisk` verifies all 33 extracted owner files, then inserts the
19-file WMT subset pinned in `tools/release/ha100_ram_runtime.json`. The release
builder checks this subset against its ELF dependency audit. Inserting all 33
files would exceed the 16 MiB boot partition: modem firmware and unused libraries
belong only in userdata. CPIO decoding has bounded size/entry counts, rejects
traversal, duplicates, unsupported types and trailing data, and refuses overlay
collisions or non-directory parents.

`assembly::boot_image` rechecks the exact official stock boot hash. It retains the
owner's Android header addresses and DTB, recomputes the Android SHA-1 image ID,
and rejects images over 16 MiB. A supplied public zImage selects the normal/RAM
kernel; `None` retains the stock kernel for the small recovery ramdisk. No owner
boot image is published. The OTA stock boot kernel used for this recovery image
needs physical recovery validation; previous private trials used a separately
saved stock recovery template.

`assembly::logo_image` replaces only frame zero in the independently verified
original logo backup. It validates MTK offsets, bounded zlib output and checksum,
preserves header extensions and every other compressed frame, and pads to the
original partition size. The host captures this original before binding the
composed logo image hash into the install plan. Public tar contains `logo.bgra`,
never an owner-derived `logo.img`.

## Authenticated stage overlay protocol

The optional plan field `vendor_source_sha256` is the compiled official OTA archive
SHA-256. Its presence requires compact userdata. Omission preserves the previous
private-image protocol. An unknown source fails before writes.

After `backups_complete` is durably acknowledged and before any partition write,
for each of the 33 compile-pinned files in sorted path order the stage emits:

```json
{"event":"vendor_file","path":"system/bin/linker","size":847124,"sha256":"<compiled file digest>"}
```

The host validates the exact inventory entry and sends the existing raw/zlib
12-byte-header chunk framing; raw and encoded chunks are bounded to 1 MiB, and the
last chunk may be smaller and unaligned. The stage verifies the entire file,
syncs its new private RAM file, then emits `vendor_received` with `path` and
`sha256`. Host ACK is exactly `{"ack":"vendor_received","target":"<path>","sha256":"<digest>"}`.
After all files and the directory are synced, `vendor_inputs_verified` carries the
OTA digest; its ACK uses target `none`. Inputs total less than 32 MiB and are
removed on exit. Colliding preexisting directories are never removed.

Only after the compact image's independent readback and filesystem expansion does
the stage create approved `/opt/couch/system/...` and `/opt/couch/vendor/...`
files. It verifies parent directory types, rejects existing file collisions,
sets 0755 for `bin` executables or 0644 otherwise, and checks every file's size,
SHA-256 and mode by independent filesystem readback. Filesystem checking follows;
optional saved network configuration follows the vendor overlay. Final boot stays
last. Transfer, hash, ACK, filesystem or identity failure stops the transaction.

## Validation

Host unit tests exercise CPIO bounds/traversal/duplicates and Android header
packing. A local full official-owner fixture produced byte-for-byte identical
Android output to `kernel/pack.py`. Linux stage tests cover malformed vendor
transfers, incorrect durable ACKs, owned temporary cleanup, and real regular ext4
insertion/readback/permissions/collision rejection. The ignored
`full_pinned_inventory_overlays_neutral_regular_ext4` fixture takes explicit
`COUCH_OWNER_VENDOR_FIXTURE` and `COUCH_NEUTRAL_IMAGE_FIXTURE` paths, copies and
expands a regular image, inserts all 33 verified files and runs e2fsck. It never
opens a device block node.

Physical acceptance still requires the native driver to boot the assembled RAM
stage, associate Wi-Fi, preserve originals/calibration, perform the complete
compact install, boot normal Couch, and exercise the independently assembled
recovery image. No local fixture establishes those hardware results.
