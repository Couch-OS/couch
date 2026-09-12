# Wi-Fi installer stage

The private Wi-Fi installer uses a Linux image that runs from RAM. USB is used
for selecting the remote and starting this image; authenticated Wi-Fi carries
large backups and OS images. The native host migration and public desktop
package are still being completed. The older FunctionFS USB benchmark is
separate research, documented in [the RAM probe guide](installer-ram-probe.md).

## Bootstrap

The host verifies the reviewed MediaTek adapter and download agent, selected
physical port, eMMC CID, both GPT copies and supported partition boundaries.
It independently saves identity/calibration, original boot and recovery before
writing the temporary installer boot image. It never writes a bootloader.
The legacy DA cannot currently be assumed to boot the Linux image directly
from RAM; the temporary boot-slot write is part of this explicit bootstrap.

The MT6580 watchdog startup exchange replaces the legacy FINISH/HOME_SCREEN
exchange that powered off this development unit. Matching ADB identity permits
an Android reboot into capture. If startup remains off, the terminal explains
how to hold and release Power. A protocol acknowledgement alone does not prove
that the installer stage or normal OS has booted.

## Network and transaction

USB provisioning binds the session certificate, token and plan to the selected
remote. TLS 1.3 trusts only that provisioned certificate and checks its exact
bytes before transmitting the token. There is no unauthenticated discovery or
fallback to another device. The host can scan networks or accept a manually
entered SSID; passwords are masked and excluded from logs.

The stage checks CID, GPT, partition mappings and retained calibration before
storage access. It rejects mounted partitions, swap and holders. Write targets
are a fixed allowlist; the protocol accepts no arbitrary shell commands or
block offsets. See [the protocol](../tools/installer/linux_stage/PROTOCOL.md).

All selected original backups complete before final OS writes. Original boot
comes from the verified bootstrap capture, not the temporary installer image.
Backups are independently read again on the device and verified from the saved
host file. The host durably journals each completed phase before acknowledging
it. YOLO mode omits only Android userdata backup.

Image transfer uses bounded 1 MiB chunks and pinned image/chunk hashes. Device
verification flushes and closes the writer, then uses a new aligned direct-I/O
read to avoid merely hashing cached input. Verification progress is distinct
from transfer progress. A failed check stops the transaction and retains its
journal and originals; there is no automatic retry of an ambiguous write.

## Compact filesystem and saved Wi-Fi

The installed userdata image is a compact ext4 filesystem. Only its actual image
prefix is written and independently verified. The stage then expands the
filesystem to the validated partition size and checks it. This avoids writing
an unused zero tail; it is not a secure erase of old free space.

With the stage network capability enabled, the validated SSID/PSK is applied
to `/opt/couch/networks.conf` after base-image verification and filesystem growth.
The stage verifies the exact file contents and mode 0600, flushes storage and
checks the filesystem again. Hidden networks use `scan_ssid=1`. Failure prevents
the final boot write. Private restore images reject this customization.

Final boot is written last. First normal boot, network association and recovery
remain separate hardware acceptance checks.

## Recovery and validation

Before userdata changes, restoring the verified original boot can recover the
previous OS if the other originals remain unchanged. After userdata changes,
restore all affected verified originals, including the complete userdata backup
when available, with original boot last. Calibration is compared, never written.
Use the saved run's restore instructions and [device recovery guide](device-recovery.md).

The stage has Linux storage, interruption, malformed-frame, ext4-personalization
and readback fixtures. Native host framing, TLS pinning and journal fixtures run
on Linux, macOS and Windows. These checks do not establish physical Windows
USB-driver support, fresh Android enrollment or power-loss recoverability.
The full release requires a recorded end-to-end run of its exact packaged bytes.
