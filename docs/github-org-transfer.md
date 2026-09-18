# Moving the repositories to Couch-OS

Couch lives in the [Couch-OS](https://github.com/Couch-OS) organization.
`couch`, `couch-installer`, `couch-kernel`, `couch-site` and
`couch-integration-denon` moved there on 2026-09-17. `couch-integrations` has
not moved yet.

GitHub redirects the web links and Git remotes of a transferred repository, but
not its GitHub Pages site, and its API reports release assets under the new
owner. Anything a deployed remote checks against an exact owner therefore has to
accept the new one before its repository moves.

## The retained dangerouslaser/couch archive

Creating a repository at a moved repository's old name permanently removes
GitHub's redirects, so `dangerouslaser/couch` is a deliberate exception: it was
recreated as an archive that carries no source code and serves only what
already-installed systems need.

- Remotes running `v0.1.0-alpha.20260916.170` or earlier list releases from
  `api.github.com/repos/dangerouslaser/couch` and accept only assets under
  `github.com/dangerouslaser/couch/releases/download/`. The archive serves
  `v0.1.0-alpha.20260917.173`, whose signed manifest names that same owner, so
  those remotes can still take one update. From `.173` on, the updater lists
  releases by the repository's permanent GitHub ID, which resolves to
  `Couch-OS/couch`, and accepts assets under either owner.
- The published `.170` install commands download the desktop installer and the
  OS payload from that archive, and the `.170` `installer.json` pins the payload
  there.
- Its README and release notes link to the corresponding source at the new
  location, which is how the GPL source requirement is met for the binaries it
  serves.

Because the redirects are gone, anything else that used the old address must
name `Couch-OS/couch` directly: Git remotes, the couch-site build checkout and
`source-pin`, the integrations feed's core pin, and integrations' Cargo Git
dependencies.

## Before moving `couch-integrations`

The signed integrations feed is served by GitHub Pages, which is not redirected
when a repository changes owner, and remotes fetch the index with redirects off
(`fetch_index_from` in `daemon/couch-integrations/src/management.rs`). Runtimes
up to `.175.dev` know one address, `dangerouslaser.github.io/couch-integrations`;
moving the repository, or attaching a custom domain to its Pages site (which
turns that address into a 301), breaks their package manager. Later runtimes try
`https://packages.couch-os.dev/{stable,preview}` first and the old address
second, with the same official key, so they work on either side of the move.

1. Ship that runtime and give remotes that use integrations time to take it.
   The address lives in an integration contract path
   (`daemon/couch-integrations`), so the tested integration set is renewed with
   it (`tools/release/tested-integrations.json`).
2. Move the repository, attach `packages.couch-os.dev` to its Pages site, point
   the Cloudflare CNAME (DNS only) at `couch-os.github.io.`, and confirm HTTPS
   enforcement. A remote still on an older runtime keeps its installed
   integrations but cannot browse or update packages until it updates; a static
   copy of the feed at the old address is the fallback if that matters.
3. Afterwards, switch the feed's own `source-pin.json` tooling entry and the
   `gh workflow run` command in its README.
4. Check that the `package-signing` and `github-pages` environments, their
   secrets and the `admission` ruleset survived the transfer.

## Publishing after the move

- New releases are published under `Couch-OS/couch`: `release::PREFIX` selects
  the Couch-OS entry of `PREFIXES`, and the payload URL in
  `tools/release/package_public_installer.py` and the package metadata URL in
  `tools/integrations/build-apk.sh` name it too.
- `tools/release/bump_release.py <tag> --repository Couch-OS/couch` moves the
  published install commands once an installer release is cut there. Until then
  they stay on the `.170` assets in the retained archive.
- `tools/release/tested-integrations.json` keeps the owner recorded when each
  integration was tested; `verify_integration_set.py` accepts either owner.
- `kernel/release-pin.json` still records the old kernel repository URL. It is
  hashed into boot payload provenance, so change it with the next boot release.
