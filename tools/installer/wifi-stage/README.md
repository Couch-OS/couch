# Private RAM WiFi stage

This bootstrap uses WiFi for large transfers and USB for provisioning, diagnostics,
and the optional FunctionFS benchmark. The default build is a read-only probe;
an explicit `--installer` package contains the `private-install` service feature.
Neither variant is a public release. No credentials are included in the RAM image.

Build on Linux using `tools/release/prepare_wifi_ramdisk.py` with explicit
`--template`, `--kernel-manifest`, `--busybox`, `--service`, `--vendor-bundle`,
`--apk-cache`, and a new private `--output` directory. The builder verifies the
pinned kernel, official vendor provenance, cached APK hashes, ARM ELF dependency
closure, and the 16 MiB boot limit. It never accesses USB or flashes hardware.
The vendor bundle remains private; its inclusion does not grant redistribution.

The initramfs contains WMT launcher/loader with their Bionic dependencies, WMT/WLAN
firmware, Alpine WPA supplicant with its musl dependencies, static BusyBox, and
the static authenticated TLS/USB service. Cellular modem firmware and unused
library dependencies are excluded. Identical ELF aliases share symlink targets.
The original kernel and DTB are retained.

`wifi-init` discovers only the connectivity character-device majors. It does not
scan/create all device nodes, load obsolete vendor modules, replay Android
properties, mount userdata, or write boot markers. The only block device packaged
is recovery, mode 0400, for the existing read-only hash operation. Calibration
behavior currently follows the driver's missing-NVRAM defaults; this remains a
hardware validation item before any production release. The driver may generate an
ephemeral MAC: this stage does not fabricate the normal runtime's CID-derived
NVRAM record. Existing calibration partitions remain untouched, and full stock
calibration behavior is not claimed.

Before reporting ready, `wifi-init` starts one supplicant with an empty RAM-only
configuration. A capability advertised in USB status enables the host's network
picker. USB opcode 7 requests a bounded fresh scan through separate Unix control
and event sockets: clear cached BSS entries, drain old events, request SCAN, then
require scan-start and scan-complete events before enumerating individual BSSes.
The 12-second operation returns at most 64 networks and marks partial lists.
It never scans the host computer or logs raw scan output.

Provisioning atomically replaces the empty config, requests RECONFIGURE from the
same supplicant, and creates `/tmp/couch-wifi.request` only after acknowledgement.
The watcher then starts DHCP once. `scan_ssid=1` supports manually entered hidden
networks, both in RAM and in the private installed Wi-Fi profile. DHCP publishes
`/tmp/couch-wifi.ip`; `/tmp/couch-wifi.status` reports progress. The host reads the
assigned address through USB status; it must not assume a previous
DHCP lease or the remote's normal address. Network
configuration, credentials, TLS material, logs, and transfer buffers remain in
RAM. There is no setup AP, SSH dependency, or persistent network configuration.
A failed attempt requires a new bootstrap session.

For the private installer variant, `probe --features private-install` uses a
separate block backend and USB-bound TLS transaction after provisioning. It
creates only fixed allowlisted block nodes after checking device identity and
layout; the init script still mounts no device partitions. Original backups,
image writes, close/fsync and full direct readback are enforced before final
boot. The host includes the entered network in a privately customized OS image,
so that network intentionally persists in the newly installed Couch system.
See [private flow](../../../docs/installer-linux-usb-stage.md) and the
[protocol extension](../linux_stage/PROTOCOL.md).

The host checks radio readiness before requesting an SSID or password. The
interactive benchmark prompts for both. The private Wi-Fi TUI connects that
interaction to USB bootstrap and the OS installation transaction; the old
USB-only TUI remains separate.
Do not infer that a displayed Couch boot logo means the RAM probe has stopped:
older probe images had no framebuffer UI. New packages can include the isolated
progress renderer using `--display`. USB diagnostics establish liveness.

New diagnostic builds publish fixed failure codes through USB status and the
serial diagnostic log: detection/transport nodes, loader/launcher exit,
transport timeout, radio power, interface creation/up, supplicant exit, or DHCP
exit. They do not publish SSIDs, keys, or arbitrary vendor log text. A DHCP exit
does not distinguish failed association from a missing DHCP server. Older stage
images return only `failed`; they must be rebooted into a diagnostic build to
identify the failing step. USB status also reports whether credentials have
already been provisioned, so the host refuses a second provisioning attempt.

Validation still required: WMT bring-up from this independent root, association,
DHCP, pinned TLS over WiFi, USB disconnect survival, measured throughput and RAM
pressure. Local hashing and transport do not establish permission or transaction
safety for subsequent partition writes.
