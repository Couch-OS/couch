# Device system service

`daemon/couch-system` owns runtime Wi-Fi trials, credential persistence, recovery
hotspot transitions and SSH enrollment/control. It is a separate binary from
`couch-confd` and `couch-gui`, in the existing daemon workspace. Recovery can run
it without either application. It starts in the outer initramfs root and executes
Alpine helpers through the fixed `/mnt/alpine` chroot. Both roots share `/tmp`.

## Boot boundary

`stage2/stage2.sh` is the rootfs entry point. It sources `hardware-init.sh` for
MediaTek firmware/device setup, starts the `system.sh` supervisor, sources
`gui-start.sh` during normal boot, and runs station association and the
normal/local/recovery setup policy behind it. A service or GUI failure leaves
the initramfs USB recovery shell available. These files stay on the rootfs so
runtime changes do not require replacing the kernel or boot ramdisk. Versioned
application slots continue to mount vendor firmware from the immutable base at
`/mnt/alpine/opt/couch`, because vendor files are not application update inputs.

### The GUI does not wait for Wi-Fi

`hardware-init.sh` defines `radio_up()` instead of running it, so stage2 decides
when to wait for the radio. Nothing the panel needs comes from it: the display
and input nodes, the Alpine root and the system service are all ready first, and
`initramfs/boot-health.sh` deliberately does not sample network state.

| | before | after |
|---|---|---|
| `hardware-init.sh` prologue: nodes, vendor binds, wmt probe, `mdev -s` | ~1 s | ~1 s |
| WMT loader, STP mode, `wlan0`, association, DHCP | serial, 0-73 s (~19 s typical) | background |
| `system.sh` + its control socket | after the radio | ~1 s, before the GUI |
| `couch-confd`, backlight-keeper stop, hotplug floor, `couch-gui` | ~20 s | ~2 s |
| `touch /tmp/stay` (init's 15-minute dead-man) | ~20 s | ~2 s |
| setup policy (`setup-mode.sh`), `ssh-start`, second dmesg snapshot | ~20 s | with the lease |

The `mdev -s` sweep moved into the serial prologue because it is what creates
`/dev/input` on a kernel with no devtmpfs, and the GUI resolves its keypad and
touch nodes there. It used to run only as the first statement of the radio
block, so a remote whose radio is parked had no input nodes at all.

The setup decision needs the merged network count, which needs the radio, so it
lands after the GUI has drawn. `stage2.sh` writes `/tmp/couch.network-pending`
before backgrounding the radio and removes it in `network_settle`, after any
`/tmp/couch.onboarding`; the GUI treats the marker as "not decided yet" and
polls once a second instead of concluding from an absent `couch.onboarding` that
the remote has saved networks. A remote that does have them therefore never
flashes Wi-Fi onboarding while it is still associating, and a fresh one opens
**Welcome to couch.** a few seconds in rather than at its first frame.
`/tmp/couch.setup` needs no marker: the GUI already re-reads it every second, so
a hotspot the decision asks for is picked up whenever it arrives.

Marker sectors are unchanged, and `mark()` stamps each with the uptime, so
`S4 stage2 done` now reads earlier than the `S5`/`S6` radio markers above it in
`tools/markers.sh` output. The second dmesg snapshot moved from the end of
`gui-start.sh` into the background job for the same reason: taken where it was,
it would no longer contain the Wi-Fi bring-up.

Recovery is unchanged. `COUCH_NO_UI=1` calls `radio_up` inline, in its old
position, because that image exists for its connectivity and has no UI to hold
back.

`portal.sh` and `station.sh` are fixed hardware helpers owned by the service at
runtime. The MT6580 exposes its AP personality as `ap0`; it must not be replaced
with a generic request to turn `wlan0` into an AP. Vendor detection and property
ordering remain in shell because they are board-specific boot prerequisites.
`wifi-conf.sh` assembles the initial supplicant configuration; runtime saving
uses the Rust network policy. Radio diagnostics live in `tools/diag-wmt.sh`.

## Local API and ownership

The service listens on `/tmp/couch-system/control.sock`, in a root-owned 0700
directory with a 0600 socket. Linux peer credentials must identify root. Requests
are typed, length-framed JSON, bounded to 16 KiB; arbitrary commands and paths
are not accepted. At most eight connections are handled concurrently, with one
exclusive system-operation owner. The GUI keeps all blocking calls for Wi-Fi
configuration off the rendering thread.

A network session exposes scan, test, save and cancel. Credentials are passed
over the socket, not argv. Tests create a temporary supplicant network and
require matching network ID/SSID, association and DHCP before Save is allowed.
The service retains the existing rollback journal and atomic 0600 credential
writes. A failed test, cancellation, 60-second unsaved-trial expiry or client
disconnection attempts restoration of the previous network; a failed rollback
retains its journal for recovery. An interrupted service recovers the recorded
trial when the next network worker starts. Read-only RSSI/status polling remains
shared library code and does not grant the GUI mutation authority.

## Power

`Power { action: off | restart | recovery }` is answered with `Done(Ok)` and
performed a second later: `busybox sync`, then `poweroff -f` or `reboot -f`.
`recovery` first writes the 512-byte `boot-recovery` block init itself arms
into the first sector of the bootloader control block (`/dev/mmcblk0p10`), so
the next boot is the recovery image, once. It is a system operation like the
others: refused while another (an update, a network trial) holds the gate.

## Recovery portal and SSH

The portal's CGI files only execute the Rust HTTP adapter. Form decoding rejects
malformed encodings, duplicate/unexpected fields and oversized bodies. Its scan
list is cached before switching to AP mode, avoiding a disruptive live rescan.
Network joining acknowledges the request before switching radio modes, tests
and saves through the same Rust policy, and returns to the hotspot on failure
without rebooting the remote. A successful join reloads the supervised GUI.

`portal.sh` rolls back any failure that happens after it has torn the station
down: it stops hostapd, dnsmasq and the portal web server, removes
`/tmp/couch.setup` and re-runs `station.sh`, so a half-built hotspot never
leaves the remote with neither a station nor a working AP. That web server is
our own multi-call binary invoked as `busybox httpd`, so `killall` cannot match
it; `portal.sh` records its pid in `/tmp/portal-httpd.pid` and `station.sh`
stops it, which is what takes the setup CGI surface off the home LAN after a
join.

Enrollment is accepted only while recovery setup is active. The service
serializes approval requests, drains pre-request events and accepts only a new
keypad EV_KEY press. Key releases, repeats, synchronization and touch events do
not approve access. A bounded physical approval precedes any key/password write;
SSH public keys are checked with ssh-keygen. Enabling Wi-Fi alone does not enroll
anyone or enable SSH password authentication. Automatic SSH startup honors the
saved off setting and requires an enrolled key or usable root password.

## Validation

Run `cargo test --manifest-path daemon/Cargo.toml -p couch-system`, the GUI
workspace tests, and `python3 -m unittest discover -s tools/tests`. IPC fixtures
cover cancellation and disconnected clients; network policy tests cover exact
SSID handling, tested-only saving and rollback. Release inventory requires the
static ARM service, bootstrap helpers and CGI adapters together.

Physical acceptance must check normal boot, recovery without the GUI, Wi-Fi
scan/test/save/cancel, GUI/service restart during a trial, hotspot failure
recovery, and SSH approval timeout/key press. Host tests do not establish these
hardware results. The refactor is prepared for that acceptance test and has not
yet been validated on the remote.
