# Desktop installer dependencies

The native host's dependency pin is `tools/release/host_dependencies.json`.
It currently covers Google's official Platform-Tools 37.0.1 ADB archives for
Linux x86-64, macOS Intel/Apple Silicon (universal binary), and Windows x86-64.
The versioned Google download URLs, archive sizes/SHA-256 and each retained
file's size/SHA-256 are checked in. Pins were measured from those official
downloads on 2026-09-10; they are Couch's reviewed content pins, not an
upstream detached signature.

`host_dependencies.py --output /path/outside/checkout/new-directory --smoke`
downloads into temporary storage, verifies the complete archive and selected
members, and creates a fresh owner-local directory. `--archive` supports an
already downloaded pinned ZIP. The same open file is hashed and parsed.
Unknown ZIP paths, symlinks, duplicates, excessive expansion, wrong sizes and
wrong hashes fail before extraction. Existing outputs are never overwritten.
An interrupted output has no completion receipt and must not be used.

`receipt.json` records schema, platform, file hashes/sizes, relative executable
paths and the reviewed MTK revision. Consumers must compare it with the
checked-in pins and reverify files; an editable receipt alone establishes no
trust. This component contains no credentials and does not replace the native
session guard's private directory and Windows ACL protections.

Only ADB, its packaged libraries, `NOTICE.txt` and `source.properties` are
retained. `fastboot` and filesystem writers are excluded. The optional smoke
check reverifies bytes and executes only `adb version`; it starts no ADB server
and accesses no USB device. CI performs the actual download and version check
on all three operating systems, without uploading the binaries as artifacts.

These are owner-local upstream downloads, not binaries redistributed by Couch.
Google's [Platform-Tools page](https://developer.android.com/tools/releases/platform-tools)
contains the release notes and download terms; the downloaded package's full
notices remain beside ADB. ADB source is maintained in
[AOSP packages/modules/adb](https://android.googlesource.com/platform/packages/modules/adb/).

This receipt deliberately marks `complete_mtk_runtime: false`: pinned Python,
MTK adapter wheels and native libusb delivery are separate work. No download
agent, device backup, calibration or vendor runtime is included. Windows USB
driver binding and actual HA100 operation still require separate validation.
Linux ARM and Windows ARM are not covered by these ADB pins.
