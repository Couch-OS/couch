# Bluetooth

Couch can be the Bluetooth remote a TV expects: the HA100 advertises as a
Bluetooth Low Energy HID peripheral called **Couch Remote** (keyboard plus
consumer-control keys), the TV pairs to it from its own Bluetooth menu, and
the remote's mapped buttons then reach the TV over Bluetooth with no network,
address or credentials. It complements infrared for TVs that ship with a
Bluetooth remote and expect one, and it works with the screen off and the TV
out of line of sight.

## Status: experimental

Pairing and the keys work: a TV or a computer pairs from the pairing card,
the remote keeps a bond per device, one link is active at a time, and the
keys keep working across reboots. As things stand Bluetooth is off after
every restart — it does not start at boot, so it has to be switched on by
hand each time, which is a known gap rather than a settled choice — and a TV
paired on an older build has to pair again (see
[troubleshooting](#troubleshooting)).

**The known problem is Wi-Fi.** The remote's Wi-Fi and Bluetooth share one
radio, and a *connected* Bluetooth device can slow Wi-Fi badly: measured on a
MacBook link (15 ms connection interval, no slave latency), a 355 KB download
took 40 seconds and stalled where it takes about 1 second with no link, and
an update check failed. So while a Bluetooth device is linked, expect
Home Assistant control, the web UI and update downloads to be slow or to
fail; **turning Bluetooth off restores them**. A TV that chooses a gentler
interval should cost far less (the LG negotiated 60 ms with slave latency 5),
but that has not been measured yet. See [Bluetooth and BLE](bluetooth.md) for
the design and the kernel side.

## Requirements

- A boot image with the Bluetooth kernel (the row under Settings → Bluetooth
  says "no kernel support" otherwise; install the current boot image from
  Settings → Updates first).
- Bluetooth turned on: Settings → Bluetooth on the remote, or the Remote
  settings page on the web. The row says **STARTING…** for a few seconds
  while the stack comes up (bridge, dbus, bluetoothd, the HID daemon), then
  **ON**. It stays on until turned off; it does not start at boot. The very
  first time, the remote downloads about 2 MB of system packages over Wi-Fi
  before starting, so allow up to a minute; if it says it could not reach the
  package server, check Wi-Fi and switch it on again.

## Setup

Bluetooth is a **transport of a device**, the way infrared is: a TV in
Rooms & devices carries its own Bluetooth bond, next to its network
connection (webOS, Android TV, Apple TV, Samsung) and its IR commands if it
has them. Two TVs each get their own bond. Pairing is done from the device
itself, and the remote is not discoverable at any other time: outside a
window it advertises for the one TV it is currently talking to and the
controller drops everyone else's connection request on the air, so a laptop
or phone in the room never lists a "Couch Remote" to grab.
Pairing a TV is a deliberate two-minute **pairing mode** opened for one
device:

1. Turn Bluetooth on (above). The row says **ON**, and names the TV once
   one is connected (**ON · webOS TV OLED48B4PUA**).
2. Open the device. On the web: **Rooms & devices** → the room → the TV's
   card → **Bluetooth** section → **Pair over Bluetooth**. On the remote:
   open the TV from its room, then **Commands** (an infrared or Bluetooth
   TV) or **Apps** (a network TV) → **Pair over Bluetooth**. The device
   need not have anything else configured: a TV added with no connection
   and no IR commands is a Bluetooth-only device once paired.
3. The window opens. The card on the remote (or the section on the web)
   reads **DISCOVERABLE**: "On your TV, open Bluetooth settings and choose
   Couch Remote." On the TV, open its Bluetooth or remote-control settings
   and choose **Couch Remote**. Pairing is "just works": no PIN. TVs that
   pair a Bluetooth remote at first setup (LG, Samsung, Android/Google TV,
   Fire TV) usually have a "pair a Bluetooth device" or "connect Bluetooth
   remote" entry. The card follows along: **CONNECTED** when the TV has
   connected, **PAIRED** once it has bonded. TVs paired earlier stay
   paired — a window forgets nothing — but they are kept off the link while
   it is open: a 4.0 controller holds one link and does not advertise while
   it has one, so a TV that reconnected would leave the new one nothing to
   connect to. Keys to that TV fall through to its other transports
   meanwhile, and it reconnects when the window ends.
4. Some TVs (LG webOS among them) then ask you to *press any key on the
   keyboard*. Press **OK** on the remote: in pairing mode OK sends an Enter
   key from the keyboard half of the HID profile, which is the key such a
   prompt is waiting for (the volume and navigation keys are consumer-control
   keys, which the prompt ignores). On the web, use **Send key**.
5. The card says **DONE** ("Done: *name* is paired.") once the TV has bonded
   and subscribed to the remote's input reports; press **Back** to close it.
   Within a second the bond (the TV's address and name) is stored on the
   device: the device's card reads **Paired with *name***, and its room row
   on the remote lists Bluetooth among its transports. The window closes by
   itself after two minutes with **NOT PAIRED** if no TV got that far, and
   **Back** (or **Cancel**) during the window cancels it. Either way the
   remote goes back to non-discoverable. The TV that just paired is now the
   one the remote talks to (the *active* one), and it reconnects
   whenever both are on.

**Unpair** on the device's Bluetooth section (or **Unpair Bluetooth** in the
same list on the remote) drops that one bond, on the device and on the
remote's Bluetooth stack; the TV then needs pairing mode again. Deleting the
device does the same. **Pair again** replaces the device's bond with the TV
that pairs next. Turning Bluetooth off does not forget anything.

The remote can hold a pairing with every TV in the house, but a Bluetooth
remote talks to one TV at a time: only the active one can connect, and the
remote answers nobody else on the air. Which TV is active follows the device
being controlled — the one whose screen is open, or the main device of the
running activity — and it survives a reboot, because the remote remembers it
on its own. Switching is instant on the remote's side and takes the TV a few
seconds to notice and reconnect.

**Settings → Bluetooth → Pair with TV** on the remote, and **Pair with TV**
on the web UI's Remote page, still open a window, but one that belongs to no
device: the TV bonds to the remote and nothing is stored on a device. It is
there for trying a TV before its device exists, and for the remote's own
tests; a TV you mean to control is paired from its device. **Forget
pairings** on the Remote page drops every bond the stack holds, the devices'
included, without touching the configuration: pair those devices again
afterwards.

A device that was set up before this (the older Bluetooth connection, provider `bluetooth-tv`,
with a device on it) is carried over on the daemon's next start: the device keeps
Bluetooth as its transport, with the bond's address unrecorded, and the
connection disappears. It keeps working with whichever TV the remote is
connected to, as it did; pair it again from the device once, and it is
pinned to its own TV.

The first pairing has only been tried on one TV, so record what each make
needs in [bluetooth.md](bluetooth.md#open-questions).

## Controls, transports and activities

Selecting a Bluetooth-only TV from a room opens the one-way TV screen:
d-pad, OK, Back, Home, Menu, volume, mute and channel keys go over
Bluetooth, and the **Commands** list offers every key the transport knows.
The TV gives no feedback, so the status line says so. A TV that also has a
network connection opens its usual screen (inputs, apps, playback state);
one that also has IR commands opens the one-way screen with both lists.

**Which transport a key takes.** A device may have up to three: infrared
(its IR commands), the network (its connection) and Bluetooth (its bond).
The device's **Preferred control** (Edit device on the web, offered once the
device has more than one) picks which is tried first; **Automatic** is
infrared, then the network, then Bluetooth, which is what a device did
before it had a bond. A key falls through to the next transport when the
preferred one is not available *right now*: no IR code is assigned to that
key, the network client cannot connect (the TV is asleep), or the device's
TV is not the one on the Bluetooth link. Power-on is the everyday case: a
sleeping TV has no network to answer on and no Bluetooth link to receive a
key, so a TV set to prefer Bluetooth or the network still wakes by infrared
when it has an IR code for `power-on`. A transport that was tried and
refused the key is the end of the press: a failed infrared write is never
re-sent over the network, and a network command the TV rejected is not
repeated by IR.

**One link at a time.** The remote is one Bluetooth peripheral and holds
one link. Whichever device is on screen holds it: when an activity starts,
its main-screen device's bond becomes the active link (the remote tells the
Bluetooth stack to drop any other TV and accept only that one), and opening
another device's screen moves the link to that device. An activity with two
Bluetooth-paired TVs therefore reaches the second TV over its other
transport (its connection or IR commands) while the first is on screen. A
second TV that has *nothing* else cannot be reached while the activity runs:
the activity editor on the web says so above the device list as soon as the
combination is saved, and the remote says so once when the activity starts,
rather than dropping keys silently. Give that TV IR commands or a
connection, or make it the activity's main screen.

Activity button mappings and start/stop sequences can use: `up`, `down`,
`left`, `right`, `ok`, `back`, `home`, `menu`, `power-off`, `volume-up`,
`volume-down`, `mute`, `channel-up`, `channel-down`, `play`, `pause`,
`play-pause`, `stop`, `next`, `previous`, `rewind`, `fast-forward`. Each is
one HID consumer-control usage; which ones a TV honours depends on its make
(volume, navigation and playback are near-universal, channel keys less so).

Power is the consumer-control **power toggle** and only reaches a TV that is
on: a TV that is off has no Bluetooth link to receive it, so there is no
`power-on` over Bluetooth. Give the device IR commands or a network
connection for waking, or leave the TV's own remote for that.

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

Pairing mode and the per-device bonds are a few more words on that socket
(`pair`, `pair-stop`, `forget`, `forget <ADDR>`, `activate <ADDR>`,
`activate none`) and a state file the daemon writes,
`/tmp/couch-bt-pair.state`, that the remote's card, the web page and the API
read (`idle`, `pairing`, `connected <name>`, `paired <name>`, `done <name>`,
`failed timeout|cancelled`, plus `peer <ADDR> <name>` naming the TV that
just bonded, `link <ADDR> <name>` while a TV is connected and `active <ADDR>`
for the bond the daemon holds active). A window opened from a device is
remembered in `/tmp/couch-bt-bond.request`; when the state file says `done`,
the web daemon stores the `peer` on that device. During the window the
adapter is pairable and the advertisement general-discoverable and open to
anyone; outside it the daemon advertises for the active bond alone (the
controller's white list), or not at all when no bond is active, so only the
TV being controlled reconnects. The controller runs LE-only
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
  `StartNotify` arrived.
- **It paired once and never reconnects.** Turn Bluetooth off and on
  (Settings › Bluetooth); the row should say `ON · <TV name>` within a few
  seconds of the TV being on. If the TV was paired to the remote before the
  address policy above (the `00:00:46:65:80:xx` addresses), delete it on the
  TV and pair again once.
- **Keys worked until a reboot, an update or Bluetooth off and on; the TV
  still reconnects but ignores every key.** The TV subscribed to the
  remote's key reports when it paired and does not do so again when it
  reconnects; the remote has to remember that subscription. Runtimes that
  carry `couch-bluetoothd` do (Bluetooth's own patched BlueZ, see
  [bluetooth.md](bluetooth.md#subscriptions-across-restarts-couch-bluetoothd)).
  A TV paired before the remote had it must pair once more: delete **Couch
  Remote** from the TV's Bluetooth list, forget the pairing on the remote,
  and pair again. `/tmp/system.log` on the remote says which bluetoothd
  started (`started /opt/couch/runtime/current/couch-bluetoothd`), and the
  TV's bond file
  (`/var/lib/bluetooth/<remote>/<TV>/info`) gains a `[GattCCC]` group once
  the TV has subscribed.

- **A key does nothing on a TV that says it is connected.** Check which
  device holds the link: the Remote page shows the TV on the link and the
  active bond, and a key from a device whose TV is *not* the link falls
  through to that device's other transports (or reports "*name* is not
  connected over Bluetooth" when it has none). Open the device's screen, or
  start its activity, to move the link to it.
- **Paired from the device, but the device does not show it.** The bond is
  stored by the web daemon within a second of DONE; if the device's card
  still says not paired, the daemon's log on the remote says why
  (`couch-confd: bluetooth bond for <device>: ...`). A window opened from
  Settings rather than from a device stores nothing by design.

A HID peripheral holds one link at a time. The remote keeps a bond per TV
and lets one of them, the active one, connect: the controller's white list
drops anyone else's connection request on the air. Which TV that is follows
the device being controlled; see
[Controls, transports and activities](#controls-transports-and-activities)
for what that means in an activity with two paired TVs, and
[bluetooth.md](bluetooth.md#bonds-and-the-active-link) for the mechanics.
