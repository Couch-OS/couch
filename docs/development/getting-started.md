Title: Getting started
Description: Build and test a package integration from the Echo example.
Order: 2

# Getting started

This tutorial starts from Couch's Echo TV example and ends with a host-tested
integration binary. It covers the developer preview in the current source tree;
it is not a promise that integration packages are available in a published
Couch release.

## Prerequisites

Install a recent stable Rust toolchain. The current repository is validated
with Rust 1.98.1. Add the remote's build target when you are ready to package:

```sh
rustup target add armv7-unknown-linux-musleabihf
```

Clone the Couch source and run the example's tests from the repository root:

```sh
cargo test --manifest-path clients/Cargo.toml \
  -p couch-sdk --features testing
cargo test --manifest-path clients/Cargo.toml \
  -p couch-plugin -p couch-echo
```

These tests use local fake peers. They need no remote, television, or
credentials.

## 1. Copy the example

Copy `clients/couch-echo` to `clients/couch-YOUR_ID`, add the crate to
`clients/Cargo.toml`, and rename:

- the crate and binary;
- `EchoTv` and its settings type;
- `DeviceClient::KIND` and `DeviceClient::LABEL`;
- `plugin.json` fields `id`, `label`, `version`, and `executable`.

Package IDs use lowercase ASCII letters, digits, `-`, or `_`, with at most 64
characters. The executable is a normal relative path inside the package.

## 2. Define settings and capabilities

Settings should contain only what opens one device connection. Validate them
before any network I/O. Keep credentials out of logs and device configuration.

```rust
impl DeviceClient for ExampleReceiver {
    type Settings = Settings;

    const KIND: &'static str = "example-receiver";
    const LABEL: &'static str = "Example receiver";

    fn capabilities() -> &'static [Capability] {
        &[
            ("power-on", "Power on"),
            ("power-off", "Power off"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
        ]
    }

    // connect, execute, status, inputs…
}
```

Every capability ID must be part of Couch's shared command vocabulary. Dynamic
input commands are not listed: set `supports_inputs` in the manifest and
validate each identifier in `supports_input`.

## 3. Add the adapter

The adapter embeds the manifest and lets `couch-plugin` serve your
`DeviceClient`:

```rust
fn main() {
    let manifest = serde_json::from_str(include_str!("../../plugin.json"))
        .expect("embedded integration manifest");
    if couch_plugin::serve::<example_receiver::ExampleReceiver>(manifest).is_err() {
        std::process::exit(1);
    }
}
```

Do not print diagnostics to standard output. It is the protocol transport. Use
standard error for development diagnostics and do not include secrets.

## 4. Test the device boundary

Start with the Echo package tests and replace its fake line protocol with the
requests and replies your device uses. Cover at least:

- a successful command, status read, and input list;
- rejected and malformed replies;
- an unavailable device and an absolute timeout;
- refusal of an undeclared command before device I/O;
- matching manifest and Rust capability declarations;
- no retry after an ambiguous failure.

Run the whole package-facing suite:

```sh
cargo test --manifest-path clients/Cargo.toml \
  -p couch-plugin -p couch-echo -p couch-YOUR_ID
```

## 5. Build for the remote

```sh
(cd clients && cargo build --release \
  --target armv7-unknown-linux-musleabihf \
  -p couch-YOUR_ID --bin couch-plugin-YOUR_ID)
```

A host binary will not run on the remote. Pure Rust clients use the repository's
`clients/.cargo/config.toml` target configuration, which is why this command
runs Cargo from `clients/`. Dependencies containing C or assembly, such as some
TLS stacks, need the repository's Zig-based cross-compiler setup.

Continue with [packaging and signing](packaging.md), then use the
[testing and compatibility checklist](testing.md) before sharing a package.

## Source references

- [`clients/couch-echo/src/lib.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-echo/src/lib.rs)
- [`clients/couch-echo/src/bin/couch-plugin-echo.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-echo/src/bin/couch-plugin-echo.rs)
- [`clients/couch-echo/tests/plugin.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-echo/tests/plugin.rs)
- [`docs/client-sdk.md`](https://github.com/dangerouslaser/couch/blob/main/docs/client-sdk.md)
