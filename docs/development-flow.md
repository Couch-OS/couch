# Development flow

Two branches, three update channels, one rule: what testers receive is
decoupled from what got merged.

## Branches

- **`dev`** is the integration branch. Feature branches are opened against it
  and merged as soon as they are reviewed and their tests pass. It moves many
  times a day and nobody's remote follows it except the development remote.
- **`main`** is the release branch. It changes only by promoting `dev` to it
  with one pull request, and every promotion is a real alpha release. Community
  remotes on the Alpha channel see promotions and nothing else.

Open feature pull requests against `dev` (`gh pr create --base dev`). Hotfixes
that cannot wait go to `dev` first and are promoted straight away; `main` is
never patched directly, so the two branches do not diverge.

## Channels

The updater's channel decides which GitHub releases a remote considers:

| Channel | Takes                                                   | Who       |
|---------|---------------------------------------------------------|-----------|
| Stable  | finished versions                                       | everyone, eventually |
| Alpha   | Stable plus `alpha.<date>.<n>` prereleases              | community testers |
| Dev     | Alpha plus `alpha.<date>.<n>.dev` builds from `dev`     | the development remote |

A dev build's tag has the same `alpha.<date>.<n>` shape as an alpha with `.dev`
appended, and takes the next `<n>` after whatever was published last, alpha or
dev. A promotion then takes the `<n>` after the last dev build. Semver orders
all of them by `<date>.<n>`, so a remote on Dev follows the dev builds and
still picks up the next promotion, and Alpha rejects any tag carrying the `dev`
identifier. The channel is
chosen on the web UI's Updates page or under Settings → Updates on the remote.

## Cutting a dev build

From the `dev` branch, the same runtime recipe as a release
([release-cutting notes](releases.md), and `docs/runtime-updates.md#publishing`)
with a dev tag: `v0.1.0-alpha.<date>.<n>.dev`, where `<n>` is one more than
the last published build of either kind. Publish as a prerelease with the
runtime archive, its signed manifest and `SHA256SUMS`; corresponding source and
long notes are for promotions. Dev prereleases are disposable: delete one as
soon as the next dev build or a promotion supersedes it, the way a failed
candidate is, so the release list stays readable.

## Promoting to `main`

1. On `dev`, prepare the promotion pull request with the runtime tag
   `v0.1.0-alpha.<date>.<n>` the release will carry. Runtime promotions do not
   change the published installer commands. Installer builds have their own
   `tools/installer/VERSION` and `installer-v…` release tags; see
   [installer release boundaries](installer-release-boundary.md).
2. Open a pull request from `dev` to `main` titled for the batch, listing the
   feature pull requests it carries. Merge it with a merge commit.
3. Tag the merge commit `v0.1.0-alpha.<date>.<n>` (the `<n>` after the last
   dev build, and the tag selected in step 1) and publish the full prerelease:
   runtime archive and manifest, corresponding source, `SHA256SUMS`, notes that
   name the changes since the previous promotion and what was validated on
   hardware.
4. Delete the dev prereleases the promotion supersedes. Remotes on Dev move to
   the promotion on their next check because it sorts above the dev builds.
5. Bring `dev` back in line: `git checkout dev && git merge --ff-only main`
   (the promotion merge is the only new commit on `main`, so this is always a
   fast-forward).

## Publishing installer commands

After a separately validated installer release is available, run
`python3 tools/release/bump_release.py installer-v0.1.0` with its actual tag.
When moving published launchers to the separate repository, also pass
`--repository Couch-OS/couch-installer`.
This updates `README.md`, `docs/installer.md`, and the legacy
`tools/release/current-release.txt` pointer read by the separate `couch-site`
build. `tools/release/installer-repository.txt` records the published repository;
the site must consume it alongside the tag before a repository cutover. Existing
published commands stay unchanged until that step. CI checks
their consistency with `bump_release.py --check`. This tool updates references;
it does not build, tag, upload or publish a release.

## Why not automate the dev builds

The runtime is built on the release host because it holds two things that are
not in the repository: the Sonos developer key compiled into the binaries and
the Ed25519 seed that signs update manifests. A GitHub Actions workflow could
build and publish a dev prerelease on every push to `dev`, and it is the next
step once those two secrets are allowed to live in repository secrets. Until
then dev builds are cut by hand, which is quick because the release host keeps
its build cache between them.
