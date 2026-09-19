# Built-in integrations that became packages

Integrations live in their own repositories and reach a remote through the
signed package feed; the OS carries only the host that runs them. Denon was the
first built-in integration to leave (`Couch-OS/couch-integration-denon`), and
what happens to a connection saved before it left is described here. The same
road is meant for the built-ins that follow.

## What a user sees

After the update that removes a built-in client, every connection of that kind
still exists and still has its rooms, devices, activities, shortcuts and button
assignments. Nothing drives it until it belongs to the package, and the remote
makes that happen by itself:

1. shortly after `couch-confd` starts, it looks for such connections;
2. if the package is not installed, it refreshes the repository indexes and
   installs it through the same package manager as **Integrations → Install**,
   but only from an official repository: official stable first, then official
   preview. Nobody asked for this install, so a repository the owner added is
   never used for it, even when it is the only one offering the package; the
   connection then keeps waiting and says that the official feed does not
   offer the package yet. A package the owner installed by hand, from any
   repository they trust, is used as it is;
3. it gives the package the address the connection carried, lets the package
   validate it, and switches the connection over in place.

The connection keeps its id and name, so nothing that points at it changes. No
command is sent to the device by the conversion. It is one way: there is no
built-in client to go back to.

While a connection is still waiting it reads **Needs the Denon package**:

- in the web UI on the connection's card and page (with the reason the last
  attempt gave, for example that the package feed could not be read, and a
  **Try again** button), on the Integrations page under **Connections waiting
  for a package**, and on its devices' cards;
- on the remote on the device's row in its room, and as a toast with the same
  words when that row is opened, a shortcut points at it, or one of its volume,
  mute or power keys is pressed. A device that also has infrared codes or a
  Bluetooth bond keeps working over those.

The usual cause of a wait is a first boot on which Wi-Fi is not up yet when
the daemon first looks, about a second after it starts. While the feed cannot
be reached the remote therefore looks again after 5, 10 and 20 seconds, and
only then settles into 30 s, 1, 2, 5 and 15 minutes and hourly; any other
failure starts at 30 s. It also tries when the Integrations page refreshes or
installs anything and when **Try again** is pressed, but never more often than
once in five seconds, however often it is asked.

Known limits:

- Two saved connections to the same receiver (an old-style one and a package
  connection made by hand, or two old-style ones) both end up as package
  connections. The receiver accepts one control connection at a time, so
  whichever is used second waits for the other to go idle. Remove the spare.
- A receiver named directly on a device, from before named connections, is
  given a connection first and then converts like any other. One saved without
  an address or with port 0 never worked and has nothing to hand to a package;
  it is left exactly as it is, reads **Needs the Denon package**, and does not
  stop the rest of the file loading. Delete the device, or add the receiver
  again as a package connection.

Adding a receiver afterwards is an ordinary package connection: the connection
picker keeps a **Denon AVR · integration package** entry that leads to
Integrations until the package is installed, and a new built-in connection is
refused by the API.

## How it is built

`couch_model::LEGACY_BUILTINS` (`model/couch-model/src/integration_migration.rs`)
is the table: for each departed built-in, the connection kind, the package id,
the package's name in a sentence, and the function that turns the saved
connection's fields into package settings. Denon's row maps `host` and `port`.

- **The file still loads.** `Provider::LegacyDenon` and
  `Integration::LegacyDenon` keep the `"denon"` tag on disk. They exist to read
  an older file and keep its saved commands valid; no code drives them. A
  variant is never removed: `Provider` and `Integration` are tagged enums with
  no fallback, and a file that does not parse is a daemon that does not start.
- **`Config::migrate`** gives a receiver named inline on a device (the shape
  from before named connections) a connection of its own, so that everything
  waiting for a package is a connection. An inline receiver without a usable
  address is skipped: a connection is validated for one, and a connection made
  from it would make the whole file invalid.
- **`Config::convert_legacy`** switches one connection to the package's
  `Provider::Plugin` snapshot and validates the whole document, so a package
  that lacks a saved command refuses the conversion and leaves the file as it
  was.
- **`couch-confd`** (`daemon/couch-confd/src/api/integration_migrations.rs`)
  runs the attempt on its own thread, through the same package manager and the
  same settings validation as the web UI. Starting the package and letting it
  check the address happens with the configuration unlocked; the lock is then
  taken only to confirm that the connection and the package selection are
  still the ones that were checked, save the settings and commit one
  configuration write per connection. `GET /api/integrations/legacy` reports what is waiting and
  why; `POST /api/integrations/legacy/retry` makes the next attempt immediate.

Adding the next built-in to the table takes a row, a `Legacy*` variant kept for
reading, and the removal of its client. The daemon, the web UI and the panel
need no further change.

## Core rollback

`config.json` still carries a projection an older runtime can read. An
unconverted connection is written exactly as before, so an older runtime that
still has the built-in client drives it. A converted connection is a package
connection like any other: a runtime with the integration host uses the
installed package, and one from before the host sees the device as
unconfigured rather than a malformed file
(`docs/runtime-updates.md#integration-configuration-across-core-rollback`).

## History: the reversible pilot

From `v0.1.0-alpha.20260916.171` until built-in Denon was removed, the move was
an explicit, per-connection choice on the Integrations page (**Denon migration
pilot**), reversible with **Restore built-in control**. The pilot recorded a
receipt (`denon_migrations`) beside each switched connection, blocked the
built-in TCP owner for that receiver so two owners never held one socket, and
warned that the protocol-v1 package could not show the dB level or set an
absolute volume. Protocol v2 closed both gaps (`VolumeDb` status and the typed
`set_volume_db` action, Denon 0.2.0 and later), which is what made removing the
built-in client possible.

What remains of it: a file the pilot wrote still loads. Its receipts are read,
because such a file only matches its own rollback projection with them, and
are dropped on the daemon's next start; the connection is already a package
connection and needs nothing else. The migrate / restore-native API, the pilot
section of the Integrations page and `couch_control`'s receiver hand-over are
gone.

One limit predates all of this and still applies to the shared input grammar
rather than to Denon: a source token containing spaces (`HD RADIO`) is only
bindable by a protocol-v2 core, which writes it in the v2 envelope
(`model/couch-model/src/storage.rs`).

## Validation and hardware scope

Host tests cover loading a built-in-era file, the inline and pilot shapes, the
in-place conversion with an installed package fixture (same id, rooms and
activities still resolving, address moved into private settings, no receiver
I/O), an unreachable feed with its quick first retries, **Try again** and its
five-second floor, the choice of repository (an owner's repository offering
the package is not chosen), a package that fails verification or changes
between the check and the commit, an inline receiver without an address, and a
failed configuration write. The browser tests cover
the waiting state on the Integrations page. None of this establishes behaviour
on a remote: the first conversion on hardware, including the package install
inside the Alpine root, is validated separately with a development build.
