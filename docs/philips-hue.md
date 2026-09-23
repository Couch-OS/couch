# Philips Hue package

Philips Hue is an independently installed protocol 3 integration package. The
core image contains the package host and native light/scene controls, but no
Hue client or Hue-specific HTTP API. Package source and releases live in
`Couch-OS/couch-integration-hue`; the official integration repository offers
it on the preview channel.

One connection represents one local Hue bridge. The package pairs through the
standard Couch pairing dialog and exposes three child kinds:

| Kind | Resource ID | Couch control |
| --- | --- | --- |
| Hue light | `<light uuid>` | light |
| Hue room | `room/<grouped-light uuid>` | light |
| Hue scene | `scene/<scene uuid>` | scene |

The only public setting is the bridge address. Couch stores the Hue application
key and the exact TLS certificate separately in
`connections/<id>/plugin-credential.json`, mode `0600`, and passes them to the
package during configuration. They never enter the exportable house document,
process arguments, or environment.

## Adding a bridge

Open **Integrations**, install **Philips Hue** from the preview repository, and
then open **Connections**. Add the installed Philips Hue integration, enter the
bridge address, start pairing, and press the bridge link button while the Couch
dialog is waiting. After pairing, add its lights and rooms from **Rooms &
devices**; add bridge scenes from the same child picker.

The package trusts the selected bridge certificate during that physical
link-button pairing and pins the exact DER certificate. A changed certificate
requires pairing again. The package proves the issued application key with an
authenticated request before completing the dialog.

## Upgrade from the built-in integration

A saved provider with JSON kind `hue` loads as `Provider::LegacyHue`. Shortly
after startup, the daemon installs the official `hue` package if needed and
prepares the existing connection against it. Conversion is atomic: if the
package, settings, credential, or saved resources cannot be validated, the
legacy record remains intact and every surface says **Needs the Philips Hue
package**.

Successful conversion preserves the connection ID and name and performs these
rewrites in one configuration commit:

- the private built-in `hue-connection.json` supplies package setting `host`,
  credential `application_key`, and the base64-encoded pinned certificate;
- a light UUID remains unchanged and receives a saved `light` child snapshot;
- `room:<uuid>` becomes package resource `room/<uuid>` with a `group` child
  snapshot;
- a legacy `Scene.hue` becomes `Scene.resource` at `scene/<uuid>` of kind
  `scene`.

The old private file is left in place for rollback. Package settings and the
new credential are written before the configuration changes provider, so the
connection is never committed in an unpaired state. A fresh child listing then
heals conservative migrated light traits with the bridge's exact dimming,
colour-temperature, and colour capabilities.

## Package behavior and limits

The package uses the local Hue CLIP v2 API over pinned HTTPS. It lists lights,
Hue rooms, and scenes; zones are not group controls, though their scenes are
listed with the zone name as a room hint. Light power, brightness, and colour
temperature are supported when the light reports the corresponding traits.
XY colour writes remain unsupported.

State is maintained from the bridge event stream with polling recovery. A
status request reads the package cache rather than contacting the bridge.
Grouped-light writes are coalesced to Hue's one-command-per-second room rate;
single-light writes are sent directly.

## Validation

The package repository runs the shared protocol 3 children and pairing
admission harnesses against a real package subprocess and a loopback TLS bridge
fixture containing 48 lights, 14 rooms, and 181 scenes. It also builds the
static ARMv7 package binary with Couch's pinned toolchain.

The package was installed and exercised on an HA100 against a BSB002 bridge on
2026-09-19. The trial covered pairing, certificate pinning, discovery of all
243 children, light and room control, scene recall, event updates, restart,
replacement, uninstall, and rollback. The detailed trial receipt remains in
`docs/plans/hue-package-hardware-trial.md`.
