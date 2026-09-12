# Device system service

`daemon/couch-system` owns runtime Wi-Fi trials, credential persistence, recovery
hotspot transitions and SSH enrollment/control. It is a separate binary from
`couch-confd` and `couch-gui`, in the existing daemon workspace. Recovery can run
it without either application. It starts in the outer initramfs root and executes
Alpine helpers through the fixed `/mnt/alpine` chroot. Both roots share `/tmp`.

## Boot boundary

`stage2/stage2.sh` is the rootfs entry point. It sources `hardware-init.sh` for
MediaTek firmware/device setup and initial station association, starts the
`system.sh` supervisor, applies the normal/local/recovery setup policy, and
sources `gui-start.sh` only during normal boot. A service or GUI failure leaves
the initramfs USB recovery shell available. These files stay on the rootfs so
runtime changes do not require replacing the kernel or boot ramdisk. Versioned
application slots continue to mount vendor firmware from the immutable base at
`/mnt/alpine/opt/couch`, because vendor files are not application update inputs.

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

## Recovery portal and SSH

The portal's CGI files only execute the Rust HTTP adapter. Form decoding rejects
malformed encodings, duplicate/unexpected fields and oversized bodies. Its scan
list is cached before switching to AP mode, avoiding a disruptive live rescan.
Network joining acknowledges the request before switching radio modes, tests
and saves through the same Rust policy, and returns to the hotspot on failure
without rebooting the remote. A successful join reloads the supervised GUI.

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
