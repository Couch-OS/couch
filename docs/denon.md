# Denon AVR integration

Denon support is an integration package. Its source, tests and releases live in
[`Couch-OS/couch-integration-denon`](https://github.com/Couch-OS/couch-integration-denon);
the signed package is published in the Couch package feed as
`couch-integration-denon`. The OS no longer contains a Denon client: it carries
the host that runs the package and nothing that is specific to a receiver.

## Set up a receiver

1. Open **Integrations**, choose **Refresh packages**, and install **Denon AVR**.
   The remote needs internet access for that one step.
2. Open **Connections**, choose **Denon AVR · installed package**, and enter a
   name, the receiver's address and its port (23 unless you changed it). Before
   the package is installed the picker still lists **Denon AVR · integration
   package**, which leads to the Integrations page.
3. Add the receiver from its connection inside a room.

The connection page offers **Refresh status**, the package's commands, the dB
volume target and input selection. On the remote, the room row answers the
volume, mute and power keys and opens the core control screen; activity
mappings can route volume, mute, main-zone power and inputs to any named Denon
connection. Activities discover the receiver's renamed input labels.

Enable **Network Control / Always On** in the receiver's network settings for
access during standby. Power controls affect the main zone, not other zones.
Denon volume is shown in dB; the receiver's minimum sentinel remains distinct
from an ordinary numeric volume. The package's private settings contain the
receiver's address and nothing else: this LAN protocol has no credentials.

## Connections from before the package

A Denon connection saved while the client was part of Couch converts by
itself. After the update the remote installs the package from the feed and
switches the connection over in place; rooms, activities, shortcuts and button
assignments stay attached, and the address is carried over. Without internet
the connection reads **Needs the Denon package** in the web UI and on the
remote, and the remote keeps trying. See
[Built-in integrations that became packages](integration-migration.md).

## Working on the client

Protocol behaviour (the CR-delimited TCP control protocol on port 23,
half-step volume encoding, unsolicited updates, no automatic retries, readback
of absolute commands), its fake-receiver tests and its command-line tool are
documented in the package repository. Changes to how Couch talks to a receiver
go there, followed by a feed pin; see
[Integration packages](integration-packages.md). Changes to how a receiver is
drawn or keyed on the remote belong here, because a package declares data and
Couch draws it.

Protocol reference: [Denon Ethernet/RS-232 specification](https://downloads.denon.com/documentmaster/us/avr3313ci_avr3313_protocol_v04.pdf).
Model-specific source IDs vary; discover them rather than assuming the renamed
input's display label is its command token.
