# Couch installer

The integrated native installer prepares the host dependencies, enrolls the
remote over USB, and uses authenticated Wi-Fi for backups and OS transfer.
Release `installer-v0.2.0`, published from
[Couch-OS/couch-installer](https://github.com/Couch-OS/couch-installer), is a
prerelease; it installs the OS payload its release descriptor pins. Its
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
  https://github.com/Couch-OS/couch-installer/releases/download/installer-v0.2.0/install.sh | sh
```

Windows x64, from PowerShell:

```powershell
Invoke-RestMethod 'https://github.com/Couch-OS/couch-installer/releases/download/installer-v0.2.0/install.ps1' | Invoke-Expression
```

The release launcher verifies the native host, terminal and release configuration
before execution. The terminal's heading row shows the release it is running
(`installer v0.1.0-…`), and the finish screen repeats it; check that against the
command above before reporting a problem, and include it in any report. Selecting **Cancel** at the first menu creates no installation
session and opens no device.

## Installation flow

1. Choose **Install with Android backup**, **YOLO — skip Android data backup**,
   **Reinstall existing Couch**, or **Restore stock Android**. Reinstallation uses
   your saved Android enrollment if you have it, and can otherwise start fresh
   without it (see [Reinstalling Couch](#reinstalling-couch)). Restore needs the
   saved Android enrollment; it returns a Couch remote to stock Android over
   Wi-Fi; see [Restore stock Android](installer-android-restore.md).
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

From `installer-v0.2.0` the installer writes an OS image that carries **alpha
.215**, so a fresh install starts on the current release and Settings → Updates
reports nothing newer. The image also carries the boot script that clears the
recovery flag after a rolled-back update, and the D-Bus and BlueZ packages that
Bluetooth needs, so neither the first-use package download nor the "stuck in
COUCH RECOVERY" failure of older images applies to a remote installed from it.

The OS image and the application release are still decoupled: the image changes
only when the Alpine package set, the kernel or the boot scripts change, while
application releases are cut continuously and the remote installs them itself
from **Settings → Updates**. A remote installed from an older installer (`.24`
image) updates itself to the current release the same way; see
[runtime updates](runtime-updates.md#compatibility-floor) for the floor that keeps
that working.

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

### My remote shows COUCH RECOVERY every time it starts

A runtime update that fails its health check is rolled back correctly, but on
images whose bootstrap predates the rollback fix the reboot enters recovery with
the `boot-recovery` flag still armed, and recovery keeps that flag on purpose -
so the remote returns to the COUCH RECOVERY screen on every boot until the flag
is cleared. Run the installer and choose **My remote shows COUCH RECOVERY** at
the first menu. It needs no release configuration and downloads nothing: connect
the remote by USB while that screen is showing, and the installer finds it on
its serial port, checks that it really is a Couch recovery shell on your remote,
shows which Couch version the next boot will start, and, once you confirm,
clears the flag, reads the block back to prove it is clear, and restarts the
remote. Nothing else is written: only the first 512 bytes of the `para`
partition change, the `ENV_v1` area is preserved, and no other partition is
touched. If more than one remote in recovery is connected it refuses and asks
you to disconnect the others; if the readback does not come back clear it stops
without restarting and says so. Watch the remote restart, then open
**Settings → Updates** to finish updating it. The same fix by hand, from the
recovery shell, is in the [device recovery guide](device-recovery.md#leaving-recovery-after-a-rejected-runtime-candidate).

If you are reinstalling from a Mac or Linux computer anyway, you do not need this
first: **Reinstall existing Couch** offers to clear the flag itself once the
remote has been on for about three minutes.

## Reinstalling Couch

Choose **Reinstall existing Couch** with Couch running on the remote and USB
connected. After the downloads the installer lists the saved Android enrollments
it finds on this computer.

Before it restarts the remote, the installer asks it over USB for its storage ID
and whether its next start is a normal one. It restarts it only once that is so.
Couch arms its recovery flag at every start and clears it once its screen has come
up properly, so a remote that has only just started is given until three minutes
of uptime. If the remote has been on longer and would still start COUCH RECOVERY
(it is in COUCH RECOVERY, or its screen never comes up properly), on macOS and
Linux the installer offers to clear that flag itself, exactly as **My remote shows
COUCH RECOVERY** does, and restart it straight into the installer; on Windows, use
**My remote shows COUCH RECOVERY** first. The same check runs for Restore and for
every retry.

### With the saved Android enrollment
Copy the **complete saved Android enrollment and original backups** from the
previous computer, keep the original copy, and pick it. The installer imports the
evidence into a new private session and checks it against the live remote before
writing: same storage ID (eMMC CID), capacity, calibration, layout and overlay.
Existing Couch backups are kept separate from the historical Android originals. An
enrollment saved before Android was started again no longer matches the remote's
calibration; pick the newest enrollment for the remote. The installer lists the
saved enrollments it finds newest first, each with the date its `enrollment.json`
was saved (a copied folder shows its copy date), and marks the one used last time.

### Without a saved enrollment
If none is found, or you choose **I don't have a saved enrollment**, the installer
can reinstall Couch from what is on the remote now. It explains the trade-off
first:
- **Restore stock Android is not available for this remote afterwards**, because
  the saved enrollment is the only copy of its original Android system. If the
  original enrollment folder turns up later, Restore works with it as usual.
- The remote's calibration and its current boot, recovery and logo images are
  still saved and independently verified before any write, and calibration is
  checked again on the remote before, during and after installation. The installer
  never writes calibration.
- Current Couch data is backed up or skipped, as chosen.

The remote must be running Couch. Its storage ID, read over USB before the
restart, must match the one read in download mode. Where the remote cannot be
asked over USB, you restart it with its Power button after confirming its normal
screen has been up for three minutes, and the storage ID read in download mode on
the same physical USB port becomes the binding. Before any write the installer
checks the saved boot and recovery images: both must be Couch's. An Android boot
or recovery stops the installation with nothing written (use **Install with
Android backup** for a remote that runs Android; that creates the enrollment), and
so does an image that is neither.

The session records `original_os: Couch` and `android_enrollment: none` in
`current-couch-snapshot.json`. It is never offered or accepted as a saved Android
enrollment.

New native sessions live under `~/.couch-installer` on Linux/macOS or
`%LOCALAPPDATA%\CouchInstaller` on Windows. The TUI reports the exact session path.
Credentials, calibration and original images must remain private and outside Git.
Never write `preloader_*` or `lk`.

## Implementation and validation

Installer builds now have an independent version and can select an existing
pinned OS payload. See [installer release boundaries](installer-release-boundary.md)
for the source layout, compatibility contract and release procedure. This does
not change the published installation commands above.

The [Ratatui terminal](https://github.com/Couch-OS/couch-installer/blob/dev/tools/installer/tui/README.md)
and [native Rust host](https://github.com/Couch-OS/couch-installer/blob/dev/tools/installer/host/README.md)
run the integrated flow.
Python remains only in the verified, supervised MediaTek transport bridge;
Rust owns orchestration and the Linux stage's storage writer and verifier.

The `.24` public payload passed the native host's six-file admission check, and
all three desktop launchers passed actual download-and-Cancel acceptance.
Physical startup, complete installation, Android restoration and update rollback
must be recorded separately. These host tests do not certify those device flows.

- [Owner-side official inputs](installer-public-inputs.md)
- [Restore stock Android](installer-android-restore.md)
- [Wi-Fi stage and transaction](installer-linux-usb-stage.md)
- [Wire protocol](https://github.com/Couch-OS/couch-installer/blob/dev/tools/installer/linux_stage/PROTOCOL.md)
- [Storage policy and direct readback](installer-storage-policy.md)
- [Saved enrollment](installer-saved-enrollment.md)
- [Native launcher packaging](installer-native-launchers.md)
- [Corresponding source and notices](corresponding-source.md)

Private trial artifacts and session history are kept outside published releases.

Worker startup failures report an allowlisted exception category, numeric USB error codes, and a reviewed adapter source filename/line. Exception messages, paths, locals, and device data are excluded. A failure stops the worker; the diagnostic does not authorize an automatic retry or restore.

On Linux, a newly enumerated preloader node may appear before udev applies its existing permissions. The adapter allows up to one second for access to that exact selected device, retrying only libusb access-denied errors before any handshake. Persistent access denial stops installation: check that the installer user's effective groups include the group granted by the device's udev rule. Do not run the installer as root or broaden access to unrelated USB devices.

If the installer stops with "original boot/overlay pair is not HA100 Android firmware", the saved originals did not pass the structural check (an Android boot image with a gzip cpio ramdisk carrying `init.rc`, and a MediaTek dtbo overlay). The message names the failing check. `couch-installer/tools/installer/support/originals_check.py SESSION_DIR` reads the session's journal and the first bytes of the saved originals and prints the installer release that ran, the ramdisk compression, the ramdisk root entries and the overlay magic, without sending or writing anything; attach its output to a report. Releases before .115 stopped instead with "differs from reviewed HA100 Android firmware", a firmware allowlist that no longer exists; the fix for that message is the current release command.

On Windows, the worker resolves the preloader and the installer stage to their COM ports by vendor, product and physical port chain (`serial.tools.list_ports`), accepts exactly one match, and reports the same USB-style timeouts and disconnects as the libusb path so every protocol step above the transport is unchanged. If Windows recorded WinUSB for every preloader instance, the worker claims it through libusb instead.

On macOS, "needs administrator rights" before the downloads means no sudo credential is cached: start through `install.sh`, or run `sudo -v` in the same terminal and start again. A libusb access error at the interface claim means the worker was not elevated after all. The host refreshes the credential in the background until the worker has started, so a long firmware download cannot let it expire.

For a local acceptance build or an already-downloaded OS archive, pass `--local-payload /absolute/path/package.tar.gz` alongside `--config installer.json` and `--native-backend`. The installer copies the regular file into its private session with bounded progress and applies the same pinned size, SHA-256, manifest, and member checks as downloaded inputs. This option does not make dependency/official owner-input preparation offline and does not weaken HTTPS downloads.
