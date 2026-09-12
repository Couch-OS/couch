# Code ownership and refactoring boundaries

Keep the independent Rust workspaces and lockfiles. They serve different build
and deployment targets; a single workspace is not a prerequisite for sharing
libraries. Share policy and protocol types through path dependencies, as the
GUI and daemon already do with `model`, `clients` and `couch-system`.

## Current system boundary

The [system service](system-service.md) owns runtime networking and SSH, while
`stage2` retains board initialization and service startup. The native GUI owns
presentation. `couch-confd` owns configuration, browser APIs and shared media
connections. Keep recovery access independent of the application processes.

## Further useful refactors

- `ui/couch-gui/src/tv.rs` combines transport commands, power/wake behavior,
  background work, presentation caching and Slint callbacks. Extract transport
  work and presentation/cache logic into separate modules with explicit
  connection/generation ownership. Preserve current cache invalidation and
  cancellation behavior; moving functions alone does not improve that boundary.
- `daemon/couch-confd/src/api.rs` still contains common request handling and
  configuration CRUD despite integration routes already living in `api/`.
  Extract configuration routes while retaining one authentication/body-limit
  boundary and existing validation/persistence errors.
- `tools/installer/` contains the simulation engine, private hardware workflow,
  packaging and terminal presentation. Migrate host orchestration to Rust after
  the current restore/fresh-install acceptance checkpoint, retaining the Python
  MediaTek adapter. Keep the remote writer independent and carry receipt,
  device-binding, write-order and cancellation regressions into the new host.
- `gui/`, `spike/` and Android-era deployment tools coexist with the Slint UI
  and release tools. Establish which are supported diagnostics versus historical
  prototypes before archiving anything. Do not delete recovery tools simply
  because the main application no longer calls them.
- `ui/couch-gui/src/main.rs`, `lights.rs` and `activity.rs` remain large
  composition/controller modules. Prefer extracting independently testable
  state or ownership boundaries as features change, rather than applying a
  repository-wide mechanical split during installer validation.

Generated icon/font tables are large by design; moving their generated rows
into more files would not improve architecture. Browser configuration (`web/`),
public documentation (`site/`), interactive previews (`preview/`) and the small
recovery portal have different runtime requirements and should not be merged
solely because they contain HTML or UI code.
