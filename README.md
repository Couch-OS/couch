# Couch

An open-source Linux OS for the Sanytron Astrion HA100 universal remote.

Couch brings your TVs, media players, speakers and smart-home devices together
on a dedicated remote. Use its physical buttons and touchscreen for everyday
control, and configure your setup from a browser.

[Project site](https://dangerouslaser.github.io/couch/) ·
[Kernel source](https://github.com/dangerouslaser/couch-kernel)

## What it does

- Organize devices into rooms and activities, with custom buttons and scenes.
- Control devices through infrared and network integrations.
- Manage connections, button mappings and appearance through the web UI.

Couch runs directly on the HA100 hardware, replacing its Android interface with
a Slint GUI, Rust services and an Alpine Linux userland.

## Installation

The installer is in active development and end-to-end testing. Public releases
and a complete installation guide will be linked here when they are ready.

For development testing, see the [installer workflow](docs/installer-wifi-wizard.md).

## Development

See the [repository guide](AGENTS.md), [web UI guide](docs/webui.md) and
[kernel build guide](kernel/README.md). Hardware work starts with the
[partition layout and recovery guide](docs/device-recovery.md).
