# The settings menu

`ui/couch-gui/ui/components/settings.slint`, opened by holding the menu key
(KEY_MENU, code 139) on the home screen. Tiered: a root list of sections, and
a panel per section. It owns the D-pad while up, the way the keyboard does -
its own FocusScope, focus on show, focus back to the shell on close.

## Navigation

- Up/down move the cursor, wrapping.
- OK enters a section from the root, or fires a row's action.
- Left/right step a value where a row has one (the Display rows).
- BACK climbs one tier, and closes from the root.

Opening, entering a section and going back all slide, the same memcpy page
slide the hub and chooser use (`Panel::slide`): the host owns the `panel`
index and performs each change as a snapshot, an instant state change, a B
render and a compose, so the menu is never rendered twice a frame. Because the
menu carries its own FocusScope, closing it - by the slide, or when Change
network hands off to the keyboard - refocuses the shell, or the hub would stop
answering the D-pad.

## Sections

**Display** — three stepper rows, all applied live and saved at once:
- Brightness, 10-100% in tens. `system::brightness_level` maps percent to an
  8-bit backlight with a floor (10% -> 30, so the screen never goes dark), and
  the standby dim level follows at a sixth of it.
- Dim after: 15s / 30s / 1m / 2m / 5m.
- Screen off after: 30s / 1m / 2m / 5m / 10m / Never (Never only dims).

**Wi-Fi** — shows the connected SSID and signal, and a "Change network" row.
Change network takes the SSID on the keyboard, then the passphrase (masked,
blank for an open network), then hands both to wpa_supplicant via wpa_cli
(add/set/enable/select/save), persists them to /opt/couch/networks.conf for the
next boot, and runs udhcpc. Association is asynchronous, so a toast shows
"Connecting to X" and then "Connected to X" or "Could not connect" once the
tick sees wpa_state. The passphrase keyboard is opened a frame after the SSID
keyboard closes, not from inside its accept callback - reopening it there
leaves focus on the shell and the field takes no input. COUCH_WIFI_DRYRUN
prints the plan and changes nothing, so the flow can be driven over the very
link a real switch would drop.

**SSH** — a single toggle. OK starts or stops sshd (host keys generated on
first use), and the choice persists as `ssh=` in settings.conf, which the boot
`couch-system` reads so a device that was turned off stays off across a reboot.
Enrolment - a key or a root password - stays gated on the setup portal's
physical-press step; the toggle only runs the daemon for someone already
enrolled, and shows "unset" otherwise. Turning SSH off from here stops the
listener but not an existing session.

## Persistence

Brightness and the two timeouts live in `/opt/couch/settings.conf`
(`key=value`, atomic temp-then-rename), read at startup and written on every
change. The saved brightness is applied to the panel on the first frame, not
left at the splash's full.

## The menu key

The hold is timed by the host (`MENU_HOLD_US`, 500ms), not the component: the
keypad reports the key's down/up edges the way it does the microphone's
(`keypad::KEY_MENU`), and `main.rs` opens settings once the hold passes, but
only on the home screen - a modal already up owns the key. The down edge is
recorded before the wake-swallow, so a hold that begins on a dark panel still
opens settings after the wake.

## Testing it by injection

`COUCH_SETTINGS=1` opens the menu at startup so it can be driven without the
menu key. Injected key events MUST be framed with a SYN_REPORT (a 16-byte zero
event) after each, or evdev buffers them and the GUI never reads them; and only
key codes the device declares in its capability bitmap can be injected at all
(the input core drops the rest) - OK is code 353 here, not KEY_ENTER. See
`docs/keyboard.md` on the keybit filtering.
