# Frozen desktop launcher acceptance

This fixture runs the exact generated `install.ps1` and real frozen host/TUI
binaries in a Windows ConPTY, selects Cancel, and requires exit 0 without an
installer session. It does not install an OS or contact a device.

The manual workflow requires an independently selected installer repository,
installer and OS versions, a binary run ID, public configuration bytes/hash, and a final launcher hash.
Build receipts, the workflow run, and generator checkout must match the independently
supplied forty-digit installer `source_commit` (current host:
`57a3e22b4e86d8d6620dbaedbf30847e26b9bbd4`). The configuration must instead match
an independently supplied OS `source_commit` (current payload:
`271704728c77c13add1763aea7d1f9bd629c63ca`). Both pins are required workflow inputs;
neither is inferred from downloaded artifacts. This permits rebuilding the installer
without rebuilding an unchanged OS payload. The frozen OS fixture remains
`v0.1.0-alpha.20260910.24`; independent schema-2 runs also pin the installer version.
The frozen host-source generator must reproduce the independently supplied launcher
hash. The admission receipt records both identities and the selected installer repository. No executable or launcher is
patched for the test.

The installer repository input is restricted to `dangerouslaser/couch` and
`dangerouslaser/couch-installer`. It selects the build-run API, frozen generator
checkout and binary artifact download, and must match the schema-2 descriptor's
exact `installer-v…` release URL. The OS payload remains pinned to
`dangerouslaser/couch`. Historical schema-1 fixtures use that original repository
for the installer as well. No repository is inferred from downloaded metadata.

Runs using artifacts from the same repository use the workflow token. For
cross-repository runs, configure `INSTALLER_ARTIFACT_TOKEN` with read access to
Actions artifacts and source contents in the selected installer repository;
[GitHub's artifact action requires target-repository access](https://github.com/actions/download-artifact/tree/v4#download-artifacts-from-other-workflow-runs-or-repositories).
The workflow uses this optional token for the frozen source checkout, artifact
download and admission API only; it is not passed to the launcher execution.

The historical schema-1 fixture remains admissible with its original versionless
binary receipts. Versionless receipts are refused for schema 2, where every native
and universal receipt must carry the independently supplied installer version.

Before publication, only network routing is substituted: an ephemeral hosted
Windows runner maps `github.com` to loopback and trusts a temporary fixture
certificate through the normal Windows trust store. An HTTPS listener serves
exactly the three admitted assets in their expected order. The runner's hosts
file, certificate store, and SSL binding are restored afterward. These fixture
changes are never made on a developer or user machine. The script intentionally
refuses to run outside a GitHub-hosted Actions runner.

The result records the admitted source/binary/config/launcher hashes, exact three
requests, ConPTY transcript, exit status, and absence of a native session.
Physical driver binding and installation acceptance remain separate tests.

The ConPTY child explicitly clears inherited redirected standard handles, as
[described by the Windows Terminal maintainer](https://github.com/microsoft/terminal/discussions/15814),
so hosted runner pipes cannot replace the actual console.
