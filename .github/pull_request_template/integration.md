## Integration

- Catalog ID:
- Admission tier: test-only / preview / production
- Device protocol and transport:
- Declared protocol version and typed actions (if applicable):
- Known limitations:

## Required admission cases

- [ ] Conformance: handshake, configure, commands, status, and inputs
- [ ] Failure paths: invalid settings, refusals or malformed replies, capability gate
- [ ] Timeout/no retry: one ambiguous command attempt, explicit recovery only
- [ ] Spike: bounded queue or burst behavior, stale work never reaches the device
- [ ] Typed actions (if applicable): invalid range and step values refused before I/O
- [ ] `python3 tools/integrations/validate_catalog.py --run-tests`

## Hardware evidence

- Device model and firmware:
- Validation date:
- Behaviors exercised:
- Evidence link or repository path:

Use “not tested on physical hardware” for a preview when that is the truth.
Production catalog entries require evidence; simulator and fake-peer results do
not satisfy that requirement.
