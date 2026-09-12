# CoreELEC client

`clients/couch-coreelec` combines the existing Rust Kodi JSON-RPC client with optional CoreELEC OS management. It is a library and `couch-coreelec` command-line program. CoreELEC is selectable in the web UI’s Connections page, with Kodi playback on the remote and optional OS management in the browser.

## Supported behavior

| Interface | Features | Prerequisites |
| --- | --- | --- |
| Kodi JSON-RPC | Playback, navigation, now playing, volume, notifications and settings through the public `client.kodi` handle | Enable Kodi remote application control; CLI uses TCP 9090 by default |
| Discovery | Three-second IPv4 SSDP media-server search, capped at 64 candidates | Kodi UPnP sharing enabled; multicast reachable |
| CoreELEC SSH | Read OS version, architecture, project, optional device/build identifiers | Explicit SSH enablement, key authentication and verified known_hosts file |
| CoreELEC SSH | Read `kodi.service` load/active/substate; request Kodi restart, reboot or poweroff | Same SSH enrollment; disruptive CLI commands require `--confirm` |

Discovery advertisements are untrusted hints, and may include non-Kodi media servers. The client does not fetch advertised URLs or identify an OS from its name. Use a selected IP address and authenticated `identity` to establish CoreELEC identity. Disabled UPnP, multicast filtering or IPv6-only networks require manual addressing; SSH accepts IPv4 and IPv6 literals.

The library accepts an existing `couch_kodi::Kodi` handle, including an HTTP handle with explicitly supplied authentication. It exposes that handle rather than duplicating Kodi transport and playback policy. The CLI is a small TCP control interface; applications can use all existing Kodi settings, playback and notification APIs directly.

## Usage

```sh
(cd clients && cargo build --release -p couch-coreelec)
clients/target/release/couch-coreelec discover
clients/target/release/couch-coreelec 192.0.2.20 status
clients/target/release/couch-coreelec 192.0.2.20 play-pause
```

For OS access, enable SSH in CoreELEC Settings → Services, provision a dedicated key in the device's authorized keys, and verify its host-key fingerprint independently before putting that key in your known_hosts file. Keep both credential files outside Git. Encrypted private keys that require prompting are not supported by this unattended transport.

```sh
export COUCH_COREELEC_SSH_KEY=/absolute/private/path/coreelec_key
export COUCH_COREELEC_KNOWN_HOSTS=/absolute/private/path/coreelec_known_hosts
clients/target/release/couch-coreelec 192.0.2.20 identity
clients/target/release/couch-coreelec 192.0.2.20 service
clients/target/release/couch-coreelec 192.0.2.20 restart-kodi --confirm
```

Optional environment settings: `COUCH_COREELEC_KODI_PORT` (9090), `COUCH_COREELEC_SSH_PORT` (22), `COUCH_COREELEC_SSH_USER` (root). No password is embedded, inferred or tried. Key and known_hosts paths must be absolute existing files; paths containing quote, backslash, dollar, percent or newline characters are rejected. On Windows use forward-slash paths, for example `C:/Users/name/.ssh/coreelec_key`.

OS management calls a locally installed **OpenSSH-compatible `ssh` executable**; this is not a native Rust SSH implementation. Dropbear's client does not implement the required options. A Couch runtime package must include OpenSSH before exposing these OS controls. Kodi operations need no SSH executable. The process runs with ambient SSH configuration disabled, explicit identity, strict host-key checking, agent/password/interactive authentication disabled, and forwarding disabled. It never accepts an unknown host automatically.

Each SSH operation has a 12-second wall-clock deadline, five-second connection timeout and 64 KiB limit on each output stream. Timeout kills and reaps the local process. Error output is not copied into application errors. Only fixed remote commands are available; caller-provided text never becomes a shell command. Before service queries or mutations, a fixed guard checks the exact CoreELEC `ID` in `/etc/os-release`. The identity reader parses data without sourcing it.

A successful disruptive action means systemd accepted an asynchronous request, not that the reboot or restart completed. A lost SSH connection can leave the outcome unknown. The client never retries a mutation automatically. Poll identity/service/Kodi status separately to establish recovery.

## Scope and validation

This implementation does not install CoreELEC updates, alter boot selection, run `ceemmc`, write partitions, adjust device-specific Dolby Vision/CEC sysfs nodes, or claim wake capability. Those features depend on CoreELEC version, hardware and explicit policy. CoreELEC firmware updating is separate from Couch's own update channels.

Fixture tests cover CoreELEC release parsing, foreign/ambiguous identities, opt-in OS access, guarded commands, SSDP candidate parsing, bounded output, subprocess timeouts and error mapping. No physical CoreELEC device has been contacted. Physical validation should check enrollment failure, successful identity/service reads, Kodi controls, a requested Kodi restart and reconnection before enabling GUI actions.

## Couch UI flow

1. Open **Connections → CoreELEC**, name the connection, enter its address and Kodi TCP port, and save. Configure optional Kodi HTTP credentials for authenticated control/artwork in the same card.
2. Open **Rooms & devices**, select the room and CoreELEC connection, then **Add to this room**. The remote uses the existing Kodi player/navigation/metadata flow; CoreELEC can also be an activity source and mapped button target.
3. For optional OS control, expand **Enroll SSH access** in the connection card. Enter the dedicated private key and independently verified known_hosts entry, then **Verify & save SSH access**. Enrollment checks both SSH trust and CoreELEC identity before replacing saved access. Failed enrollment leaves prior credentials intact. Credentials live in private per-connection files, never the shared configuration export.
4. **Refresh OS status** displays release/architecture and Kodi service state. Select an OS action and tick the interruption confirmation before **Run OS action**. **Remove SSH access** deletes active enrollment; Kodi control remains available.

The daemon scopes these authenticated endpoints to `/api/connections/:id/coreelec/{connection,status,action}`. Changing a connection address invalidates its saved SSH enrollment. CoreELEC resolves to the existing Kodi integration for remote control, avoiding a duplicate player state machine. Sonos is a separate connection type described in `docs/sonos.md`.

The runtime must include OpenSSH for optional OS actions. No physical CoreELEC behavior is claimed from fixture tests.

Browser regression: after building the web bundle and host daemon, run `NODE_PATH=build/webui-review/node_modules node tools/tests/media-connections.cjs`. It uses an isolated real configuration server and intercepts every device endpoint; it verifies both connection types, room assignments, SSH enrollment/credential clearing, disruptive-action confirmation and Sonos coordinator-gated playback. Screenshot output is `build/media-connections-web.png`.

## Primary references

- [CoreELEC SSH guide](https://wiki.coreelec.org/coreelec:ssh): SSH enablement and access requirements.
- [CoreELEC image builder](https://github.com/CoreELEC/CoreELEC/blob/coreelec-22/scripts/image): `/etc/os-release` identity and build fields.
- [CoreELEC Kodi systemd unit](https://github.com/CoreELEC/CoreELEC/blob/coreelec-22/packages/mediacenter/kodi/system.d/kodi.service): managed `kodi.service` lifecycle.
- [Kodi UPnP implementation](https://github.com/xbmc/xbmc/blob/master/xbmc/network/upnp/UPnP.cpp): optional media-server advertisements; these alone do not identify CoreELEC.
- [CoreELEC update guide](https://wiki.coreelec.org/coreelec:updates): OS update mechanisms and version-dependent constraints, outside this client's current scope.
