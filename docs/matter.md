# Matter devices

> **Status: experimental.** Validated only against a simulated Matter light on a
> development Mac. Not yet tested with a real device or on the remote itself;
> expect the pairing flow and device list to change.

`clients/couch-matter` makes the remote a Matter controller with its own small
fabric. Devices that another ecosystem already set up (Apple Home, Google Home,
Alexa, Home Assistant) join that fabric through the ecosystem's "pair with
another app" sharing code, over Wi-Fi or Ethernet. The device keeps its original
ecosystem and gains Couch as an additional controller; Couch never holds another
ecosystem's credentials. This is a Matter controller only: the remote is not a
Thread border router and does not commission devices over Bluetooth, so a device
still in its box must be set up in its own app first.

## Pair a device

1. Open the remote's web editor and add a **Matter** connection under
   **Connections**. The first request creates the fabric: a certificate
   authority, one controller certificate and a random fabric ID, kept privately
   on the remote.
2. In the app that set the device up, choose to pair it with another app or
   service. It shows an 11- or 21-digit pairing code and opens the device's
   commissioning window for a few minutes.
3. Enter the code and a name, then **Pair device**. Couch checks the code's
   check digit, finds the device with mDNS, runs PASE and CASE, installs its
   own operational certificate and reads the device's endpoints. Expect up to a
   minute; a failure leaves nothing paired.
4. Open **Rooms & devices**, choose the Matter connection under **Add devices to
   this room** and add the device's on/off controls. Devices store
   `{"via":"connection","connection_id":"<matter>","resource_id":"<node>/<endpoint>"}`.

In the room view on the remote, OK on a Matter light with a Level Control
cluster opens its controls, where the brightness can be set; Power switches it,
and left/right on the row still dims it in place. A device with only on and off
keeps OK as a switch. Matter lamps offer brightness only: the controller reads
and writes on/off and level, so the controls show one brightness bar, no colour
temperature, and the channel keys do nothing there. Activities, scenes and physical
button mappings can use `on`, `off`, `toggle` and a `dim:<n>` level, the last
only on an endpoint with a Level Control cluster. A mapped `toggle` reads the
state first and turns the device on if it will not say. The room view, the
area's quick-access keys and mapped buttons share one controller per fabric, so
a key press reuses the CASE session the room list already opened. Every read
and command opens a CASE session and reads the state back after the command;
there is no subscription yet, so a device changed elsewhere is only as fresh as
the last refresh.

## Forgetting a device

**Forget device** asks the device to remove this remote's fabric before the
remote forgets it, so the device stops advertising for a controller that no
longer exists. If the device does not answer, it is forgotten locally and keeps
a stale fabric entry until it is factory reset or the entry is removed from its
own app. Deleting the connection while rooms still reference its devices is
rejected, as for other connections. Unlike other connections, deleting a Matter
connection does not remove its private folder: the fabric's keys stay on the
remote (see [Removing a connection](connections.md#removing-a-connection)).
Forget its devices first, or they keep listing a controller that is gone.

## Storage and security

The fabric lives under `connections/<connection-id>/matter/` beside `config.json`:
`pem/` holds the CA and controller keys, `devices.json` the node addresses and
`inventory.json` the endpoints. The directory is mode `0700` and keys `0600`;
none of it is part of the exported house configuration and the API never
returns key material. Device attestation certificates are not checked against
the Connectivity Standards Alliance's trust store: the pairing code, shown by an
app the user already trusts, is the trust decision. Pair only on your own LAN.

The controller binds one UDP socket for Matter and shares port 5353 for mDNS
with the other services on the remote. On Linux the socket is dual-stack so
devices that answer only on IPv6 link-local addresses are reachable; set
`COUCH_MATTER_BIND` to override the bind address.

## CLI and validation

```sh
(cd clients && cargo test -p couch-matter)
couch-matter --dir ./matter commission 3497-011-2332 "Reading lamp"
couch-matter --dir ./matter nodes
couch-matter --dir ./matter lights
couch-matter --dir ./matter toggle 1/1
couch-matter --dir ./matter brightness 1/1 40
couch-matter --dir ./matter remove 1
```

Build for the remote with `(cd clients && cargo build --release --target
armv7-unknown-linux-musleabihf -p couch-matter)`. The daemon exposes the same
operations under `/api/connections/<id>/matter/{connection,commission,devices,
devices/<node>,devices/<node>/refresh,lights,lights/<node>/<endpoint>,
lights/<node>/<endpoint>/command}`; commissioning, forgetting and refreshing
hold the connection lock, reads and commands run concurrently.

Host validation used `matc`'s simulated On/Off light: commissioning, toggle,
off and forget all round-trip on a Mac with the device's log confirming each
command. No real Matter device or the remote's radio has been tested yet; the
first real pairing on the HA100 is the next validation step, particularly
IPv6 multicast through its Wi-Fi driver and kernel 3.18.

Protocol implementation: [matc](https://github.com/tom-code/rust-matc)
(BSD-2-Clause), a controller-side Matter library in pure Rust.
