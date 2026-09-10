# Private RAM WiFi stage

This bootstrap uses WiFi for large transfers and USB for provisioning, diagnostics,
and the optional FunctionFS benchmark. It is a private prototype, not an installer
release or partition writer. No credentials are included in its image.

Build on Ollie using `tools/release/prepare_wifi_ramdisk.py` with explicit
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
hardware validation item before any production release.

After USB provisioning creates `/tmp/couch-wpa_supplicant.conf` and then
`/tmp/couch-wifi.request`, the watcher starts supplicant and DHCP once. DHCP
publishes `/tmp/couch-wifi.ip`; `/tmp/couch-wifi.status` reports progress. Network
configuration, credentials, TLS material, logs, and transfer buffers remain in
RAM. There is no setup AP, SSH dependency, or persistent network configuration.
A failed attempt requires a new bootstrap session.

Validation still required: WMT bring-up from this independent root, association,
DHCP, pinned TLS over WiFi, USB disconnect survival, measured throughput and RAM
pressure. Local hashing and transport do not establish permission or transaction
safety for subsequent partition writes.
