Title: Catalog admission
Description: The tests and hardware evidence required before an integration can advance.
Order: 7

# Catalog admission

The catalog in `integrations/catalog.json` is the review boundary for external
integrations in the Couch monorepo. It records what is being tested and what has
actually been validated. It is a developer-preview gate for this repository,
not an announcement of a public package repository or enabled branch protection.

## Admission tiers

| Tier | Meaning | Hardware rule |
| --- | --- | --- |
| `test-only` | A synthetic protocol fixture used to prove the SDK and host. It is not offered as device support. | Must be marked synthetic and `not-applicable`. |
| `preview` | A real-device integration still under compatibility review. | May be `not-tested` when its limitations say so. |
| `production` | Eligible for a production catalog once distribution exists. | Must have validated hardware evidence. |

Echo TV is synthetic and test-only. Denon is a preview: its fake-receiver tests
exercise the protocol implementation, but no physical receiver evidence is
recorded yet. Neither status should be read as a published package.

## Required tests

Every entry names four distinct Rust tests. The admission validator confirms
that each exact `#[test]` exists and CI executes it:

- `conformance`: handshake, configuration, declared commands, status, and
  inputs across the real plugin subprocess boundary;
- `failure`: invalid settings, refused or malformed device replies, and the
  capability gate before device I/O;
- `timeout_no_retry`: an ambiguous timeout sends one command only and a later
  explicit request may recover;
- `spike`: burst or queue pressure remains bounded, expires stale work, and
  does not leak commands to the device.

Shared `couch-plugin` protocol tests still cover framing, process containment,
deadlines, and manifest validation. The per-integration cases prove that the
package adapter and its device transport preserve those properties.

## Hardware evidence

Moving an entry to `production` requires at least one evidence record with:

- exact device model and firmware;
- the manifest version and full source commit that were tested;
- an ISO validation date;
- the behaviors exercised on that device;
- an HTTPS report or repository-relative evidence file.

Simulator and fake-peer results stay in test results. They are not hardware
evidence. A production label without evidence fails catalog validation.

## Run the gate

From the repository root:

```sh
python3 tools/integrations/validate_catalog.py
python3 tools/integrations/validate_catalog.py --run-tests
```

The first command checks catalog structure, manifest and Cargo identity, binary
source, named test cases, tier rules, and hardware claims. The second runs the
four exact cases for every catalog entry. CI also runs the shared host protocol
suite.

For integration-related changes, the monorepo workflow has three required
layers:

1. catalog policy, shared host protocol tests, and every exact package case;
2. daemon lifecycle tests plus the HTTP/panel/fake-device product flow;
3. ARMv7 cross-build, ephemeral APK signing and indexing, native sideload and
   repository admission under QEMU, and rejection of untrusted and tampered
   packages without changed state.

The workflow's final admission job always appears on pull requests. The
expensive layers are skipped for unrelated changes and required when the
integration, daemon, model, catalog, or packaging paths change. This describes
the workflow behavior only; branch protection is not claimed to be configured.

## Review checklist

An integration change should include its manifest, adapter, fake device tests,
catalog entry, limitations, and documentation in one review. Use the
integration pull-request template in `.github/pull_request_template/` to record
the commands and any physical-device evidence.

## Source references

- [`integrations/catalog.json`](https://github.com/dangerouslaser/couch/blob/main/integrations/catalog.json)
- [`tools/integrations/validate_catalog.py`](https://github.com/dangerouslaser/couch/blob/main/tools/integrations/validate_catalog.py)
- [`clients/couch-plugin/tests/protocol.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-plugin/tests/protocol.rs)
