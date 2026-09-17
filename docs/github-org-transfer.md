# Moving the repositories to Couch-OS

The Couch repositories are moving from the `dangerouslaser` account to the
[Couch-OS](https://github.com/Couch-OS) organization. GitHub redirects web
links and Git remotes of a transferred repository, but not GitHub Pages sites,
and its API reports release assets under the new owner. Anything a deployed
remote checks against an exact owner therefore has to accept the new one before
its repository moves.

Already moved: `couch-installer`, `couch-kernel`, `couch-site` and
`couch-integration-denon`. Still under `dangerouslaser`: `couch` and
`couch-integrations`.

Never create a new repository at a moved repository's old name. Doing so
permanently removes GitHub's redirect.

## Before moving `couch`

Remotes find runtime and boot updates through the release listing of this
repository. Updaters before the owner-agnostic change list releases from
`api.github.com/repos/dangerouslaser/couch` and accept only assets under
`github.com/dangerouslaser/couch/releases/download/`. After the move GitHub
lists every asset under `Couch-OS/couch`, so those remotes would silently find
no update, including the one that fixes this. Their only recovery is a reinstall.

1. Ship the owner-agnostic updater in a runtime release. It lists releases by
   the repository's permanent ID and accepts assets and signed manifests under
   either owner (`daemon/couch-updates/src/release.rs`). Release tooling keeps
   publishing `dangerouslaser/couch` URLs, because older updaters must be able
   to install that release.
2. Wait until remotes have installed it. Remotes left on older runtimes need a
   reinstall after the move.
3. Merge the couch-installer change that accepts OS payloads from
   `Couch-OS/couch`. Published installers keep working through redirects.

## Before moving `couch-integrations`

The signed integrations feed is served by GitHub Pages, which is not redirected.
Remotes read it from `https://packages.couch-os.dev/{stable,preview}` once the
feed domain change ships.

1. Add the DNS record `packages.couch-os.dev CNAME dangerouslaser.github.io.`
   Then set it as the custom domain of the couch-integrations Pages site and
   enforce HTTPS.
2. Verify the preview index and public key are served from the new host.
3. Ship a runtime whose official feed URL is `packages.couch-os.dev` to every
   remote that uses integrations. The URL lives in an integration contract path,
   so the tested integration set evidence must be renewed first
   (`tools/release/tested-integrations.json`).

## After each move

`couch-integrations`:
- Point the DNS record at `couch-os.github.io.` and confirm the custom domain and
  HTTPS are still set.
- Update the feed's `source-pin.json` core repository once `couch` has moved.

`couch`:
- Publish under the new owner: set `release::PREFIX` to the Couch-OS entry of
  `PREFIXES`, and use the Couch-OS URL for the payload in
  `tools/release/package_public_installer.py` and in
  `tools/integrations/build-apk.sh`.
- Move the published install commands with
  `tools/release/bump_release.py <tag> --repository Couch-OS/couch`.
- couch-site: update the Pages workflow checkout, `source-pin`,
  `GITHUB_MAIN` in `scripts/render_developer_docs.py`, and the install command
  URLs.
- Update the Denon repository's Git dependency URLs and the documentation links
  that still name `dangerouslaser/couch`.
- Point local clones at the new remote:
  `git remote set-url origin https://github.com/Couch-OS/couch.git`.
