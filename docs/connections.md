# Connections and private settings

Add named connections in the web UI, then assign their devices in **Rooms & devices**.
Multiple Hue bridges, Home Assistant servers, Kodi players, LG TVs and Samsung TVs are supported.
A device keeps its upstream resource ID and a separate connection ID. The same
Home Assistant entity ID or Hue resource ID can appear on different servers.

Installed integration packages appear in the same **Add a connection** picker.
Creating one copies only its public manifest label, command capabilities,
input-discovery flag and native presentation recipe into `config.json`. Its declarative settings form appears
on the new connection's page. Text, integer, boolean and secret fields are
rendered by Couch; packages cannot inject JavaScript or markup into the editor.
Secret fields are blank after every load, say when a value is already saved,
and must be explicitly selected for clearing.

Infrared has one connection: the remote's built-in blaster. Each IR device in a
room chooses its own codeset. No external IR transmitter is required. Actual IR
sending/learning remains unavailable on the current kernel.

## Storage and migration

Private files are beside `config.json`, under `connections/<connection-id>/`:

- `hue-connection.json`: URL, application key, pinned certificate.
- `ha-connection.json`: server URL and access token.
- `webos-connection.json` and `webos-wake.json`: TV pairing and wake address.
- `androidtv-connection.json`, `appletv-connection.json`, `tizen-connection.json`: streaming TV pairings (Samsung's holds its token, pinned certificate and MAC).
- `kodi-connection.json`: bound host, HTTP port, username/password, control mode.
- `matter/`: the remote's own Matter fabric: CA and controller keys, node
  addresses and endpoint inventory. See [Matter devices](matter.md).
- `plugin-connection.json`: one external integration's manifest-defined
  settings. Secret values never appear in the settings response or house
  configuration.

Credentials are mode 0600 and absent from exported house configuration. The daemon
copies former singleton files into their original named connection on startup,
keeping the originals. `connection-legacy-map.json` prevents reassignment of a
legacy pairing after its connection is deleted. Retained credential directories
reserve their IDs; creating another connection with the same name gets a new ID.
Deletion is rejected while devices or scenes still reference the connection.

Provider operations use `/api/connections/<id>/<hue|ha|webos|kodi|androidtv|appletv|tizen|matter>/...`.
Legacy provider-only routes reject ambiguous requests when several connections
exist. Per-connection operation locks allow independent servers to operate at once.
Hue state and SSE subscriptions are separate per bridge; UI cache keys include the
connection ID. The upstream ID is stripped before sending any command.

External integrations use `GET /api/integrations` for the installed manifest
catalog and `/api/connections/<id>/plugin/{settings,status,inputs,action}` for
one connection. The action body carries one typed function string already
declared by the installed manifest. The physical-button, sequence and custom
page pickers use the cached capability labels; selectable inputs are loaded
from the package when requested. On the panel, commands travel over the
owner-only `plugin.sock` beside `config.json` to the daemon's integration host.
The GUI never starts a package process or reads its private settings.

A package may compose its on-screen controls from Couch's curated native
components: command groups, status text, boolean toggles and an input selector.
The browser and physical remote render the same manifest recipe with their own
built-in styling. Status and inputs come from the versioned protocol. Packages
cannot ship executable UI code, arbitrary HTML, JavaScript or Slint.

Removing or temporarily losing a package does not discard its connection,
devices or mappings. The cached manifest fields keep the configuration valid
and readable; settings and live controls remain disabled until a compatible
package is installed again. The first integration-capable core writes a
legacy-readable configuration projection for older rollback cores. New package
connections are inactive there; explicitly migrated Denon connections retain
their original native provider so older cores can still control them. See
[Denon migration](integration-migration.md) and
[configuration recovery](runtime-updates.md#integration-configuration-across-core-rollback).

## Kodi credentials

Create the Kodi connection with its host and TCP port, then open **Kodi web access**.
Enter HTTP port, username and password, choose control mode, and **Test & save**.
TCP retains push notifications and uses credentials for artwork only. Authenticated
HTTP uses the login for commands and artwork, with playback refreshes every five
seconds and immediately after commands. Kodi's TCP interface does not authenticate.

A blank password field preserves an existing login. **Use an empty password**
explicitly clears it. Failed tests leave saved credentials intact. Changing the
host invalidates the old login. Reopen a running activity after changing its login
or transport. See [Kodi services](https://kodi.wiki/view/Settings/Services/Control).

## Validation

Run `cargo test` in model, clients, daemon and ui. Daemon tests cover credential
privacy, rejected login preservation and legacy migration; model tests cover
repeated server resource IDs and one blaster with multiple codesets.
Browser/device fixture checks additionally cover two bridges, HA servers and TVs,
custom authenticated Kodi playback/artwork, and creating multiple named connections.
Integration tests cover manifest validation, private setting redaction, adapter
dispatch and missing-package configuration round trips.
