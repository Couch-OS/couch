Title: Catalog admission
Description: The tests and hardware evidence required before an integration can advance.
Order: 7

# Catalog admission

The catalog in `integrations/catalog.json` is the review boundary for native
integration sources retained in the Couch monorepo. An independent integration
repository carries the equivalent identity in `integration.json`, its runtime
manifest, locked source, and admission tests. The curated feed separately pins
that repository at a full commit; a branch, tag, or mutable default branch is
not an admission identity.

## Admission tiers

| Tier | Meaning | Hardware rule |
| --- | --- | --- |
| `test-only` | A synthetic protocol fixture used to prove the SDK and host. It is not offered as device support. | Must be marked synthetic and `not-applicable`. |
| `preview` | A real-device integration still under compatibility review. | May be `not-tested` when its limitations say so. |
| `production` | Eligible for a production catalog once distribution exists. | Must have validated hardware evidence. |

Echo TV is synthetic and test-only. Denon is a preview: its fake-receiver tests
exercise the protocol implementation. Read-only status and input enumeration
also passed against a physical receiver from the HA100 package host, but exact
model/firmware evidence and physical command validation are still outstanding.
Its complete hardware-validation status therefore remains `not-tested`. Sonos is
a preview whose cases run against a fake local Control API: the client behind it
has been used against real players, but no packaged build has ever run on one,
so its hardware validation is `not-tested` too. No tier here should be read as a
published package.

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

Independent repositories reuse these cases through the `testing` feature on a
full-revision Git dependency on `couch-plugin`. Their normal and development
dependencies on `couch-plugin` and `couch-sdk` must all name the same Couch
commit. Vendoring protocol types or copying the harness is not equivalent: it
allows the source under test to redefine the contract it is supposed to meet.

An independent repository is ready for feed review only when its lock file,
normal dependencies, development dependencies, manifest, and test output all
identify the same full Couch revision. The feed then records a separate full
commit for that repository and rebuilds the reviewed source; a branch name,
tag, or an APK supplied by a contributor is not a substitute.

The curated feed requires a fifth `#[test]` in the same file,
`concurrent_package_startup_is_offline_and_race_free`, and checks that it names
`testing::Package::new` itself. It needs no device: several threads each build a
package and an endpoint against an address nothing answers, proving that the
package slot, the executable copy and the child's socket do not collide and that
startup contacts nothing. Write it per integration rather than calling a shared
function, or the feed's textual check will not see it.

## The fake device is yours

The harness starts the fake device, hands the packaged adapter the settings that
address it, and compares the device's own request log with what the case
declared. It never builds or parses a request, so it does not need to understand
the wire format, and an integration supplies its own device by implementing two
methods:

```rust
pub trait FakeDevice {
    /// Settings that point a configured adapter at this device.
    fn settings(&self) -> serde_json::Value;
    /// Every request it has seen, oldest first, in your own spelling.
    fn requests(&self) -> Vec<String>;
}
```

A case names a `Fixture`, which is how it starts one - `failure` starts three,
each with a fresh log:

```rust
testing::spike(
    adapter(),
    Spike {
        device: Fixture::new(|| FakeSonos::start(Plan::new().otherwise(Answer::Silence))),
        command: "play-pause",
        initial_requests: &["GET /api/v1/players/local/info"],
    },
);
```

`couch-sonos` is the worked example: `clients/couch-sonos/tests/fake/mod.rs`
serves the Control API's JSON over loopback HTTP/1.1 and logs `METHOD /path`,
and `tests/admission.rs` supplies nothing else.

A fixture must be reachable through ordinary settings. The pinned-certificate
integrations take their trust material as settings already, and `couch-sonos`
accepts an explicit `api_root`, whose validator confines plain HTTP to loopback.
That rule is shipping behaviour. **An integration that needs a test-only trust
bypass in shipping code to produce these cases is not admissible**; change the
configuration surface instead, and say so in review.

`MockHost` remains the default for line protocols. `ConformanceCase`,
`FailureCase`, `TimeoutCase` and `SpikeCase` keep their fields and convert into
the transport-agnostic cases, so `couch-denon` and `couch-echo` are unchanged.

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

Protocol v2 remains unreleased. A future v2 admission must additionally prove
that declared typed actions reject malformed, out-of-range, and off-step
values before device I/O, retain no-retry behavior on an ambiguous write, and
match the manifest's minimum core protocol version. Until the catalog schema
and validator accept that protocol version, it cannot enter a curated feed.

## Require admission before merging

Once this workflow is merged into each target branch and has completed a run,
an administrator can enable a branch ruleset for `dev` and `main`:

1. Open **Settings → Rules → Rulesets**, create an active branch ruleset, and
   target those branches (or edit their existing branch protection rules).
2. Enable **Require a pull request before merging** and **Require status checks
   to pass**. Add the exact check name `admission`, with **GitHub Actions** as
   its expected source. In REST check-run results that app is `github-actions`
   with app ID `15368`; the workflow title is not the check name.
3. Require branches to be up to date before merging. Preserve any other
   required checks. Limit bypass permissions if admission must apply to admins.
4. Verify with a failing test PR that merging is blocked, then fix it and verify
   the required check succeeds. A check must have run recently to appear in
   GitHub's selector.

Merge the workflow before enforcing its check so older branches do not become
blocked on a check they cannot produce. Keep the final job on every PR, without
workflow-level path filters. If enabling a merge queue, first add a
`merge_group` trigger and validate the gate on queue commits. A separate
integration repository needs its own equivalent workflow and branch rule;
protection in this monorepo does not transfer automatically. The feed adds a
second gate: it checks the exact source commit again, runs the locked repository
suite, builds the ARM binary without secrets, and passes only that reviewed
artifact to the protected signer.

See GitHub's [required status check rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#require-status-checks-to-pass-before-merging).

## Review checklist

An integration change should include its manifest, adapter, fake device tests,
source metadata, limitations, and documentation in one review. A monorepo
integration also updates the catalog. An independent release updates its own
lock file first, then a separate feed review advances only its immutable source
pin. Use the integration pull-request template to record the commands and any
physical-device evidence.

## Source references

- [`integrations/catalog.json`](https://github.com/Couch-OS/couch/blob/main/integrations/catalog.json)
- [`tools/integrations/validate_catalog.py`](https://github.com/Couch-OS/couch/blob/main/tools/integrations/validate_catalog.py)
- [`clients/couch-plugin/tests/protocol.rs`](https://github.com/Couch-OS/couch/blob/main/clients/couch-plugin/tests/protocol.rs)
