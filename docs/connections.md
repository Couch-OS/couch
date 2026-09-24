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
- `protect-connection.json`: NVR address, Integration API key, and exact API and
  media certificate pins recorded during enrollment.
- `ha-connection.json`: server URL and access token.
- `webos-connection.json` and `webos-wake.json`: TV pairing and wake address.
- `androidtv-connection.json`, `appletv-connection.json`, `tizen-connection.json`: streaming TV pairings (Samsung's holds its token, pinned certificate and MAC).
- `kodi-connection.json`: bound host, HTTP port, username/password, control mode.
- `matter/`: the remote's own Matter fabric: CA and controller keys, node
  addresses and endpoint inventory. See [Matter devices](matter.md).
- `plugin-connection.json`: one external integration's manifest-defined
  settings. Secret values never appear in the settings response or house
  configuration.
- `plugin-credential.json`: the key a package's device handed over when it was
  paired (protocol 3, unreleased, so never in a shipped build). Couch never
  looks inside it, never sends it over HTTP and never exports it; it goes back
  to the package when its child is started, and nowhere else. Beside it,
  `plugin-pairing.json` holds the one line a page shows about the pairing and
  which package it was made for (`{"summary": …, "paired_at": …, "package": …}`),
  which is not a secret. A key file that cannot be read, or that was made for
  a package this connection no longer uses, counts as no key at all: the
  connection says it needs pairing rather than refusing everything.

Credentials are mode 0600 and absent from exported house configuration. The daemon
copies former singleton files into their original named connection on startup,
keeping the originals. `connection-legacy-map.json` prevents reassignment of a
legacy pairing after its connection is deleted.

## Removing a connection

Deletion is rejected while devices or scenes still reference the connection.
Removing a connection (`DELETE /api/connections/<id>`, **Remove connection** on
its page) also removes `connections/<connection-id>/` and everything in it: the
pairing, keys, tokens, passwords, a package's settings and a package's pairing
key and summary. A package connection's
running child is stopped first. To use the same bridge, server or TV again, add
a connection and pair or sign in again; a new connection with the same name gets
the same ID and starts with nothing saved.

The order is fixed: the configuration is saved first, and only a saved deletion
removes anything. A deletion that is refused (devices still assigned, a stale
revision, the connection busy pairing or answering a request) removes nothing. A
power cut between the two steps leaves a folder that no connection reads.

What is deliberately not removed:

- **Matter.** A Matter connection's folder holds the remote's own fabric: the CA
  and controller keys every paired device trusts, which cannot be issued again.
  Deleting a Matter connection leaves its folder alone, and a `matter/` folder
  found under any other connection is kept too. **Forget device** on the
  connection's page is how a paired device is released.
- **Whole-configuration changes.** `PUT /api/config` and `POST /api/config/reset`
  never remove private files, so importing a backup that drops a connection and
  later one that brings it back keeps its pairing.
- **Files outside `connections/`**: the former singleton files kept for
  rollback, `connection-legacy-map.json` and the household Sonos API key.

A folder that is still there (a Matter fabric, a connection dropped by an import,
a deletion made by an older release) reserves its ID: creating another connection
with the same name gets a new ID and never inherits what is in it. Nothing
removes such folders automatically, at startup or otherwise.

Provider operations use `/api/connections/<id>/<hue|ha|webos|kodi|androidtv|appletv|tizen|matter>/...`.
Legacy provider-only routes reject ambiguous requests when several connections
exist. Per-connection operation locks allow independent servers to operate at once.
Hue state and SSE subscriptions are separate per bridge; UI cache keys include the
connection ID. The upstream ID is stripped before sending any command.

External integrations use `GET /api/integrations` for the installed manifest
catalog and `/api/connections/<id>/plugin/{settings,status,inputs,action}` for
one connection. The action body carries one typed function string already
declared by the installed manifest. A connection whose package offers devices
of its own (protocol 3, unreleased, and so never in a shipped build) also has
`/plugin/children`, `/plugin/children/refresh` and
`/plugin/children/<id>/{status,action,typed-action}`, where `<id>` is the rest
of the path up to the verb; every one of them answers 404 "This integration
does not list devices" for a package that offers none. See
[listing the children of a connection](development/protocol.md#the-daemon-listing-the-children-of-a-connection).

A package that describes how it pairs (protocol 3, unreleased, and so never in
a shipped build) also has `POST /plugin/pair`, `POST /plugin/pair/<session>`,
`DELETE /plugin/pair/<session>` and `DELETE /plugin/credential`; all four
answer 400 "This integration does not pair" for a package that does not, which
is every package this build can run. The package describes one step at a time -
press the button, approve on the device, type this code - and Couch draws the
dialog and keeps the key in `plugin-credential.json`. A device that will not
answer without a key is refused `unpaired` before its package is even started.
Pairing again runs in a second child of the package, so the connection keeps
working on the key it already has until the new one succeeds; a pairing that
fails, is cancelled or runs out writes nothing and leaves the old key alone.
**Forget pairing** (`DELETE /plugin/credential`) removes Couch's copy of the
key, the line beside it and anything a half-finished write left behind; it says
nothing to the device, which may still list Couch as paired. See
[pairing a connection](development/protocol.md#the-daemon-pairing-a-connection). The physical-button, sequence and custom
page pickers use the cached capability labels; selectable inputs are loaded
from the package when requested. On the panel, commands travel over the
owner-only `plugin.sock` beside `config.json` to the daemon's integration host.
The GUI never starts a package process or reads its private settings.

A refused request answers with `{"error": "<sentence>", "code": "<code>"}`. The
sentence is what a page shows; `code` is the integration protocol's error code
(`invalid`, `unsupported`, `busy`, `expired`, `rejected`, `timeout`, ...). A
package that speaks protocol 3, which no released Couch loads yet, may add a
`reason`; one of kind `invalid_setting` names a setting, and the settings form
marks that setting with the package's words. See
[what reaches the panel and the web page](development/protocol.md#what-reaches-the-panel-and-the-web-page).

A package may compose its on-screen controls from Couch's curated native
components: command groups, status text, boolean toggles and an input selector.
The browser and physical remote render the same manifest recipe with their own
built-in styling. Status and inputs come from the versioned protocol. Packages
cannot ship executable UI code, arbitrary HTML, JavaScript or Slint.

Removing or temporarily losing a package does not discard its connection,
devices or mappings. The cached manifest fields keep the configuration valid
and readable; settings and live controls remain disabled until a compatible
package is installed again. The first integration-capable core writes a
legacy-readable configuration projection for older rollback cores. Package
connections are inactive there, including a Denon connection that was converted
from the built-in client; one that has not been converted yet is written as
before. See
[Built-in integrations that became packages](integration-migration.md) and
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
