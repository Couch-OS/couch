# Bluetooth and BLE

Status as of 2026-09-14 (end of day): **the remote is a working BLE HID
remote.** The Bluetooth kernel (`CONFIG_BT` + `CONFIG_BT_HCIVHCI`, unified
kernel `06b21c74`) ships as a boot payload; the toggle under Settings brings
up bridge, dbus, bluetoothd and `couch-bt-hid`; a real TV paired to "Couch
Remote" and took volume keys; and a **Bluetooth TV** connection/device routes
the remote's mapped buttons and the one-way TV screen over Bluetooth
([user guide](bluetooth-tv.md)). Idle Bluetooth has no measurable power or
Wi-Fi cost. Pairing is a deliberate two-minute window from Settings or the
web page ([pairing mode](#pairing-mode)); outside it the remote is not
discoverable. The daemon holds any number of TV bonds and lets exactly one
of them, the active bond, connect ([bonds and the active
link](#bonds-and-the-active-link)); the app chooses which. An
in-kernel HCI driver now exists (`hci_stp`, see
[kernel-backports-research.md](kernel-backports-research.md#outcome-2026-09-15-the-in-kernel-driver-on-both-cores)). The sections below are the design record: "what
exists today" describes the starting point, the
[implementation plan](#implementation-plan-branch-bluetooth-rebased-onto-dev-2026-09-14)
and [staging checklist](#staging-checklist) what was done. Kernel-side tasks are mirrored in the kernel tree at
`Documentation/couch/bluetooth.md` on the `bluetooth` branch of
[dangerouslaser/couch-kernel](https://github.com/dangerouslaser/couch-kernel).

## Goal

Let the HA100 act as a Bluetooth Low Energy HID peripheral (keyboard plus
consumer-control remote) towards a TV or streaming box, keep one bond per
target device, and switch which device it is connected to when the user
switches activity. Simultaneous connections are not a goal.

## What exists today

| layer | state | evidence |
|---|---|---|
| radio | MT6580 CONSYS combo chip, Bluetooth 4.0 with LE | MediaTek datasheet; same block that provides Wi-Fi, which works |
| MediaTek transport | built in: `CONFIG_MTK_COMBO_BT=y`, `CONFIG_MTK_BTIF=y` | `kernel/couch-ha100.config` |
| `/dev/stpbt` | created by `connectivity/common/conn_soc/linux/pub/stp_chrdev_bt.c`; opening it powers the BT function on through WMT and pumps raw HCI packets over STP | kernel source |
| Linux Bluetooth core | **off**: `# CONFIG_BT is not set` | `kernel/couch-ha100.config` line 987 |
| HCI device for BlueZ | **none**: `/dev/stpbt` is a character device, not an `hci_dev` | kernel source |
| firmware | the CONSYS ROM patch is already linked into `/` for Wi-Fi by `stage2/hardware-init.sh`; the same WMT patch load covers Bluetooth | stage2 |
| Bluetooth MAC | recorded by the installer as `bluetooth_mac` in the identity backup; stock loads it from NVRAM, our kernel has no NVRAM path | `tools/installer` |
| userland stack | **none**: no BlueZ in the Alpine image, no Bluetooth code in the daemon, GUI or stage2 | repository grep |
| validation | "built in, never exercised" on both from-source and stock | `kernel/README.md` matrix |

Why the core is off: stock Android on MediaTek runs the Bluedroid stack in
userland straight over `/dev/stpbt` through `libbt-vendor`, so the vendor tree
never configured the kernel stack. Nothing in Couch replaces Bluedroid.

## The gap

BlueZ needs an `hci0` registered with the kernel Bluetooth core. The MediaTek
driver only offers a character device. Two ways to close the gap:

1. **Virtual HCI bridge (spike).** Enable `CONFIG_BT` and `CONFIG_BT_HCIVHCI`,
   then run a small daemon that shuttles packets between `/dev/vhci` and
   `/dev/stpbt`. About a hundred lines, no new kernel code, and it answers
   "does the radio work" in a day.
2. **In-kernel STP HCI driver (proper).** A driver that calls
   `mtk_wcn_wmt_func_on(WMTDRV_TYPE_BT)`, registers an `hci_dev`, and maps
   `hdev->send` to `mtk_wcn_stp_send_data(..., BT_TASK_INDX)` and the STP
   receive callback to `hci_recv_frame`. Replaces the daemon once the spike
   proves the radio.

## Kernel constraints to plan around

- Linux 3.18 has no separate LE toggle; enabling `CONFIG_BT` includes LE.
- The management `Add Advertising` command arrived in Linux 4.2, so the BlueZ
  `LEAdvertisingManager1` D-Bus API will not work here. Advertising, including
  directed advertising for device switching, must be driven through raw HCI
  or the older management commands.
- LE Secure Connections landed after 3.18. Pairing falls back to legacy LE
  pairing, which TVs and streaming boxes generally accept. Verify per target.
- The controller comes up as `00:00:46:65:80:01` on every remote unless a
  vendor command programs an address after power-on (it does: see
  [pairing mode](#pairing-mode), "address").
- Whether a newer Bluetooth core could be backported onto this 3.18 base, and
  what it would cost, is surveyed in
  [kernel backports research](kernel-backports-research.md) (`Add Advertising`
  is 4.1, not 4.2; `backports-4.4.2-1` is the last release that carries
  Bluetooth at all).

## Multi-device switching design

BLE HID peripherals connect to one central at a time. The model that works
for multi-device keyboards applies here:

- BlueZ stores one bond per central; nothing extra is needed to keep several.
- Each activity references the bonded address it should talk to.
- On activity switch: disconnect the current central, then directed-advertise
  (or advertise with a whitelist) to the target's address until it connects.
- Pairing a new device is a GUI flow: undirected connectable advertising for a
  bounded time, then store the bond and offer it in the activity editor.
  The bounded window exists ([pairing mode](#pairing-mode)); the per-activity
  bond store does not yet.

## Pairing mode

The remote is only pairable and discoverable while the user asks it to be.
`couch-bt-hid` owns the policy; everything else drives it through the key
socket and reads its state file.

- **LE only.** `couch_system::bluetooth` writes `/etc/bluetooth/main.conf`
  (`ControllerMode = le`, `Pairable = false`) before starting bluetoothd. The
  MT6580 is dual-mode, and with the classic side up a TV's Bluetooth menu can
  find "Couch Remote" over BR/EDR first: an LG OLED77G5 did (2026-09-15),
  paired with SSP, searched SDP for a HID record, found only PnP, and dropped
  the link three times over, showing nothing paired on its side, while the
  remote's card said PAIRED (bluetoothd reports the classic bond as
  `Paired`). The trigger was setting `Adapter1.Discoverable`, which turns
  inquiry/page scan on; the daemon no longer touches it and, LE-only, there
  is no classic side to find. `btmgmt info` shows
  `current settings: powered le secure-conn` (no `br/edr`).
- **Address.** `couch_system::bluetooth` programs the controller's public
  address at every bring-up: the Wi-Fi MAC of `wlan0` with the last byte plus
  one (`derive_address`, unit-tested), sent with MediaTek's vendor command
  (`hcitool cmd 0x3f 0x001a <six bytes, little-endian>`, opcode 0xFC1A) while
  hci0 is up, then `hciconfig hci0 down; up`, verified against `hciconfig`,
  and hci0 is put down again so that bluetoothd is the one to power it on:
  the core rejects MGMT's BR/EDR-off on a powered adapter, and an hci0 left
  up after this step came back dual-mode (.158.dev), reopening the classic
  trap above. Before bluetoothd starts, because bluetoothd binds its ATT server to the
  address it saw at init: after a live change every central (an LG and a Mac)
  got no ATT MTU response and hung up. The address survives down/up and the
  toggle's func off/off but not a reboot, hence every bring-up. Why: the
  firmware default is the same on every remote and every boot, and the LG
  keeps per-address state (after one bad round it listed the remote and said
  "unable to connect" without ever sending a CONNECT_REQ); a stable unique
  address makes bonds survive reboots and keeps two remotes apart. Not
  `btmgmt static-addr` (random static): it advertises, but bluetoothd never
  answers ATT on it, verified twice. No Wi-Fi MAC: the default is kept and
  logged.
- **Socket words** (`/tmp/couch-bt-hid.sock`, mode 0600, one word per
  datagram; `Control` in the lib parses and renders them): `pair` drops
  every link, sets `Pairable` on the adapter, advertises discoverable and
  open to anyone (managed advert with `Discoverable=true`; raw-HCI path:
  flags byte `0x06`, filter policy `0x00`) and opens a `PAIR_WINDOW_SECS`
  (120 s) window; it forgets nothing. `pair-stop` closes the window early.
  `forget <ADDR>` removes that one device (`Adapter1.RemoveDevice`, bond
  included) and `forget` alone removes every device. `activate <ADDR>`
  makes that bond the active link and `activate none` lets nobody connect
  (below). Addresses are six colon-separated hex pairs, accepted in either
  case and written uppercase (`44:27:45:4E:33:25`); a control word with a
  malformed argument is refused with a line in the log, never treated as a
  key. Keyboard words `enter`, `escape`,
  `space`, `tab`, `backspace` and `kbd:<hex>` (`kbd:28`, or `kbd:0204` with
  modifiers) send a boot-style report `[mods, 0, key, 0, 0, 0, 0, 0]` then an
  all-zero release on the keyboard report characteristic; the consumer words
  are unchanged. The GUI sends words itself (it links the lib for the paths and
  words); the web daemon goes through `Request::BluetoothPair { action }` on
  the system service, whose `PairAction` is a closed set, so nothing typed on
  a web page reaches the socket.
- **Window edges.** Ends on a *new* peer (one not bonded when the window
  opened) that is paired-or-bonded *and* has subscribed to a report
  characteristic (`done <name>`), on the timeout (`failed timeout`) or on
  `pair-stop` (`failed cancelled`). A TV that was already bonded before the
  window is disconnected on sight while it is open: a 4.0 controller holds
  one link and does not advertise while it has it, so a bonded TV that
  reconnected would leave the new one nothing to connect to. At the edge
  the adapter goes back to `Pairable=false`, the managed advertisement is
  unregistered, and the daemon advertises for the active bond alone (after
  `done`, the TV that just bonded), or not at all when there is none.
  On the managed path the 4.4 core composes the flags itself: with the
  controller LE-only an `hcidump` of a window shows `LE Set Advertising Data`
  with `02 01 06` while the window is open (with BR/EDR still on the core
  wrote `0x02`, which is what the LG's classic side answered); the raw path
  writes the same bytes, and `02 01 04` outside a window.
- **Peers** come from polling `org.freedesktop.DBus.ObjectManager
  .GetManagedObjects` on `org.bluez` (every 500 ms in the window, every 3 s
  otherwise) for `Device1` `Connected`/`Paired`/`Bonded`/`Name`/`Alias`;
  `StartNotify`/`StopNotify` per characteristic and any report `push()`
  dropped for want of a subscriber are logged in `/tmp/couch-bt-hid.log`,
  because a TV that pairs but never subscribes looks, from outside, exactly
  like keys being ignored.
- **State file** `/tmp/couch-bt-pair.state` (format and parser in the crate's
  lib, `PairStatus`): line 1 `<phase>[ <detail>]` with phase
  `idle|pairing|connected|paired|done|failed` (detail = the peer's name, or
  `timeout`/`cancelled`); then, each optional and in any order, `link <ADDR>
  <name>` while a TV is connected (`PairStatus.link`, a `Peer { address,
  name }`; `peer` keeps the label for older readers), `active <ADDR>` when
  a bond is the active one (`PairStatus.active`), and `peer <ADDR> <name>`
  after a window ended in `done`, naming the TV that bonded
  (`PairStatus.bonded`), until the next `pair`. The parser still reads the
  older `link <name>` (a peer with an empty address) and skips lines it does
  not know. The daemon rewrites the file on every poll while a window is
  open, so readers treat a window phase older than `PAIR_STALE_SECS`
  (window + 15 s) as idle: the daemon died. `done` and `failed` are final
  until the next `pair`; turning Bluetooth off removes the file.
- **Readers.** `couch_system::ui_settings::bluetooth_pairing()` and
  `bluetooth_peer()`; the GUI's Settings › Bluetooth panel (row value
  `ON · <peer>`, a second row **Pair with TV** while on, and a modal that owns
  OK = `enter` and Back = `pair-stop`, polled every 250 ms while shown); the
  web Remote page's Bluetooth section and `GET /api/remote/device`
  (`bluetooth.pairing {phase, detail}`, `bluetooth.peer`) with
  `POST /api/remote/bluetooth {"action": pair|stop|forget|enter}`.
- **Why the keyboard report.** An LG webOS TV completed LE Secure Connections
  pairing and bonding, read the HID service, then showed "press any key on the
  bluetooth keyboard" and dropped the link after a while; every key sent
  during that prompt was a consumer-page report (id 2). The prompt is expected
  to want a keyboard-page report (id 1), which is what OK sends in pairing
  mode. Hardware status is in the staging checklist below.

## Bonds and the active link

BlueZ keeps one bond per TV and nothing stops several from existing; what
it cannot do is pick which one talks to us. Its GATT server notifies every
subscribed client, and a TV is the one that initiates the connection. So
the daemon enforces the single link at the controller, and everything else
follows from that.

- **Active bond.** `activate <ADDR>` (or the TV that bonds in a window)
  makes one bond the active link. The daemon disconnects any other
  connected TV (`Device1.Disconnect`) and advertises for that address
  alone: `LE Clear White List` (OCF 0x0010), `LE Add Device To White List`
  (0x0011, the device's `AddressType` then the address little-endian),
  then `LE Set Advertising Parameters` with **filter policy 0x03** (scan
  and connect requests from the white list only), the non-discoverable
  data (flags `0x04`), the scan response, and enable. A connect request
  from anyone else is dropped by the controller on air; bluetoothd never
  sees it (`hcidump -R`: the parameters command is `< 01 06 20 0F …` and
  its byte 14 is the policy). The bonded TV reconnects by itself, as it
  did with the open advertisement. `activate none`: no advertising at all
  (nobody bonded may connect, so nothing is on air) and any link is
  dropped.
- **Raw HCI, on purpose.** bluetoothd's managed advertisement (the 4.4
  core's `Add Advertising`) has no filter-policy field, and the core
  re-issues `LE Set Advertising Parameters` with policy `0x00` whenever it
  touches the instance (on register, and `mgmt_reenable_advertising` after
  every disconnect). So while a bond is active there is **no managed
  instance**: the daemon unregisters it and drives the controller with the
  raw commands above, on both kernels. The managed advertisement is used
  for pairing windows only, where the policy should be open anyway. The
  controller stops advertising when the TV connects and the kernel restarts
  nothing it did not start, so the daemon restarts the raw advertisement
  itself: the device poll (every 2 s idle, 500 ms in a window) sees
  `Device1.Connected` go false and re-runs the sequence, and a 15 s tick is
  the backstop for an enable the controller refused because the disconnect
  had not finished.
- **The kernel and the white list.** The 4.4 core rewrites the white list
  only when it starts its own LE passive scan (`hci_req_add_le_passive_scan`
  → `update_white_list`, `net/bluetooth/hci_request.c`), which
  `__hci_update_background_scan` runs only while `pend_le_conns` or
  `pend_le_reports` is non-empty, i.e. when bluetoothd has asked it to
  auto-connect to or report a device. A peripheral-only bluetoothd adds
  none, and with both lists empty the function only ever *disables* a scan.
  The daemon nevertheless clears and rewrites the entry at every (re)start
  of the advertisement, so a rewrite would cost one reconnect at most.
- **Persistence.** The active address is the daemon's own file,
  `/opt/couch/bluetooth-active` (`ACTIVE_PATH`; from outside Alpine,
  `/mnt/alpine/opt/couch/bluetooth-active`): one line, the address or
  `none`. It is read at start, so a reboot or a toggle restores the same
  link with nothing re-sent. Without the file (a remote from before bonds
  were chosen), the one bond there is becomes the active one; with several,
  none is, and the app chooses. Turning Bluetooth off leaves the file.
- **Address types.** The white list matches on address *and* type. The
  entry takes the type from bluetoothd's `Device1.AddressType`; a central
  using resolvable private addresses (a Mac, a phone) rotates its address
  every few minutes and the entry stops matching then. TVs use public or
  static addresses. A bond bluetoothd does not know is white-listed as
  public and nothing connects until it pairs.
- **Windows and the active bond.** A window forgets nothing and does not
  change the active bond unless it ends in `done`, in which case the new
  TV becomes it. Bonded TVs are dropped on sight during the window (above).

## Implementation plan (branch `bluetooth`, rebased onto `dev` 2026-09-14)

The first milestone is the spike: prove the radio through Linux's virtual HCI
device with BlueZ on top. Everything below it needs a kernel with `CONFIG_BT`
and `CONFIG_BT_HCIVHCI`. Since [boot image updates](runtime-updates.md#boot-image-updates)
that kernel reaches a remote on the Dev channel as the boot payload of a dev
build, no reinstall; only the BlueZ packages still wait for a new OS image and
are `apk add`ed over SSH on the development remote meanwhile.

**What is on this branch**

- `clients/couch-bt`: `couch-bt-bridge`, the daemon that shuttles H4 packets
  between `/dev/vhci` and `/dev/stpbt`. It reassembles the STP side's byte
  stream into whole frames (vhci requires one packet per write), creates the
  virtual controller explicitly (events written before it exists are
  refused), handles the STP whole-chip reset errnos (88 waits, 99 reopens),
  and can send the set-address vendor command first so `hci0` carries the
  owner's recorded `bluetooth_mac`. The opcode defaults to MediaTek's 0xFC1A
  and is a flag because the HA100 has not confirmed it. Host-testable: the
  framer and the pump are exercised with socket pairs standing in for the two
  devices. Static musl, one dependency (`libc`, for `poll`), like
  `couch-voice`.
- `kernel/couch-ha100.config`: `CONFIG_BT=y`, `CONFIG_BT_HCIVHCI=y`, every
  other Bluetooth profile and transport explicitly off. This changes the
  config hash `kernel/release-pin.json` pins, so a Bluetooth kernel is a new
  candidate through `docs/kernel-release-candidate.md`, not a drop-in. Built
  clean on Ollie 2026-09-14 from the pinned source commit (`ea122a39`, the
  kernel repository's `bluetooth` branch adds only documentation on top of
  it) into `~/couch-kernel/out-bluetooth`; `kernel/release-pin.json` on this
  branch pins that build (zImage `9dd8e84c…`, effective config `1568fe8c…`)
  so `prepare_public_boot.py` exports it. `CONFIG_BT` alone pulled no extra
  symbols in; `olddefconfig` kept the rest of the config byte for byte.
- `tools/provision-alpine.sh`: `bluez` and `bluez-deprecated` join the image
  package list, for `bluetoothd`, `btmgmt`, `hciconfig` and `hcitool`.

**Milestones**

1. *Spike kernel.* Dev build `.140.dev` carries the kernel above as a boot
   image payload: install the runtime, then the boot image the Updates page
   offers after the reboot, then over SSH `apk add bluez bluez-deprecated`,
   `scp` the `couch-bt-bridge-armv7` release asset to `/opt/couch/couch-bt-bridge`
   and run it. Acceptance: `/dev/vhci` and `/dev/stpbt` exist, the bridge
   creates `hci0`, `hciconfig hci0 up`, `btmgmt info` reports LE,
   `hcitool lescan` sees nearby advertisers, and the bridge log shows the
   set-address command answered (or names the opcode that must replace 0xFC1A).
   Done 2026-09-14 (`hci0` up, LE scan sees advertisers). The system service
   starts and stops `couch-bt-bridge` for the **Bluetooth** toggle in Settings
   and on the web UI's remote page (`Request::Bluetooth`, setting `bluetooth=`
   in `settings.conf`, off by default).

   **Wi-Fi and Bluetooth share one combo radio and STP transport.** The first
   spike that took Wi-Fi down (2026-09-14) did so because the bridge was not a
   singleton and started at boot racing Wi-Fi: two bridges on the un-guarded
   `/dev/stpbt` desynced STP, which hit its retry limit and reset the whole
   chip, dropping Wi-Fi. The fixes (all userspace): the bridge takes a
   whole-file lock so only one ever runs; it opens the devices non-blocking and
   backs off on a full transport (ENOSPC) instead of dying and wedging in an
   uninterruptible read; and it is no longer started from `stage2.sh` at boot,
   only by the toggle, by which time Wi-Fi is up. No kernel change was needed;
   stock Android runs both together on this chip. The bridge ships in the
   runtime bundle from .143 (updaters from .142 accept extra top-level
   `couch-*` executables), with a copy in the boot ramdisk as a fallback; the
   service prefers the runtime copy, so bridge fixes are runtime-only updates.
   The kernel still has to go through the candidate checks before promotion.

   Follow-up hardening: if a chip reset ever does fire, re-run DHCP on `wlan0`
   so Wi-Fi re-associates without a reboot.
2. *Power and coexistence.* **Measured 2026-09-14** on the .144 kernel,
   unplugged, screen asleep, over Wi-Fi (fuel gauge `current_now`; the averaged
   field reads 0 on battery). Idle draw was ~110 mA screen-off with Bluetooth
   off, and unchanged with Bluetooth on and the controller idle: the idle radio
   cost is below the gauge's resolution (~5 mA quantization, ~50 mA background
   swing). Wi-Fi throughput (iperf3): ~30 down / ~37 up Mbit/s with BT off; the
   same with BT enabled but the controller down (the toggle's actual state) or
   up-idle (down unchanged, up dips to ~27); it drops to ~13 down / ~23 up only
   during a *continuous* LE scan. Takeaway: enabling Bluetooth costs no
   measurable idle power and does not hurt Wi-Fi; the shared radio time-slices
   Wi-Fi only under sustained BT activity. A HID peripheral advertises and holds
   a low-rate connection rather than scanning, so its expected impact is small;
   confirm once HID lands, and prefer duty-cycled advertising over any scanning.
3. *HID over GATT.* **In progress (`clients/couch-bt-hid`, 2026-09-14).** A
   separate daemon on zbus (bluer was rejected: it needs libdbus, a C lib).
   It registers a HID-over-GATT application (Device Information, Battery, and
   HID with a keyboard + consumer-control report map) via bluetoothd's
   GattManager1, runs a just-works agent, and advertises "Couch Remote" over
   raw HCI (this 3.18 kernel has no MGMT advertising, so LE Set Advertising
   Parameters/Data/Enable go out through hcitool). Confirmed on the .144 remote:
   the app registers, all advertising commands return success, the daemon
   advertises. Remaining: first real TV pairing (the controller address is the
   synthetic 00:00:46:65:80:01 until the set-BD_ADDR vendor command is
   confirmed), sending a key on connect, then wiring startup (dbus + bluetoothd
   + bridge + this daemon behind the toggle; add dbus to the OS image) and a
   dev build. Original notes:
   A second daemon (or the same one grown) that registers the
   HID service with BlueZ over D-Bus, with the keyboard and consumer-control
   report map, and drives advertising through raw HCI since 3.18 has no
   `Add Advertising` management command. First pairing with a real TV; record
   which pairing method each target accepts.
4. *Switching.* One bond per target, an activity references its bonded
   address, and an activity switch disconnects and directed-advertises to the
   new target. GUI: pair-new-device flow and per-activity target picker.
5. *Proper driver.* Replace the bridge with an in-kernel `hci_dev` over STP,
   per the kernel repository's task list, once the spike has proven the radio.

## Staging checklist

Kernel (couch-kernel `bluetooth` branch, built on Ollie):

- [x] `CONFIG_BT=y`, `CONFIG_BT_HCIVHCI=y` in `couch-ha100.config`; keep
      `BT_RFCOMM`, `BT_BNEP`, `BT_HIDP` off unless a profile needs them
      (on this branch; the candidate still needs building and re-pinning).
- [x] Build `normal` (Ollie `out-bluetooth`, 2026-09-14; pinned on this branch).
- [x] First boot on the HA100 through the `.140.dev` boot image; `/dev/vhci`
      and `/dev/stpbt` both appear; Wi-Fi, IR, display, keys and keypad wake
      validated on the unified kernel (`06b21c74`, `.144.dev`).
- [ ] Later: in-kernel STP HCI driver replacing the bridge daemon.

Userland (this repo):

- [x] Add `bluez` (and `bluez-deprecated` for `hciconfig`/`hcitool`) to the
      image package list (`tools/provision-alpine.sh`; takes effect in the next image).
- [x] Bridge daemon between `/dev/vhci` and `/dev/stpbt` (`clients/couch-bt`).
- [x] Spike acceptance (2026-09-14): `hciconfig hci0 up`, `hcitool lescan` sees
      advertisers. (Alpine bluez has no `btmgmt`; used `bluetoothctl`/`hciconfig`.)
- [x] Program a stable address at bring-up (2026-09-15): the Wi-Fi MAC plus
      one through vendor command 0xFC1A, before bluetoothd; the identity
      record's `bluetooth_mac` is not needed for this.
- [x] HID-over-GATT peripheral (2026-09-14, `clients/couch-bt-hid`): GATT HID
      service via bluetoothd GattManager1, keyboard + consumer-control report
      map, advertising through bluetoothd's `LEAdvertisingManager1` where the
      kernel's Bluetooth core has it and raw HCI where it does not.
- [x] First pairing with a real TV (2026-09-14): just-works pairing accepted;
      after connect, a consumer volume-up report changed the TV volume. Paired
      with the synthetic controller address 00:00:46:65:80:01 (set-BD_ADDR not
      needed for this TV). Record other targets as they are tried.
- [x] Toggle brings up the whole stack (`couch_system::bluetooth`), publishes
      starting/on/off/error in `/tmp/couch-bt.state`; the GUI row, web page
      and API show it (`.145.dev`, `.147`).
- [x] Key routing: `Provider::BluetoothTv` / `Integration::BluetoothTv`; the
      GUI's mapped-button executor and the one-way TV screen send the
      function id as a datagram to `/tmp/couch-bt-hid.sock` (mode 0600).
- [x] Power validation (2026-09-14, `.144.dev`, unplugged, screen off):
      ~110 mA idle with or without Bluetooth on; Wi-Fi throughput unchanged
      with Bluetooth on and idle, halved only during a continuous LE scan.
- [x] Pairing mode (2026-09-15): `pair`/`pair-stop`/`forget` words, keyboard
      words, the 120 s pairable + discoverable window, non-discoverable
      advertising outside it, `/tmp/couch-bt-pair.state`, the Settings modal,
      the web section and `POST /api/remote/bluetooth`
      ([details](#pairing-mode)). Verified on the dev remote (.155.dev/.156.dev,
      backported core): window opens with Discoverable and Pairable on and the
      advert re-registered discoverable, `pair-stop` → `failed cancelled`,
      timeout → `failed timeout`, both edges back to non-discoverable, `enter`
      with no subscriber logged as a dropped keyboard report, `forget` empties
      the device list, Wi-Fi unaffected. bluetoothd re-applies its main.conf
      `Pairable` default when the adapter finishes starting, after the daemon's
      first Set, so the daemon re-asserts it on every poll (and main.conf now
      says `Pairable = false`). First TV attempt (LG OLED77G5) went over
      classic Bluetooth and failed, see "LE only" above; the LE-only
      controller is the fix (.157.dev). **Validated end to end with the LG on
      .157.dev**: window → connect → bond → TV subscribes → DONE, OK sends
      Enter, advert hidden afterwards, TV off/on reconnects, volume keys work.
- [x] Several bonds, one active link (2026-09-15, branch
      `bluetooth-bond-slots`): `activate <ADDR>`/`activate none`,
      `forget <ADDR>`, `pair` no longer forgets, the white-list advertisement
      with filter policy 0x03 over raw HCI, `/opt/couch/bluetooth-active`,
      state lines `link <ADDR> <name>`, `active <ADDR>`, `peer <ADDR> <name>`
      ([details](#bonds-and-the-active-link)). Which bond an activity or
      device picks is the app's side.
- [ ] Re-advertise immediately on disconnect. Done where bluetoothd exports
      `LEAdvertisingManager1` (the backported core, see
      [kernel backports research](kernel-backports-research.md)):
      `couch-bt-hid` registers an `org.bluez.LEAdvertisement1` object at
      `/couch/hid/adv0` instead of running the raw-HCI path, and bluetoothd
      restores the advertisement itself. The 3.18 core keeps the 15 s
      re-enable tick. Host-tested only; not yet run on a remote.

## Open questions

- Does the CONSYS BT function power on cleanly alongside Wi-Fi under the
  built-in WMT driver, or does it need the sleep/wake handling stock uses?
- ~~Which HCI vendor command programs the address on CONSYS_6580?~~ OGF 0x3f
  OCF 0x001a (0xFC1A), six bytes little-endian, effective after a down/up of
  hci0 (2026-09-15).
- Does each target (LG, Apple TV, Android TV, Fire TV) accept a BLE HID
  keyboard with legacy pairing? Record results here as they are tested.
