# Activity button mappings

![Command picker](activity-buttons-picker.png)

In the web UI, open **Activities**, choose an activity, and use **Physical
buttons**. Every key shows its short-press (dot) and long-press (line) slots.
Click a slot to open the command picker, search for a function or device,
and select a command to save it immediately. The slot displays the saved
function and target device. Saving does not send the command; reopen the
activity on the remote to load changes.

Choose a device in the picker to discover Denon inputs or LG webOS inputs
and apps. Previously saved mappings remain intact when discovery is unavailable.
Use **Use activity default** to remove an override, or **Do nothing** to consume
the press. Close the picker without choosing to leave a mapping unchanged.
The dialog supports keyboard navigation, focus trapping and Escape; when the
search field contains text, the first Escape clears it.

The interaction follows Unfolded Circle’s [official activity configuration
guide](https://support.unfoldedcircle.com/hc/en-us/articles/13269248303516-How-to-create-an-activity)
and [button mapping documentation](https://support.unfoldedcircle.com/hc/en-us/articles/13214914883356-Button-mapping):
visible press slots open a searchable command picker. The same slot layout is
shown in the [Remote 3 setup guide](https://unfolded.community/t/getting-started-with-the-unfolded-circle-remote-3/5722).
Couch retains its own 600 ms threshold and repeat-key restrictions.

![Mobile command picker](activity-buttons-picker-mobile.png)

For example, use a Kodi source with D-pad/OK left at their defaults, volume and
mute assigned to a Denon receiver, and power assigned to an LG TV. Targets may
belong to different Couch rooms. Only supported functions are offered. IR
remains unavailable until the built-in blaster can send codes.

- **Activity default** removes an override. **Do nothing** explicitly consumes it.
- **Long Back is reserved globally for returning to Couch** from a device or
  activity screen, including open Kodi/TV panels. Short Back still controls the
  device and fires on release. The held press and release never send device
  Back; existing long-Back overrides are retained in configuration but ignored.
- Other short and long presses are independent. A configured long press fires once
  after 600 ms. Its short action waits for release and never also fires on a hold.
- D-pad, volume and channel keys retain repeat; they cannot have long bindings.
  Repeat events only repeat navigation, volume and channel functions, even when
  a repeat-capable button is assigned to a toggle or power function.
- A volume or mute press mapped to a device that can report its level (Sonos,
  Kodi, LG webOS, Denon) shows the volume card with the level read back after
  the command, the same card the room list shows for a highlighted speaker.
  Sonos, Kodi and webOS show a 0–100 meter; Denon shows its dB figure; a muted
  device shows "Muted". Devices without a readable level show nothing.
- Overrides apply while the activity screen is open, including its touch sheets.
  Directly opening a device uses normal controls. The touch back arrow always
  exits to Couch. Closing an activity cancels pending holds and local queued mappings;
  commands already dispatched cannot be recalled.
- Deleting a mapped device disables its bindings instead of silently redirecting
  them to the source device. Missing references and unsupported commands are
  rejected before saving.

On the home screen the shortcut and color keys are the area's
[quick-access keys](area-shortcuts.md) instead; activity mappings apply only
while the activity screen is open.

Key names follow the [HA100 front-panel layout](https://www.sanytron.com/cdn/shop/files/A.2066.png).
The four shortcut buttons above the colored keys were physically captured as
Linux codes 62, 63, 64 and 65 (left to right). Other measured keys are recorded in
`model/couch-model/src/buttons.rs`; mapping slots use accessible HTML buttons.

`Activity.buttons` stores `{button, gesture, action}` entries. `gesture` defaults
to `short`; `action: null` disables the key. Old configurations default to no
bindings. Shared model validation and function catalogs serve both UIs.
Device I/O uses a bounded worker queue, with generation and age checks.

Validation: model/API regression tests, GUI input timing tests, browser
save/reload and mobile-layout checks, and injected evdev presses on the HA100
against isolated Kodi/Denon fixtures. The physical fixture covered short/long
exclusivity, repeat, shortcut and microphone mapping, and cancellation on exit.
