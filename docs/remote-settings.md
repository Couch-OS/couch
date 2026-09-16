# Remote settings

Open **Remote settings** in the configuration web UI (`/settings`). Clock/dock
preferences and accent color live here; appearance was moved out of Areas.

- **Timezone:** choose an installed IANA region, or follow the system timezone.
  Regional rules apply daylight saving changes automatically; `tzdata` is included
  by `tools/provision-alpine.sh`.
- **Time format:** 12-hour with AM/PM, or 24-hour. Applies to the status bar and dock.
- **Show a digital clock while charging:** after the normal dim timeout, keep a dim
  clock on the display instead of switching the panel off. Enabled by default.
  A small position change each minute avoids keeping glyphs fixed in one place.

Tap or press a key to dismiss the clock. The wake action is consumed so it does
not activate the underlying room/device; holding a wake key does not send repeats.
The microphone key retains its hold-to-talk behavior. Undocking dismisses the clock
and starts the usual idle timers. Pairing, setup and recording take precedence.

The kernel battery status (`Charging` or `Full`) enables the dock behavior. It
also applies while charging over USB; this hardware interface does not identify
the physical dock separately. Charger presence without active charging is not
sufficient. Normal dim/off timeouts remain adjustable on the remote itself.

Settings are stored in `config.json` under `remote`, with backward-compatible
defaults. `PUT /api/remote` validates timezone against `GET /api/remote/timezones`.
The GUI reads updates without restarting. A worker formats the clock using local
tzdata once a minute or when settings change; timezone subprocesses never block
rendering. No system-wide timezone file or clock is changed.

Validation includes browser save/reload, timezone path traversal rejection and
an on-device mount-namespace fixture for charging, time format changes and wake.
Physical dock detection should be checked against the real charging indicator.

## The remote's settings on the web

The **Remote settings** page mirrors the remote's own Settings menu below the
clock and wake options: **Display & keys** (brightness, keypad backlight, dim
and screen-off timeouts), **Bluetooth** (one toggle that brings up the whole stack;
the row says "Starting the Bluetooth stack…" for the few seconds that takes,
then that the remote is advertising as Couch Remote, and "no kernel support"
on a boot image without `/dev/vhci` and `/dev/stpbt`; see
[Bluetooth](bluetooth-tv.md)), **SSH**, **Network** (read-only) and **Power**. The
daemon reads and writes the same file the remote does
(`/opt/couch/settings.conf`, owned by `couch-system`'s `ui_settings`), and the
remote notices a change to it within a second and applies it, so the two
never disagree for long. Clock, wake and appearance stay web-only. The
endpoints are in [the web UI guide](webui.md).

## Network on the remote

The remote's own Settings menu (hold Menu on the home screen) has a
**Network** section after Wi-Fi. It is read-only: the address and prefix
length, the gateway, up to two DNS servers, the Wi-Fi MAC, and the web UI's
address (`http://couch.local`, with the plain address beside it as the
fallback). It is read from the kernel's own tables (`/proc/net/route`, the
resolver file, sysfs) every two seconds while the panel is up; nothing is run.

## Power on the remote

The **Power** section at the end of the menu has three rows: **Power off**,
**Restart** and **Restart into recovery**. The first two act on one OK.
Recovery takes two presses within six seconds, because it leaves the remote
on a screen with no UI: recovery brings up Wi-Fi, SSH and a USB shell and
stays there until the flag is cleared, see [device recovery](device-recovery.md).
The rows ask the root system service (`Power { action }`), which answers,
waits a second, and for recovery writes the same `boot-recovery` marker init
uses into the bootloader control block before `reboot -f`.

## Updates on the remote

The Settings menu also has an **Updates** section. It shows the installed
build and the release channel, checks for updates, downloads and verifies an
offered build, and installs it with a two-press restart. It drives the same
system service the web UI's Updates page does; see
[runtime updates](runtime-updates.md).
