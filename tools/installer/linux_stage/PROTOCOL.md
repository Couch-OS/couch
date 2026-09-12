# Read-only RAM Linux USB benchmark prototype

The default binary remains the read-only probe described below. The explicit
`private-install` build adds the private installer protocol at the end of this
document; it must be packaged as `private-ram-wifi-installer`, not as a read-only
probe, and the bootstrap requires `--allow-private-wifi-install`.

This is an offline-built throughput experiment, not an installer or an approved
boot image. No command writes storage. No host helper resets USB, changes its
configuration, detaches kernel drivers, flashes, or boots the device.

`probe/` builds a Rust static ARMv7 musl binary named
`couch-installer-probe`. Run `/bin/couch-installer-probe /dev/ffs-couch` in the
private initramfs. It registers descriptors and strings, opens ep1/ep2 and creates
`/tmp/couch-probe.ready`. Init may then enable the legacy android_usb gadget.
The service continuously consumes ep0 events, waits for ENABLE, and exits on
disconnection/disable or malformed protocol. Unsupported control requests stall.

Startup ordering was checked against the actual device kernel's
`drivers/usb/gadget/function/f_fs.c`: the strings write creates endpoint files,
sets `FFS_ACTIVE`, then invokes the ready callback (lines 302–318 in the reviewed
tree). `ffs_epfile_open` (1000–1021) checks ACTIVE and exclusive ownership, then
returns; it does not wait for USB enable. The enable wait is in endpoint I/O,
which this service delays until its ep0 ENABLE event. The android gadget ready
callback only marks the function opened when configuration is disabled.
Consequently opening ep1/ep2 and creating the marker before init enables USB does
not introduce a circular wait. This is source validation, not a hardware test.

The ABI uses the packed legacy v1 FunctionFS header supported in this device's
Linux 3.18 tree (`include/uapi/linux/usb/functionfs.h`), with FS and HS descriptors,
a vendor interface and bulk OUT/IN endpoints. Actual addresses can be remapped;
the host discovers directions from descriptors. No configfs, modern DMABUF,
SuperSpeed or zero-copy support is assumed. Upstream reference:
https://raw.githubusercontent.com/torvalds/linux/v3.18/include/uapi/linux/usb/functionfs.h

## Framing

Requests have a 16-byte little-endian header: magic `CBP1`, u32 operation,
u64 argument. Responses use `CBR1`, u32 status (zero), u64 following payload size.
Malformed requests terminate the service; there is no recovery/retry protocol.

| Operation | Argument | Behavior |
| --- | --- | --- |
| 0 | 0 | Return four bytes `CBP1` |
| 1 | 1–67,108,864 | Send that many RAM bytes, all `0xa5` |
| 2 | 1–67,108,864 | Receive/check RAM pattern, then send empty acknowledgment |
| 3 | 1 | Hash fixed recovery partition, return 64 ASCII hex bytes + u64 elapsed nanoseconds |
| 4 | 1–16,384 | USB-only JSON Wi-Fi/TLS provisioning payload; empty acknowledgment |
| 5 | 0 | USB-only bounded JSON Wi-Fi status/IP |

RAM streaming uses a fixed 64 KiB buffer. Hash command accepts no filename or
partition name: it checks `/sys/class/block/mmcblk0p9/size` equals 32768 sectors,
then invokes `/bin/busybox sha256sum /dev/mmcblk0p9` without a shell. Init must
provide only the read-only recovery node and BusyBox's SHA applet. This fixed
size/path is not device identity verification; the private RAM boot process must
already bind the correct hardware. SHA timing includes BusyBox launch and local
block reads; warm page-cache effects mean repeated timings are not cold eMMC
throughput guarantees.

## Build and verify

Build on Ollie, not the Mac. With Rust and the ARM musl standard library installed:

```sh
cargo test --manifest-path probe/Cargo.toml
CC_armv7_unknown_linux_musleabihf=arm-linux-gnueabihf-gcc \
CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld \
  cargo build --manifest-path probe/Cargo.toml --release \
  --target armv7-unknown-linux-musleabihf
python3 -m unittest discover -s . -p test_benchmark.py
```

The Python host tool requires PyUSB/libusb and explicit VID/PID plus physical
USB bus/ports. Only run after a separately reviewed RAM boot:

```sh
python3 benchmark.py --vid 0xVID --pid 0xPID --bus 1 --ports 2.1 --mib 32
```

Optional `--hash-recovery` requests the fixed read-only SHA benchmark. Reports
include transfer direction, bytes, elapsed seconds and measured MiB/s. Each USB
operation has a 30-second timeout; a failure aborts without reset or retry. This
prototype does not yet benchmark flash writes, identity transport, compression,
or installation. No physical throughput has been measured by its tests.


## Wi-Fi primary transport prototype

`wifi_benchmark.py` uses USB only to identify the exact service, provision its
RAM-only network/TLS session and obtain its DHCP address. Both large RAM transfers
then use TLS over Wi-Fi. Optional recovery SHA runs locally and returns only its
hash and timing. The TLS service survives subsequent USB disconnection; USB
re-enumeration/reprovisioning requires a new RAM-stage boot.

The host prompts for SSID and password rather than accepting passwords in process
arguments. Scope is WPA2-Personal (8–63 printable ASCII passphrase) or explicitly
selected open networks; WPA3-only, enterprise authentication and captive portals
are unsupported. SSID is encoded as at most 32 bytes of hex. WPA2 PSK is derived
on the host using the standard PBKDF2 construction, so no raw network text is
interpolated into configuration. Neither SSID nor password is transmitted over
LAN by the installer protocol or logged by the helpers.

An ephemeral EC P-256 certificate, PKCS#8 key and random 32-byte token are generated
in a private temporary host directory. The key, token and network configuration
are sent only over the selected physical USB link. Rust validates all fields,
certificate/key and bounds before atomically renaming a mode-0600 tmpfs WPA
configuration and `/tmp/couch-wifi.request`. Init uses
`/tmp/couch-wpa_supplicant.conf`, updates `/tmp/couch-wifi.status` and writes
`/tmp/couch-wifi.ip` after DHCP. No credentials or TLS keys are stored on eMMC.

LAN traffic uses **TLS 1.3 only** through rustls with its ring crypto provider.
The Python client trusts only the freshly generated certificate, checks the
`couch-probe` hostname and requires ordinary certificate verification. The server
requires the USB-issued token inside TLS before accepting any command, comparing
it with the vetted subtle constant-time helper. LAN accepts only operations 0–3,
at most 16 requests per connection, with 30-second socket I/O timeouts. USB
provisioning operations cannot be invoked over LAN. Each data request remains
bounded to 64 MiB with 64 KiB buffering. This measures secure transport; it does
not authorize any installer partition write or publish a public release.

```sh
python3 wifi_benchmark.py --vid 0xVID --pid 0xPID --bus 1 --ports 2.1 \
  --mib 32 --hash-recovery
```

Rust dependencies are locked; only ring's C implementation needs the ARM cross
compiler, installed on Ollie. No AWS-LC, full Android NDK or new kernel build is
required. Tests include actual loopback rustls handshakes rejecting an untrusted
certificate and wrong session token, plus the known WPA2 derivation vector and
credential injection/length validation. Loopback results are not device Wi-Fi
throughput measurements.

Official TLS API references: [rustls configuration](https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html)
and [ring provider](https://docs.rs/rustls/latest/rustls/crypto/ring/index.html).

## Private installer extension

Only the feature-enabled binary accepts these extra CBP1 commands:

| Operation | Transport | Argument | Meaning |
| --- | --- | --- | --- |
| 6 | USB only | 1–512 bytes | JSON `plan_sha256` and random 32-byte hex `nonce`; set once per boot |
| 10 | Authenticated TLS only | 0 | Consume one install attempt and enter the transaction protocol |

Operation 6 returns the ordinary empty CBR1 acknowledgment. Operation 10 switches
directly to length-prefixed JSON: little-endian u32 length followed by 1–1,048,576
UTF-8 bytes. The host sends exact canonical plan bytes whose SHA-256 must match
the prior physical USB commitment. The plan carries full CID/GPT identity,
calibration hashes, bootstrap/original-boot hashes, selected manifest hash, and
exact image sizes/full hashes/1 MiB chunk hashes. No paths or caller-selected
block offsets are accepted as commands.

The device reports its checked binding, requests acknowledgment of the durable
original-boot backup, then streams all remaining original target and identity
partitions. Each `backup` JSON event names a fixed target and exact size; binary
chunks follow. A chunk header is three little-endian u32 fields: raw length,
wire length, and encoding (0 raw, 1 zlib). Both lengths are bounded by 1 MiB;
expanded length must equal the remaining expected chunk length, and trailing
compressed data is rejected. Compression never omits original bytes or skips
bytes within the transmitted image. Compact userdata excludes the unused partition tail. The device sends `checking_backup` and then `backup_verified` with
a new O_DIRECT full readback hash. The host checks its own saved file and fsyncs
the journal before replying `{"ack":"backup_verified","target":...,"sha256":...}`.
Original boot is acknowledged against the USB bootstrap receipt, so the
temporary stage never overwrites the saved Android original.

After `backups_complete` is durably acknowledged, the device runs the shared
transaction policy in recovery/userdata/logo/odmdtbo/boot order, omitting absent
optional targets. `writing` is journaled and acknowledged before the host sends
image chunks; `synced` follows writer fsync and close; `verified` follows full
independent O_DIRECT readback of exactly the transmitted image. These acknowledgments have the same shape with
an empty sha256. The `complete` phase does not yet certify final identity;
only the subsequent `installed` event lets the host set its complete flag.
The host then sends `{"action":"reboot"}` or `{"action":"leave"}`. Reboot is
available only after all checks; it is not proof that the normal OS started.

The host releases its USB interface after provisioning and before the long TLS
transaction. TLS read waits allow up to 900 seconds for full local eMMC hashing;
write waits stay bounded. Any error consumes the attempt, closes an open writer,
and prevents automatic reconnect/resume. Retain the host journal and originals.
Tests exercise both protocol sides against fixtures, including corrupted chunks,
wrong order, missing durable acknowledgments and failed independent readback.
Physical radio bring-up and complete stock-to-Couch acceptance remain separate.


### Compact userdata, progress and backup selection

A userdata image may be smaller than its GPT partition, with positive 4096-byte
alignment. All other images must still cover their exact partition. The host
personalizes a private regular ext4 image, checks it, shrinks it with `resize2fs -M`,
and checks both its superblock geometry and legacy feature bits before hashing.
The device writes only that prefix and independently reads/hashes exactly those
bytes. It does not zero or read back the unused tail. After `verified`, compact
userdata adds durable `expanding` and `expanded` phases. The fixed backend checks
an unmounted exclusive userdata node, runs packaged `e2fsck -fn`, grows with
`resize2fs` to the independently checked GPT capacity, and checks the resulting
superblock and filesystem. Growth changes filesystem metadata; the image hash
certifies the pre-growth contents, while post-growth filesystem checks certify
expansion. Any failure prevents the final boot write. No partition table edit,
discard or generic device-path command is exposed.

Independent backup and write readbacks emit `verify_progress` JSON with exactly
`event`, `phase` (`backup` or `write`), `target`, `done` and `total`. Counters start
at zero and advance once per read/hash chunk through the full expected byte count.
These events never replace the final checked digest/verified phase or require
ACKs. The host rejects mismatched targets, totals, regression or early completion.
Legacy pinned services without progress frames are still understood. Host file
hashes also produce measured TUI counters.

The plan-bound boolean `skip_userdata_backup` defaults to false. YOLO sets it
true and omits only Android userdata backup; all other originals/calibration and
all image verification remain mandatory. It never changes image write sizes.

An explicit `--wifi-retry-from` flow accepts an interrupted run only after complete
full original backups are rehashed and no final boot write was recorded. It binds
`reused_backups`, an exact target/calibration hash inventory, into the new USB plan.
The service checks its inventory and calibration hashes, omits retransmission of
originals, and still requires the durable `backups_complete` ACK before writing.
The host makes and verifies new sparse/reflink copies of the saved originals;
it never backs up the partially overwritten userdata as Android. This is an
operator-started new boot and transaction, not automatic in-session retry.

## Remote Wi-Fi discovery

USB status (opcode 5) advertises `"scan": true` when opcode 7, length zero, is
available. Hosts must check this capability before using it: older stages reject
unknown operations and must retain manual SSID entry. Scan is USB-only and is
unavailable once credentials have been provisioned; authenticated TLS does not
expose the operation.

The normal CBR1 response contains at most 16 KiB of JSON:
`{"status":"ok","networks":[{"ssid_hex":"6578616d706c65","security":"wpa2","dbm":-45}],"truncated":false}`.
Security values are `wpa2`, `open`, `enterprise`, `wpa3`, `wep`, and `unsupported`;
only the first two can be selected for provisioning. Results preserve SSID bytes,
deduplicate by bytes/security, and order by strongest signal. Hidden empty SSIDs
are omitted. A completed scan failure returns `status: "unavailable"` and an empty
list, allowing rescan/manual entry. Invalid, incomplete or timed-out USB frames
stop the host session; they must not be treated as a clean fallback response.

The stage uses [wpa_supplicant's control interface](https://w1.fi/wpa_supplicant/devel/ctrl_iface_page.html),
with separate command/event sockets, a 12-second absolute scan deadline, bounded
datagrams and BSS enumeration to avoid truncated SCAN_RESULTS tables. No new
external CLI is packaged: the existing supplicant provides the control socket.

### Optional stage-side network configuration

A private installer advertises `stage_network_config: true` in USB Wi-Fi status.
A future native host may bind `network: {"ssid_hex": "...", "psk_hex": "..."}`
into its TLS install plan. Omit `psk_hex` for an open network. SSIDs contain
1–32 bytes encoded as hex; derived WPA PSKs contain exactly 32 bytes encoded as
hex. The stage rejects malformed values and full-size restore images before
opening the installation disk. Existing hosts omit `network` and retain their
previous image-personalization behavior.

For compact userdata, the original image hash is independently read back first.
After expansion, the stage writes `/opt/couch/networks.conf` using packaged
`debugfs` against the guarded, unmounted partition. It checks the exact saved
bytes and regular-file mode 0600, syncs the block device and reruns read-only
`e2fsck`. Failure prevents the `expanded` acknowledgement and final boot write.
Secrets use root-only temporary RAM files, never process arguments; commands
have fixed paths and deadlines. This removes filesystem-tool dependencies from
the eventual native host path; it does not itself provide native USB orchestration.
