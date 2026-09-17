Title: Testing and compatibility
Description: Prove package behavior without hardware, then record real-device limits.
Order: 6

# Testing and compatibility

Tests can prove your settings, command gate, wire parser, timeouts, subprocess
contract, and package lifecycle without a remote. They cannot prove that a real
device behaves like the protocol documentation. Keep those claims separate.

## Host test ladder

Run the smallest relevant suites while developing:

```sh
cargo test --manifest-path clients/Cargo.toml \
  -p couch-sdk --features testing
cargo test --manifest-path clients/Cargo.toml \
  -p couch-plugin -p couch-YOUR_ID
```

The shared host tests cover framing, manifest validation, handshake mismatch,
malformed and oversized replies, absolute deadlines, process cleanup, bounded
queues, expired requests, and restart after a failed child.

Your package tests should use a fake implementation of the device protocol and
exercise real subprocess boundaries. Copy the structure from
`clients/couch-echo/tests/plugin.rs`.

Before review, run the same catalog and exact-case checks that CI uses for
curated packages:

```sh
python3 tools/integrations/validate_catalog.py
python3 tools/integrations/validate_catalog.py --run-tests
cargo test --locked --manifest-path clients/Cargo.toml \
  -p couch-plugin --test protocol
```

The catalog command names and runs each integration's four admission cases;
the final command exercises the shared framing and manifest host. Do not call a
missing cross compiler, an uninstalled ARM target, or unavailable device a
passing check.

## Product-flow test

Couch also has an end-to-end host test that builds the daemon and Echo plugin,
installs a fixture package into an isolated store, configures it through the
HTTP API, and controls a fake television through both HTTP and the panel socket.

```sh
cargo build --manifest-path clients/Cargo.toml \
  -p couch-echo --bin couch-plugin-echo
cargo build --manifest-path daemon/Cargo.toml -p couch-confd
python3 tools/tests/integrations-e2e.py
```

The test mocks only APK extraction on the host. Native signature and repository
checks belong in the Alpine packaging-tool tests.

## Typed-action coverage when protocol v2 ships

Protocol v2 is unreleased, so it is not an admission target for the published
v1 preview. When adding a v2 package, retain all v1 cases and add fake-device
coverage that proves: absent, reading, and `minimum` dB status remain distinct;
the declared action accepts its endpoints and one valid step; out-of-range and
off-step values are rejected before I/O; and a timed-out dB change is sent once
only. Exercise the same path through the plugin host and HTTP endpoint. Update
the catalog validator and its policy before allowing a v2 manifest into a
curated feed.

## Compatibility rules

Treat these values as one compatibility set:

- manifest `protocol_version`;
- the host protocol version;
- manifest ID, version, executable, capabilities, and presentation;
- manifest actions and minimum core protocol version, when a future protocol
  version uses them;
- the binary's hello manifest;
- the `DeviceClient` capability declaration.

The installed manifest and hello manifest must match. A protocol mismatch is
`incompatible`, not a best-effort downgrade. Existing connection metadata
remains readable when a package is missing, but no command can run without a
compatible active package.

## Failure behavior to verify

- Unknown commands are refused before device I/O.
- Settings validate without opening the device.
- Credentials never appear in logs, process arguments, environment, or exported
  house configuration.
- Each operation has an absolute deadline, including partial replies.
- Ambiguous command failures are never retried.
- A dead child is reaped and only a later explicit request starts another.
- Input identifiers are bounded and validated before persistence.
- Typed values are within their declared range and step before device I/O.
- Standard output contains only framed protocol messages.

## Real-device validation

After host tests pass, record the exact hardware and firmware tested. Check
power states, sleep and wake, authentication expiry, malformed device data,
network loss, input enumeration, and every declared command. Report what was
not tested.

Do not turn a simulator result into a hardware support claim. A package can be
correct up to its wire format while still misunderstanding a vendor's device.

### Validate the package host on an HA100

Use an isolated directory under `/opt/couch` for the test daemon, config,
connection settings, package store, and public trust key. Bind its HTTP API to
loopback and use a separate socket and port. Keep the production runtime slot,
configuration, and trust keys unchanged. Do not install synthetic test packages
into the user's active store.

Exercise signed sideload, a real version upgrade, rollback, signed repository
installation, untrusted-key rejection, removal, and reinstallation. Confirm
that settings survive replacement and that HTTP and the panel socket share one
plugin/device connection. Send queue spikes, malformed replies, and ambiguous
timeouts only to a controlled fake peer; verify wire command counts to detect
unintended retries.

Inspect the live plugin process credentials on the remote as well as testing
transport behavior. The HA100 kernel's Android network restrictions differ
from ordinary Linux CI containers, so a successful container test alone does
not establish device compatibility.

For screen validation, use a separate GUI home and settings file, capture the
framebuffer, and verify an input event reaches the fake integration. Restore
the production GUI with a bounded recovery timer if temporarily stopping its
supervisor. Confirm its heartbeat advances afterward and compare production
configuration hashes and runtime selections before and after the trial.
Record physical button/touch tests separately from injected evdev events.

A receiver trial limited to status and input enumeration does not certify
power, volume, input changes, reconnect behavior, or all receiver models. Keep
such an integration in preview until the full declared behavior and the exact
hardware/firmware are recorded.

## Compatibility record

For each published build, record:

| Field | Example |
| --- | --- |
| Package | `example-receiver 0.1.0` |
| Couch source or release | full commit SHA or release tag |
| Protocol | `1` |
| Target | `armv7-unknown-linux-musleabihf` |
| Host tests | command and date |
| Hardware | model, firmware, and tested behaviors |
| Known gaps | pairing, discovery, events, or device-specific limits |

An integration cannot advance by changing this prose alone. The machine-read
[catalog admission policy](admission.md) requires named conformance, failure,
timeout/no-retry, and spike tests, and requires physical-device evidence before
an entry can be marked production.

## Source references

- [`clients/couch-plugin/tests/protocol.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/tests/protocol.rs)
- [`clients/couch-echo/tests/plugin.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-echo/tests/plugin.rs)
- [`tools/tests/integrations-e2e.py`](https://github.com/Couch-OS/couch/blob/main/tools/tests/integrations-e2e.py)
