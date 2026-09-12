# Installer Python and MTK runtime

`tools/release/mtk_dependencies.py` prepares the owner-local runtime required by
the installer's supervised MTK transport. It pins Python 3.12.14 from the Astral
20260901 release, seven dependency wheels, and reviewed MTK source revision
`60e07f3b343a4469389f15967626d63e049968d4`. Linux x86-64, macOS ARM64/Intel and
Windows x86-64 have separate Python/native-library pins.

The bundle contains PyUSB, pyserial, colorama, both Crypto/Cryptodome namespaces,
libusb-package and its importlib-resources dependency. It retains their included
license files. Optional GUI/disassembler packages are omitted; their import
fallbacks are exercised by the smoke check. Binary MTK download agents, firmware,
prebuilt payloads, WinFsp DLLs and preloaders are never extracted into the bundle.
The approved DA remains a separate owner-local input.

## Preparing and checking a bundle

```sh
python3 tools/release/mtk_dependencies.py prepare \
  --platform macos-aarch64 --cache /private/cache/mtk --output /private/runtime/mtk
python3 tools/release/mtk_dependencies.py smoke \
  --root /private/runtime/mtk --receipt-sha256 PRINTED_RECEIPT_SHA256
```

Preparation verifies every archive's pinned size and SHA256 before extraction or
interpreter execution, then snapshots and rechecks its bytes immediately before
parsing from memory. A changed cache path cannot substitute a different archive.
Downloads use bounded reads and a total deadline; output must be outside Git. It extracts wheels directly, without pip resolution,
setup.py or install hooks. Safe Python archive aliases become ordinary files;
archive traversal, unexpected file types and cache collisions fail closed.
`--offline` requires every pinned input already in the verified cache. Use a new
output directory when rebuilding; partial outputs are never admitted as complete.

`runtime.json` records every installed file's size/hash, the Python executable,
MTK source root and revision, libusb path and checked-in pin/inventory hashes.
The consumer must bind this receipt's SHA256 to its trusted release metadata;
a receipt provided by an untrusted bundle cannot authenticate itself. The native
installer verifies the receipt and all files before launching Python, and passes
the explicit verified libusb path. System libusb fallback is not part of this
contract. `verify` and `smoke` require the trusted receipt digest explicitly.

`mtk_session.source_pin` admits only the Python files listed in the checked-in
`mtk_source_inventory.json`, itself bound by a fixed digest in the module. It
checks exact bytes and rejects missing, modified or extra Python modules. No Git
installation or mutable runtime checkout metadata is required. The existing
pinned import loader still compiles those verified sources directly, ignoring
cached bytecode.

The smoke check runs the exact Python `--version`, loads the bundled native
libusb, imports the required MTK modules and constructs `Mtk(preinit=False)`.
USB discovery is replaced with a rejecting stub; no device is opened, rebooted
or flashed. CI runs this check on all four native platforms. It does not prove
physical USB operation or platform driver setup. In particular, the existing
Unix deadline/lock boundary must be replaced by the native supervisor before
claiming Windows physical installation support.

The runtime is currently an owner-local preparation component, not a published
complete installer. A public bundle additionally needs its own corresponding
source/notices coverage for Python, wheel native code and libusb, and the final
host release must bind the actual runtime receipt. The upstream MTK archive is
only a verified cache input: do not publish it, since it contains excluded DA
and prebuilt payload files.

References: [Astral release](https://github.com/astral-sh/python-build-standalone/releases/tag/20260901),
[libusb-package API and license](https://pypi.org/project/libusb-package/),
[reviewed MTK source](https://github.com/bkerler/mtkclient/tree/60e07f3b343a4469389f15967626d63e049968d4).
