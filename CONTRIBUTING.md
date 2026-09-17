# Contributing to Couch

Couch is a Linux distribution for the Sanytron Astrion HA100 remote: an ARMv7
device with an Alpine userland, a Slint GUI on its panel, a configuration
daemon serving a browser UI, and a set of Rust crates that talk to the things
in your living room.

Most of it can be worked on without owning the hardware. This document says
what you can build and test on an ordinary machine, what needs the device, and
how to tell the two apart in a pull request.

If you want to add support for a device - a TV, a receiver, a hub - start with
**[docs/client-sdk.md](docs/client-sdk.md)**. It is the concrete path, and it
needs no hardware.

## Layout

Four independent Rust workspaces, each with **its own lockfile that must stay
its own**:

| Path | What it is |
|---|---|
| `model/` | `couch-model`: the shared configuration types. `no_std` + `alloc`; builds for the host, ARMv7 musl and wasm32 |
| `clients/` | the device integrations, the control broker, and `couch-sdk` |
| `daemon/` | `couch-confd`: the REST API and the web UI it serves |
| `ui/` | `couch-gui`: the Slint GUI that runs on the remote's panel |
| `web/` | `couch-web`: the browser configuration UI (wasm) |
| `couch-installer/` | git submodule of [Couch-OS/couch-installer](https://github.com/Couch-OS/couch-installer): the desktop installer and its RAM installation service, with their own workspaces and lockfiles. Change it there, then bump the pin |

Outside Rust: `initramfs/init`, `recovery/init` and `stage2/` are boot, rescue
and runtime setup; `kernel/` is the kernel build; `src/` and `gui/` are C
framebuffer and LVGL code; `tools/` holds build, deployment and diagnostic
scripts; `docs/` explains each subsystem.

There is no repository-wide workspace and no repository-wide lint
configuration. Run commands inside the workspace you changed.

## Prerequisites

- Rust stable with `cargo` (validated against 1.98.1). No nightly features are
  used and there is no toolchain file.
- `rustup target add armv7-unknown-linux-musleabihf` to build anything for the
  remote. Pure-Rust crates need nothing further; anything pulling in TLS builds
  `ring`, which needs an ARM musl C compiler supplied through
  `CC_armv7_unknown_linux_musleabihf` - see the cross-compilation notes in
  [docs/client-sdk.md](docs/client-sdk.md).
- `rustup target add wasm32-unknown-unknown` plus [Trunk](https://trunkrs.dev)
  only if you touch `web/`.
- The `couch-installer` submodule for `tools/release` tests and image assembly.
  Clone with `git clone --recurse-submodules`, or run
  `git submodule update --init couch-installer` after cloning or pulling. Every
  git worktree needs its own init. See
  [bumping the installer pin](docs/development-flow.md#bumping-the-installer-pin).
- Nothing else. Building and testing needs no device, no credential and no
  network beyond crates.io and the submodule fetch.

## Building and testing

Run `cargo test` inside each workspace you changed, and `cargo fmt --check`
there too:

```sh
(cd model   && cargo fmt --check && cargo test)
(cd clients && cargo fmt --check && cargo test)
(cd daemon  && cargo fmt --check && cargo test)
(cd ui      && cargo fmt --check && cargo test)
```

A change to `model/` is usually a change to `clients/`, `daemon/`, `ui/` and
`web/` as well, because all five read those types. Test the ones you touched.

One caveat on formatting: `cargo fmt --check` is **not** currently clean on
committed code. Every workspace reports drift - `model/`, `clients/`,
`daemon/`, `ui/` and `web/` - most of it in files nobody has touched in a
while. Check the crate you changed instead:

```sh
cargo fmt -p <your-crate> -- --check
```

Reformatting someone else's crate inside an unrelated pull request buries your
change in noise. Fix drift deliberately, in its own commit, if you want to fix
it.

The browser UI and the host daemon:

```sh
tools/build-webui.sh --host    # build the wasm bundle, then the host daemon
tools/run-webui.sh             # serve it at http://127.0.0.1:8090
tools/build-gui.sh --host      # the device GUI, for this machine
```

Building for the device, and the kernel, are documented in `README.md` and
`kernel/README.md`. Both need more than a checkout.

## Style

- Four-space indentation in Rust and Python; match the surrounding style in C,
  shell and Slint.
- `snake_case` functions and modules, `PascalCase` types.
- Keep shell scripts compatible with their declared interpreter.
- Explain hardware assumptions next to ABI constants and device operations. The
  comments in `clients/couch-ir/Cargo.toml` and `clients/couch-voice/src/alsa.rs`
  are the house style: say why the obvious dependency was not used.
- Tests are Rust's built-in framework, inline `#[cfg(test)]` modules, and
  descriptive `snake_case` names that state the behaviour being protected.
  There is no coverage threshold. Add a regression test for changed logic.

## Safety rules that are not negotiable

- **Never write the `preloader_*` or `lk` partitions.** Read the partition
  layout and the recovery procedure in `README.md` before flashing anything.
- Keep credentials, partition backups and per-device calibration out of Git.
- Credentials belong beside `config.json` at mode 0600, never inside it, and
  never in an exported house configuration or an error message.
- Nothing retries a device command automatically. A lost reply does not prove
  a lost command.

## Pull requests

- Use focused commits with descriptive, imperative subjects, optionally
  prefixed by subsystem: `docs:`, `installer:`, `couch-ir:`.
- Explain the behaviour change, not just the diff.
- **List the validation commands you ran and their results.** If you ran
  something on a device, say so separately and say which device - host test
  results and hardware results are not interchangeable.
- Include screenshots for UI changes.
- Update the subsystem document in `docs/` that your change affects.
- Say plainly what you did not verify. An unverified assumption named in the
  description is useful; one discovered later is not.

### Completion and merge ownership

If you open or take ownership of a change, carry it through the ordinary
repository workflow: make the focused commit, open or update its pull request,
run the relevant validation, address review and CI feedback, and merge it once
the repository's normal requirements are satisfied. Do not leave a completed,
green change only on a branch for somebody else to discover.

If a required review, CI check, permission, or external dependency prevents
that final step, report the exact blocker and the change's current PR/commit.
Do not bypass branch protection, required checks, or review requirements to
call work complete.

## Hardware validation

Host tests prove protocol handling, configuration validation and error paths.
They cannot prove that a real television accepts a command, that the panel
renders a new screen, or that anything fits the device's memory and flash.

Changes to framebuffer, boot, audio and IR paths need device validation,
recorded separately from the host test results. Several subsystem documents in
`docs/` are written exactly that way and are worth reading as examples.

If you do not have the hardware, that is fine: say so in the pull request and
list what a reviewer with a device would need to check. Reviewing is where that
gap gets closed, and hiding it is the only thing that makes it a problem.

## Licensing

The repository ships `COPYING` (GNU GPL v3), and the Rust workspaces declare
`license = "GPL-3.0-or-later"` in their package metadata; new crates inherit it
with `license.workspace = true`. `docs/release-licenses.md` records how
component licenses are kept distinct in a release bundle - kernel, userland
packages, fonts and artwork, and vendor firmware each keep their own terms.

Check what a new dependency links and what it is licensed under before adding
it, and note both in the pull request.
