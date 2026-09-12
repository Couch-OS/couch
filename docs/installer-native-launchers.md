# Native installer launchers

`tools/release/installer_launchers.py` generates an `install.sh` and `install.ps1`
for one exact release. Generation does not publish a release or establish device
acceptance. The old public bootstrap remains disabled until a complete installer
is ready.

The asset directory must contain these flat, regular files:

- `couch-installer-host-linux-x64`, `couch-installer-tui-linux-x64`
- `couch-installer-host-macos-universal`, `couch-installer-tui-macos-universal`
- `couch-installer-host-windows-x64.exe`, `couch-installer-tui-windows-x64.exe`
- `installer.json`

The build workflow produces native host and terminal binaries, plus ad-hoc-signed
universal macOS binaries. Release assembly assigns the flat names above. It must
use binaries and public payloads from the reviewed source commit and retain their
build receipts. Linux ARM terminal/host build artifacts are also produced, but
the complete dependency runtime and these launchers currently support Linux x64.

`installer.json` contains only `schema: 1`,
`kind: "couch-native-installer-release"`, `model: "sanytron-ha100"`, the release
`version`, a forty-digit `source_commit`, and a `payload` object with an exact
GitHub release URL, byte `size`, `sha256` and `format: "tar.gz"`. It contains no
device identity, credentials or owner firmware. Native host admission remains
responsible for the payload and owner-input verification.

```sh
python3 tools/release/installer_launchers.py \
  --assets /path/to/reviewed-flat-assets \
  --output /path/to/new-launchers \
  --version v0.1.0-alpha.1
```

Python is a maintainer generation tool. Generated launchers require no Python,
Git, compiler, package manager or archive extractor. They download the native
host, terminal and configuration from fixed release URLs, enforce byte limits
and SHA-256 pins for every file, and only then start the terminal with
`--native-backend HOST --config CONFIG`. There is no fallback to unverified
executables or mutable `latest` URLs. Downloads use temporary owner-local paths;
the native session stores durable backups separately, so launcher cleanup never
removes originals.

The Unix launcher restores terminal input for `curl | sh` and uses curl plus
sha256sum or shasum. Windows uses PowerShell 5.1+/.NET HTTPS streaming, a bounded
download cancellation token and Get-FileHash. The launcher trust anchor is the
reviewed script obtained over HTTPS; embedded asset pins are not a substitute
for reviewing or authenticating that script.

Tests exercise native dispatch with a controlling terminal, reject corrupted
downloads before execution, preserve existing outputs, reject private fields in
public configuration, and parse the generated script with Windows PowerShell.
Actual installation and Windows USB-driver acceptance remain separate checks.
