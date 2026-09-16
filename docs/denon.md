# Denon AVR integration

The `clients/couch-denon` Rust crate implements Denon's native CR-delimited TCP
control protocol on port 23. It supports main-zone power, mute, volume in 0.5 dB
steps, source selection/discovery, status queries and unsolicited updates.
No commands are automatically retried. Absolute commands require matching
readback; extension messages such as `MVMAX` are not mistaken for volume.

Add **Denon AVR** in **Connections**, enter a name, address and port, then use
**Test connection / refresh**. This reads status and discovers the receiver's
renamed input labels. Add the receiver from its connection inside a room.
Its web controls offer power, volume, mute and input selection. Activity mappings
can route volume, mute and main-zone power to any named Denon connection.
A dedicated on-device AVR screen is not included; activity mappings supply its
physical controls.

Enable **Network Control / Always On** in the receiver's network settings for
access during standby. Power controls affect the main zone, not other zones.
Denon volume is shown in dB; the receiver's minimum sentinel remains distinct
from an ordinary numeric volume. Device configuration contains the receiver's
address, without inventing credentials for this unauthenticated LAN protocol.

From the clients workspace:

```sh
cargo test -p couch-denon
AVR_IP=avr.local
cargo run -p couch-denon -- "$AVR_IP" status
cargo run -p couch-denon -- "$AVR_IP" sources
cargo run -p couch-denon -- "$AVR_IP" watch
```

The physical AVR test answered native status and input-name queries. Protocol
fixtures cover fragmented frames, unsolicited updates and
half-step volume encoding. GUI fixtures exercise physical volume and mute
commands without changing the production AVR's state.

Protocol reference: [Denon Ethernet/RS-232 specification](https://downloads.denon.com/documentmaster/us/avr3313ci_avr3313_protocol_v04.pdf).
Model-specific source IDs vary; discover them rather than assuming the renamed
input's display label is its command token.

The external-package pilot, its ownership handoff, and its dB/status limits are
documented in [External integration migration](integration-migration.md).
