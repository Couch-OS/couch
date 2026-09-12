# Couch as an Apple Home accessory

Research checked 2026-09-10 against the primary sources linked below. This is a
feasibility assessment, not an implemented feature, certification claim or device
validation. No advertisements or pairing requests were sent during this research.

## Recommendation

An opt-in **scene-button accessory** is worth a bounded prototype. A Couch button
could invoke a user-assigned “Movie time”, “Intermission” or “Good night” scene in
Apple Home, without Couch implementing every light/blind vendor integration.
A small HAP-over-Wi-Fi prototype is the shortest experiment for this specific
Apple Home experience. Before adopting an older Rust HAP stack for distribution,
compare a Matter Generic Switch prototype: its ecosystem and maintenance story
may justify the additional commissioning work.

Keep both experiments independent of the installer and normal remote operation.
Do not advertise compatibility until pairing, button assignments, reconnection
and battery behavior have been tested on actual supported Apple software.

## What advertising accomplishes

Bonjour/mDNS lets controllers discover an accessory's TCP endpoint. Apple's ADK
advertises `_hap._tcp` with identity, configuration, category and pairing-state
TXT records. Advertising our web UI's port under that name would not create a
HomeKit accessory: the endpoint must implement the accessory database, pairing,
authenticated sessions, characteristic operations and event subscriptions.
See [Apple's HAP discovery implementation](https://github.com/apple/HomeKitADK/blob/master/HAP/HAPIPServiceDiscovery.c)
and [HAP accessory service definitions](https://github.com/apple/HomeKitADK/blob/master/HAP/HAPServiceTypes.h).

Couch would be an **accessory server**. Apple Home/iPhone/HomePod/Apple TV would
be controllers. This does not grant Couch access to the home's accessory list,
scene names or controller credentials. It does not make Couch a Home hub, add
Siri to its microphone, or turn its existing Apple TV Companion/AirPlay client
pairing into HomeKit accessory pairing. Those are separate roles/protocols.
Apple describes HomeKit apps as coordinating accessories through its framework;
that Apple-platform API is not a Linux controller SDK.
[Apple's home developer overview](https://developer.apple.com/apple-home/).

## Useful product features

| Feature | Representation and limitations |
|---|---|
| Physical scene buttons | Stateless Programmable Switch events. Users assign actions in their Home setup; Couch emits the selected button event rather than naming an Apple scene. Start with one deliberately configured button. |
| Double/long press actions | HAP defines single, double and long press values. Configure these explicitly so normal remote keys do not unexpectedly run home actions. Do not promise that every Home version offers identical configuration UI. |
| Battery information | Battery Service could expose actual level/charging/low-battery information. Verify how Home displays it; do not promise a battery tile or arbitrary notification automation. |
| Find the remote | Identify can flash a visible screen indicator; a later explicit writable function could play a short sound through Couch. Identify is not Apple's Find My network. |
| Home/Siri starts a Couch activity | A separate writable switch can represent a real on/off activity state and dispatch allowlisted Couch commands. This requires well-defined state, feedback and error behavior. It is different from an event-only scene button. |
| Bridge an existing controlled device | Possible later for an accurately modeled TV/switch with reliable state feedback. IR-only toggle commands cannot truthfully report power state; avoid exposing all devices automatically. |

Apple requires a distinct Stateless Programmable Switch service for each physical
switch. Multiple such services use Service Label and unique Service Label Index
values. Battery and Identify are standard services/characteristics, but their
product presentation must be validated.
[Apple service definitions](https://github.com/apple/HomeKitADK/blob/master/HAP/HAPServiceTypes.h).
The three input event values are documented separately in
[Apple's programmable switch event API](https://developer.apple.com/documentation/homekit/hmcharacteristicvalueinputevent).

Apple Home can assign accessory actions to automations and scenes, and Siri on
Apple devices can run scenes/control suitable writable accessories. Plan to test
scene automations with a configured home hub. A scene can include existing HAP
and Matter accessories in the same Home; Couch need not speak their individual
protocols. The event service itself is not a remotely invokable Siri button.
[Apple scenes and automations guide](https://support.apple.com/en-gb/102313),
[Apple Matter integration](https://developer.apple.com/apple-home/matter/).

## Fit with this remote

The repository identifies the HA100 as a quad Cortex-A7 ARMv7 device with 1GB RAM.
It already runs Alpine, Wi-Fi and Rust GUI/system/configuration services.
Current display blanking is not full SoC suspend; true suspend remains separate
work. See [README](../README.md) and [system service](system-service.md).

Engineering assessment: a small encrypted IP accessory serving occasional button
and battery events is plausible on this hardware. No benchmark was performed,
and existing RAM capacity is not proof of acceptable latency, idle power or
kernel/toolchain compatibility. Cross-compile for the existing ARMv7 musl target;
check entropy readiness, sockets, timers, multicast/interface behavior and all
crypto dependencies on kernel 3.18. Avoid assuming modern distribution binaries
will run on the pinned Alpine 3.21.7 userland.

The harder constraint is availability. If Wi-Fi disconnects or a future suspend
mode stops networking, controllers cannot deliver writes or receive button events.
Measure screen-off reconnection and first-press behavior. Never replay an old scene
press after reconnect: delayed “Good night” actions could be surprising. Preserve
normal remote operation when Home is unavailable. Do not enable continuous wake
locks merely to keep a Home tile online without measuring the battery cost.

Use a separate optional `couch-home` process with a narrow local API/event feed.
`couch-system` can supervise lifecycle and provide bounded battery/status data;
the GUI remains the owner of button interpretation. The paired web UI can enable
the integration and show pairing/reset state. A Home client must never obtain
shell access, arbitrary URLs, filesystem paths or unrestricted system operations.

## Libraries and platform choices

- **Rust HAP (`ewilken/hap-rs`, crate `hap`)** implements an IP accessory server,
  built-in mDNS, services and file-backed configuration. Its own README explains
  that bound IP configuration is not automatically refreshed after creation;
  handle DHCP/interface changes explicitly. It is MIT/Apache-2.0 source.
  [Repository](https://github.com/ewilken/hap-rs).
  At review, its manifest was `0.1.0-pre.15` with older crypto/network dependency
  generations; GitHub reported the last push as 2024-08-05. Treat it as a candidate
  needing dependency/security review and a maintained fork decision, not a proven
  current-tvos stack. [Manifest](https://github.com/ewilken/hap-rs/blob/main/Cargo.toml),
  [repository metadata](https://api.github.com/repos/ewilken/hap-rs).
- **Apple HomeKitADK** supplies primary protocol/service reference code and
  noncommercial accessory examples. The public repository was archived on
  2025-10-28. It is C, and is not an actively maintained Rust integration solution.
  [Apple ADK](https://github.com/apple/HomeKitADK).
- **HAP-NodeJS/Homebridge** is a useful independent protocol reference or external
  bridge on a computer the user already runs. It explicitly describes itself as
  an uncertified accessory-server implementation. Do not add Node and a bridge
  framework to this small remote just for one button without comparing costs.
  [Project documentation](https://github.com/homebridge/HAP-NodeJS).
- **Matter with `project-chip/rs-matter`** offers a Rust alternative whose project
  reports embedded-Linux support and testing against Apple Home and other major
  controllers. It reports API instability and ongoing product certification work;
  those project claims do not establish HA100 compatibility or certify Couch.
  GitHub reported a 2026-09-09 push at this review.
  [Project](https://github.com/project-chip/rs-matter),
  [repository metadata](https://api.github.com/repos/project-chip/rs-matter).

## Matter alternative

Matter can operate over Wi-Fi; Thread is not necessary. No compatible Thread
radio is assumed for HA100. A Matter Generic Switch is the relevant event device
rather than pretending to be a light. The official SDK's Darwin guide lists
momentary Generic Switch support in Apple Home and explicitly warns that its
compatibility table may be out of date. Test current controller behavior.
[Apple Matter overview](https://developer.apple.com/apple-home/matter/),
[SDK Apple-platform guide](https://github.com/project-chip/connectedhomeip/blob/master/docs/guides/darwin.md),
[official switch example](https://github.com/project-chip/connectedhomeip/blob/master/examples/light-switch-app/nrfconnect/README.md).

A Matter implementation adds commissioning, fabrics, operational credentials,
subscription state, device attestation and lifecycle work. Already-connected IP
commissioning is a useful lab option; do not infer that it proves Apple's complete
Wi-Fi onboarding experience or that BLE commissioning works on this remote.
Production would need appropriate attestation inputs, supported commissioning and
certification decisions. Apple's accessory guidance additionally calls for OTA,
diagnostics and interoperability testing. Existing Couch update verification
should remain authoritative; an OTA integration cannot be an alternate unsigned
installation path.
[Apple accessory best practices](https://developer.apple.com/apple-home/downloads/Matter-Accessory-Best-Practices-for-Apple-Home.pdf).

## Pairing, persistence and distribution

Generate a unique accessory identity, key material and setup code per installation;
never ship a paired identity or reuse the existing Apple TV controller credentials.
Expose pairing only through an explicit user action and show a code/QR on the
remote or authenticated setup UI. Keep long-term keys and paired-controller
permissions in private persistent state outside versioned runtime slots, with
atomic durable writes. Stable accessory/service/characteristic IDs must survive
updates. Removing a pairing, resetting Home integration, and installing a new
runtime must have distinct behavior. Rate-limit setup attempts, bound all requests
and connections, and redact keys/codes from logs. These are proposed Couch design
requirements, not claims about the selected library's existing guarantees.

Open-source licensing does not itself confer Apple certification or permission to
use compatibility badges. Apple's current developer page says companies developing
HomeKit accessories for distribution or sale need MFi enrollment; Matter accessories
for distribution or sale require CSA certification. Apple's public ADK specifically
describes noncommercial prototyping and directs commercial work to its MFi version.
Resolve how those requirements apply to Couch firmware distribution before shipping
or marketing the feature; do not assume “free/open source” answers that question.
[Apple developer requirements](https://developer.apple.com/apple-home/),
[Apple ADK scope](https://github.com/apple/HomeKitADK).

## Bounded proof of concept

Engineering estimate, not a delivery promise: first allocate several engineering
days to a dependency/build/security review and host-only test accessory; then a
separate short hardware acceptance phase if that succeeds. A maintained shipped
integration is a multiweek effort, especially if the HAP crate needs modernization
or Matter commissioning becomes the chosen path.

1. Host-only: pin one candidate stack, implement one button service plus Identify,
   verify pairing removal, malformed input limits and durable identity using tests.
2. With separate device-test authorization: cross-compile the same accessory, pair
   it in a test Home and assign single/long press to a harmless light scene.
3. Test at least 100 intentional presses, repeated identical events, UI key-repeat
   suppression, screen-off/wake, Wi-Fi loss, DHCP change, Home hub restart and
   process restart. Report latency distribution and event loss/duplication.
4. Measure memory, idle CPU and battery impact against Couch without the service;
   test interrupted state writes and runtime update/rollback without losing pairing.
5. Decide HAP vs Matter and the distribution route before expanding to activities,
   bridge functionality or automatic UI exposure.

Success means reliable explicitly assigned home actions while normal remote
functions remain responsive. Merely appearing in an mDNS browser is not success.
