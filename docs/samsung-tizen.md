# Samsung Tizen TV

Couch's Rust `couch-tizen` client controls Samsung Smart TVs (2016 and newer,
Tizen OS) over the LAN Smart View remote channel. The Rust/Wasm configuration UI
uses the daemon's authenticated `/api/connections/{id}/tizen` routes; the access
token, pinned certificate and MAC never appear in exported house configuration.

**This integration is experimental and was written without a Samsung TV to
test against.** Every payload follows the maintained reference clients listed
below, and the loopback tests cover the exchanges they document, but nothing
here has been exercised against real firmware. Treat the first physical pairing
as validation, and record what the TV actually did in this file.

## How the TV is reached

| Channel | Port | Purpose |
| --- | --- | --- |
| `http://IP:8001/api/v2/` | 8001 | Unauthenticated information: name, model, `wifiMac`, `TokenAuthSupport`, `FrameTVSupport`, and on 2019+ firmware a `PowerState` of `on` or `standby`. |
| `wss://IP:8002/api/v2/channels/samsung.remote.control` | 8002 | Token-authenticated remote channel on 2017+ TVs. Keys, the installed-app list and app launch travel here. |
| `ws://IP:8001/api/v2/channels/samsung.remote.control` | 8001 | Unencrypted remote channel; 2016 models offer only this one and issue no token. Explicit opt-in. |
| UDP 9 | — | Wake-on-LAN to the MAC reported by the information endpoint. |
| SSDP 1900 | — | Discovery via `urn:samsung.com:device:RemoteControlReceiver:1`. |

The client name `couch.` is sent base64-encoded in the `name` query parameter
and is what the TV shows on its Allow / Deny prompt. On the first secure
connection the TV waits for that answer and then sends `ms.channel.connect`
with a `token` in its data; every later connection passes `token=` in the
query. `ms.channel.unauthorized` or `ms.channel.timeOut` mean the prompt was
denied or ignored. Couch pins the TV's self-signed certificate while the user
approves the pairing and refuses a different certificate afterwards; the
reference clients disable verification entirely, so if a TV turns out to
rotate its certificate, pair again and report the model.

Keys are `ms.remote.control` messages with `Cmd: Click` and a `KEY_*` name; a
held key sends `Press`, waits, then `Release`. There is no acknowledgement.
The installed-app list is `ms.channel.emit` / `ed.installedApp.get`, answered
by an event of the same name whose `data.data` array carries `appId`, `name`
and `app_type` (2 for web apps launched by deep link, 4 for native apps).
Launch is `ed.apps.launch` with `DEEP_LINK` or `NATIVE_LAUNCH` chosen from that
list. Older firmware does not answer the app query; the screen then reports
the app list as unavailable rather than inventing one.

## Setup

Open **Connections → Add a connection → Samsung Tizen TV**, name it and create
it. Turn on the TV, click **Find Samsung TVs** or enter its IP address, then
**Pair TV** and choose **Allow** on the TV within about thirty seconds. The
daemon first reads the information endpoint, so the connection records the
model, MAC for waking and whether the TV is The Frame; if that endpoint does not
answer, pairing still proceeds over the secure channel. Tick the legacy box only
for a 2016 model whose secure port never answers.

Then add the TV in **Rooms & devices**. The connection card offers test
controls; keys have no acknowledgement, so watch the TV. Credentials live at
`/opt/couch/connections/<id>/tizen-connection.json` (mode 0600) inside the
shared streaming-connection format. See [connection storage](connections.md).

## Controls and activities

Selecting the TV from a room opens the shared TV screen with the Samsung
adapter. D-pad, OK, Back, Home, Menu, volume, mute, channel and colour keys go
straight to the TV. The touch row offers rewind, play, pause and fast-forward;
tiles offer **Wake** and **Apps**. Activity mappings and sequences can use every
function in the Samsung command set, `input:tv`, `input:hdmi`, `input:hdmi1`
to `input:hdmi4` (source keys; the TV silently ignores a missing input) and
`app:<appId>` entries discovered from the TV.

Power is deliberately split. **Wake** (`power-on`) sends Wake-on-LAN to the
reported MAC by limited broadcast and directly to the TV's last address; it
needs the TV's network standby / "power on with mobile" setting and is never
reported as success beyond the packet being sent. **Power** (`power-off`) sends
`KEY_POWER`, which Tizen treats as a toggle; on The Frame it is held for three
seconds, which is how Home Assistant leaves Art Mode. Couch does not infer
power or mute state from the remote channel. `next`, `previous` and
`play-pause` are not offered because the documented key set has none.

A TV in standby answers neither channel; only Wake works then. On firmware that
reports `PowerState`, the screen status shows on / standby from the
information endpoint.

## Client and validation

```sh
(cd clients && cargo test -p couch-tizen -p couch-control)
TV_IP=tv.local
couch-tizen info "$TV_IP"
couch-tizen discover
couch-tizen pair "$TV_IP" /private/path/tv.json        # add --legacy for 2016 models
couch-tizen /private/path/tv.json status
couch-tizen /private/path/tv.json key KEY_VOLUP
couch-tizen /private/path/tv.json apps
couch-tizen /private/path/tv.json launch 111299001912
couch-tizen /private/path/tv.json wake
```

Unit tests run a loopback WebSocket peer through pairing with and without a
token, the client-name and token query parameters, denied prompts, key clicks,
held power, the app list interleaved with unrelated events, launch action types
and status codes, a closed socket detected before the next key, REST parsing
(string booleans, MAC normalisation, chunked bodies), SSDP reply filtering,
private settings storage and the Wake-on-LAN packet. Daemon tests cover the
unpaired routes, command validation, token privacy and wake without a MAC. GUI
tests cover the adapter's command mapping. **No physical Samsung TV has been
paired yet.** Still required:

1. Pair a 2017+ TV: confirm the Allow prompt names `couch.`, that a token is
   issued, and that reconnecting with it needs no prompt.
2. Confirm the pinned certificate survives a TV reboot and a firmware update.
3. Verify keys, source keys, the app list and launch on that TV; note which
   `app_type` values it reports.
4. Verify Wake-on-LAN from standby and `KEY_POWER` behaviour (and the held
   variant on The Frame).
5. Try a 2016 model on the legacy port if one is available.

Protocol references: [samsungtvws](https://github.com/xchwarze/samsung-tv-ws-api)
(`connection.py`, `remote.py`, `rest.py`, `event.py`) and Home Assistant's
[Samsung Smart TV integration](https://www.home-assistant.io/integrations/samsungtv/)
(`bridge.py`, `manifest.json`, `const.py`).
