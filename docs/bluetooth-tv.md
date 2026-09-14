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

1. Turn Bluetooth on (above). The remote advertises as **Couch Remote**.
2. On the TV, open its Bluetooth or remote-control settings and pair
   **Couch Remote**. Pairing is "just works": no PIN. TVs that pair a
   Bluetooth remote at first setup (LG, Samsung, Android/Google TV, Fire TV)
   usually have a "pair a Bluetooth device" or "connect Bluetooth remote"
   entry; the first pairing has only been tried on one TV, so record what
   each make needs in [bluetooth.md](bluetooth.md#open-questions).
3. On the web, **Connections → Add a connection → Bluetooth TV**, name it and
   create it. There is nothing to configure on the connection; its page
   repeats these steps.
4. **Rooms & devices** → add a device from that connection. The device is a
   TV; give it the TV's name.

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
links so neither side carries its own copy. The controller keeps
a synthetic address (`00:00:46:65:80:01`) until the vendor set-address
command is confirmed; TVs pair to it fine, but a reflashed remote will look
like the same device to a TV that paired the previous one.

Multiple TVs can pair to the remote, but a HID peripheral holds one link at a
time: whichever TV connects first after Bluetooth comes up gets the keys.
Per-activity bonds (disconnect and redirect on activity switch) are the next
step in [bluetooth.md](bluetooth.md#multi-device-switching-design).
