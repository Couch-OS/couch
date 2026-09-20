# Protocol 3, step T2 "Children and domain controls": implementation plan (approved 2026-09-19)

Studied at origin/dev 1956198 (T1 PR1 #237 and PR2 #240 merged; T1 PR3 in progress — land it before T2 PR-A/B because of literal churn).

## Owner decisions (Bryan, 2026-09-19)
- Web picker: pre-filter to the bridge room whose name matches the Couch room, AND an "Add all shown" button with a confirmation that lists what will be added. Nothing is ever added without that press.
- A child that vanishes from the listing: keep the device, its bindings and scenes; panel row shows "Unavailable"; web shows a "No longer reported by <connection>" badge with a manual Remove. Never auto-delete.
- Rollback to .188: accepted that package lights stay as inert rows, package scenes disappear until re-update, and if the old version saves, bindings to those lights are lost (same rule as T1's `x:` ids). Devices heal by themselves after re-update.
- Blinds get a normal room row now; thermostat children keep the generic package screen until Home Assistant is packaged (wave 3).
- Wording for a too-new package: "This integration needs a newer Couch".
- No packaged Hue reaches a real user before T5 (5 s refresh in T2 vs 0.5 s built-in).

## Summary
One connection (Hue bridge, Home Assistant, Protect) can offer many "children". The manifest declares child KINDS (light, room group, scene, blind, thermostat) and what each can do. Couch lists children in pages (id, kind, name, room hint). Every command, typed action and status read can name a child (`resource`). Three new typed actions: SetLight, SetCover, SetClimate (integers only). Status may carry light/cover/climate state; a write may answer with the acknowledged state. `dim:30`, `position:40`, `mode:heat` on a child are mapped to typed actions by the ONE host gate. A chosen child is saved as an ordinary room device with a small snapshot (kind + traits) so bindings validate offline and the panel draws a normal light row with the existing slider logic. Package scenes attach to the room's Scenes button. Everything lives in `integration_config_v3`; the v2 view strips it so .188 loads. Nothing is reachable with the switch off. One visible change: a too-new package says "needs a newer Couch" instead of "invalid".

## 0. Findings
- Stored devices use `Integration::Connection { connection_id, resource_id }`; `Integration::Plugin` is mostly the RESOLVED form (connection.rs:170-233) though config-crossload.rs:143 stores one directly. `Function::supports(&Integration)` only sees the resolved value, so the child kind must be saved on `Integration::Connection` and carried through `resolve_integration`.
- .188 accepts a non-empty plugin `resource_id` (validate.rs:259: <=128 bytes of [A-Za-z0-9._/+-], empty allowed). Crossload state C already saves `zone1`.
- .188 ignores unknown FIELDS on `Scene`, `Device`, `Integration`, `Provider` (no deny_unknown_fields); unknown TAGS of `PluginComponent`, `PluginActionSchema`/`TypedAction` are fatal.
- .188 refuses some bindings to a child: a binding must pass `supports_device` (so `dim:30` needs a literal capability), `ShortcutAction::Toggle` needs `can_toggle` (false for plugins). Steps only need to parse; `dim:`, `position:`, `mode:` parse at .188. So T2 needs no new command grammar; `v2_command` untouched.
- `tools/tests/config-crossload.rs` builds `Provider::Plugin`, `Integration::Connection`, `Integration::Plugin` with struct literals (lines 113, 136, 143) and compiles against BOTH models: adding a field breaks it. Rebuild those from `serde_json::json!` first and prove rows A–F and Cd stay byte-identical.
- An OLD child silently accepts a `Status` frame with `resource` (unit variant ignores extra fields even with deny_unknown_fields; noted at volume.rs:14) and would answer connection status. Only the gate protects here — mirror test must assert it.
- `plugins.rs::execute` holds the per-connection lock with `try_lock` for the whole request (same-connection requests collide as Busy) and reloads the settings file every call.
- The HTTP router has no query-string support.
- `daemon/` and `ui/` must never enable `protocol-3-preview` (guard tests). End-to-end tests against a protocol 3 child live only in `clients/`.
- "Import this room" and preselecting the room filter do NOT exist in the web picker today; they are new.

## 1. Wire (clients/couch-plugin/src/protocol.rs; clients/couch-sdk/src/children.rs new)
```rust
Command { function: String, #[serde(default, skip_serializing_if="KeyPhase::is_tap")] phase: KeyPhase,
          #[serde(default, skip_serializing_if="Option::is_none")] resource: Option<String> },
Action  { action: TypedAction, #[serde(default, skip_serializing_if="Option::is_none")] resource: Option<String> },
Status  { #[serde(default, skip_serializing_if="Option::is_none")] resource: Option<String> }, // was a unit variant
Children{ #[serde(default, skip_serializing_if="Option::is_none")] cursor: Option<String> },
// Response
Children{ children: Vec<Child>, #[serde(default, skip_serializing_if="Option::is_none")] next: Option<String> },
#[serde(deny_unknown_fields)] pub struct Child { pub id: String, pub kind: String, pub name: String,
  room_hint: Option<String>, light: Option<LightTraits>, cover: Option<CoverTraits>, climate: Option<ClimateTraits> } // Options: default + skip-none
```
- Builders: `Request::status()`, `Request::action(a)`, `Request::children(cursor)`, `fn at(self, resource) -> Self`. `Request::Status` is used in 26 places, `Request::Action {` in 14; `testing.rs` uses neither and stays untouched.
- Resource grammar: one `valid_resource(&str)` in couch-model: 1..=128 bytes of [A-Za-z0-9._/+-], segments split on `/`, none empty, `.` or `..`. Hue: bare light UUID, `room/<uuid>`, `scene/<uuid>`. Cursors: same alphabet, <=128 bytes.
- Paging: worst-case Child ~1.2 KB; MAX_PAGE = 32; SDK helper `ChildPage::fill(iter, cursor)` also stops at 48 KiB serialized. (Bryan's bridge: 243 resources = 8 pages, ~25 KB.)
- `requires()`: Children -> 3; any `resource: Some` -> 3; Action with SetLight/SetCover/SetClimate -> 3; SetVolumeDb stays 2.
- A <3 child is never sent `children`, any `resource`, or the new actions. `accept` retires a <3 child that answers `Response::Children`, a Status with light/cover/climate, or Status as reply to a write.
- Status gains `light: Option<LightState>`, `cover: Option<CoverState>`, `climate: Option<ClimateState>` (skip-none; `serve` strips them for a <3 manifest as it strips volume_db for protocol 1).
- Write ack: for a request with `resource`, `validate_response` also accepts `Response::Status { status }` to Command and Action. `Ok` stays legal (caller then reads). (Decided: Status, not a `Response::Light` variant.)
- `accept` for a v3 child: brightness/position <= 100, mirek 100..=1000, xy <= 10000, tenths within -500..=1500; every Child: kind declared, id/cursor grammar, names/hints pass the 128-byte label rule, traits match the component, page <= 32, ids unique, non-empty when `next` is set.

## 2. Domain types (model/couch-model/src/domain.rs new, re-exported) and volume.rs
```rust
pub struct LightTraits { pub dimmable: bool, #[serde(default)] pub mirek: Option<(u16,u16)>, #[serde(default)] pub color: bool } // Copy, Eq
pub struct LightState  { pub on: Option<bool>, brightness: Option<u8>, mirek: Option<u16>, xy: Option<(u16,u16)> }     // None = unknown
pub struct CoverTraits { pub position: bool, pub stop: bool }
pub struct CoverState  { pub open: Option<bool>, pub position: Option<u8> }   // 0 closed, 100 open, as couch-ha
pub enum ClimateMode { Off, Heat, Cool, HeatCool, Auto, Dry, FanOnly }        // == commands::HVAC_MODES
pub enum TempUnit { Celsius, Fahrenheit }
pub struct ClimateTraits { min_tenths: i16, max_tenths: i16, step_tenths: u16, unit: TempUnit, modes: Vec<ClimateMode>, range: bool }
pub struct ClimateState { available: bool, mode: Option<ClimateMode>, heating: Option<bool>, current_tenths: Option<i16>,
                          target_tenths: Option<i16>, low_tenths: Option<i16>, high_tenths: Option<i16> }
// volume.rs: TypedAction stays Copy + Eq + deny_unknown_fields, integers only
SetLight   { on: Option<bool>, brightness: Option<u8>, mirek: Option<u16>, xy: Option<(u16,u16)> },  // at least one field; brightness 0 = off
SetCover   { position: u8 },
SetClimate { target_tenths: Option<i16>, low_tenths: Option<i16>, high_tenths: Option<i16>, mode: Option<ClimateMode> },
// PluginActionSchema gains SetLight {}, SetCover {}, SetClimate {}  (EMPTY STRUCT variants, never unit, so unknown fields are still refused)
```
`ActionKind` gains the three kinds; `accepts` checks global bounds, `low < high`, at least one field set. Low/high are included because built-in HA's thermostat uses a range (decided: include now). Connection-level `PluginComponent::{Light, Cover, Climate} { label }` for a package whose connection is itself one lamp: decided ADD NOW (one projection line + one crossload state).

## 3. Manifest (clients/couch-plugin/src/manifest.rs)
```rust
#[serde(default, skip_serializing_if="Vec::is_empty")] pub children: Vec<ChildKind>,
#[serde(deny_unknown_fields)] pub struct ChildKind { pub kind: String, pub label: String, pub device_kind: DeviceKind,
  pub component: ChildComponent /* light|scene|cover|climate */, #[serde(default)] pub capabilities: Vec<Capability>,
  #[serde(default, skip_serializing_if="Vec::is_empty")] pub actions: Vec<PluginActionSchema> }
```
validate: `children` only in a protocol 3 manifest (else Invalid, as `x:`); <= 8 kinds, unique identifier ids, label rule; <= 32 capabilities per kind, each parses and is not `input:`/`app:`; `x:` ids count towards the manifest-wide 32; `valid_set(actions)` per kind. Component table: light -> device_kind light|switch, action SetLight; cover -> blind, SetCover; climate -> thermostat, SetClimate; scene -> other, capabilities exactly `on`, no action. Capabilities carry {id,label}.

## 4. Model and rollback (PR-A)
```rust
// connection.rs — Provider::Plugin gains:
#[serde(default, skip_serializing_if="Vec::is_empty")] children: Vec<PluginChildKind>,   // mirror of ChildKind
// device.rs — BOTH Integration::Connection and Integration::Plugin gain:
#[serde(default, skip_serializing_if="Option::is_none")] child: Option<ChildSnapshot>,
pub struct ChildSnapshot { pub kind: String, light: Option<LightTraits>, cover: Option<CoverTraits>, climate: Option<ClimateTraits> }
// lib.rs — Scene gains (Scene.hue stays as it is; NOT renamed/aliased in T2):
#[serde(default, skip_serializing_if="Option::is_none")] pub resource: Option<SceneResource>,
pub struct SceneResource { pub connection_id: Id, pub resource_id: String, pub kind: String }
```
- `resolve_integration`: with `child: Some`, the resolved Plugin takes capabilities and actions from that kind, empty presentation, same `child`; unknown kind -> empty capabilities.
- `commands.rs::supports` Plugin arm: `Dim(_)` needs a light kind with SetLight and `dimmable`; `Position(_)` a cover kind with `position`; `Mode(m)` a climate kind with `m` in modes. `shortcuts.rs::can_toggle`: true for a light/cover child declaring `toggle`.
- validate.rs: kind declared; `device.kind == device_kind`; strict resource grammar ONLY when `child` is set (old `zone1` files keep loading); a device's child may not be a scene; `Scene.resource` excludes `hue` and steps and its kind must be a scene; snapshot limits mirror the manifest's. No new DeviceKind.
- `v2_projection`, exactly: (1) `Provider::Plugin.children.clear()`; (2) every device `child = None` on both variants, collecting the ids that had one; `connection_id` and `resource_id` are KEPT (inert on .188; re-upgrade then heals without re-adding lights); (3) for collected ids, UNCONDITIONALLY: bindings -> `action = None` (kept as disabled binding), steps / setup on/off Command steps / page widgets removed, `setup.forget_device(id)`, `ShortcutAction::Toggle { device }` dropped, `ShortcutAction::Device` stays (a surviving binding would be sent without a resource to the whole connection); (4) scenes with `resource.is_some()`: `remove_scene(id)` cascading to `areas[].scenes`; (5) `v2_action_schemas` unchanged; (6) `v2_component` returns false for Light, Cover, Climate; (7) `v2_command` unchanged. It is the identity on anything .188 can hold.
- config-crossload.rs: rebuild the three struct literals from json; new states H (light, group, scene children with `dim:30` and `toggle` in all six saved places + a Toggle shortcut), I (H on a package that also has connection-level capabilities), J (cover and climate children with `position:`/`mode:` bindings), K (a `Scene.resource` referenced by an area), L (H on a Denon-converted connection), M (connection-level light component); extend `leaks` to fail on `"child"`, `"children"`, `"resource":{`, `set_light`, `set_cover`, `set_climate` outside the v3 layer; section G gains refused states: child on a non-plugin connection, unknown kind, scene kind used as a device, `Scene.resource` with steps, `dim:` bound to a non-dimmable child, `../x` as a child id. Unit tests extend the frozen .188 mirrors and the property tests (idempotent, identity, release save never resurrects, tampered layer fails closed).

## 5. Host gate and SDK (PR-B)
- One gate: `admit(manifest, kind: Option<&str>, request)`; `Endpoint::request_child_detailed(kind, req)`; `Pending` gains `kind`; `request_detailed` passes None. With `resource: Some`, in order: requires is 3; grammar; `kind` present (Invalid if missing) and declared; a Command's function is in the kind's capabilities, an Action's schema in the kind's actions and `accepts`. Level mapping lives in `admit` only: `dim:N` on a light kind -> `Action { SetLight { brightness: Some(N), .. }, resource }`; `position:N` -> SetCover; `mode:m` -> SetClimate { mode }. The kind is never sent on the wire (resource is a plain string; the host derives the kind from the config).
- `host::list_children(ask: &mut dyn FnMut(Request) -> Result<Response, Failure>) -> Result<Vec<Child>>` in couch-plugin: MAX_CHILDREN = 1024, <= 64 pages, seen-cursor set (repeat = Protocol), duplicate ids = Protocol, 10 s overall deadline; any violation retires the child.
- DeviceClient defaults (every existing client compiles unchanged and stays byte-identical): `child_kinds() -> &[..]` default `&[]` (serve checks it equals manifest.children); `children(cursor) -> Result<ChildPage>`; `child_command(resource, &Function, KeyPhase) -> Result<Option<Status>>`, `child_action(..)`, `child_status(..) -> Result<Status>`; all default `Err(Unsupported)`. `serve` routes on `resource` and clears the new Status fields for a <3 manifest.
- Harness: do NOT edit `testing.rs` in T2. Add `clients/couch-plugin/src/testing_v3.rs` behind `protocol-3-preview` with `children(adapter, case)`: paging terminates, ids stable across two listings, unknown resource refused, write ack matches a following read, nothing answered to a resource of another kind. It moves into testing.rs at T7.
- Echo (`couch-echo/src/v3.rs`): fake bridge with 70 lights, 3 groups, 5 scenes (forces 3 pages) + one cover + one climate; in-memory state, ack equals state; a `hostile` setting selects cyclic cursor / oversized page / undeclared kind / duplicate ids. Tests in `tests/protocol3.rs` + new wire_mirror / wire_golden rows (golden file only GAINS rows; the existing 41 must not move; the mirror asserts the gate never emits `resource`, `children` or the new actions to a 1/2 manifest, including the silent Status case). `tools/tests/old-package-wire.sh` unchanged and passing.

## 6. Daemon (PR-C)
- plugins.rs: `execute(connection, plugin, kind, request)`; allow-list gains Children for internal use. Children cache: `Mutex<HashMap<connection, Listing { generation, settings, at, children }>>`, memory only, filled on demand by `list_children`, 5 min TTL, dropped on settings save, package change, `retire`, `reap`. The PANEL never triggers a listing (it works from saved devices); only the browser and device creation do, so the 750 ms queue TTL is never in the way. While a cold listing runs other requests to that connection get Busy (documented).
- Kind lookup in `Api::plugin_request`: from the config `(connection_id, resource)` -> device's `child.kind` or `Scene.resource.kind`; else the cache (test-before-adding); unknown resource 404. `connection_id` alone selects the child process, so a resource can never reach another connection's package.
- Routes (no router change): `GET …/plugin/children`, `POST …/plugin/children/refresh`, `GET …/plugin/children/<id…>/status`, `POST …/plugin/children/<id…>/action` (body {command}), `POST …/plugin/children/<id…>/typed-action`. The id is the path remainder (segments never empty, verb always last). List reply `{kinds, children:[{…, assigned:{room,device}|null}], fetched_ms}`.
- plugin.sock: `LocalRequest` UNCHANGED; the resource rides inside `request`, the daemon derives the kind (decided: no `LocalRequest.resource_id`).
- Stamping (api.rs create/update device, create/update scene): for a connection whose provider declares children the DAEMON, never the browser, fills `child` / `SceneResource.kind` from the cache and forces `kind = device_kind`; unknown id 400; a rename with the package offline keeps the old snapshot; the same code HEALS a device whose `child` was stripped by a .188 save on the next successful listing.
- `refresh_plugin_metadata` copies `children`, keeping any kind a device still references.
- `couch-integrations::read_manifest`: parse `struct Probe { protocol_version: u32 }` first; above `accepted_protocol_version()` -> "This integration needs a newer Couch (protocol N)" before the strict parse.
- Tests: list loop and hostile cases in clients/; daemon parts (cache, routes, stamping, heal, kind lookup) with an injected lister closure; guard test stays green.

## 7. GUI (PR-D, not frozen)
- `lights.rs::configured_in`: `Some(Integration::Plugin { connection_id, resource_id, child: Some(c), .. })` with a light or cover component -> `Entry { id: "plugin:<conn>/<res>", plugin: Some(..) }`. Climate child keeps the `device:` row and the generic plugin screen. Scenes are not devices.
- State adapter: LightState + traits -> `couch_ha::Light`, CoverState -> `couch_ha::Cover`, so `brightness_pending`, `brightness_flight`, `brightness_step`, `position_targets`, `description` are reused unchanged.
- `perform`: Brightness sends `Request::action(SetLight { brightness }).at(res)` (or SetCover) and uses the ack Status as `Answer::State`; on `Ok` one `status().at(res)` follows. Toggle sends `command("toggle").at(res)`. Busy requeues the latest target silently.
- Cadence: the existing 5000 ms branch (lights.rs:992); `Operation::List` makes one Status per plugin row sequentially on the one worker (est. 5–10 ms each; packages answer from cache). Status reads use a 1.5 s timeout; after the first transport/timeout error the other rows of that connection are skipped and shown Unavailable for that round. T5 replaces this with daemon-side States/watch. No 500 ms cadence, no push, no keep_alive in T2.
- `scenes.rs`: `scene.resource` sends `command("on").at(resource_id)`. `activity_buttons.rs`, `shortcuts.rs`, `tv.rs`: thread `resource_id` into every plugin request (today only `connection_id` is used; find every `resource_id: String::new()`); `F::Dim` is sent as a plain command and the gate maps it; toggle shortcut gains a Plugin arm; a light child never opens `plugin:<device>`. Vanished child row shows "Unavailable".
- Tests: closures in place of the socket (as `plugin_worker` does) + the existing rendered pictures with a plugin row added.

## 8. Web (PR-E, not frozen)
- `device_picker.rs:64`: the `discover` branch also takes `Provider::Plugin { children, .. } if !children.is_empty()`; fetches `…/plugin/children`; kind select from `kinds[].label`; hint select from distinct `room_hint` values (generalises `hue_room`), PRESELECTED when a hint equals the Couch room's name (case-insensitive); search covers name and hint; "Add all shown" with a confirmation listing names and count.
- `assigned()` gains a Plugin arm reading the daemon's `assigned` field. Add = `POST /api/rooms/<room>/devices` `{via:"connection", connection_id, resource_id}` (daemon stamps the snapshot). Scene kind = `POST /api/scenes {name, rooms, resource:{connection_id, resource_id}}`. Nothing is ever assigned automatically. `scenes.rs` lists resource scenes. `controls()` shows a light or cover panel over the child routes. A saved device missing from the list gets a "No longer reported by <connection>" badge with Remove.

## 9. Security and robustness
One child process per connection selected only by `connection_id`; panel cache keys `<conn>/<res>` (connection ids cannot contain `/`). Limits: 8 kinds, 1024 children, 32 per page, ids/cursors 128 bytes, names/hints 128 bytes without control characters, frame 64 KiB, 10 s listing deadline. Hostile lists (10^6 children, cyclic/repeated cursors, empty non-final pages, duplicate ids, undeclared kinds) -> Protocol + child retired; the cache keeps the last good listing. The browser never supplies `kind` or traits; child names are never trusted as HTML. Nothing is auto-deleted. Until T3's per-package uid two packages share uid 65534; T2 stores no credentials.

## 10. PR split (merge commits; each green and shippable as a protocol-2 core with the switch off)
| PR | Content | Frozen paths | Size |
| --- | --- | --- | --- |
| A model | sections 2 + 4; compile fixes for the new field in ui/web/daemon (57 `Integration::Connection` sites, ~30 `Integration::Plugin`, most need only `..`) | 8 model files (+ new domain.rs to add to contract_paths at T7) | L ~1200 |
| B wire | sections 1, 3, 5; echo bridge; testing_v3.rs; compile fixes in confd and GUI | couch-plugin, couch-sdk; NOT testing.rs | L ~1500 |
| C daemon | section 6 + manifest probe | plugins.rs, api/plugins.rs, api.rs, couch-integrations | M ~700 |
| D GUI | section 7 | none | M ~600 |
| E web | section 8 | none | M ~500 |
Order: A, B, C, then D and E in parallel. Evidence already stale; one renewal later; harness digest untouched by T2.
Risks: literal churn vs T1 PR3 (land PR3 first); heal logic / "old save wins" loses bindings on a rollback that saves (accepted); Busy collisions between the panel and a browser listing; `refresh_plugin_metadata` silently failing validation when a package drops a kind; worst-case frame arithmetic.
