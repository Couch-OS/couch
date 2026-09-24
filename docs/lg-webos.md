# LG webOS TV

LG webOS support is distributed as the independently versioned `webos`
integration package. The core image supplies the package host and native TV
controls, but no longer contains an SSAP client, pairing endpoint, or webOS
credential parser.

## Install and set up

Open **Integrations**, refresh the package catalog, and install **LG webOS**.
Then open **Connections**, add the installed LG webOS package, and enter the
TV's IPv4 address. The package accepts either that address or an explicit root
`ws://`/`wss://` URL, normalizes it, and asks the TV to approve pairing.

Encrypted `wss://IP:3001/` is the default. Pairing trusts and pins the TV's
certificate; later connections require the same certificate. The legacy
unencrypted transport is available only when an explicit `ws://IP:3000/` URL
is entered. There is no silent downgrade.

Add the resulting connection to a TV in **Rooms & devices**. The remote renders
the package's television profile with Couch's native controls, including the
D-pad, volume, mute, channel, playback, inputs, and apps declared by the
package. Physical buttons and touchscreen controls use the same package
connection and bounded command path. Commands are not retried automatically.

Package settings and credentials live in the connection's private store:

- `plugin-connection.json` contains the normalized URL setting.
- `plugin-credential.json` contains the client key and, for TLS, the pinned
  certificate.

Neither file appears in exported home configuration.

## Migration from the built-in client

An older configuration still deserializes its provider as `kind: "web-os"`,
but that provider is now inert. At startup, `couch-confd` obtains the official
`webos` package and converts the connection in place. The connection ID stays
the same, so rooms, activities, shortcuts, and button mappings continue to
refer to it.

The converter reads the old private `webos-connection.json`, maps `url` to the
package's public settings, and maps `client_key` plus the optional certificate
to its private credential. The old file is deliberately retained for rollback.
Until conversion succeeds, every surface reports **Needs the LG webOS package**
instead of trying the removed built-in transport.

The old `webos-power.json` and `webos-wake.json` sidecars are also left on disk
for rollback; the package does not read them. Power-on therefore needs a
separately configured core IR command or a package version that explicitly
declares `power-on`. The current package declares network power-off and never
guesses that an unreachable TV is off.

See [integration migration](integration-migration.md),
[connection storage](connections.md), and
[integration packages](integration-packages.md).

## Validation

The package repository owns SSAP transport and admission tests. Core validation
covers legacy JSON compatibility, private credential mapping, automatic package
adoption, native package presentation, and physical-key dispatch. Hardware
validation must still be repeated for a release that changes the package or the
core package host; a host build alone does not prove TV behavior.

## HA100 physical key map

Captured on the physical remote on 2026-09-09 (all on `mt_gpio_kpd`): Power
**60**, Home **59**, Mute **113**, Red **66**, Green **67**, Blue **68**, and
Yellow **87**. These differ from standard Linux color/power key codes. The GUI
reserves Slint F13–F18 for power, mute, and colors; Home uses `Key.Home`.
One-shot keys do not auto-repeat, while volume and directional keys do.

![TV screen using the system theme](webos-system-theme.png)
