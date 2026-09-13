# Couch installer

The integrated native installer prepares the host dependencies, enrolls the
remote over USB, and uses authenticated Wi-Fi for backups and OS transfer.
Release `v0.1.0-alpha.20260913.115` is published as a prerelease. Its
launchers passed download, checksum and safe-Cancel tests on Linux, macOS and
Windows. Complete installations have run on hardware from Linux and, with this
release's elevated USB worker, from macOS on 2026-09-13. A Windows installation
has not yet been run on hardware.

## Before starting

- Use Linux x64, an Intel or Apple Silicon Mac, or Windows x64. Linux ARM64 and
  Windows ARM are not supported by these launchers.
- Use an interactive terminal, internet access, a USB data cable and a Wi-Fi
  network visible to the remote. Linux/macOS need `curl` and `sha256sum` or
  `shasum`; Windows needs PowerShell 5.1 or later.
- The installer downloads and verifies its own ADB, Python, MediaTek adapter and
  libusb. You do not need to install Python, pip, Git or host filesystem tools.
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

On Windows, libusb can only open a device whose driver is **WinUSB**. Two
identities of the remote matter:

- the Android/Couch device (VID `0e8d`, PID `201c`), whose USB descriptor the
  installer reads when it binds the authorized ADB serial to a physical port;
- the MediaTek preloader (VID `0e8d`, PID `0003`), which exists for only a few
  seconds after the installer restarts the remote and is the device the
  download agent is delivered to.

Android ADB uses its own interface and proves nothing about either. Windows 10
and 11 usually bind the preloader to their built-in serial-port driver
(`usbser`, shown as "USB Serial Device"), or to a MediaTek VCOM driver if one
was ever installed, and libusb cannot open those. Bind WinUSB to the preloader
*before* it appears, using Zadig:

1. Download Zadig (zadig.akeo.ie) and run it as Administrator.
2. Choose **Device > Create New Device** (enable **Options > Advanced Mode** if
   it is greyed out).
3. Enter USB ID `0E8D` `0003`, a name such as "MediaTek Preloader", select
   **WinUSB** as the driver and click **Install Driver**.
4. If **Options > List All Devices** already shows a `0E8D 0003` entry, such as
   "MediaTek PreLoader USB VCOM" or "USB Serial Device", select it instead and
   choose **Replace Driver**.

Before opening any device, the installer reads the driver Windows has recorded
for the preloader under `HKLM\SYSTEM\CurrentControlSet\Enum\USB` and shows it.
If no recorded instance is bound to WinUSB, it offers to stop; continuing anyway
ends in the 120-second download-mode timeout with nothing written. The
installer never installs or replaces drivers. Configure only this remote's
interfaces. Platform fixture success is not physical Windows driver validation.

A Windows installation has not yet completed on hardware. The reliable
alternative on a Windows machine is to boot a Linux live USB (an Ubuntu live
session, no installation needed) and run the Linux command below from it; that
is the libusb path validated on hardware and needs no driver work. WSL is not a
shortcut: WSL2 USB passthrough needs usbipd-win, which replaces the device's
driver itself and re-attaches too slowly for the preloader's window.

## Release commands

These commands download the published release named below. Do not substitute an
unverified script or a different release's configuration.

Linux x64 and macOS, from an interactive terminal:

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.20260913.115/install.sh | sh
```

Windows x64, from PowerShell:

```powershell
Invoke-RestMethod 'https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.20260913.115/install.ps1' | Invoke-Expression
```

The release launcher verifies the native host, terminal and release configuration
before execution. Selecting **Cancel** at the first menu creates no installation
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

On macOS, "needs administrator rights" before the downloads means no sudo credential is cached: start through `install.sh`, or run `sudo -v` in the same terminal and start again. A libusb access error at the interface claim means the worker was not elevated after all. The host refreshes the credential in the background until the worker has started, so a long firmware download cannot let it expire.

For a local acceptance build or an already-downloaded OS archive, pass `--local-payload /absolute/path/package.tar.gz` alongside `--config installer.json` and `--native-backend`. The installer copies the regular file into its private session with bounded progress and applies the same pinned size, SHA-256, manifest, and member checks as downloaded inputs. This option does not make dependency/official owner-input preparation offline and does not weaken HTTPS downloads.
