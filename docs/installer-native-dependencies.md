# Native installer dependency bootstrap

The native host prepares its own Python, MTK, libusb and ADB dependencies. Users
do not need Python, pip, Git, curl or a system libusb installation for this step.
The compiled host reads reviewed pins from `mtk_dependencies.json` and
`host_dependencies.json`, downloads them directly over HTTPS, verifies exact
sizes and SHA256 values, and parses the verified memory snapshots.

```sh
couch-installer-host prepare-dependencies NEW_PRIVATE_PARENT
```

The parent must be new and outside Git. The host creates it with the platform's
private ownership policy, acquires a `SessionGuard`, and builds a new
`run/dependencies` tree. Existing or interrupted outputs are preserved and never
silently resumed. Download timeouts, extraction size/member limits, unsafe paths,
archive aliases and source inventories are checked before execution.

The library entry point is `dependencies::prepare(&SessionGuard, platform,
progress)`. Its progress callback can return an error when the TUI disconnects;
that aborts preparation before another download/extraction step or any execution.
It returns verified paths plus remembered receipt digests. Keep the guard alive
and call `PreparedDependencies::verify()` before using those paths. Consumers must
not rebuild trust from an arbitrary on-disk receipt.

The runtime receipt describes Python, seven pinned wheels and the 96 reviewed
MTK Python modules. ADB has its own receipt and includes only its explicitly
pinned companion libraries/notices. No system-library discovery fallback is
needed: the transport uses the verified libusb path.

The approved download agent is a separate owner-local input:
`mtkclient/Loader/MTK_DA_V5.bin`, 22,483,280 bytes,
SHA256 `aef234190ccb8145d2e3b8459741e9adb70f2caa8481aa216c1b25152afaca1f`.
It is extracted only from the already verified upstream archive into
`owner-da/loader.bin` and carries a non-redistributable receipt. It is never
included in the runtime or a public Couch asset. This removes the need for the
owner to find a loader manually without republishing vendor bytes.

`--dependency-smoke NEW_PRIVATE_PARENT` performs the same native bootstrap and
then runs verified Python/ADB version commands, loads the bundled libusb, and
imports MTK with initialization disabled and USB discovery prohibited. Native
CI runs it on Linux x86-64, macOS ARM/Intel and Windows x86-64 with an empty PATH.
The smoke check is not a physical install, reboot or flash validation.

The existing [Python preparation helper](installer-mtk-runtime.md) remains an
independent validation tool. Native owner-side downloads avoid republishing the
third-party runtime/DA archives. Later hosted runtime binaries would require
separate corresponding-source/notices coverage; do not upload the private
bootstrap cache as a release artifact.
