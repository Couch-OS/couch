# Shared media control and GUI configuration

`couch-control` owns Kodi, webOS and streaming-TV control transports. (It owned
the built-in Denon transport until that client became a package, which the
integration host runs instead.) The configuration
daemon runs the service; the GUI calls it over `control.sock` beside `config.json`.
The daemon's HTTP handlers use the same pool directly. Explicit webOS pairing
still uses the registration client. Hue and Home Assistant retain their HTTP
clients and existing push subscriptions; activity mappings dispatch them through
independent connection workers.

## Ownership and bounded work

Each endpoint has one worker and a queue of 16 requests. Different endpoints can
make progress independently. Queued work expires after 750 ms; a full queue
returns an error. Failed commands are never automatically retried, because a
lost response does not prove the command failed. Volume steps and mute toggles
execute their read and write in one worker job.

Each consumer holds a lease. Releasing the final lease closes the transport;
unused leases expire after 30 seconds, including clients lost to process crashes.
Idle registry entries are pruned after 60 seconds, with at most 128 entries.
Changing connection credentials discards the old transport. The private socket
has mode 0600, bounded frames and concurrent callers, and transports credentials
only locally. An existing socket error does not trigger a second command through
another transport. Standalone GUI fixtures can use an in-process pool when no
service socket exists.

Authenticated `GET /api/diagnostics/control` exposes request counts, dropped
requests and aggregate/maximum queue delay in microseconds per endpoint. These
measure queue time, not device response time, and never include credentials.

## Configuration and input

The GUI loads one validated `Arc<Config>` at startup. A background watcher checks
file metadata every 250 ms and publishes changed, valid snapshots with an
increasing serial. Invalid or incomplete files leave the last good snapshot in
place. Controllers share snapshots; rendering does not reread or parse the file.
Snapshot load timings are logged separately from room/slide rendering.

`input.rs` owns wake/touch policy and microphone/menu hold state; `navigation.rs`
owns settings/area framebuffer transitions. Device operations and animation
ordering remain unchanged. Activity generations cancel pending local mappings
when navigation leaves their context.

`couch_model::commands::Function` provides typed provider functions while keeping
existing configuration strings compatible. Discovered inputs and apps use
`input:<id>` and `app:<id>` at the persistence boundary and are validated against
the device provider before execution.

## Verification

Run `cargo test` in `clients`, `model`, `daemon` and `ui`. Broker fixtures cover
independent progress, shared socket leases, actual Unix IPC and frame limits.
Snapshot tests cover atomic replacement and invalid updates. Hardware fixtures
must separately verify physical key routing, hold cancellation and panel wake.
