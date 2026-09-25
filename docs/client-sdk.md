# Writing a Couch client

A *client* is a Rust crate in `clients/` that knows how to talk to one kind of
home device: Kodi's JSON-RPC, an LG TV's SSAP socket, a Denon receiver's
CR-delimited TCP protocol, a Home Assistant server, the remote's own IR
blaster. `clients/couch-sdk` is the contract those crates share, and
`clients/couch-echo` is a complete worked example you can copy.

Everything below can be built, run and tested on an ordinary Linux or macOS
machine. **You do not need an Astrion HA100 remote, a television, or any
credential to write a client and know that it is correct up to the wire
format.** What you cannot do without hardware is prove that a real device
behaves as you assumed; that boundary is spelled out at the end.

## Choosing a built-in client or an integration package

Existing clients remain directly linked into the configuration daemon and
device GUI. New network integrations can also implement `DeviceClient` and
run through `clients/couch-plugin` as independent executables, and that is
where integrations are headed: they live in their own repositories and the OS
carries only the host. Echo provides both forms here. `couch-denon`, the first
real client written against this SDK, has moved to
[`Couch-OS/couch-integration-denon`](https://github.com/Couch-OS/couch-integration-denon)
and is no longer linked into the daemon or the GUI; where this document
mentions it as a worked example or records results measured on it, that
repository is where to look. The SDK is maintained in this repository rather than
crates.io; an external integration pins the Couch Git repository at a full
commit. The installation boundary is a versioned JSON protocol, not a Rust ABI.

An external package supplies `plugin.json` with its identity, protocol version,
capabilities, relative executable path, and declarative settings. The daemon
discovers installed manifests, the browser renders their settings, and both
browser and panel commands use the daemon's persistent subprocess owner.
Adding such a package requires no provider enum, daemon, or UI registration
edits. The [registration instructions](#registering-a-provider) below apply to
directly linked clients. Package build, sideload, activation, and rollback
instructions are in [Integration packages](integration-packages.md).

## Building and testing an external integration

Start with `clients/couch-echo`: its ordinary SDK implementation, standalone
`src/bin/couch-plugin-echo.rs` adapter, `plugin.json`, and `tests/plugin.rs`
form a complete example. The adapter is small:

```rust,ignore
fn main() {
    let manifest = serde_json::from_str(include_str!("../../plugin.json"))
        .expect("embedded integration manifest");
    if couch_plugin::serve::<couch_echo::EchoTv>(manifest).is_err() {
        std::process::exit(1);
    }
}
```

Build just the example and run fake-device subprocess tests from the repository
root:

```sh
cargo build --manifest-path clients/Cargo.toml -p couch-echo --bin couch-plugin-echo
cargo test --manifest-path clients/Cargo.toml -p couch-plugin -p couch-echo
```

Copy and rename the crate for your integration, change `DeviceClient::KIND`,
settings, capabilities, and device transport, and make its embedded manifest
match. Register the new crate only in the clients workspace when developing
in-tree. Build the ARM executable with `--target armv7-unknown-linux-musleabihf`
for the remote. A host binary cannot run on the remote.

For an independent repository, depend on both shared crates at the same exact
Couch revision:

```toml
[dependencies]
couch-plugin = { git = "https://github.com/Couch-OS/couch.git", rev = "FULL_COMMIT" }
couch-sdk = { git = "https://github.com/Couch-OS/couch.git", rev = "FULL_COMMIT" }

[dev-dependencies]
couch-plugin = { git = "https://github.com/Couch-OS/couch.git", rev = "FULL_COMMIT", features = ["testing"] }
couch-sdk = { git = "https://github.com/Couch-OS/couch.git", rev = "FULL_COMMIT", features = ["testing"] }
```

Commit `Cargo.lock`. Use `couch_plugin::serve` for the executable and the
cases in `couch_plugin::testing` for admission. Do not copy `protocol.rs`,
manifest validation, the framed transport, or the test harness into the
integration repository: that creates a second contract which can drift while
still appearing to implement protocol v1. Updating the SDK is an explicit pin
and lock-file change followed by the complete integration suite.

The manifest's `capabilities` must exactly match `DeviceClient::capabilities()`.
Settings use `text`, `secret`, `integer`, or `boolean` fields; secret fields
cannot have defaults. Configuration validates schema and typed settings without
connecting to the device. The first command/status/input request connects;
an offline receiver therefore cannot make package activation fail.

The daemon sends credentials over the inherited socket, never argv or the
environment. Reserve stdout entirely for framed protocol traffic. The SDK
adapter owns one client per configured connection; never retry failed commands
inside your transport. Tests exercise actual child processes, fake devices,
malformed replies, protocol mismatches, partial-frame deadlines, queue overflow,
stale requests, and restart on the next explicit request after child failure.
The frame limit is 64 KiB, the endpoint queue holds eight requests, and queued
requests expire after 750 ms. Startup and configuration each have a three-second
deadline; device operations have a twelve-second absolute deadline.

Version 1 covers fixed commands, status, and enumerated inputs. Protocol 5 adds
installed-app enumeration and launch through the same bounded selectable and
`app:<id>` command vocabulary. Advanced automatic discovery and unsolicited
events still require future protocol extensions or a built-in integration.
This is a network integration pilot; it does not expose privileged hardware
access to plugins.

### Composing native integration controls

The optional `presentation` array selects from Couch's component library. The
browser and remote panel render these declarations with their own controls and
styling; packages supply no HTML, JavaScript, Slint, or arbitrary layouts. An
omitted array remains compatible with the basic command list.

| Component | Fields | Binding |
| --- | --- | --- |
| `command_group` | `title`, `commands` | One to 32 distinct declared capability IDs |
| `status_text` | `label`, `field` | `on`, `playing`, `muted`, `volume`, `input`, or `title` |
| `toggle` | `label`, `state`, `on`, `off` | Boolean status field plus two distinct declared commands |
| `input_selector` | `label` | Enumerated inputs; requires `supports_inputs: true` |
| `sound_output_selector` | `label`, `outputs` | One to eight distinct declared output command IDs; protocol 5 |

On the remote, Couch recognizes a native television profile without adding a
vendor-specific manifest field. A package qualifies when it declares an input
selector, supports enumerated inputs, and advertises the complete standard TV
set: power off, volume up/down, mute, D-pad/OK/Back/Home, and
play/pause/stop/rewind/fast-forward. The screen then uses Couch's TV hero and
transport row and sends physical navigation keys to the package. A package
missing any of that contract keeps the generic tile navigator, so a receiver
with a few cursor commands is never mistaken for a television.

A protocol-5 television may also set `supports_apps: true`. Couch then asks
for `Apps`, renders the returned launch points in its native Apps sheet, and
sends the selected `app:<id>` through the ordinary command gate. Optional
`sound_output_selector` data creates the native Sound output sheet without
letting the package provide UI code.

For example, a receiver can declare:

```json
"presentation": [
  {"kind":"toggle", "label":"Power", "state":"on", "on":"power-on", "off":"power-off"},
  {"kind":"command_group", "title":"Volume", "commands":["volume-up", "volume-down"]},
  {"kind":"toggle", "label":"Mute", "state":"muted", "on":"mute-on", "off":"mute-off"},
  {"kind":"input_selector", "label":"Source"}
]
```

Declarations are limited to sixteen components with bounded plain-text labels.
Undeclared commands, unsupported input selectors, and toggles bound to nonboolean
state are rejected during package validation. `clients/couch-echo/plugin.json`
and, in its own repository, `couch-integration-denon`'s `plugin.json` contain
working examples. Use only state
your device actually reports: Denon's decibel scale must not appear as a
percentage volume reading.

## The architecture, in one screen

```
config.json ──────────► model/couch-model ◄──────── the shared vocabulary:
  rooms, devices,        Config, Connection,         Provider, Integration,
  scenes, activities     Device, Action              commands::Function,
  (no credentials)                                   buttons::functions
                                 ▲
        ┌────────────────────────┼────────────────────────┐
        │                        │                        │
 daemon/couch-confd        ui/couch-gui              web/couch-web
 REST API + web assets     the Slint GUI on          the browser UI,
 on the remote             the remote's panel        served by the daemon
        │                        │                        │
        └──────────┬─────────────┘                        │
                   ▼                        (HTTP to the daemon) ┘
        clients/couch-control ── the broker: one owner per endpoint,
        a 16-deep queue, a 750 ms queue deadline, leases, and no retries.
        Private Unix socket, mode 0600, beside config.json.
                   │
                   ▼
   clients/couch-kodi   couch-sonos   couch-ha      couch-tizen
   couch-androidtv      couch-appletv couch-ir      couch-voice
                   ▲
                   └── clients/couch-sdk: the contract they share
```

Two rules the rest of this document follows from:

- **Credentials never enter `config.json`.** They live beside it in
  `connections/<connection-id>/<prefix>-connection.json`, mode 0600, and are
  absent from an exported house configuration. See `docs/connections.md`.
- **Nothing is retried.** A lost reply does not prove a lost command, and
  re-sending a power or input command that did arrive is worse than reporting
  the failure. The broker does not retry, and neither may your client.

## Prerequisites

| Tool | Why | Checked with |
|---|---|---|
| Rust stable, `cargo` | everything here | `cargo --version` |
| `armv7-unknown-linux-musleabihf` target | building for the remote | `rustup target add armv7-unknown-linux-musleabihf` |
| `wasm32-unknown-unknown` + [Trunk](https://trunkrs.dev) | only if you touch `web/` | `trunk --version` |

Validated against rustc 1.98.1. There is no `rust-toolchain.toml`, so any
recent stable should work; nothing here uses a nightly feature.

A pure-Rust client needs no cross C toolchain. `clients/.cargo/config.toml`
points the ARM target at `rust-lld`, which ships with the toolchain, so
`couch-sdk`, `couch-echo`, `couch-denon`, `couch-kodi` and `couch-ir` all
cross-compile with nothing else installed.

A client that pulls in TLS does need one, because `rustls` builds `ring`, and
`ring` is C and assembly. The repository's answer is `tools/arm-musl-cc.py`,
a wrapper around Zig, selected with the environment variable
`CC_armv7_unknown_linux_musleabihf`; `tools/build-webui.sh` sets it up on
macOS, and `docs/home-assistant.md` shows it by hand. Without it, the
whole-workspace ARM build stops at
`failed to find tool "arm-linux-musleabihf-gcc"`. This is the single strongest
reason to think twice before adding a dependency.

## Quickstart

From the repository root. Each of these was run exactly as written:

```sh
cd clients
cargo test -p couch-sdk --features testing     # the SDK's own tests
cargo test -p couch-echo                       # the example client's contract tests
cargo run -p couch-echo --example demo         # a whole session against a fake TV
cargo test -p couch-sonos                      # a real client, through the SDK, over HTTPS
cargo doc -p couch-sdk --features testing --no-deps   # rustdoc, harness included
```

`cargo run -p couch-echo --example demo` prints a success, a refusal by the
device, three refusals by the client, and a timeout, then lists every request the
fake television actually received:

```
volume-up          -> Ok(())
status             -> Ok(Status { on: Some(true), muted: Some(false), volume: Some(31), ... })
inputs             -> Ok([Selectable { id: "hdmi1", name: "Blu-ray" }, ...])
power-off          -> Err(Remote("the TV is locked"))
fast-forward       -> Err(Unsupported)
not-a-function     -> Err(Unsupported)
input:../escape    -> Err(Unsupported)
home (no reply)    -> Err(Timeout)
```

## Implementing a client

Copy `clients/couch-echo` to `clients/couch-<yours>`, add it to the `members`
list in `clients/Cargo.toml`, and rename the types. Then:

### 1. Settings

One serde struct holding what you need to reach one device, implementing
`ClientSettings`:

```rust
impl ClientSettings for Settings {
    const FILE_PREFIX: &'static str = "echo";   // -> echo-connection.json
    fn validate(&self) -> Result<()> { /* reject what cannot address a device */ }
}
```

`validate` runs before every save and after every load, so a file edited by
hand into an unusable state never reaches a socket. The provided `load`/`save`
write the file atomically at mode 0600 and fsync both the file and its
directory. Temporary names are unique across concurrent saves and stale files,
and the temporary file is removed whichever step fails. Do not
reimplement this: losing power mid-write is how a pairing key becomes
indistinguishable from a revoked one.

The underlying `load_private`/`save_private` are typed `std::io::Result`, not
`couch_sdk::Result`. An existing client that already distinguishes "the file is
not there" from "the file is not usable" keeps that distinction by calling them
directly - `couch-denon` does, and a test pins it. The trait's `load`/`save`
are the opinionated wrapper: unreadable and unparseable are both
`Error::Invalid`, because to someone setting up a connection they mean the same
thing.

`ClientSettings::path_in` returns `Result<PathBuf>`: connection IDs are one
directory component, never a path fragment. An empty ID remains the legacy
singleton layout; nonempty IDs containing `/`, `.` or `..` are rejected.

Reject anything that could break your wire format here - a hostname containing
whitespace, a token containing a control character - rather than at the point
of use.

### 2. Capabilities

Declare exactly the functions you implement, using
`couch_model::commands::Function` IDs:

```rust
fn capabilities() -> &'static [Capability] {
    &[("power-on", "Main zone on"), ("volume-up", "Volume up (0.5 dB)"), ...]
}
```

The IDs are the ones persisted in `config.json` and parsed by the GUI; the
labels are what the button-mapping picker shows. Declaring something you have
not implemented puts a key on screen that silently does nothing, so the test
harness checks every ID parses, is canonical, is unique, and is accepted by
your own `supports`.

`x:<id>` ids (a package's own button names) are part of protocol 3. A protocol
1 or 2 manifest that declares one is invalid. See the
[protocol reference](development/protocol.md#protocol-3).
The same goes for `Error::Unpaired`, `Error::because`,
`DeviceClient::execute_phased` and everything under
[One connection, many children](#one-connection-many-children) below: they
compile, and a protocol 1 or 2 package that uses them still sends exactly the
bytes it always sent (`Unpaired` leaves as `rejected`, the reason is dropped,
every key is a tap, and a manifest that declares children is invalid).

Dynamic functions - `input:<id>` and `app:<id>` - are **not** listed here.
Declare them by overriding `supports_input` / `supports_app`, which are asked
about one specific ID. Constrain those IDs: they are persisted and read back by
other processes.

### 3. Commands, state and errors

`command(&str)` is the boundary where a stored string becomes an action. It
parses, checks the capability gate, and only then calls your `execute`. Unknown
text and undeclared functions are refused **before any I/O**, which is worth a
test of its own: a stale button mapping must not be able to make a device do
something arbitrary, and must not cost a round trip to find out.

`status()` returns what the device *said*. Every field of `Status` is optional
because "does not report its volume" and "is at volume zero" are different
answers. Leave a field `None` rather than inferring it from the last command
you sent. `couch-denon` leaves `volume` empty on purpose: the receiver reports
decibels, and a percentage would be an invention.

Errors are seven variants, chosen to match what the broker already reports:

| Variant | Means | Shown as |
|---|---|---|
| `Protocol` | it answered, and the answer did not parse or did not confirm | "response this client could not use" |
| `Transport` | unreachable, or the connection dropped | "could not be reached" |
| `Timeout` | no reply before the deadline. **Not** proof the command failed | "did not reply before the deadline" |
| `Rejected` | understood and refused | "refused the request" |
| `Unsupported` | never declared; raised before any I/O | "does not support that function" |
| `Invalid` | the settings cannot address a device | "settings are incomplete" |
| `Remote(String)` | anything with a message worth showing | the message, verbatim |

Put the device's own explanation in `Remote` when it gives one: "the TV is
locked" is useful to the person holding the remote, and `Protocol` is not.
Never put a credential in one - these strings reach the browser and the panel.

`Error::retryable()` is true only for `Transport`. `Timeout` is deliberately
not retryable.

### One connection, many children

Protocol 3. Most devices are one connection and
one device. A bridge - a Hue hub, Home Assistant, a Protect controller - is one
connection and many, and Couch calls those its *children*. A client that has
none implements nothing here: every method below has a default that refuses,
`child_kinds()` is empty, and the bytes such a client puts on the wire are
unchanged.

A package declares the *kinds* of child it offers, not the children
themselves: what a kind is called, what kind of room device it becomes, which
built-in control it is drawn with (`light`, `cover`, `climate` or `scene`), the
commands it takes and the one typed action that goes with its control. They go
in the manifest's `children` and in `child_kinds()`, and `serve` refuses to
start if the two lists differ, exactly as it does for capabilities and actions.

```rust
fn child_kinds() -> &'static [couch_model::PluginChildKind];
fn children(&mut self, cursor: Option<&str>) -> Result<ChildPage>;
fn child_command(&mut self, resource: &str, function: &Function, phase: KeyPhase)
    -> Result<Option<Status>>;
fn child_action(&mut self, resource: &str, action: TypedAction) -> Result<Option<Status>>;
fn child_status(&mut self, resource: &str) -> Result<Status>;
```

`children` returns one page. If the device hands you everything at once, let
`ChildPage::fill(all, cursor)` do the cutting: it stops at 32 children or 48
KiB of serialized page, whichever comes first, always returns at least one
child while any are left, and uses the id of the first child of the next page
as the cursor - so paging is stable as long as your ids are, and a cursor
naming no child is `Error::Invalid`. A page with a cursor is never empty, and
the host reads at most 1024 children over at most 64 pages in ten seconds
before it decides the listing will never end and retires you.

A `Child` is an id, a kind, a name, an optional room hint, and the traits of
this particular lamp, blind or thermostat. Keep the ids stable: they are saved
with the room device and are how a binding made today still names the same lamp
tomorrow.

`child_command` and `child_action` answer `Ok(None)`, or `Ok(Some(status))`
with the state the child is in afterwards, which saves the panel a read after
every slider move. The host has already checked that the resource is well
spelt and that the child's kind declares what is being asked, so what is left
to you is the device. `dim:30`, `position:40` and `mode:heat` never arrive as
commands: the host turns each into the typed action the kind declares before it
writes anything.

`couch_plugin::testing_v3::children` is the admission case: it lists twice and
compares, checks the paging ends where the count says it should, refuses an
unknown resource, and proves a write's acknowledgement agrees with the next
read and that a kind never answers for another.

### Pairing, and the key Couch keeps

Protocol 3. A package that needs a key from the
device - a Hue hub's application key, a webOS client key, an Android TV
certificate - never draws a dialog and never stores anything. It describes one
step at a time; Couch draws its own headline, collects what the person types,
and keeps the key in `connections/<id>/plugin-credential.json` (root, mode
0600). A client that needs no key implements nothing here.

Declare it in the manifest, and only a protocol 3 manifest may:

```json
"pairing": { "required": true, "max_seconds": 120 },
"keep_alive": true
```

`required` means the connection is unusable until Couch holds a key, so the
daemon answers `unpaired` without starting you. `max_seconds` is how long you
want one attempt to last, from 10 to 300; Couch gives you that and never more.
`keep_alive` asks for your child not to be reaped when the connection goes
idle, for a device that costs seconds to reconnect to.

Three methods on `DeviceClient`, all defaulted:

```rust
fn connect_with(settings: &Self::Settings, credential: Option<&Credential>) -> Result<Self>;
fn pair_start(settings: &Self::Settings, existing: Option<&Credential>)
    -> Result<Box<dyn PairFlow>>;
fn take_credential(&mut self) -> Option<Credential>;
```

`connect_with` is the only way you ever see the key: it is not in the settings,
not in the environment, and not on disk anywhere you can read. `take_credential`
is for a device that rotates its key under you - return it once, and Couch
stores it beside the answer to the request that discovered it. `existing` on
`pair_start` is the key Couch already holds, for a device that issues a second
one against the first.

A `PairFlow` is the conversation:

```rust
use couch_sdk::{CodeAlphabet, Credential, PairFailure, PairFlow, PairInput, PairPrompt, PairStep, Result};

struct BridgePairing { bridge: Bridge }

impl PairFlow for BridgePairing {
    fn step(&mut self, input: Option<PairInput>) -> Result<PairStep> {
        // Whatever the person typed for the last prompt, if it asked for
        // anything. Couch has already checked it against that prompt.
        if let Some(PairInput::Code { code }) = input {
            return Ok(match self.bridge.submit(&code)? {
                Some(key) => PairStep::done(Credential::new(key)?, "Paired with the hall bridge"),
                None => PairStep::failed(PairFailure::WrongCode).because("That was not the code shown"),
            });
        }
        match self.bridge.poll()? {
            Pairing::Pressed(key) => Ok(PairStep::done(Credential::new(key)?, "Paired with the hall bridge")
                .with_settings(self.bridge.corrected_settings())),
            Pairing::NeedsCode => Ok(PairStep::waiting(PairPrompt::enter_code(6, CodeAlphabet::Hex), 0)),
            Pairing::Waiting => Ok(PairStep::waiting(
                PairPrompt::press_button().saying("The button is on top of the bridge"), 2000)),
            Pairing::Expired => Ok(PairStep::failed(PairFailure::TimedOut)),
        }
    }

    fn cancel(&mut self) { self.bridge.give_up(); }
}
```

`step` is called once to begin, and again for every poll or every submitted
code, and it must return well inside the host's twelve second request timeout:
**describe** a wait with `PairStep::waiting`, do not sleep through one. `serve`
owns at most one flow, names the session (`p1`, `p2`, …), refuses a step for
any other session, and drops the flow the moment a step is `done` or `failed`
or the person cancels. A second `pair_start` replaces the first.

Three prompts, and Couch writes the headline for each: `press_button` ("Press
the button on the device"), `approve_on_device` ("Approve on the device") and
`enter_code` ("Enter the code shown on the device"). `saying(..)` adds your one
line under it. `poll_after_ms` is how long Couch waits before asking you again:
500 to 10 000 milliseconds, or 0 for a code prompt, which means "come back when
they have typed it". `with_settings` on a `done` step hands back settings the
device would rather Couch saved - a corrected port, the address it answered on
- and they have to pass your own manifest.

Every bound is enforced by the host, and a step that breaks one is a protocol
error that costs you your child process. The constructors above clamp, so an
author using them cannot emit one: `PairStep::waiting` clamps the delay,
`PairPrompt::enter_code` clamps the length to 1..=16, and `saying`, `because`
and `done`'s summary clamp text to 160 printable bytes. A `Credential` is a
JSON object of at most 16 KiB, prints as `Credential(..)` and has no `Display`,
so nothing that formats a value containing one can leak it. Never put a key in
a `Reason`, a summary or a status field: those are shown to a person and
written to logs.

`couch_plugin::testing_v3::pairing` is the admission case: it pairs, checks
every step's bounds, proves the key reaches you and is never echoed back in an
answer, and drives a refusal, an expiry and a cancellation.

### 4. Tests, with no device

`couch_sdk::testing` (feature `testing`, enable it in `[dev-dependencies]`)
gives you a scripted TCP peer and a conformance check:

```rust
let host = MockHost::start(
    Script::new().terminator(b'\n')
        .on("CMD volume-up", Reply::line("OK"))
        .on("CMD power-off", Reply::line("ERR the TV is locked"))
        .on("CMD home", Reply::Silence)      // the client must time out
        .on("CMD ok", Reply::Close)          // and survive a hang-up
);
// The second argument is the actual host that observes every refusal the
// harness tries, so a client cannot claim to be silent with a made-up counter.
assert_contract::<EchoTv>(&settings(&host), &host);
```

`Reply::Silence` and `Reply::Close` are there because those are the two failure
paths clients get wrong. `host.requests()` is the assertion that matters most
often: it proves what your client did *not* send.

`contract_findings::<C>()` returns every problem at once rather than failing on
the first, and `assert_contract::<C>()` is the same check as an assertion.
Checking the returned error is not enough on its own - a client that pings the
device and *then* refuses returns exactly the right error and is still wrong -
so both inspect the mock host's request log and fail the client if a refusal
appears in it.
It also round-trips your settings through a real file to confirm they survive
being written at 0600 and read back.

`MockHost` answers a line protocol, and that is the only thing in this module
that assumes one. If your device speaks HTTP, TLS or WebSocket, write the fixture
your protocol needs and keep `MockHost` as the observer `contract_findings`
requires; the packaged admission cases in `couch_plugin::testing` take your
fixture directly through a `FakeDevice`, described in
[Catalog admission](development/admission.md). Point the client at it through
ordinary settings - a pinned certificate, an explicit API root - never through a
test-only branch in shipping code.

None of this reaches a shipped binary. The harness is behind an off-by-default
feature and belongs in `[dev-dependencies]`, so a production build links no
listener and spawns no thread:

```sh
cargo tree -p <your-crate> -e normal -f "{p} {f}" | grep couch-sdk   # "default"
cargo tree -p <your-crate> -e dev    -f "{p} {f}" | grep couch-sdk   # "default,testing"
strings target/release/libcouch_sdk.rlib | grep -c MockHost          # 0
```

### 5. Pinned TLS and Wake-on-LAN, if the device needs them

A LAN device that speaks TLS presents a self-signed certificate no public root
can verify. `couch_sdk::tls` (feature `tls`, off by default) is the answer the
existing clients converged on: `Pin` records the certificate seen while the
user approves the pairing and refuses any other one afterwards, carrying the
sentence that names *your* device:

```rust
let config = couch_sdk::tls::pinned_client_config(Arc::new(couch_sdk::tls::Pin::new(
    certificate,                                  // Arc<Mutex<Vec<u8>>>, empty until paired
    "Toaster certificate changed; pair again",
)))?;
```

`pinned_config_builder` is the same thing stopping before client
authentication, for a device that wants a client certificate too;
`verify_tls12_signature` and its two siblings are there for a client that pins
something else, such as a digest, and still needs the signature half. `Socket`
is the plain-or-TLS stream a client needs when the same device answers `ws://`
on one port and `wss://` on another, and `couch_sdk::wol` is the magic packet.

The feature is off by default deliberately: `rustls` builds `ring`, which is C
and assembly, and `couch-ir`, `couch-kodi`, `couch-denon` and `couch-echo` must
keep cross-compiling for the remote with no C toolchain at all. Ask for it only
in a crate that already speaks TLS.

## Registering a provider

The SDK does not wire your client into the product. Until you make these edits,
your crate compiles and its tests pass, and nothing else in the repository can
reach it. Five subsystems have to learn about a new provider - the model, the
control broker, the daemon, the device GUI and the web UI - which is seven
concrete edits spread across the five Rust workspaces `AGENTS.md` lists
(`model/`, `clients/`, `daemon/`, `ui/`, `web/`): `couch_sdk::catalog_differences::<YourClient>(&integration)` reports
the state of step 3 and is worth a test either way - `couch-echo` asserts that
it is *unregistered*, and `couch-denon` asserts that it matches exactly.

1. **`model/couch-model/src/connection.rs`** - add a `Provider` variant plus its
   `kind()` slug and `label()`, and a `resolve_integration` arm.
2. **`model/couch-model/src/device.rs`** - add the matching `Integration`
   variant and its `via()` string. This is the discriminator in saved
   configurations, so pick it once.
3. **`model/couch-model/src/buttons.rs`** - add the `functions()` arm. It must
   equal your `capabilities()`, in the same order, including labels. If you
   support `input:`/`app:`, also extend `commands.rs`'s `Function::supports`.
   Add a rule in `validate.rs` if your provider has one (there may only be one
   IR blaster, for instance).
4. **`clients/couch-control`** - a `Spec` variant, the `Op` variants for your
   operations, a `Client` enum arm, a `From<your::Error>` impl, and a proxy
   struct in `proxies.rs`. This is what gives you a single owner per endpoint,
   leases, and the queue. Do not open your own long-lived socket from the GUI.
5. **`daemon/couch-confd/src/api/<kind>.rs`** - the REST surface, plus `mod` in
   `api.rs` and an arm in `api/connections.rs` mapping
   `/api/connections/<id>/<kind>/...` to it and naming your credential file.
6. **`ui/couch-gui/src/activity_buttons.rs`** - an arm in `execute_with_input`,
   and a helper in `connections.rs` if you load credentials there.
7. **`web/couch-web/src/screens/`** - a setup screen, plus the two matches in
   `screens/connections.rs` that offer and render your provider.

Then update `docs/connections.md`, which is the document a user reads.

Do not skip step 4 by talking to the network from the GUI: two consumers, a
browser and the panel, reach the same device, and the broker is what stops them
opening two sockets and interleaving a mute read with someone else's write.

## What the broker guarantees, and what it expects

From `docs/control-service.md` and `clients/couch-control/src/lib.rs`:

- One worker per endpoint, so a slow receiver cannot block a different device.
- A 16-deep queue per endpoint; a full queue is an error, not a wait.
- Work that has sat in the queue for 750 ms is dropped rather than sent late.
- Each consumer holds a lease; the transport closes when the last one is
  released, and unused leases expire after 30 seconds, including those lost to
  a crashed process.
- Changing a connection's credentials discards the old transport.

What it expects from you in return: **one instance owns one device's transport,
does no internal queuing, and is never shared between threads.**

## Building for the remote

```sh
cd clients
cargo build -p couch-echo --release --target armv7-unknown-linux-musleabihf
file target/armv7-unknown-linux-musleabihf/release/libcouch_echo.rlib
```

A cross-built binary should come out static and stripped - `couch-denon` is an
`ELF 32-bit LSB executable, ARM, EABI5, statically linked, stripped` of about
380 KB.

Notes that will save you a day:

- The workspace release profile is `opt-level = "s"`, LTO, one codegen unit,
  `panic = "abort"`, stripped. Binaries land in
  `clients/target/armv7-unknown-linux-musleabihf/release/`.
- `panic = "abort"` means `catch_unwind` is not available to you.
- The linker is `rust-lld`, set in `clients/.cargo/config.toml`, because Apple's
  `ld` rejects the flags rustc passes for this target. No cross-gcc, no Docker.
- **Any dependency that binds a C library costs you a cross toolchain.** That
  is why `couch-voice` implements ALSA ioctls against `libc` rather than using
  an ALSA crate, why `couch-ir` writes the MediaTek PWM ABI directly, and why
  the daemon uses `tiny_http` rather than an async stack. The one place the
  repository pays the price anyway is TLS: `ring` needs an ARM musl C compiler,
  supplied through `CC_armv7_unknown_linux_musleabihf` as described above.
  Before adding a dependency, check what it links.
- Cross-compiling proves it builds and links. It does not prove it runs: the
  remote is a quad-A7 with an Alpine userland and a flash-backed rootfs.

## Limits, honestly

- **No runtime loading, no ABI, no distribution.** Covered above, and worth
  repeating because every "SDK" implies otherwise.
- **The SDK is 0.1.0 and sourced from this repository.** It has no stability
  guarantee or crates.io release. Independent repositories must pin one full
  Couch commit and update deliberately.
- **Two existing clients have been adapted.** `couch-denon` uses the shared
  settings helper and implements `DeviceClient`; `couch-sonos` implements both
  over its HTTPS Control API transport, and shows what a client does when the
  mock host cannot speak its protocol (`docs/sonos.md`). The other clients keep
  their own shapes; there is no migration in progress and none is required.
- **No streaming or subscription API.** `couch-kodi` receives pushed updates,
  and that path stays in the client and the broker. The SDK
  covers request/response, status and enumeration only.
- **No async.** Everything is blocking with explicit deadlines, matching the
  rest of the repository.
- **Discovery is the daemon's.** The SDK's `Discover` trait carries a service
  name and a translation from a found address to settings - the same fact
  `couch-androidtv` and `couch-appletv` already publish as `MDNS_SERVICE`, and
  `couch-echo` implements it - but the mDNS browser stays in
  `daemon/couch-confd/src/api/streaming_tv.rs`, so one process rather than five
  holds a multicast socket. Nothing calls `Discover` yet.
- **`MockHost` is a line protocol; the admission harness no longer is.**
  `MockHost` itself still fits Denon, Kodi over TCP and the example, and is not
  an HTTP server, a TLS endpoint or a WebSocket peer. A client needing one of
  those writes its own fixture, as `couch-ha`, `couch-hue` and `couch-sonos` do.
  What changed is that `couch_plugin::testing` takes that fixture: see
  [the admission cases](development/admission.md) for the two-method
  `FakeDevice` trait. `contract_findings` is unaffected and still earns its
  place: everything it decides before connecting - the slug, the labels,
  canonical and unique capability ids, the gate agreeing with the declaration,
  and the settings surviving a real 0600 file - runs against any settings, and a
  client whose settings can name its fixture's origin (`couch-sonos` has an
  `api_root`) gets the connected half as well, with `MockHost` staying on as the
  observer that proves a refusal cost no round trip.
- **`cargo fmt --check` is not clean on committed code** in any of the five
  workspaces, including `clients/`. Check your own crate with
  `cargo fmt -p <crate> -- --check` and leave the rest alone unless you are
  fixing it deliberately.

## Validation of this SDK

Recorded so the next person knows what was actually run, on 11 September 2026,
with rustc 1.98.1 on both machines.

Host tests, all passing, on the Linux build host and on macOS:

| Workspace | Result |
|---|---|
| `clients/` (`cargo test --workspace`) | 179 passed, 0 failed |
| `model/` | 41 passed, 0 failed |
| `daemon/` | 44 passed, 0 failed |
| `ui/` | 109 passed, 0 failed |

The `couch-denon` adaptation is covered by its own regression tests: the
on-disk JSON schema and filename, mode 0600, a rejected save leaving the
previous file byte for byte, a write that fails after the temporary file exists
cleaning up after itself, the absent-versus-corrupt error split, and the
settings validation table. Its transport tests are unchanged.

One behaviour there is deliberately *not* preserved. Before this change, a
failed read or write of the settings file produced `couch_denon::Error::Io`,
whose message is "Cannot reach the AVR. Check its address and Network Control
setting." - so a full or read-only flash sent the user to their receiver's
network settings. Those two paths now return a separate `Error::Storage` with
a storage-specific, credential-safe message. Every network path still returns
`Io` with its original message, and nothing outside the crate reads or writes
that file.

Also run: `cargo fmt -p couch-sdk -p couch-echo -p couch-denon -- --check`
clean on both machines; `cargo doc -p couch-sdk --no-deps` with and without
`--features testing`, 0 warnings; and `cargo run -p couch-echo --example demo`,
which printed the acceptance, refusal, gate and timeout lines shown earlier.

Cross-compilation for `armv7-unknown-linux-musleabihf`:

- `couch-sdk`, `couch-echo` and `couch-denon` build on the Linux host with no
  cross C toolchain at all. `couch-denon` comes out a statically linked,
  stripped ARM ELF of 385 KB.
- The **whole** `clients/` workspace needs `ring`'s C compiler. It built on
  macOS with `CC_armv7_unknown_linux_musleabihf=tools/arm-musl-cc.py` and Zig,
  and it does **not** build on the Linux host as that host is currently set up,
  because it has neither a musl cross gcc nor Zig. That is a property of the
  host, not of this change.

Not validated, and not validatable without hardware: anything on the remote
itself. No device, USB, serial or flashing operation was involved in any of the
above.

## Hardware validation boundary

Passing tests mean your protocol handling, capability gating, error mapping and
settings persistence are right. They say nothing about whether a real device
accepts your commands, whether the remote's panel renders your provider, or
whether anything behaves under the device's memory and flash constraints.

Device validation is separate, is recorded separately, and needs the hardware:
see `AGENTS.md` and the validation notes in `docs/`. IR transmission in
particular is still unavailable on the current kernel (`docs/connections.md`).
Never write the `preloader_*` or `lk` partitions. Keep credentials, partition
backups and per-device calibration out of Git.
