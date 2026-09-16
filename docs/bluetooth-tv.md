# Bluetooth TV

Couch can be the Bluetooth remote a TV expects: the HA100 advertises as a
Bluetooth Low Energy HID peripheral called **Couch Remote** (keyboard plus
consumer-control keys), the TV pairs to it from its own Bluetooth menu, and
the remote's mapped buttons then reach the TV over Bluetooth with no network,
address or credentials. It complements infrared for TVs that ship with a
Bluetooth remote and expect one, and it works with the screen off and the TV
out of line of sight.

The remote's Wi-Fi and Bluetooth share one radio. Idle Bluetooth costs
nothing measurable; only sustained radio use (a continuous scan, which a HID
peripheral never does) slows Wi-Fi. See [Bluetooth and BLE](bluetooth.md) for
the design and the kernel side.

## Requirements

- A boot image with the Bluetooth kernel (the row under Settings → Bluetooth
  says "no kernel support" otherwise; install the current boot image from
  Settings → Updates first).
- Bluetooth turned on: Settings → Bluetooth on the remote, or the Remote
  settings page on the web. The row says **STARTING…** for a few seconds
  while the stack comes up (bridge, dbus, bluetoothd, the HID daemon), then
  **ON**. It stays on until turned off; it does not start at boot.

## Setup

The remote is not discoverable by default: with Bluetooth on it advertises
in a way only a TV that has already paired will connect to, so a laptop or
phone in the room never lists a "Couch Remote" to grab. Pairing a TV is a
deliberate two-minute **pairing mode**:

1. Turn Bluetooth on (above). The row says **ON**, and names the TV once
   one is connected (**ON · webOS TV OLED48B4PUA**).
2. **Settings → Bluetooth → Pair with TV** on the remote, or **Pair with
   TV** in the Bluetooth section of the web UI's Remote page. This opens
   the window; TVs paired earlier stay paired, though they are kept off the
   link while the window is open. The card on the remote reads
   **DISCOVERABLE**: "On your TV, open Bluetooth settings and choose Couch
   Remote."
3. On the TV, open its Bluetooth or remote-control settings and choose
   **Couch Remote**. Pairing is "just works": no PIN. TVs that pair a
   Bluetooth remote at first setup (LG, Samsung, Android/Google TV, Fire TV)
   usually have a "pair a Bluetooth device" or "connect Bluetooth remote"
   entry. The card follows along: **CONNECTED** when the TV has connected,
   **PAIRED** once it has bonded.
4. Some TVs (LG webOS among them) then ask you to *press any key on the
   keyboard*. Press **OK** on the remote: in pairing mode OK sends an Enter
   key from the keyboard half of the HID profile, which is the key such a
   prompt is waiting for (the volume and navigation keys are consumer-control
   keys, which the prompt ignores). On the web, use **Send key**.
5. The card says **DONE** ("Done: *name* is paired.") once the TV has bonded
   and subscribed to the remote's input reports; press **Back** to close it.
   The window closes by itself after two minutes with **NOT PAIRED** if no
   TV got that far, and **Back** during the window cancels it. Either way
   the remote goes back to non-discoverable. The TV that just paired is now
   the one the remote talks to (the *active* one), and it reconnects
   whenever both are on.
6. On the web, **Connections → Add a connection → Bluetooth TV**, name it and
   create it. There is nothing to configure on the connection; its page
   repeats these steps.
7. **Rooms & devices** → add a device from that connection. The device is a
   TV; give it the TV's name.

**Forget pairings** on the web page drops the bonds without opening a
window; a TV then needs pairing mode again. Turning Bluetooth off does not
forget anything.

The remote can hold a pairing with every TV in the house, but a Bluetooth
remote talks to one TV at a time: only the active one can connect, and the
remote answers nobody else on the air. Switching is instant on the remote's
side and takes the TV a few seconds to notice and reconnect; which TV is
active follows the device the app is controlling, and survives a reboot.

The first pairing has only been tried on one TV, so record what each make
needs in [bluetooth.md](bluetooth.md#open-questions).

## Controls and activities

Selecting the TV from a room opens the one-way TV screen: d-pad, OK, Back,
Home, Menu, volume, mute and channel keys go over Bluetooth, and the
**Commands** list offers every key the integration knows. The TV gives no
feedback, so the status line says so.

Activity button mappings and start/stop sequences can use: `up`, `down`,
`left`, `right`, `ok`, `back`, `home`, `menu`, `power-off`, `volume-up`,
`volume-down`, `mute`, `channel-up`, `channel-down`, `play`, `pause`,
`play-pause`, `stop`, `next`, `previous`, `rewind`, `fast-forward`. Each is
one HID consumer-control usage; which ones a TV honours depends on its make
(volume, navigation and playback are near-universal, channel keys less so).

Power is the consumer-control **power toggle** and only reaches a TV that is
on: a TV that is off has no Bluetooth link to receive it, so there is no
`power-on`. Pair infrared or a network integration on the same device for
waking, or leave the TV's own remote for that.

## How it works

`couch-bt-hid` (the HID daemon) registers a HID-over-GATT service with
bluetoothd and then advertises one of two ways, depending on the kernel it
finds. Where bluetoothd offers an advertising manager (the backported
Bluetooth core), the daemon registers an advertisement object and bluetoothd
owns it: it comes back by itself after a TV disconnects. On the 3.18 kernel,
whose BlueZ has no advertising manager, the daemon drives the controller with
raw HCI commands and re-enables advertising every 15 seconds, because that
kernel stops advertising when a TV connects and never restarts it. Both
adverts carry the same name, HID service and appearance, so a TV pairs the
same way either way. The GUI sends one datagram per key press,
the function's id, to `/tmp/couch-bt-hid.sock`; the daemon turns it into an
input report (usage down, 30 ms, usage up) on the notifying connection. The
socket is mode 0600 and root-owned, because writing one word to it presses a
key on a paired TV. The same words work from a root shell on the remote for
testing; the path and the vocabulary are `couch-bt-hid`'s lib, which the GUI
and the system service link so nobody carries their own copy.

Pairing mode is three more words on that socket (`pair`, `pair-stop`,
`forget`) and a state file the daemon writes, `/tmp/couch-bt-pair.state`,
that the remote's card, the web page and the API read (`idle`, `pairing`,
`connected <name>`, `paired <name>`, `done <name>`, `failed timeout|cancelled`,
plus a `link <name>` line while a TV is connected). During the window the
adapter is pairable and the advertisement general-discoverable; outside it,
neither, so only a bonded TV reconnects. The controller runs LE-only
(bluetoothd's `ControllerMode = le`): the chip can do classic Bluetooth too,
but the HID service only exists over LE, and a TV that found the remote over
classic paired and then found nothing to use. The details are in
[bluetooth.md](bluetooth.md#pairing-mode).

The controller's address is the remote's Wi-Fi MAC plus one (`02:28:7d:8f:e1:6e`
→ `02:28:7d:8f:e1:6f`), programmed at every bring-up with MediaTek's
set-address command before bluetoothd starts. The firmware's own default is
`00:00:46:65:80:01` on every remote and every boot, and a TV keeps its bonds
and its grudges per address: two remotes would look like one, and a bond
would not survive a reboot. With the derived address a paired TV reconnects
after the remote reboots, a reflashed remote looks like the same device to
its TV, and two remotes in one house are two devices.

## Troubleshooting

- **The TV lists nothing, or "unable to connect", while the card says
  DISCOVERABLE.** LG's scanner wedges after a failed round: the remote is on
  the air but the TV keeps stale state for it and will not send a connect
  request. Delete the remote from the TV's Bluetooth list if it is there,
  then power the TV off at the wall (standby is not enough) and try again.
- **The TV lists "Bluetooth Keyboard" instead of "Couch Remote".** Same
  device: the name rides in the scan response and the TV missed it, so it
  shows the appearance (HID keyboard) instead. Choose it.
- **PAIRED, then nothing.** The TV bonded but has not subscribed to the
  remote's reports; most TVs do that on their own within a second or two,
  and some (LG) first ask you to press a key: press OK. If the card never
  reaches DONE, `/tmp/couch-bt-hid.log` on the remote says whether a
  `StartNotify` arrived and which reports were dropped for want of one.
- **It paired once and never reconnects.** Turn Bluetooth off and on
  (Settings › Bluetooth); the row should say `ON · <TV name>` within a few
  seconds of the TV being on. If the TV was paired to the remote before the
  address policy above (the `00:00:46:65:80:xx` addresses), delete it on the
  TV and pair again once.

A HID peripheral holds one link at a time. The remote keeps a bond per TV
and lets one of them, the active one, connect: the controller's white list
drops anyone else's connection request on the air. The mechanics are in
[bluetooth.md](bluetooth.md#bonds-and-the-active-link).
