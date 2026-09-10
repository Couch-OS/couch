# Read-only RAM Linux USB benchmark prototype

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
