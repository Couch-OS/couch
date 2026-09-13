# Quick-access keys

The four shortcut keys above the color row (light, curtain, media and climate,
left to right) and the four color keys (red, green, blue, yellow) do nothing
on the home screen until an area assigns them. Each area assigns its own
set, so the climate key in **Living Room** opens that room's thermostat and
the same key on **Upstairs** opens the bedroom's.

In the web UI, open **Areas**, choose an area, and use **Quick-access keys**.
Every key shows one slot with what it reaches. Click a slot to open the picker,
search, and choose a target to save it immediately; **Clear this key** puts the
key back to doing nothing. A key can reach one of four things:

- **Show area** slides the home screen to another area's page, the way Left
  and Right do.
- **Open activity** opens an activity, running its start sequence when it has
  one, exactly as choosing it from the strip does.
- **Switch on / off** toggles a light, light group or cover without leaving
  the home screen, and shows a short toast with the new state. Offered for
  Hue lights and rooms, Matter lights, and Home Assistant `light` and `cover`
  entities: the integrations whose state the room list already reads.
- **Open controls** opens what a tap on the device's row would open: the
  thermostat screen for a Home Assistant climate entity, the TV screen for a
  paired or infrared TV, the player for Kodi or Sonos, the camera view for
  UniFi Protect, and the room list focused on the device for a light or cover.
  A device with no controls yet says so in a toast rather than doing nothing.

Keys act on the home screen only, whichever row is focused. A device screen,
the room list, an activity, the chooser, settings and the keyboard all keep
their own keys, and an activity's [physical button mappings](activity-buttons.md)
are unaffected. A press on a dark or dimmed panel only wakes it. The ALL ROOMS
page has no assignments. Saved changes reach the remote within a second or
two, when it reloads the configuration.

Deleting a device, activity or area removes the keys that reached it, so a
key never points at something that is gone; the slot shows **Not assigned**
afterwards. A key whose target disappeared between the save and the press
reports that in a toast.

`Area.shortcuts` stores `{button, action}` entries, one per key, with
`action` tagged by `kind`: `{"kind":"device","device":…}`,
`{"kind":"toggle","device":…}`, `{"kind":"activity","activity":…}` or
`{"kind":"area","area":…}`. Button names are the ones
`model/couch-model/src/buttons.rs` gives the keys (`lights`, `activity`,
`music`, `tv`, `red`, `green`, `blue`, `yellow`); the printed icons are the
light, curtain, media and climate glyphs. Old configurations have no
assignments. `PUT /api/areas/{id}/shortcuts` replaces the list; validation
rejects other keys, two actions on one key, unknown targets, a toggle on a
device that cannot be switched, and an area pointing at itself.

Validation: model round-trip, validation and removal tests; a daemon API
test; GUI tests for the press-to-action plan including stale targets; and the
browser test `node web/tests/area-shortcuts.mjs`, which assigns every kind of
target, searches, clears a key and checks that deleting the target area clears
its key. Physical presses on the HA100 remain to be checked against a real
thermostat and light.
