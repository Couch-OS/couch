# Protocol 3, step T1 "Groundwork": implementation plan (approved by the orchestrator 2026-09-19)

Status: complete (couch #237, #240, #241).

Studied at origin/dev a89051b. `v0.1.0-alpha.20260919.188` has zero tree diff to that commit; tested_commit f387898 is its ancestor.

## Summary
T1 teaches Couch the vocabulary of protocol 3 without letting any protocol-3 package in. The host decides what to send a package from the version in its manifest. The saved configuration gains a third, outermost layer (`integration_config_v3`) holding anything an older Couch cannot read; the layers underneath are exactly what .188 writes, so rolling back to .188 always loads, and a .188 save wins after re-upgrade. Errors may carry a short reason, there is a new `unpaired` code, a package may name buttons of its own (`x:info`), and a key press can say tap / repeat / long press. None of it is reachable in a shipped build: a manifest that says 3 is still refused, `PROTOCOL_VERSION` stays 2, the feed accepts only 1 and 2, and every byte sent to the published Denon/Sonos/Kodi packages is identical.

## Findings that shape the plan
- Old children are strict: at SDK rev 00ab4da `Request`, `Response`, `Envelope`, `Manifest` are `deny_unknown_fields` (clients/couch-plugin/src/protocol.rs:68-94). Any new request field that is SERIALIZED kills an old child's parse. Every addition must vanish from the bytes when absent/default.
- `Request::Command { function }` struct literals exist in the digest-pinned harness (clients/couch-plugin/src/testing.rs:401,457,478), couch-echo/tests, couch-sonos/tests/admission.rs, confd (api/plugins.rs:143) and the GUI (tv_plugin.rs:154, activity_buttons.rs:1190). Adding `phase` breaks them at compile time, so T1 must touch testing.rs (3 mechanical lines -> `Request::command(..)`).
- `local_request` already converts `Response::Error` to `Err(code)` (host.rs:525): GUI arms `Ok(Response::Error{code})` are dead and a reason would be lost. A detailed error path is needed.
- Scene/activity STEPS are validated by `Function::parse().is_some()` only (validate.rs:355,413). Once `x:foo` parses, a step `x:foo` aimed at ANY device is valid on the new core and fatal on .188. Both projection and validation must handle it.
- .188 validate.rs:171,185 rejects a Plugin snapshot with an `x:` capability or `actions.len() > 1`; .188 `PluginComponent`/`PluginActionSchema`/`TypedAction` are closed + deny_unknown_fields. `Provider::Plugin`/`Integration::Plugin`/`Config`/`StoredConfig` are NOT deny-unknown: unknown fields are ignored, unknown enum tags are fatal.
- The verifier pins the literal `pub const PROTOCOL_VERSION: u32 = 2;` and the `--supports-integration-protocol=N` strings in confd main.rs (verify_integration_set.py:138-147). T1 leaves both alone; main.rs is not touched.
- GUI and confd ship in one runtime bundle, so plugin.sock has no version skew.

## Rollback: v2 today, v3 extension
Today (model/couch-model/src/storage.rs:8-50): file = `Config` flattened (= `projection(config)`: no Plugin connections, plugin devices -> Integration::None, their bindings nulled) + `integration_config` (= `v1_projection`; written only if != rollback) + `integration_config_v2` (full; written only if != v1). `into_config` recomputes each projection from the richer layer and FAILS CLOSED on mismatch. An old core ignores the unknown key and rewrites the file without it on save, so its save is authoritative.

v3: one more layer, computed BY COMPOSITION so .188's checks hold by construction:
```rust
pub struct StoredConfig { #[serde(flatten)] rollback: Config,
  integration_config: Option<Config>, integration_config_v2: Option<Config>,
  #[serde(default, skip_serializing_if = "Option::is_none")] integration_config_v3: Option<Config> }
pub fn new(c: &Config) -> Self {
  let v2 = v2_projection(c); let v1 = v1_projection(&v2); let rollback = projection(&v2);
  Self { integration_config: (rollback != v1).then(|| v1.clone()),
         integration_config_v2: (v1 != v2).then(|| v2.clone()),
         integration_config_v3: (v2 != *c).then(|| c.clone()), rollback } }
// into_config: v1 as today; v2 = v2 layer (checked v1_projection(v2)==v1) or v1;
// then Some(c) if v2_projection(&c) == v2 => Ok(c); Some(_) => Err("Protocol-v3 configuration does not match its protocol-v2 projection"); None => Ok(v2)
// has_integrations() |= integration_config_v3.is_some()
```
`v1_projection` and `projection` are NOT edited. `v2_projection` strips by VOCABULARY, not by version, as small predicates T2-T6 extend:
- `v2_capability(id)`: drop `x:` ids from `Provider::Plugin.capabilities` and `Integration::Plugin.capabilities`.
- `v2_component`: T1 = drop a `CommandGroup` left empty after removing `x:` ids and a `Toggle` whose on/off is `x:`.
- `v2_action_schema`: `actions.retain(is SetVolumeDb); actions.truncate(1)`.
- `v2_command(&str)`: `!starts_with("x:")`, applied at the same six sites `v1_projection` uses (buttons -> `action = None` kept as explicit disabled binding; `steps`, `setup.on/off`, `pages[].widgets`, `scenes[].steps`) FOR EVERY DEVICE, not only plugin devices.
- Hook for T2+: new FIELDS on Plugin variants must be skip_serializing_if empty and cleared here; new VARIANTS must be removed here.
Required properties (unit-tested): idempotent; `v2_projection(c)==c` for any config .188 can produce (plain/v1/v2 files keep their exact bytes: extend `plain_files_keep_their_shape...`); `v2.validate()` passes; a .188 save (file minus v3 key) loads as v2 and never resurrects `x:` bindings; a tampered v3 layer fails closed.

Cross-version test (PR1, mandatory): `tools/tests/config-crossload.sh` (NOT under tools/release/**): `git archive v0.1.0-alpha.20260919.188 model | tar -x -C $TMP/old`, scratch crate in $TMP with `old = { package = "couch-model", path = "$TMP/old/model/couch-model" }` and `new = { package = "couch-model", path = "<repo>/model/couch-model" }`, then for each state do what `Store::open` does (StoredConfig parse -> into_config -> validate). new-writes/old-reads: (A) seed, no plugin; (B) v1 plugin; (C) v2 plugin with dB control + spaced inputs; (D) v3: `x:` capability + `x:` in all six binding sites + `x:` in a CommandGroup/Toggle; (E) D on a Denon-converted connection; (F) D where v1==v2 (v2 layer absent, v3 present); (G) `x:` step aimed at a non-plugin device must be refused by new validate. Assert old loads+validates every file, old's view equals new's v2_projection, then old SAVES and new loads that save == v2 projection. old-writes/new-reads: A-C written by old `StoredConfig::new` load on new with equal Config, and new re-serializes them byte-identically. Run it in the integration-admission.yml model step.

## Type / serde changes per file
model/couch-model/src/commands.rs:
```rust
pub enum Function { /* ... */ Custom(String) }   // parse: "x:" + 1..=48 bytes of [a-z0-9_-]; id(): format!("x:{id}")
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum KeyPhase { #[default] Tap, Repeat, LongPress }  impl KeyPhase { pub fn is_tap(&self) -> bool }
```
`supports()`: Plugin branch already matches by id(); non-plugin integrations -> false. `repeatable()` false for Custom. Max 32 custom ids per manifest.
buttons.rs: `function_choices` lists `x:` under the package label; add `pub fn key_phase(gesture: Gesture, repeat: bool) -> KeyPhase` (Long -> LongPress, repeat -> Repeat, else Tap).
volume.rs: `TypedAction::kind()` / `PluginActionSchema::kind()` (small `ActionKind` enum); no new variants in T1.
validate.rs: `actions.len() > 8 || duplicate kinds`; `VolumeDbControl` needs A valid SetVolumeDb schema rather than len()==1; steps/scene steps whose command is `Function::Custom` additionally require `supports_device`.
connection.rs, device.rs: no change. lib.rs: re-export KeyPhase. storage.rs: above. integration_migration.rs: untouched.

clients/couch-plugin/src/protocol.rs:
```rust
pub const PROTOCOL_VERSION: u32 = 2;                       // unchanged literal
pub const NEXT_PROTOCOL_VERSION: u32 = 3;
pub const fn accepted_protocol_version() -> u32 { if cfg!(feature = "protocol-3-preview") { NEXT_PROTOCOL_VERSION } else { PROTOCOL_VERSION } }
pub enum Error { /* nine */ Unpaired }                     // "unpaired"
Command { function: String, #[serde(default, skip_serializing_if = "KeyPhase::is_tap")] phase: KeyPhase },
Error { code: Error, #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<Reason> },
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reason { InvalidSetting { field: String, text: String }, Message { text: String } }
pub struct Failure { pub code: Error, pub reason: Option<Reason> }   // Display; From<Error>
impl Request { pub fn command(function: impl Into<String>) -> Self; pub fn key(function, phase) -> Self }
```
`Reason` lives in couch-sdk/src/error.rs and is re-exported. All call sites move to `Request::command(..)`.
Compatibility: `phase` skip_serializing_if => host->old-child bytes identical; default => a v3 child reads a phase-less frame. `reason` default => old error shape parses; skip => new-SDK v1/v2 children's error bytes identical. `Unpaired` may only travel child->host when the manifest says 3.
manifest.rs: range check uses `accepted_protocol_version()`; `actions.len() > if v>=3 {8} else {1}` + unique kinds; `validate_action` finds schema by kind; a capability parsing to `Function::Custom` is Invalid when protocol_version < 3.
host.rs — one gate (`Host::request_detailed`, `request()` = `.map_err(|f| f.code)` so existing signatures are unchanged): before I/O: `if manifest.protocol_version < 3 { phase = Tap }` (downgrade, not refusal); `x:` to a v<3 package is Unsupported via manifest.supports; `fn requires(&Request) -> u32` min protocol per request. After I/O: for protocol_version < 3 a reply carrying `reason` or `code: unpaired` is `Error::Protocol` and retires the child (precedent host.rs:283). For v3: text <= 160 bytes, no control chars, `field` must be a declared setting id, else Protocol. `Endpoint`/`Pending` carry `Result<Response, Failure>`; add `Endpoint::request_detailed`, `local_request_detailed`; `LocalRequest` unchanged.
server.rs: inner error becomes Failure; reason dropped unless manifest.protocol_version >= 3; `Command{function, phase}` -> `client.command_phased`; an `x:` function refused unless declared.
clients/couch-sdk: error.rs gains `Reason`, `Error::Unpaired`, `Error::Explained { error: Box<Error>, reason }` + `Error::because`; client.rs gains `execute_phased` (default = execute) and `command_phased`; validate_action by kind; testing.rs accepts `x:` ids.
testing.rs (digest-pinned): three literals -> `Request::command(..)`. Nothing else. DO NOT edit tools/release/tested-integrations.json in train PRs; the digest is updated in the next renewal commit.
confd plugins.rs: `execute` returns `Result<Response, Failure>`; save_settings/prepare_legacy surface InvalidSetting. api/plugins.rs: HTTP error body gains additive "code" and "reason" keys; Unpaired -> 409; socket path relays reason. main.rs untouched.
GUI/web: PR2 = compile fixes only; PR3 = detailed errors + `Request::key(id, key_phase(gesture, repeat))`; web marks the offending field from reason.field.

## The switch, the harness, the gate
- Single switch: Cargo feature `protocol-3-preview` on couch-plugin (non-default, no new deps). Only changes `accepted_protocol_version()`. Shipped binaries never enable it; guard with a default-feature test in couch-confd and couch-integrations: `assert_eq!(couch_plugin::accepted_protocol_version(), couch_plugin::PROTOCOL_VERSION)`. The MODEL is deliberately not gated: it must read everything it ever wrote.
- From the merge of PR1 the evidence verifier fails: devbuild.sh refuses to build and any PR touching tools/release/**, daemon/couch-updates/**, daemon/Cargo.lock, stage2/runtime-boot.sh gets a red compat job. Never edit tested-integrations.json, shrink contract_paths or skip the gate to get green. Say so in each train PR description. Feed validate_feed.py stays {1,2} until T7.

## PR split (each green alone and releasable as a protocol-2 core)
PR1 — model: vocabulary + v3 envelope (commands/buttons/volume/validate/storage/lib + tools/tests/config-crossload.sh in CI).
PR2 — plugin + sdk: wire types, gate, switch; mechanical compile fixes in confd, GUI, echo/sonos tests. Tests: golden bytes for every v1/v2 request/response/manifest captured from 00ab4da + frozen mirror copies of the 00ab4da deny-unknown Request/Response proving both directions; scripted-child tests (v3 manifest refused without the feature; with it: phase serialized only for v3, downgraded for v2, undeclared `x:` refused before I/O, reason/unpaired from a v2 child retires it, oversize reason retires a v3 child); REAL OLD BINARIES: `tools/tests/old-package-wire.sh` archives 00ab4da, builds couch-plugin-echo and couch-plugin-sonos there, and runs the new tree's admission suites against them via a `COUCH_ADMISSION_BINARY_<ID>` override in clients/couch-{echo,sonos}/tests (non-frozen); echo gains a feature-gated v3 fixture declaring `x:info` and recording phase.
PR3 — confd + GUI + web: carry the reason, send the phase.

## Settled questions
1. reason/unpaired from a v1/v2 child = protocol error (strict). 2. The child-side `serve` obeys the same switch. 3. `x:` grammar: `x:` + [a-z0-9_-]{1,48}, max 32 per manifest, never repeatable in T1, never valid for a device that does not declare it. 4. Dropping stale queued repeats: deferred to T4. 5. Only `InvalidSetting` and `Message` reasons in T1. 6. Probing protocol_version before the full manifest parse: T2. 7. A config imported over HTTP may contain `x:` with the switch off: accepted (envelope keeps rollback safe; the live manifest gate refuses execution).
