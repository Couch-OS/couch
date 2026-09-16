# Couch installer

The integrated native installer prepares the host dependencies, enrolls the
remote over USB, and uses authenticated Wi-Fi for backups and OS transfer.
Release `v0.1.0-alpha.20260913.148` is published as a prerelease. Its
launchers passed download, checksum and safe-Cancel tests on Linux, macOS and
Windows. Complete installations have run on hardware from Linux, from macOS
with the elevated USB worker (2026-09-13), and from Windows with the serial-port
route described below, reported working by a tester on the .128.dev test build
on 2026-09-13.

## Before starting

- Use Linux x64, an Intel or Apple Silicon Mac, or Windows x64. Linux ARM64 and
  Windows ARM are not supported by these launchers.
- Use an interactive terminal, internet access, a USB data cable and a Wi-Fi
  network visible to the remote. Linux/macOS need `curl` and `sha256sum` or
  `shasum`; Windows needs PowerShell 5.1 or later.
- The installer downloads and verifies its own ADB, Python, MediaTek adapter and
  libusb. You do not need to install Python, pip, Git, host filesystem tools or,
  on Windows, any USB driver.
- Allow enough local disk space for downloads and the selected backups. The
  installer checks backup space before writing. Keep the complete saved session
  directory after installation, preferably with another copy on separate storage.
- For a fresh Android installation, enable **USB debugging**, connect the remote
  and accept Android's authorization prompt. Retain the Device ID and MAC values
  from Android settings; the installer may ask for values it cannot read.

### USB permissions and Windows drivers

The user running the installer needs access to the remote's USB and serial
interfaces. On Linux, prepare the appropriate device permissions for that user;
working ADB alone does not establish access to the download interface. Close
other tools that may own the remote before starting.

On macOS, the built-in CDC ACM driver binds the remote's MediaTek download
interface, and only root can release it to libusb. The launcher therefore asks
for an administrator password once, before the terminal UI starts, and the
native host runs only the MediaTek USB worker elevated; the host, the terminal
and everything under `~/.couch-installer` stay under your account. When starting
the terminal UI by hand, run `sudo -v` in the same terminal first. The
unprivileged serial-port transport (`/dev/cu.usbmodem*`) reads every partition
correctly but could not complete a download-agent write on hardware, so it is
kept only for read-only attachment.

The installer stops any running ADB server immediately before it binds the
authorized ADB serial to a physical USB port, because a server holding the
device makes Windows refuse the descriptor read. Later steps restart the
server on demand; the device's USB debugging authorization is unaffected.

On Windows the installer needs no driver work: it talks to the remote through
the serial ports Windows creates by itself. libusb on Windows can only open a
device bound to WinUSB, and nothing binds WinUSB by accident, so the two USB
identities the installer must open take the serial route instead:

- the MediaTek preloader (VID `0e8d`, PID `0003`), which exists for only a few
  seconds after the installer restarts the remote, is a CDC ACM device. Windows
  10 and 11 bind their built-in `usbser` driver to it ("USB Serial Device
  (COMx)"), or a MediaTek VCOM driver over the same class if one was ever
  installed, and the worker delivers the download agent over that COM port, the
  way SP Flash Tool always has on Windows;
- the RAM installer stage that boots after the bootstrap write (VID `0e8d`,
  PID `201c`) carries a CDC ACM function beside its vendor interface. Windows
  binds `usbser` to it as well, and the stage answers its Wi-Fi provisioning
  protocol on that port. The vendor interface stays unbound on Windows, which
  is harmless.

The first time the preloader ever appears on a machine, Windows usually spends
the whole download window installing that driver. The installer then restarts
the remote through ADB and tries again, up to three times; the second
appearance is instant. Startup is read-only, so a missed window writes nothing.
A remote that does not return to Android within 90 seconds is not restarted
automatically: hold the side Power button until it turns off, start it again
and run the installer again.

Before opening any device, the installer reads the driver Windows has recorded
for the preloader under `HKLM\SYSTEM\CurrentControlSet\Enum\USB`. A serial
port driver, WinUSB (or libusbK or libusb0, which libusb opens directly) or no
record at all are all fine; only an instance bound to some third driver is
shown, with the Device Manager steps to remove it. The installer never installs
or replaces drivers. Windows ADB drivers are a separate matter: `adb devices`
must list the remote before the installer can start, as on any platform.

If a Windows machine still cannot reach download mode, booting a Linux live USB
(an Ubuntu live session, no installation needed) and running the Linux command
below from it is an alternative. WSL is not a shortcut: WSL2 USB passthrough
needs usbipd-win, which replaces the device's driver itself and re-attaches too
slowly for the preloader's window.

## Release commands

These commands download the published release named below. Do not substitute an
unverified script or a different release's configuration.

Linux x64 and macOS, from an interactive terminal:

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.20260913.148/install.sh | sh
```

Windows x64, from PowerShell:

```powershell
Invoke-RestMethod 'https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.20260913.148/install.ps1' | Invoke-Expression
```

The release launcher verifies the native host, terminal and release configuration
before execution. The terminal's heading row shows the release it is running
(`installer v0.1.0-…`), and the finish screen repeats it; check that against the
command above before reporting a problem, and include it in any report. Selecting **Cancel** at the first menu creates no installation
session and opens no device.

## Installation flow

1. Choose **Install with Android backup**, **YOLO — skip Android data backup**,
   **Reinstall existing Couch**, or **Restore stock Android**. Reinstallation and
   restore both need the saved Android enrollment described below. Restore returns
   a Couch remote to stock Android over Wi-Fi; see
   [Restore stock Android](installer-android-restore.md).
2. Follow the dependency and input preparation prompts. The installer downloads
   pinned official inputs and assembles vendor-dependent pieces locally; the
   public OS payload contains no device backups or owner firmware.
3. Select the remote. The installer binds its physical USB connection and storage
   identity, verifies the supported partition layout, and saves and independently
   verifies the required originals before temporary boot installation.
4. Choose a Wi-Fi network scanned by the remote, or enter an SSID manually,
   including a hidden network. Enter the credentials yourself. Unsupported
   networks remain identified in the list; unavailable scans offer manual entry.
5. Keep USB connected through startup, backups, installation and readback.
   Progress distinguishes transfer from verification. Follow the on-screen
   restart instructions; if the screen stays off, hold the side Power button
   until it turns on, then release it.
6. After verification, choose the final restart and check the Couch welcome
   screen and network connection. Wi-Fi configuration carries into the installed
   OS, including hidden networks.

The default preserves Android userdata. **YOLO** skips only that large backup;
boot/recovery originals, calibration preservation and write verification remain
required. Without a userdata backup, previous Android apps and data cannot be
restored from this session. Reinstallation separately offers backup or YOLO for
current Couch data and preserves imported Android originals in either case.

Installation writes a compact filesystem image and grows it on the remote; it
does not transfer a full partition of unused zeros. An interrupted operation
retains its originals and journal. Do not automatically retry an ambiguous write.
Follow the saved restore instructions and [device recovery guide](device-recovery.md).

## What version a fresh install runs

The installer writes a **full OS image**, and that image carries its own bundled
Couch software - currently the **.24** release, no matter which release the
install command came from. (The exact tag is in
[runtime updates](runtime-updates.md#compatibility-floor); it is deliberately
not spelled out here, because every dated tag in this file is an install command
a release bump rewrites.) So immediately after installing, the web UI and
**Settings → Updates** report .24.

That is expected. The version on the remote is the version of the software in
the image that was written; it is not the version of the installer that wrote
it. The two are decoupled on purpose: the OS image changes only when the Alpine
package set or the stable boot scripts change, while application releases are
cut continuously and the remote installs them itself.

**Finish the install by updating.** Open the paired web UI, or hold Menu on the
home screen for **Settings → Updates**, set the channel to **Alpha**, **Check
for updates**, then **Download & verify** and **Install & restart**. The remote
comes back on the current release, and only then does its reported version match
the releases page.

### Known task: rebuild the installer OS image

A fresh install should start current rather than weeks behind. The same stale
image is also what pins the [compatibility
floor](runtime-updates.md#compatibility-floor): .24's updater is the oldest in
the field, so no release may contain a file name it does not know. Rebuilding
the OS image with a current runtime fixes both. Not done yet - it is a full OS
build (`tools/release/prepare_rootfs.py` and the package closure), not a release
cut, and it needs its own physical install acceptance.

## Troubleshooting

### The web UI reports an older version than the installer I used

Expected; see [what version a fresh install runs](#what-version-a-fresh-install-runs).
The remote boots the software bundled in the OS image (.24) and updates itself
from **Settings → Updates**. Check for updates there; nothing is wrong.

### Checking for updates fails, or says the build cannot be installed

An update is checked and refused on its signed manifest, before anything is
downloaded, and a check only ever offers the single newest release on the
channel - it does not fall back to an older one. So a newest release the remote
cannot accept leaves it stuck rather than one version behind.

That happened: the `.160.dev`, `.163.dev` and `.164.dev` prereleases were
published carrying `couch-bt-bridge`, `couch-bt-hid` and `couch-bluetoothd` in
the runtime bundle. The .24 updater's file list has no entry for those names, so
it refuses the whole bundle; and because it predates the Dev channel it reads a
`.dev` tag as an ordinary alpha, so an Alpha remote is offered `.164.dev` and
nothing else. .148 itself is perfectly installable by a .24 remote - it is only
unreachable.

The bundle side is fixed: the Bluetooth binaries travel in the boot ramdisk
again and the release tooling now refuses to sign a bundle the oldest deployed
updater would reject ([compatibility
floor](runtime-updates.md#compatibility-floor)). The releases already published
are the remaining half: until those three `.dev` prereleases are withdrawn, or a
newer promoted release is cut above them, an affected remote is still offered
`.164.dev`. Retry **Check for updates** after the next release.

## Reinstalling Couch on a new computer

Copy the **complete saved Android enrollment and original backups** from the
previous computer, retain that original copy, and select **Reinstall existing
Couch**. The installer imports the evidence into a new private session and checks
it against the live remote before writing. Existing Couch backups are kept
separate from the historical Android originals.

A running Couch screen, its configuration, or its current MAC address cannot
replace the saved Android enrollment. Without that evidence, the current
installer cannot admit a Couch reinstall on another computer. It does not treat
Couch's current partitions as original Android backups. See
[saved-enrollment requirements](installer-saved-enrollment.md), including support
for verified older Python trial records.

New native sessions live under `~/.couch-installer` on Linux/macOS or
`%LOCALAPPDATA%\CouchInstaller` on Windows. The TUI reports the exact session path.
Credentials, calibration and original images must remain private and outside Git.
Never write `preloader_*` or `lk`.

## Implementation and validation

The [Ratatui terminal](../tools/installer/tui/README.md) and
[native Rust host](../tools/installer/host/README.md) run the integrated flow.
Python remains only in the verified, supervised MediaTek transport bridge;
Rust owns orchestration and the Linux stage's storage writer and verifier.

The `.24` public payload passed the native host's six-file admission check, and
all three desktop launchers passed actual download-and-Cancel acceptance.
Physical startup, complete installation, Android restoration and update rollback
must be recorded separately. These host tests do not certify those device flows.

- [Owner-side official inputs](installer-public-inputs.md)
- [Restore stock Android](installer-android-restore.md)
- [Wi-Fi stage and transaction](installer-linux-usb-stage.md)
- [Wire protocol](../tools/installer/linux_stage/PROTOCOL.md)
- [Storage policy and direct readback](installer-storage-policy.md)
- [Saved enrollment](installer-saved-enrollment.md)
- [Native launcher packaging](installer-native-launchers.md)
- [Corresponding source and notices](corresponding-source.md)

Private trial artifacts and session history are kept outside published releases.

Worker startup failures report an allowlisted exception category, numeric USB error codes, and a reviewed adapter source filename/line. Exception messages, paths, locals, and device data are excluded. A failure stops the worker; the diagnostic does not authorize an automatic retry or restore.

On Linux, a newly enumerated preloader node may appear before udev applies its existing permissions. The adapter allows up to one second for access to that exact selected device, retrying only libusb access-denied errors before any handshake. Persistent access denial stops installation: check that the installer user's effective groups include the group granted by the device's udev rule. Do not run the installer as root or broaden access to unrelated USB devices.

If the installer stops with "original boot/overlay pair is not HA100 Android firmware", the saved originals did not pass the structural check (an Android boot image with a gzip cpio ramdisk carrying `init.rc`, and a MediaTek dtbo overlay). The message names the failing check. `tools/installer/support/originals_check.py SESSION_DIR` reads the session's journal and the first bytes of the saved originals and prints the installer release that ran, the ramdisk compression, the ramdisk root entries and the overlay magic, without sending or writing anything; attach its output to a report. Releases before .115 stopped instead with "differs from reviewed HA100 Android firmware", a firmware allowlist that no longer exists; the fix for that message is the current release command.

On Windows, the worker resolves the preloader and the installer stage to their COM ports by vendor, product and physical port chain (`serial.tools.list_ports`), accepts exactly one match, and reports the same USB-style timeouts and disconnects as the libusb path so every protocol step above the transport is unchanged. If Windows recorded WinUSB for every preloader instance, the worker claims it through libusb instead.

On macOS, "needs administrator rights" before the downloads means no sudo credential is cached: start through `install.sh`, or run `sudo -v` in the same terminal and start again. A libusb access error at the interface claim means the worker was not elevated after all. The host refreshes the credential in the background until the worker has started, so a long firmware download cannot let it expire.

For a local acceptance build or an already-downloaded OS archive, pass `--local-payload /absolute/path/package.tar.gz` alongside `--config installer.json` and `--native-backend`. The installer copies the regular file into its private session with bounded progress and applies the same pinned size, SHA-256, manifest, and member checks as downloaded inputs. This option does not make dependency/official owner-input preparation offline and does not weaken HTTPS downloads.
