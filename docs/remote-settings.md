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

External power reported by the USB/AC/wireless `online` nodes enables the dock
behavior, including while Full or charging is paused. On older kernels without
readable supply nodes, `Charging`/`Full` status is the fallback. This hardware
interface does not distinguish the physical dock from a USB cable. The caption
reports Charging, Full, or Plugged in independently from percentage availability.
Normal dim/off timeouts remain adjustable on the remote itself.

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
on a boot image without `/dev/vhci` and `/dev/stpbt`. Bluetooth is
experimental — a connected device can slow the remote's Wi-Fi badly, and both
panels carry a note saying so; see [Bluetooth](bluetooth-tv.md)), **SSH**,
**Network** (read-only) and **Power**, including the status-bar battery
percentage toggle. The daemon reads and writes the same file the remote does
(`/opt/couch/settings.conf` inside Alpine; the initramfs GUI reaches it at
`/mnt/alpine/opt/couch/settings.conf`, owned by `couch-system`'s `ui_settings`).
The remote notices a change to it within a second and applies it, so the two
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

The **Power** section at the end of the menu starts with **Battery percentage**.
Left, right or OK toggles the percentage beside the status-bar battery icon;
it is off by default and persists across GUI restarts. Missing or invalid readings
show the generic icon without a percentage; the percentage is an uncalibrated
kernel estimate (see the [battery gauge review](ha100-battery-gauge.md)). The
charging bolt follows charge status, not external power alone. The remaining rows are
**Power off**, **Restart** and **Restart into recovery**. The first two act on
one OK. Recovery takes two presses within six seconds, because it leaves the
remote on a screen with no UI: recovery brings up Wi-Fi, SSH and a USB shell
and stays there until the flag is cleared, see
[device recovery](device-recovery.md). The action rows ask the root system
service (`Power { action }`), which answers, waits a second, and for recovery
writes the same `boot-recovery` marker init uses into the bootloader control
block before `reboot -f`.

Preview examples: [percentage setting](images/battery-percentage.png),
[full dock clock](images/battery-dock-full.png), and
[unavailable percentage while plugged in](images/battery-dock-unknown.png).
These use the bundled Lucide SVG battery icons; the percentage is text beside
its icon.

## Updates on the remote

The Settings menu also has an **Updates** section. It drives the same system
service the web UI's Updates page does; see
[runtime updates](runtime-updates.md).

Its rows, in order: **Software**, the installed build in full; **Channel**,
stepped left and right; **Check for updates**, whose value says where things
stand at a glance; one step row that comes and goes with the service's state
and is what OK acts on; and **Kernel**, read-only and last, so the rows above
keep the fixed indices the D-pad cursor uses. Under them is one paragraph,
which shows the service's own line while something is running, staged or
failed, and otherwise the sentence that names the update in progress.

Couch installs in two signed parts, and the section's job is to make that
obvious rather than to offer "an update" twice:

* Software alone: **Check for updates** reads `.166 available`, the step row
  reads **Download & verify** and then **Install & restart**, which takes OK
  twice within six seconds.
* A release that also publishes a kernel: the check row reads
  `.166 · step 1 of 2` and the paragraph says the kernel ships as a second
  signed image with its own restart, offered as step 2 once this one is done.
* After that restart: the check row reads `Finish: step 2 of 2` and the step
  row reads **Finish update: kernel**, then **Install & restart** again.
* Half-finished, before any check: the check row reads `Update unfinished`,
  the **Kernel** row reads `.124 · older than software`, and the paragraph
  says the software is .165 while the kernel is still .124. Opening the
  section also sends one rate-limited automatic check, so step 2 usually
  appears without the user pressing anything. Nothing downloads or installs
  without a press either way.
* Kernel and software in step: the **Kernel** row reads `.165 · up to date`.
  A kernel from an earlier release for which no boot payload is published -
  the usual state between kernel changes - is shown as the bare version, with
  no warning attached to it.

A kernel that fails to bring the GUI up puts the remote into recovery by
itself, and the image it replaced stays on the remote at
`/opt/couch/boot/previous.img` to be written back; see
[runtime updates](runtime-updates.md#boot-image-updates).
