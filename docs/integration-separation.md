# Integration separation

This assesses moving built-in device integrations out of the `couch` monorepo
into independent repositories, using the package mechanism described in
[integration architecture](integration-architecture.md). It covers what the
package protocol supports today, which built-in integrations could move under
that protocol, what is missing for the rest, how a retirement must be staged
without breaking deployed remotes, and open questions the current design does
not answer.

This is a planning document. It has no deadline and authorizes no code change
by itself.

## The rule (decided 2026-09-19)

Integrations live in their own repositories and are installed and updated
through the package feed, independently of the OS. The OS carries only the
host: the plugin protocol and SDK, package management, and the screens that
draw what a package declares. A built-in integration is retired by moving its
source to its own repository, publishing it in the feed, and then removing the
built-in client, with every saved connection converting by itself
([Built-in integrations that became packages](integration-migration.md)).

Denon was retired this way first. Sonos stays built in until a package can
drive the full player screen (artwork, track, seek, groups); its package ships
as a preview beside it. The rest of this document is the assessment that led
here, kept because the capability gaps and the retirement rules it lists still
decide the order of what follows. Where it describes built-in Denon or the
reversible pilot, read it as history.

## What exists today

An integration reaches a device one of two ways:

- **Built-in**: linked into `couch-confd`, part of the core release, using
  whatever native APIs it needs (TCP, TLS, SSH, mDNS, D-Bus, a privileged
  device node). Configuration and credentials live in the daemon's own
  connection store (`docs/connections.md`).
- **Package**: a signed Alpine APK containing one subprocess, speaking
  protocol v1 over a framed JSON socket to `couch-confd`
  (`clients/couch-plugin`, `docs/integration-architecture.md`). Denon is the
  only integration shipped this way; Echo is the SDK's synthetic template.

Protocol v1's `Request` enum has five variants
(`clients/couch-plugin/src/protocol.rs:71-77`):

```rust
pub enum Request {
    Hello { protocol_version: u32 },
    Configure { settings: serde_json::Value },
    Command { function: String },
    Status,
    Inputs,
}
```

`Response` is symmetric: `Hello`, `Ok`, `Status`, `Inputs`, `Error`. There is
no request or response for pairing, for a plugin to hand back a
generated/negotiated credential, for the device to push an unsolicited event,
for listing or launching an app, or for addressing more than one resource per
connection. `docs/development/index.md` states the same limits for
integration authors.

The shared conformance harness that every package's admission tests run
against (`clients/couch-plugin/src/testing.rs`) is built directly on
`couch_sdk::testing::MockHost` — a scripted TCP peer that answers a
line-oriented request/response protocol (`clients/couch-sdk/src/testing.rs:1-11`).
`ConformanceCase`, `FailureCase`, `TimeoutCase` and `SpikeCase` all take a
`DeviceSettings = fn(&MockHost) -> Value`, so a package whose device speaks
HTTP, TLS or WebSocket cannot construct the fixtures the four required
admission tests need. **This is in progress, not merged**: making the fake
device pluggable so a non-line-oriented transport can still produce admission
evidence. Until it lands, only line-protocol devices can pass admission.

Catalog admission itself requires four named tests, not five — `conformance`,
`failure`, `timeout_no_retry`, `spike`
(`tools/integrations/validate_catalog.py:16`, `docs/development/admission.md`).
An earlier draft of this document said five; that was a mix-up with the five
protocol request types above, corrected here.

## Per-integration inventory and verdict

`Provider` (`model/couch-model/src/connection.rs:68-109`) is the exhaustive
list of connection kinds Couch understands; every built-in network
integration and `Plugin` (the package escape hatch) is a variant of it.

| Integration | Crate | Transport | Core-only capability it uses today | Verdict |
| --- | --- | --- | --- | --- |
| Denon | `Couch-OS/couch-integration-denon` (was `clients/couch-denon`) | line-oriented TCP | none beyond the protocol itself | **Retired from the OS.** Shipped as a package since 0.1.1 (protocol v1), with dB readout and absolute-dB volume since 0.2.0 (protocol v2). Saved built-in connections convert automatically (`docs/integration-migration.md`). |
| Sonos | `clients/couch-sonos` | HTTPS/TLS on port 1443 | an API key resolved from env/file/build-time constant (`clients/couch-sonos/src/lib.rs:22-40`) | Next candidate once the harness above lands; blocked only on that, per the source material. |
| Kodi | `clients/couch-kodi` | line-oriented TCP, holds the socket open | unsolicited push notifications (`next_notification`, `clients/couch-kodi/src/lib.rs:150-162`) | Needs an events capability first. |
| CoreELEC | `clients/couch-coreelec` | wraps Kodi's TCP plus SSH, host-key verified | privileged OS management over SSH (`clients/couch-coreelec/src/lib.rs:1-2`) | Not assessed by the source material; flagged here because its filename is one of the two hard-pinned entries in the deployed-updater allowlist (see Retirement below). Needs pairing/credential write-back for host-key trust at minimum. |
| Tizen | `clients/couch-tizen` | WebSocket + REST | pairing, saved token/cert, app list/launch (`clients/couch-tizen/src/lib.rs:316-535`) | Needs pairing, credential write-back, apps. |
| webOS | `clients/couch-webos` | WebSocket | pairing, saved key, app list/launch (`clients/couch-webos/src/lib.rs:151-392`) | Needs pairing, credential write-back, apps. |
| Apple TV | `clients/couch-appletv` | Companion protocol, encrypted | PIN pairing, stored session keys, app catalog (`clients/couch-appletv/src/lib.rs`, `src/crypto.rs`) | Needs pairing, credential write-back, apps. |
| Android TV | `clients/couch-androidtv` | pinned mutual TLS | PIN pairing, stored identity, deep-link launch but no app *list* (`clients/couch-androidtv/src/lib.rs:1,234,521-533`) | Needs pairing, credential write-back; launch exists, list does not. |
| Home Assistant | `clients/couch-ha` | REST/WebSocket | many entities per connection (`clients/couch-ha/src/entities.rs`) | Needs multi-resource connections and typed actions. |
| Hue | external `couch-integration-hue` package | HTTPS, local bridge | light, group, and scene children | Protocol 3 package in the official preview feed. |
| Matter | `clients/couch-matter` | Matter/CHIP | one fabric hosting many nodes/endpoints, pairing-code commissioning (`model/couch-model/src/connection.rs:93-94`, `clients/couch-matter/src/pairing.rs`) | Needs multi-resource connections, typed actions, and commissioning (a pairing variant). |
| UniFi Protect | `clients/couch-unifi-protect` | RTSPS/SRTP video, REST | live video stream decode (`clients/couch-unifi-protect/src/media.rs`, `src/player.rs`) | Needs multi-resource connections, typed actions, and media delivery. |
| IR | `clients/couch-ir` | remote's own IR blaster | privileged hardware | Stays in core. |
| Voice | `clients/couch-voice` | ALSA capture device | privileged hardware | Stays in core. |
| Bluetooth (`couch-bt`, `couch-bt-hid`) | `clients/couch-bt*` | `/dev/vhci`, D-Bus to a patched BlueZ | privileged hardware | Stays in core. |
| Echo | `clients/couch-echo` | synthetic (`MockHost`) | none — it's the worked example | Stays as the SDK template; never a device integration to retire. |

## Missing capabilities

Each of these is a core protocol/host change, not something a package author
can work around:

- **Interactive pairing.** Tizen, webOS, Apple TV, Android TV and Matter all
  drive a user-facing approve/PIN/code flow before a connection works. There
  is no `Request`/`Response` pair for it.
- **Credential write-back.** Built-in integrations persist what pairing
  produces — a token, a pinned certificate, session keys — into their own
  file beside `config.json` (`docs/connections.md`: `hue-connection.json`,
  `ha-connection.json`, `webos-connection.json`, `androidtv-connection.json`,
  `appletv-connection.json`, `tizen-connection.json`, `matter/`). `Configure`
  is one-directional (host → plugin); no response variant lets a plugin push
  a newly obtained credential back for the daemon to store.
- **Unsolicited events.** Kodi pushes state changes down its socket
  unprompted. Protocol v1 is strictly request/response; there is no channel
  for the plugin to speak first.
- **A discovery contract that's actually used.** `clients/couch-sdk/src/discovery.rs`
  defines a `Discover` trait (an mDNS service name plus `settings_for`).
  Only `couch-echo`, the template, implements it
  (`clients/couch-echo/src/lib.rs:236`). The daemon's real discovery paths
  (`daemon/couch-confd/src/api/streaming_tv.rs`,
  `daemon/couch-confd/src/api/airplay.rs`) call a separate ad hoc
  `discover_service` helper against plain `MDNS_SERVICE` constants defined
  independently in `couch-appletv` and `couch-androidtv`; they don't go
  through the SDK's `Discover` trait at all. `couch-sonos` also does its own
  discovery outside this trait (`clients/couch-sonos/src/lib.rs:1098`). So the
  contract exists but nothing in production calls it — the gap is real, but
  it's an unused abstraction problem as much as a missing one.
- **App list and launch.** Tizen, webOS and Apple TV enumerate and launch
  apps; Android TV can launch a deep link but has no list. No protocol
  operation carries either.
- **Multi-resource connections.** Home Assistant, Hue, Matter and UniFi
  Protect each expose many controllable things (entities, rooms/scenes,
  fabric nodes/endpoints, cameras) from one connection. Protocol v1 has one
  `Status`/`Inputs` pair per connection, modeling one device.
- **Typed domain actions beyond volume.** `Command { function: String }` is
  an opaque string with no typed parameter. `Status.volume` is the only typed
  numeric field. Denon's own dB readout and absolute-dB command are already
  documented as out of reach of v1 for exactly this reason
  (`docs/integration-migration.md`, "Known parity limits" and "Future
  protocol work").
- **Media delivery.** UniFi Protect decodes an RTSPS/SRTP video stream
  (`clients/couch-unifi-protect/src/media.rs`, `src/player.rs`). Nothing in
  the framed JSON protocol carries a media stream.

## Per-repository shape

The only precedent is `Couch-OS/couch-integration-denon`. Its layout:

```text
integration.json   # id, tier, cargo package/binary, manifest path
plugin.json         # protocol-v1 manifest
Cargo.toml          # couch-plugin / couch-sdk pinned by git rev, both normal
                     # and dev-dependencies (testing feature)
Cargo.lock
src/
tests/
  admission.rs       # calls couch_plugin::testing::{conformance,failure,
                     #   timeout_no_retry,spike}
  plugin.rs
.github/workflows/admission.yml
```

`Cargo.toml` pins `couch-plugin` and `couch-sdk` to one commit
(`5e0cc20adad6ea54032002a8adf0888e80499bca`, matching
`tools/release/tested-integrations.json`'s `sdk_commit`/`tooling_commit`) —
exactly the rule stated in `docs/integration-architecture.md`: "An
independent source repository pins `couch-plugin` and `couch-sdk` to the same
full Couch commit and runs the shared admission harness. It must not carry a
private protocol copy."

What's duplicated today, with only one repository to compare against:

- **The admission workflow.** `couch-integration-denon`'s
  `.github/workflows/admission.yml` is 34 lines of hand-written YAML
  (checkout, toolchain, a Python check that both dependency revisions match,
  `cargo fmt`, `cargo test`). A second repository would either copy this file
  or diverge from it. Neither is desirable once there's more than one
  independent repository — a shared, versioned reusable workflow (called with
  `uses:` and pinned like the SDK dependency) should replace the copy so the
  admission bar can be raised once and taken up by every source repository
  the next time they update the pin, rather than N hand-edits.
- **The starting crate.** `docs/development/index.md`'s "shortest path" is
  already "copy `clients/couch-echo` and rename it." A separate template
  repository (rather than a monorepo directory) would give that same starting
  point its own CI, its own admission workflow already wired up, and a clean
  `git log` for a new integration author, instead of asking them to first
  extract a subtree from `couch`.

Neither the reusable workflow nor the template repository exists yet; both
are proposed here, not implemented.

## Migration mechanics

What is implemented now is the automatic, one-way conversion described in
[Built-in integrations that became packages](integration-migration.md):
`couch_model::LEGACY_BUILTINS` names the package and the settings mapping for
each departed built-in, and `couch-confd` installs the package and converts the
connection in place. The list below is the reversible pilot that preceded it
(`v0.1.0-alpha.20260916.171` until built-in Denon was removed), kept for the
reasoning about ownership and rollback, which carried over:

- **Explicit and per-connection.** Installing a package does not convert any
  existing connection. A user opens **Integrations → Denon migration pilot**
  and picks one named connection.
- **Ownership handoff, same connection ID.** A built-in connection is owned
  by `couch-control` (`clients/couch-denon` in-process); a migrated one is
  owned by the plugin host and its subprocess. `migrate_denon`
  (`model/couch-model/src/integration_migration.rs:183`) changes the
  connection's `Provider` in place and records the original in
  `denon_migrations` so it can be restored; devices and activity bindings are
  untouched.
- **No dual ownership window.** The daemon explicitly blocks the native TCP
  owner for any connection recorded in `denon_migrations`
  (`daemon/couch-confd/src/main.rs:84-89`, `couch_control::block_denon`) so a
  rollback core can't reopen a socket the package already owns.
- **Reversible today, by construction.** `restore_native_denon`
  (`model/couch-model/src/integration_migration.rs:218`) switches the
  connection's `Provider` back and drops the migration record. This works
  even if the package has since been removed, because the built-in Denon
  implementation the restore switches back to is still compiled into the
  core — see the open question below.
- **One atomic config document survives core rollback.** `config.json`
  carries a legacy-readable projection plus the modern extension in one
  write; an old core sees the connection as unconfigured rather than seeing a
  malformed file (`docs/integration-architecture.md` "Compatibility and
  independent source";
  `docs/runtime-updates.md#integration-configuration-across-core-rollback`).

## Retirement criteria

Two rules are non-negotiable because violating either breaks already-deployed
remotes with no recovery short of reinstall:

1. **Never remove a filename from the deployed updater's required-file
   allowlist.** `couch-sonos` and `couch-coreelec` are both in it
   (`daemon/couch-updates/src/staging.rs:14-19`, mirrored in
   `tools/release/update_floor.py:48-50`). The allowlist that matters is the
   **floor** — the oldest updater still deployed, currently the one tagged
   `v0.1.0-alpha.20260910.24` — not the updater in this checkout, which
   already accepts any top-level `couch-*` executable (added in `.142`, per
   `update_floor.py`'s comments). The floor updater refuses a signed bundle
   *before download* if it doesn't recognize every file in the manifest, and
   it never falls back to an older release. So dropping either filename
   strands every remote still on the floor, or any older updater, until a
   full reinstall — it already happened twice
   (`docs/runtime-updates.md`, "What this cost, twice"). Retiring an
   integration's built-in binary means keeping the filename as a stub, not
   deleting it, until the floor itself moves (which needs a rebuilt public
   installer OS image and every remote updated past the new floor first).
2. **Never remove a `Provider`/`Integration` serde variant.**
   `Provider` (`model/couch-model/src/connection.rs:68`) and `Integration`
   (`model/couch-model/src/device.rs:354`) are internally tagged enums with
   no fallback variant. `Store::open` parses `config.json` into them and maps
   a parse failure to `Error::Parse`
   (`daemon/couch-confd/src/store.rs:73`), which `main` turns into
   `std::process::exit(1)` (`daemon/couch-confd/src/main.rs:79`). A user with
   that provider configured would have a daemon that refuses to start at all,
   not a daemon that starts without that device.

Beyond those two:

- **A renewed tested-integration set.** Any change that touches the paths
  `tested-integrations.json` pins as contract paths
  (`tools/release/tested-integrations.json`: `clients/couch-plugin`,
  `daemon/couch-confd/src/{main,store,plugins}.rs`,
  `model/couch-model/src/{integration_migration,lib,seed,storage,validate}.rs`,
  etc.) invalidates the current receipt. `tools/release/verify_integration_set.py`
  must produce a fresh one before the release ships
  (`docs/integration-release-rollout.md`). Removing built-in integration code
  is exactly this kind of change. The one exception is a compiled-out admission
  harness listed in `core.harness_paths`, which costs a rerun of the admission
  suite and a recorded digest update instead.
- **Migration path before retirement, not after.** The Denon precedent is:
  ship the package, let users opt in, keep the built-in implementation until
  usage/confidence justifies dropping it. Retiring the built-in path before a
  package alternative has field evidence leaves no fallback.
- **Hardware evidence at the target tier.** `docs/development/admission.md`'s
  tiers (`test-only` / `preview` / `production`) apply to the replacement
  package the same way they apply to the current Denon preview: a
  `production` label needs validated hardware evidence, not just passing
  fixtures.

## Wave ordering

> Superseded on 2026-09-19 by the
> [integration extraction roadmap](plans/integration-extraction-roadmap.md),
> which replaces the per-wave protocol additions below with one protocol 3
> effort. The retirement criteria above still stand.

The source material's number — 2 to 4 repositories over roughly two quarters,
not twelve — is a scope recommendation, not something derivable from the
code; treat it as an estimate someone made, not a fact this document confirms.
What *is* grounded is the dependency order the capability gaps impose:

1. **Denon** — done, and retired from the OS.
2. **Sonos** — blocked only on the pluggable-fixture harness landing; no new
   protocol capability needed.
3. **Kodi** — blocked on an events capability.
4. **Tizen, webOS, Apple TV, Android TV** — each blocked on pairing,
   credential write-back, and (except Android TV's list) apps. Likely one
   wave once those three land together, since they share the same three
   gaps.
5. **Home Assistant, Hue, Matter, UniFi Protect** — blocked on multi-resource
   connections and typed actions; Matter additionally needs a commissioning
   flow; UniFi Protect additionally needs media delivery. The largest lift,
   reasonably last.
6. **IR, voice, Bluetooth, Echo** — not candidates. Echo is the template, the
   other three need privileged hardware access no subprocess boundary
   currently grants.

## User-facing configuration migration, and its reversibility

**Settled on 2026-09-19 with the second option below:** a retired built-in's
variant stays so the file parses (`Provider::LegacyDenon`), its code is
deleted, and the conversion is automatic and one way. There is no "restore
built-in control"; an unconverted connection says what it needs instead. The
original framing follows.

The pilot's migration (Denon, described above) was reversible
because "restore" means "switch the `Provider` back to a variant whose
built-in implementation is still linked into the core." That holds as long as
retirement rule 2 is followed — the variant and its code stay.

**The question as it stood:** does migration stay
reversible once a built-in implementation is *retired* — i.e., once its
`Provider` variant is kept for deserialization compatibility (rule 2) but its
actual device-control code is deleted or reduced to a stub? Two options exist
and neither is documented or decided:

- Keep enough of the built-in implementation alive (behind the variant) to
  actually restore a working connection, indefinitely — which undercuts the
  point of retiring it.
- Let the variant round-trip through config (so the daemon doesn't crash) but
  make "restore built-in control" fail or become unavailable once retired —
  which means the current one-click reversibility is a preview-era property,
  not a permanent guarantee, and that needs to be told to users before they
  migrate.

## Open question: a packaged Sonos and its API key

`clients/couch-sonos` resolves its Sonos Control API key three ways, in
order: `COUCH_SONOS_API_KEY`, a `sonos-api-key` file beside `config.json`, or
a build-time constant `BUILT_IN_API_KEY` baked in via
`COUCH_SONOS_API_KEY_FILE`/`option_env!` at compile time
(`clients/couch-sonos/src/lib.rs:22-40,436-446`). The built-in key is a Couch
release secret (per the release-cutting process, provisioned on the release
build host) — something an independent, community-published `couch-sonos`
package repository would not have.

**Open question, not settled by the current design:** does a packaged Sonos
integration ship expecting the household to supply its own
`integration.sonos.com` developer key (the documented fallback path,
`PLACEHOLDER_API_KEY`'s doc comment), or does Couch's own release process
need to keep building and signing the Sonos package centrally, the same way
it builds the current in-core `couch-sonos` binary, so a household key is
never required? These have different support and trust implications and
neither is written down anywhere today.

## Risks

- **Silent stranding.** Getting the allowlist rule wrong doesn't fail loudly
  in review — it fails on remotes months later, permanently, per-device. The
  floor check (`tools/release/update_floor.py`) must run on every release
  that touches integration binaries, not just ones that look like they touch
  the updater.
- **Config that won't parse.** Getting the serde-variant rule wrong doesn't
  fail at compile time in an obviously integration-shaped way — it's a
  daemon that won't start, discovered by a user, not in review.
- **Harness dependency.** The wave order above assumes the pluggable-fixture
  work lands as scoped. If it doesn't generalize past Sonos's HTTPS
  transport, WebSocket-based integrations (Tizen, webOS) stay stuck even
  after their pairing/credential/app gaps are closed.
- **Duplicated admission surface.** Until a shared reusable workflow exists,
  every new independent repository hand-writes (or copies and drifts from)
  its own CI gate, the way `couch-integration-denon` already has.
- **Reversibility expectations.** If retirement quietly narrows what "restore
  built-in control" can do, and that isn't communicated, a user who migrated
  expecting a safety net finds out it's gone at the worst time — mid-incident,
  not during planning.

## Further reading

- [Integration architecture](integration-architecture.md)
- [Integration packages](integration-packages.md)
- [Built-in integrations that became packages](integration-migration.md)
- [Integration release rollout](integration-release-rollout.md)
- [Connections and private settings](connections.md)
- [Build an integration](development/index.md)
- [Catalog admission](development/admission.md)
- [Runtime updates](runtime-updates.md)
