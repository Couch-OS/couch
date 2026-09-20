# Protocol 3, step T3 "Pairing and sessions" + per-package uid: implementation plan (approved 2026-09-19)

Studied at origin/dev 7437ed5 (T1 complete). Dev remote 192.168.1.127 was read (read-only) for kernel facts.

## Owner decisions (Bryan, 2026-09-19)
- Re-pairing: the device keeps working on its old pairing until the new one succeeds (pairing runs in a separate pinned child).
- Dialog wording: Couch's own headline per step ("Press the button on the device", "Approve on the device", "Enter the code shown on the device") with the package's one line under it. "Forget pairing" confirm text: "This removes Couch's copy of the key. The device may still list Couch as paired." A wrong code ends the attempt and offers "Try again".
- keep_alive cap: 8 packages; beyond it the least recently used are reaped normally; only connections referenced by a device count.
- Pairing times out after what the package asks, never more than 5 minutes.
- The panel only says "Needs pairing - open Couch in a browser"; no pairing from the panel in T3.
- PR 1 (uid) gets a dev build on the remote and its own evidence renewal before the rest of T3 builds on it.
- Earlier decisions: one uid per package before any package stores a secret; deleting a connection purges `connections/<id>/` except Matter (#233).

## Summary
A package only DESCRIBES pairing steps; Couch draws the dialog and stores the key. The key lives in `connections/<id>/plugin-credential.json` (root, 0600), apart from typed settings, never sent over HTTP, never exported, deleted with the connection (#233 removes the whole folder). NOTHING is added to config.json ("paired" is derived from the file; pairing rules come from the live manifest), so rollback to .188 needs no new projection rule. Every package gets its own uid; NOTHING on disk is chowned, so rolling back to .188 (children as 65534) just works. FINDING: `prctl(PR_SET_DUMPABLE,0)` set before exec does NOT survive `execve`; the SDK must set it inside the child and the host must verify it. No built-in needs free-text/password entry during pairing: three prompts suffice. The panel never starts LAN pairing today.

## 0. How built-ins pair today
| Built-in | Flow | Credential file | Packaged mapping |
| --- | --- | --- | --- |
| Hue `api/hue.rs:48-67` | `PUT connection` = one `Hue::pair` attempt; press button then click Pair again | `hue-connection.json` (url, application key, leaf DER) | `PressButton`, poll 2 s |
| webOS `api/webos.rs:83-103`, `couch-webos/src/lib.rs:177-190` | one HTTP call blocks up to 60 s in `register(None, 60s)` | `webos-connection.json` (client_key, certificate); GUI writes `webos-wake.json` (MAC from /proc/net/arp, `ui/couch-gui/src/tv.rs:162-201`) | `ApproveOnDevice`; package registers on its own thread; wake MAC learned later => `store_credential` |
| Android TV `api/streaming_tv.rs:26-45,158-225,228-296` | in-memory `Pending{token,deadline 120 s,pair}`, max 8, reaper; TLS session held between start and finish | `androidtv-connection.json` | `EnterCode{6,Hex}`, `poll_after_ms:0`; pinned child holds the TLS session |
| Apple TV + AirPlay metadata `api/airplay.rs` | same sessions; 4-digit PIN; a second optional pairing | `appletv-connection.json`, `appletv-metadata-connection.json` | `EnterCode{4,Digits}`; two Waiting steps in one session are legal |
| Tizen `couch-tizen/src/lib.rs:319-349` | `handshake(60 s)`; `connect` may be issued a NEW token | `tizen-connection.json` | `ApproveOnDevice`; rotation => `store_credential` |
| HA / Protect / Kodi | typed token / key / password | `*-connection.json` | manifest `secret` settings, no pairing |
Files are root 0600 under `/opt/couch/connections/<id>/`. For packages the GUI never reads credential files; it goes through plugin.sock.

## 1. Wire (clients/couch-sdk/src/pairing.rs NEW, re-exported by couch-plugin; protocol.rs, manifest.rs, host.rs, server.rs)
```rust
/// Opaque JSON object, <= 16 KiB serialised. Debug prints "Credential(..)"; no Display.
#[derive(Clone, PartialEq, Serialize, Deserialize)] #[serde(transparent)]
pub struct Credential(serde_json::Map<String, Value>);
impl Credential { pub const MAX_BYTES: usize = 16 * 1024; pub fn new(v: Value) -> Result<Self>; pub fn get(&self) -> &Map<..>; }
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairStep {
    Waiting { prompt: PairPrompt, poll_after_ms: u32 },
    Done { credential: Credential, #[serde(default, skip_serializing_if = "Option::is_none")] settings: Option<Value>, summary: String },
    Failed { reason: PairFailure, #[serde(default, skip_serializing_if = "Option::is_none")] message: Option<String> },
}
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairPrompt {   // `message`: the package's one line, <= 160 bytes, no control chars
    PressButton     { #[serde(default, skip_serializing_if="Option::is_none")] message: Option<String> },
    ApproveOnDevice { #[serde(default, skip_serializing_if="Option::is_none")] message: Option<String> },
    EnterCode       { #[serde(default, skip_serializing_if="Option::is_none")] message: Option<String>, length: u8, alphabet: CodeAlphabet },
}
#[serde(rename_all="snake_case")] pub enum CodeAlphabet { Digits, Hex, Alphanumeric }
#[serde(tag="kind", rename_all="snake_case", deny_unknown_fields)] pub enum PairInput { Code { code: String } }
#[serde(rename_all="snake_case")] pub enum PairFailure { Unreachable, Refused, WrongCode, TimedOut, Unsupported }
```
Request gains:
```rust
Configure { settings: Value, #[serde(default, skip_serializing_if = "Option::is_none")] credential: Option<Credential> },
PairStart { settings: Value, #[serde(default, skip_serializing_if = "Option::is_none")] credential: Option<Credential> },
PairContinue { session: String, #[serde(default, skip_serializing_if = "Option::is_none")] input: Option<PairInput> },
PairCancel { session: String },
```
Response gains `Pairing { session: String, step: PairStep }`; `PairCancel` is answered `Ok`. (`PairStart.credential`: decided yes — Apple TV's second pairing needs the existing key.)
Limits (checked in accept/validate_response; a breach retires the child): credential <= 16 KiB and a JSON object; session `[A-Za-z0-9._-]{1,64}` equal to the one the host holds; `poll_after_ms` is 0 only with EnterCode, else 500..=10_000; summary/message <= 160 bytes no control chars; `Done.settings` passes `manifest.validate_settings`; `EnterCode.length` 1..=16. Host validates `PairInput::Code` against length/alphabet BEFORE sending (400, child not asked). SDK constructors clamp.
Manifest, both skipped when default, both `Invalid` when protocol_version < 3:
```rust
pub pairing: Option<Pairing>,   // #[serde(deny_unknown_fields)] struct Pairing { required: bool, max_seconds: u16 /* 10..=300 */ }
pub keep_alive: bool,           // skip_serializing_if = "core::ops::Not::not"
```
Gate (host.rs, the ONE place): `requires()` = 3 for Pair*; `admit` returns Unsupported if `manifest.pairing.is_none()`; for version < 3 OR pairing none, `Configure.credential` is set to None (same pattern as the phase downgrade) so a package rolled back from v3 to v2 with a credential file still gets today's bytes. `validate_response` pairs (PairStart|PairContinue, Pairing{..}) and (PairCancel, Ok). `accept`: `Response::Pairing` or a non-empty `store_credential` from version < 3 => Protocol.
Envelope: leave `Envelope<T>` alone for requests; add
```rust
#[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub(crate) struct ReplyEnvelope { pub id: u64, pub body: Response,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub store_credential: Option<Credential> }
```
New SDK serving a <3 manifest never writes it. `Host::request_full(..) -> Result<(Response, Option<Credential>), Failure>`; existing signatures drop it.
Rotation acceptance (in confd): only on the reply to Command|Action|Status|Inputs (never Hello, Configure, Pair*), only from a v3 manifest with `pairing`, only on the connection's normal endpoint, only if a credential already exists (rotation cannot create a pairing), <= 16 KiB, written atomically under the connection lock BEFORE the body is returned, skipped (logged without contents) if a pairing session on that connection reached Done after the request was queued. On a storage failure the body is still returned.
Goldens/mirrors: `wire-00ab4da.tsv` rows stay byte-identical (assert `Configure{credential:None}` and `ReplyEnvelope{store_credential:None}` reproduce them); mirror: old child parses every admitted Configure even when the host HOLDS a credential; new v3 rows in a separate `wire-v3-preview.tsv`; `old-package-wire.sh` gains a run with a planted `plugin-credential.json`.

## 2. Host session rules (daemon/couch-confd/src/plugins.rs)
`pairings: Mutex<HashMap<String /*connection*/, PairingSession>>` on Runtime: `PairingSession { token /*host-minted 128-bit hex = the HTTP <session>*/, child_session, host: couch_plugin::Host /*raw Host, not an Endpoint*/, generation, settings, started, deadline, last_poll, prompt_polls, pending_done: Option<(Credential, Option<Value>, String)> }`.
- Pinned and exclusive: a dedicated Host never in `endpoints`, so neither `reap` nor generation/settings eviction touches it; never restarted (a dead pairing child => Failed{Unreachable}).
- Normal requests meanwhile are NOT Busy: the ordinary endpoint keeps serving with the old credential (or answers Unpaired if none). Two children of one connection for <= 300 s.
- One pairing per connection: a second start cancels and replaces the first. Global cap 8 sessions => 409.
- Each PairStart/PairContinue uses REQUEST_TIMEOUT 12 s; overall cap `min(manifest.pairing.max_seconds, 300)`; at the deadline the host sends PairCancel with a 2 s timeout then drops the Host (process-group SIGKILL).
- Browser gone: for polling prompts no poll for `max(3 * poll_after_ms, 15 s)` => cancel; for EnterCode only the overall deadline. Swept by the existing 10 s integration-reaper thread. The dialog also sends DELETE on close and pagehide.
- Locks: pair calls do NOT take the per-connection settings lock while talking to the child. Only the Done write takes it, patiently 15 s; if still busy the result is parked in `pending_done` and the reply is `waiting`, so the next poll retries the write without asking the child again. `packages.read_lease()` held for the write. Package generation changed since `started` => session failed.
- Done: write credential, then settings (if given: with_defaults + validate_settings), then `endpoints.remove(connection)`. Failed/cancelled/expired: nothing written.
- `retire(connection)` also removes the pairing session (delete_connection already calls it under the write gate).
- confd restart mid-pairing: child exits when its stdin closes; next poll gets 404; dialog says "Pairing was interrupted. Start again." Nothing written.
- The connection must already exist. Host-made Unpaired: `execute` returns Unpaired without spawning when `pairing.required` and no credential file.
keep_alive: exempts an entry from the two idle retains (IDLE = 60 s) and nothing else; only while the connection is referenced by a device (`reap(&self, in_use: &dyn Fn(&str) -> bool)`); cap MAX_KEEP_ALIVE = 8, LRU beyond. Measure child RSS in the PR 1 hardware pass (none was alive when read; confd 9 MB, gui 18 MB; expect 1–2 MB plain, 3–5 MB with rustls).

## 3. Credential storage
- Path `settings_path(connection)?.with_file_name("plugin-credential.json")`; `couch_sdk::save_private` (atomic, 0600, fsync); folder forced 0700; same `lock_for(plugin-connection.json)` lock (no new STORED_LOCKS entry).
- Loaded in `execute` next to `load_settings`; `Running` gains `credential`; the eviction test compares it; `Endpoint::start` passes it to `Host::configure`.
- Never over HTTP, never logged (redacting Debug, no Display). Config export and recovery export are built from config.json only; add a test saying so.
- Deleted with the connection (add the file to #233's test `a_packaged_connection_loses_its_settings_and_its_running_child`). Forget: `DELETE …/plugin/credential` removes the file under the lock, drops endpoint and pairing session; does not talk to the device.
- `GET …/plugin/settings` gains `"paired": bool`, `"pairing": {"required":..}` (only when the manifest declares pairing) and `"summary"` kept in a sibling `plugin-pairing.json` `{summary, paired_at}` (0600, not secret, removed with the credential).
- Legacy converter hook (design now, no row uses it): `LegacyBuiltin` gains `credential_file: Option<&'static str>` and `credential: Option<fn(&serde_json::Value) -> Option<serde_json::Value>>` (pure JSON->JSON, no I/O in the model). `prepare_legacy` takes the mapped credential, `check_settings` configures with it, `adopt_legacy` writes `plugin-credential.json` before `plugin-connection.json` and LEAVES the old file in place (rollback to a core with the built-in still finds its pairing; #233 removes both on delete).

## 4. Per-package uid and hardening (PR 1, NOT behind the switch)
Kernel facts (remote): `3.18.79-couch-normal`; /proc mounted rw,relatime (no hidepid) in both roots; no Yama; CONFIG_SECCOMP=y and SECCOMP_FILTER=y; no USER_NS; no Landlock; SELinux compiled, not mounted; CONFIG_ANDROID_PARANOID_NETWORK=y; CROSS_MEMORY_ATTACH=y; core_pattern=core, fs.suid_dumpable=0; `/proc/net/ip_tables_matches` lists `owner` (via qtaguid) but there is no iptables binary in the chroot. Slots are root-owned, dirs 0755, `bin/couch-plugin-denon` 0755.
Design:
- TABLE, not hash: `/opt/couch/integrations/uids.json` at the store ROOT (`{"next":60003,"packages":{"denon":60000,...}}`, root 0644, atomic_write, written under the store's exclusive `.lock`). NOT in `state/<id>` (`Selection` is deny_unknown_fields, couch-integrations/src/lib.rs:79, .188 would refuse the package) and not a new file in `state/` (`list()` treats every entry there as a package id). .188's `recover()` only deletes `.staging-*` at the root and `list()` reads only `state/`, so .188 ignores the table.
- Range 60000..=64999, gid = uid, never recycled (a removed package keeps its row). A hash was rejected (collisions; a custom-repo package could pick an id to collide with a victim). Package ids are unique per store.
- Allocation: `Store::identity(id) -> Result<(u32,u32)>`; assigned in `admit_expected` (lib.rs:411) at install and by one `assign_identities()` pass in `Runtime::new` for what is installed. A missing/corrupt table is rebuilt (safe: NOTHING on disk is owned by these ids). Exhaustion or a busy store => Error::Busy, never a silent fallback to 65534.
- Nothing is chowned; slot modes must not change (`tree_digest` pins modes, lib.rs:881). A child needs no writable directory: cwd `/`, empty environment, stdio is the socket.
- host.rs: `HostPolicy::for_package(uid, gid)` keeps `supplementary_gids = [3003]` (AID_INET) on arm-musl (line 37) and `[]` elsewhere; `Endpoint::start_as(.., policy)`; `Host::spawn` keeps its signature and 65534 default for tests. Store admission/rollback spawns (lib.rs:341,444) use the package's policy. `pre_exec` adds `setrlimit(RLIMIT_CORE, {0,0})` (survives exec; async-signal-safe).
- Non-dumpable: `couch_plugin::serve` calls `prctl(PR_SET_DUMPABLE,0)` first thing (Linux). The host, when root, verifies after Hello that `/proc/<pid>/environ` is owned by uid 0 (the kernel's marker of a non-dumpable task); a manifest that declares `pairing` and is still dumpable is refused Incompatible. Published v1/v2 packages stay dumpable (acceptable: no credential, cross-uid access is refused anyway).
Enforceable after PR 1: no ptrace, /proc/<pid>/{mem,environ,maps,fd}, process_vm_readv or signals between different packages; no core files; no_new_privs (already); credential files unreadable. NOT enforceable: hiding that other processes exist; any network restriction; two connections of the SAME package share a uid. Follow-ups after T7: hidepid=2, a small seccomp deny-list, per-uid firewall rules.
Off-device proof (`clients/couch-plugin/tests/isolation.rs`, Linux, `#[ignore]` unless euid 0; CI runs it with `sudo -E`): spawn probe children A (60001) and B (60002); from A assert open(/proc/B/mem) and /proc/B/environ => EACCES, ptrace(PTRACE_ATTACH,B) => EPERM, kill(B,0) => EPERM, a root 0600 file => EACCES, getrlimit(CORE)==0, NoNewPrivs: 1, groups as expected; control: two children with ONE uid and no dumpable call CAN read each other, and cannot once `serve` has run. The probe is a test-only bin in couch-echo.
PR 1 hardware plan (dev build => one evidence renewal first): (a) `ps` shows three different uids in 60000+, groups include 3003 (`/proc/<pid>/status`); (b) Denon status/volume path, Sonos and Kodi status each work incl. an idle reap and respawn; (c) install, update, rollback, remove one package from the web page; (d) cross-uid read of `/proc/<pid>/environ` fails; (e) record RSS per child; (f) roll the core back to .188: children return to 65534, all three packages still work, `uids.json` ignored; roll forward: same uids as before; (g) check a package uid can read `/proc/net/arp` (webOS wake MAC). Risks: a package that assumed uid 65534; setgroups order; the store's exclusive lock at first boot racing the first key press (mitigated by the startup pass).

## 5. Model + rollback
T3 adds NOTHING to config.json. `Provider::Plugin` is not given pairing/keep_alive: the daemon reads the live manifest, the web reads `GET /api/integrations` (the catalog is the serialised Manifest; new fields skipped when absent). No new `v2_projection` rule or crossload state; add one test: "a paired v3 connection's config is the same bytes as an unpaired one". .188 never opens plugin-credential.json, plugin-pairing.json or uids.json.

## 6. Daemon API + web
Routes in `plugin_route` (api/plugins.rs:96), under the connection's read gate:
- `POST …/plugin/pair {settings}` => 200 `{session, step, expires_in}`; 400 refused settings; 409 too many sessions; 400 "does not pair".
- `POST …/plugin/pair/<session> {input?}` => `{step}`; 404 unknown/expired; 400 malformed code.
- `DELETE …/plugin/pair/<session>` => `{cancelled:true}` (idempotent). `DELETE …/plugin/credential` => `{paired:false}`.
`Done` is returned as `{"step":"done","summary":..,"settings":<redacted view>}`; the credential is stripped in Runtime before api/ sees it. plugin.sock untouched (execute's allow-list keeps Pair* off the panel socket).
Web (`web/couch-web/src/screens/plugin_pairing.rs` NEW, called from `plugin_setup`, connections.rs:407): modal `role="dialog" aria-modal`, focus trapped and restored, Esc = cancel; Couch headline per prompt + the package's line; `aria-live="polite"` status; countdown from `expires_in`; PressButton/Approve poll at `poll_after_ms`; EnterCode = one input with `maxlength=length`, `inputmode=numeric` for Digits, uppercase for Hex, `autocomplete="one-time-code"`, submit disabled until complete. Failed maps PairFailure to Couch sentences + the package's message + "Try again" (new PairStart). 503/Busy polls retried silently; 404 => "Pairing was interrupted. Start again." Full-screen sheet under 480 px, 44 px targets. "Needs pairing": `paired:false` with `pairing.required`, or any plugin call answering 409 / `code=="unpaired"` shows a banner with "Pair" / "Pair again"; "Forget pairing" in the settings card behind the confirm text above. Browser tests `web/tests/plugin-pairing.mjs` (static bundle, intercepted API): three prompts, slow approve, wrong code, expiry, 404 mid-poll, 409 banner, keyboard-only pass, 360 px screenshot. End-to-end leg against real echo in `tools/tests/integrations-e2e.py` behind the preview feature.

## 7. GUI
When a failure's code is Unpaired the toast/row adds "Open Couch in a browser to pair" under the existing sentence (`activity_buttons.rs::plugin_failure`); Unpaired keeps NOT falling through to IR/Bluetooth. One screenshot test.

## 8. SDK
```rust
pub trait PairFlow: Send { fn step(&mut self, input: Option<PairInput>) -> Result<PairStep>; fn cancel(&mut self) {} }
// on DeviceClient, all defaulted:
fn connect_with(settings: &Self::Settings, _credential: Option<&Credential>) -> Result<Self> { Self::connect(settings) }
fn pair_start(_settings: &Self::Settings, _existing: Option<&Credential>) -> Result<Box<dyn PairFlow>> { Err(Error::Unsupported) }
fn take_credential(&mut self) -> Option<Credential> { None }   // rotation; polled by serve after each successful request
```
`serve`: keeps `credential` beside `settings`; `C::connect_with(..)`; owns `Option<(String, Box<dyn PairFlow>)>` with session ids `p<n>`; PairStart needs hello but no prior Configure; wrong session => Invalid; after Done/Failed/cancel the flow is dropped. Blocking device waits live on the flow's own thread; `step` must return well inside 12 s. `testing_v3.rs` gains `pairing(`: refused, timed out, cancelled, oversized credential, credential never echoed in any later response, step bounds. Echo fixture (`couch-echo/src/v3.rs`, fake device verbs `PAIR button|approve|code`): all three prompts, an approve that takes N polls, a wrong code, expiry, a Done that normalises settings, a rotation issued on the next Status.

## 9. PR split
| PR | Content | Switch | Frozen paths | Tests | Size |
| --- | --- | --- | --- | --- | --- |
| 1 | Per-package uid, RLIMIT_CORE, self non-dumpable + host check, docs (`integration-architecture.md` rewritten from "share a uid") | NOT behind the switch | couch-plugin/src/{host,server}.rs, couch-integrations/src/lib.rs, confd plugins.rs | isolation.rs (root), store table unit tests (corrupt, exhausted, never recycled), old-package-wire.sh, admission e2e | M |
| 2 | Wire + SDK + echo + testing_v3.rs | switch | couch-plugin/*, couch-sdk/* | goldens/mirrors, gate tests per version, echo tests/protocol3.rs | M-L |
| 3 | Daemon sessions, storage, routes, keep_alive, converter hook | switch | confd plugins.rs, api/{plugins,connections,integration_migrations}.rs, model integration_migration.rs | scripted-child tests, delete/forget/restart/expiry/rotation | L |
| 4 | Web dialog + banner | not frozen | none | plugin-pairing.mjs, wasm unit tests | M |
| 5 | GUI notice | not frozen | none | toast screenshot | S |
Order 1, 2, 3, then 4 ∥ 5. T2 is editing the same `Request`/`admit` lines: land T3's PR 2 after T2's wire PR (PR-B). Other risks: frame budget (MAX_FRAME 64 KiB must hold settings + 16 KiB credential; surface as Invalid at save time); two children per connection during re-pairing on devices that allow one client.
