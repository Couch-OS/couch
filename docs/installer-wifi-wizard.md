# Private Wi-Fi installer wizard

The native Ratatui terminal can run the guarded Wi-Fi workflow through
`tools/installer/ratatui_backend.py`. The backend owns prompts and orchestration;
the existing USB policy and private RAM-stage protocol own device access. The
public terminal's default installation gate remains unchanged. This source does
not enroll an unknown device or supply a redistributable OS package.

## Entry point and prerequisites

Build `tools/installer/tui` with `cargo build --release --locked`, then invoke:

```sh
sudo tools/installer/tui/target/release/couch-installer-tui \
  --python /path/to/python3 \
  --backend tools/installer/ratatui_backend.py \
  --config /private/package/trial.json
```

The reviewed private package must provide the exact relative paths and SHA-256
pins declared by `wifi_trial.PATHS` and `wifi_trial.PINS`, an existing trusted
same-device identity baseline, and an explicit USB bus/port chain. Do not publish
that configuration, identity, loader, board data, original images or credentials.
The Linux host needs Python dependencies from the existing installer requirements,
libusb, git, OpenSSL, debugfs, e2fsck and resize2fs. `package_private_trial.py` and
`private_curl.sh.in` assemble a checksum-pinned private Linux x86_64 launcher with
an offline Python 3.12 dependency environment; no package is included here.
Native terminal builds for other platforms do not establish that the full
private USB/filesystem workflow works on those hosts.

## User flow

The wizard offers full Android backup, YOLO (skip Android userdata backup),
explicit reinstall of an existing Couch installation, or cancellation before
USB starts. Enable USB debugging in Android and connect the remote. The ADB
helper selects only the reviewed USB topology and asks Android to reboot; the
legacy download agent requests a watchdog restart after verified bootstrap.
If the stage has not appeared, the user may keep waiting without repeating
backups, or stop and retain originals.

Once the RAM stage is available, choose from Wi-Fi networks scanned by the
remote, then enter the password yourself. The list includes security and signal
strength, with **Scan again** and **Enter network manually / hidden network**.
Use arrow keys, Page Up/Down and Home/End to navigate long lists. The native layout can be previewed without device access using `couch-installer-tui --snapshot-networks`. Older stages or
unavailable/empty scans retain manual entry. A broken USB scan response stops the
session instead of risking misaligned later commands. Only WPA2 Personal and
open networks are supported; unsupported security is identified explicitly.

SSID bytes are kept separately from safely escaped labels, including spaces and
non-UTF-8 names. Password entry preserves spaces, rejects an echoed fallback and
never enters status or log output. Directed scanning supports hidden names in
both the RAM connection and the installed profile. Network credentials and ephemeral TLS material are provisioned over
USB; authenticated TLS carries backups and OS data. The host also inserts the
selected network into the new OS image for first boot.

The host shrinks userdata to a checked filesystem image and transfers that
prefix instead of the partition's empty tail. The RAM stage verifies the written
prefix independently, expands the filesystem to the partition size and checks
it before completion. Backup/readback verification publishes measured byte
progress. This does not change partition boundaries or authorize preloader/lk
writes.

## Restore and restart provenance

`--wifi-retry-from /private/prior-run` explicitly reuses a verified completed
backup set; `--wifi-restore-from /private/prior-run` restores retained Android
originals. The bootstrap receipt binds saved originals and fresh calibration to
the same device and journal. `original_os` follows that receipt into subsequent
transactions. Couch originals cannot be presented as Android restore data.
Reinstall saves Couch originals and resets Couch settings; it does not alter
retained Android backups from earlier runs. Neither an error nor reconnect
silently retries a transaction.

## Private RAM-stage build

The probe remains read-only by default. `cargo build --release --locked
--features private-install` in `tools/installer/linux_stage/probe` explicitly
adds the USB-bound installation service. The builder's `--installer` switch must
match the binary capability marker and requires a verified filesystem-tool
closure through `--filesystem-cache`. Optional `--display` and `--wmt-properties`
inputs supply separately built ARM binaries. See the maintained RAM-stage
[build notes](../tools/installer/wifi-stage/README.md) and
[protocol](../tools/installer/linux_stage/PROTOCOL.md).

No build command accesses hardware. Physical fresh installation, reinstall,
rollback, interrupted-operation recovery and target filesystem expansion remain
separate acceptance tests; fixture tests alone do not establish them.
