# External integration migration

Denon is the protocol-v1 pilot for moving a built-in network integration to an
independently installed package. This is an explicit, per-connection choice.
Installing a package does not convert a built-in Denon connection, its devices,
or its activity bindings. There is no bulk or automatic conversion.

## Denon pilot boundary

The standalone `couch-denon` 0.1.1 source at `828719e` has the same AVR
transport and SDK adapter as `clients/couch-denon`: main-zone power, relative
volume, mute (including the read-before-write toggle), source enumeration and
selection, confirmation of absolute commands, and no automatic retries. Its
`host` and `port` settings use the same validation and default port.

The owner changes at conversion time. A built-in connection is owned by
`couch-control`, which shares one AVR socket between the daemon and panel. A
package connection is owned by the integration host and its one persistent
package process. Do not leave a built-in Denon connection and its replacement
package connection enabled for the same receiver endpoint: that would create
two independent AVR TCP owners. The migration keeps the same connection ID,
changes its provider, drains the old owner, and rejects stale native requests.
Devices and bindings do not need to be reassigned. The on-disk legacy projection
contains the original native provider; a rollback core can use it directly.

The package stores its settings in the private connection store. Hostnames and
ports are not credentials, but the conversion still must copy them into the
new package settings only after the package is installed, validated, and ready
to own the connection. A failed or cancelled conversion leaves the original
built-in connection and bindings unchanged.

## Try or restore a named connection

On an integration-capable development runtime, install Denon from the signed
Preview feed, then open **Integrations → Denon migration pilot**. Review the
limitations below, choose **Switch to Denon package**, then **Confirm switch**.
No receiver command is sent by the conversion itself. The installed package is
verified and its settings are validated before the configuration changes.

**Restore built-in control** stops the package connection before restoring its
original host and port. This works even if the package is missing. Both actions
check the configuration revision and are safe to repeat; a stale browser must
refresh before trying again. A failed preparation leaves native configuration
authoritative, with any prepared private settings retained for a safe retry.

This pilot handles named Denon connections. Duplicate native targets, including
inline device targets, must be consolidated before migration. Different names
or aliases that resolve to the same receiver cannot be detected reliably; do
not configure both as active owners.

While migrated, restore built-in control before editing the receiver address,
deleting its connection, or importing a configuration that changes migration
receipts. Unrelated configuration edits remain available. A changed native
address can conflict with retained package settings on a later migration; the
pilot rejects that mismatch instead of silently overwriting saved settings.

If an older runtime saves an edit during core rollback, that native configuration
remains authoritative on re-upgrade. The receiver does not silently switch back
to the package. The original package settings remain private and reusable.

## Known parity limits

Protocol v1 represents volume only as a 0--100 percentage. Denon reports dB,
so its SDK `Status` correctly leaves `volume` absent rather than inventing a
percentage. The built-in browser controls and physical activity feedback show
the native `volume_db` reading, including the distinct minimum-volume
sentinel. The external package retains relative volume controls but cannot show
that dB reading through protocol v1.

The built-in HTTP endpoint also accepts an absolute dB command. It is not
offered by the browser controls or by physical button mappings, which expose
only relative volume for Denon. A protocol-v1 package cannot provide that raw
API operation because a package command is a declared function string with no
typed value. Treat both the dB readout and raw absolute dB endpoint as known
losses for this preview pilot; do not describe it as complete user-interface
parity.

Some Denon source tokens can contain spaces. The AVR encoder and Denon SDK
accept those tokens, but the current global persisted `input:<id>` parser does
not. Source discovery can therefore display an input that cannot be saved as a
binding. This predates package migration and affects both paths. Preserve
existing parseable bindings such as `input:SAT/CBL`; resolve the grammar in the
model and package host together before claiming complete input parity.

## Validation and hardware scope

Host tests cover the package subprocess boundary, capability refusal before
device I/O, malformed and disconnected replies, timeout-without-retry, bounded
queue pressure, and a valid persisted source binding. They do not establish AVR
hardware behavior. The allowed hardware check for this pilot is read-only
status and source enumeration. Do not use power, volume, mute, or input changes
as migration validation without separate authorization.

## Future protocol work

Keep protocol v1 unchanged for the pilot. A future negotiated protocol version
can add an optional AVR measurement without changing the meaning of percentage
`volume`: use a bounded integer tenths-of-a-dB field plus an explicit
minimum-volume flag, rather than a floating-point value or a fabricated
percentage. A manifest that uses it must declare a minimum core protocol
version, and an older core must leave that package connection inactive while
preserving its configuration.

Absolute dB control needs a separate typed-command contract: a declared action
parameter schema with a bounded integer tenths-of-a-dB value and unit. It must
be negotiated with the same core minimum, tested for refusal before I/O and
timeout ambiguity, and rendered only by cores that understand it. Do not encode
the value into a free-form function string or overload the existing percentage
volume field.
