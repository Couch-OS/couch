# Couch

An open-source Linux OS for the Sanytron Astrion HA100 universal remote.

Couch brings your TVs, media players, speakers and smart-home devices together
on a dedicated remote. Use its physical buttons and touchscreen for everyday
control, and configure your setup from a browser.

[Project site](https://couch-os.dev/) ·
[Kernel source](https://github.com/Couch-OS/couch-kernel) ·
[Discord](https://discord.gg/KnZK6SZQhF)

## What it does

- Organize devices into rooms and activities, with custom buttons and scenes.
- Control devices through infrared and network integrations.
- Manage connections, button mappings and appearance through the web UI.

Couch runs directly on the HA100 hardware, replacing its Android interface with
a Slint GUI, Rust services and an Alpine Linux userland.

## Installation

Couch installs from a Linux or macOS computer over USB, with the large transfers
carried by Wi-Fi. Start Android on the HA100, enable USB debugging, connect the
remote, then run the pinned installer for the current prerelease in a terminal:

```sh
curl -fsSL https://github.com/Couch-OS/couch-installer/releases/download/installer-v0.1.0/install.sh | sh
```

Windows users run `install.ps1` from the same release in PowerShell. The script
downloads the installer binaries and release descriptor for your platform,
verifies their sizes and SHA-256 hashes against the values pinned inside the
script, and only then starts the installer. Nothing runs on a mismatch. On
Windows the installer talks to the remote through the serial ports Windows
creates by itself, so no driver setup is needed; the [installer guide](docs/installer.md)
has the details. Once the remote is on your Wi-Fi, open **http://couch.local**
from any browser on the same network to configure it.

The installer saves your remote's original Android partitions and identity
before writing anything, and it can later reinstall Couch or restore stock
Android from those saved originals. Keep that backup folder. The releases are
prereleases under active hardware validation; see the
[installer guide](docs/installer.md), the [Wi-Fi installer notes](docs/installer-wifi-wizard.md)
and [restoring stock Android](docs/installer-android-restore.md).

## Community

Questions, install reports and hardware findings are welcome on the
[Couch Discord server](https://discord.gg/KnZK6SZQhF). Bugs go to the
[issue tracker](https://github.com/Couch-OS/couch/issues).

## Development

See the [repository guide](AGENTS.md), [web UI guide](docs/webui.md) and
[kernel build guide](kernel/README.md). Hardware work starts with the
[partition layout and recovery guide](docs/device-recovery.md).

To write an independently installable integration, start with the
[integration developer guide](docs/development/index.md) and
[catalog admission requirements](docs/development/admission.md). These Markdown
pages are also the source for the website's developer documentation.
