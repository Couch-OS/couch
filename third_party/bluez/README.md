# couch-bluetoothd

Alpine 3.21's `bluetoothd` (BlueZ 5.79, aports `main/bluez` 5.79-r0) with one
Couch patch, shipped in the runtime bundle as `couch-bluetoothd`. The system
service starts it instead of `/usr/lib/bluetooth/bluetoothd` whenever the
runtime carries it (`daemon/couch-system/src/bluetooth.rs`).

## Why

A bonded TV reconnects to the remote without writing the Client
Characteristic Configuration descriptors (CCCs) of the HID input reports
again: for a bonded device the server is required to remember them (Core
Specification Vol 3, Part G, 3.3.3.3). BlueZ remembers them only in memory.
It writes the Service Changed CCC to storage and nothing else
(`gatt_database_free()` in `src/gatt-database.c`: `/* TODO: Persistently store
CCC states before freeing them */`, still on master), so after bluetoothd
restarts, which on the remote means every reboot, update install and
Bluetooth toggle, `send_notification_to_device()` finds no subscription and
drops every key report without a word. Seen with an LG OLED48B4 on
2026-09-15: the TV reconnected, re-read the services on Service Changed, and
never subscribed again; `couch-bt-hid` logged every report as dropped.

## The patch

`0001-gatt-database-persist-ccc-for-bonded-devices.patch` (its header has the
details) stores the CCC values of bonded devices in the device's `info` file
and restores them:

```
[GattCCC]
LE_0x012b=0x0001 00002a4d-0000-1000-8000-00805f9b34fb
LE_0x012f=0x0001 00002a4d-0000-1000-8000-00805f9b34fb
```

The key is the bearer and the CCC handle; the value is the CCC value and the
UUID of the characteristic the CCC belongs to (0x2a4d is a HID Report; on the
remote 0x012b and 0x012f are the keyboard and consumer reports).

- **Stored** on every CCC write by a bonded device, when a bonded device
  disconnects, and when a device becomes bonded (for subscriptions written
  before the keys arrived). Only in-memory states are written; value 0
  removes the entry; nothing else is erased, so `couch-bt-hid` unregistering
  (its services and their in-memory states go away) leaves the file alone.
- **Restored** for every bonded device when bluetoothd starts, and again
  whenever a service is added, for the handles that service covers: the HID
  application registers after bluetoothd is up, and again after a
  `couch-bt-hid` restart.
- **Validated**: an entry comes back only while the attribute at its handle is
  a CCC of a characteristic with the stored UUID. An entry inside a registered
  service that fails the check is removed from the file (the attribute table
  changed; the TV rediscovers on Service Changed and subscribes afresh); an
  entry no registered service covers yet is kept. Two characteristics with the
  same UUID that swapped places are not told apart. For entries to survive at
  all, the table must be the same every time, which is why `couch-bt-hid`
  pins its service handles and lists its objects in a fixed order.
- **Removed** with the bond: `RemoveDevice` deletes the device's storage
  directory.

A bond made by stock bluetoothd has no `[GattCCC]` entries, so a TV paired
before the remote ran `couch-bluetoothd` must pair once more (forget it on
both sides, then pairing mode) or have its entries written by hand.

Not sent upstream. It is written against 5.79 in BlueZ style and could be
offered to linux-bluetooth@vger.kernel.org once it has run for a while.

## Building

```sh
third_party/bluez/build.sh OUTDIR
```

On a Linux host with docker and linux/arm/v7 emulation (Ollie). The script
pins the upstream tarball (SHA-256), the aports commit (the recipe's patches
checked against its SHA-512 sums), the `alpine:3.21` arm/v7 image digest and
the versions of glib, dbus, eudev, musl, gcc and binutils; it applies the
recipe's patches in recipe order, then ours, configures with the recipe's
flags and abuild's CFLAGS/LDFLAGS, builds `src/bluetoothd` only and strips
it. About two minutes under emulation. `OUTDIR/build.json` records every
input, the installed packages, the linked libraries and the binary's
SHA-256; `OUTDIR/source/` is the corresponding source (tarball, recipe files,
patch, script, this file).

The result links `libglib-2.0.so.0`, `libdbus-1.so.3`, `libudev.so.1` and
musl, the libraries the remote's Alpine already has (glib 2.82.5-r0,
dbus-libs 1.14.10-r4, eudev-libs 3.2.14-r5), and uses the same paths as the
stock daemon (`/etc/bluetooth/main.conf`, `/var/lib/bluetooth`), so it reads
and writes the same bonds.

## Licence

BlueZ is GPL-2.0-or-later. A release carrying `couch-bluetoothd` carries
`licenses/BlueZ-GPL-2.0.txt` in the runtime bundle and publishes the
corresponding source: `tools/release/collect_external_sources.py bluez`
turns OUTDIR into the `bluez` component of the source archive
(docs/corresponding-source.md).
