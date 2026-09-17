# Installer release discovery

The Python reference terminal
(`python3 couch-installer/tools/installer/couch_tui.py`) can browse published
Couch release metadata using **r · Browse GitHub releases**. Choose `stable` or
`alpha`, optionally enter an exact tag, then select a numbered release to
inspect its descriptor. Browsing does not download or execute installer/OS
payloads and never opens USB.

The [native installation flow](installer.md) uses **Wi-Fi for image transfer**,
with USB for bootstrap and recovery. This Python metadata browser remains a
separate reference tool; it does not start that transport or replace the native
release-specific launcher. Existing simulation and private trial paths remain
separate.

## Release selection

Discovery queries the unauthenticated
`/repos/dangerouslaser/couch/releases` API, with bounded pagination and response
sizes. It never reads the operator's GitHub CLI credentials. Drafts are excluded;
an unpublished alpha is consequently unavailable to public discovery.

Stable tags use `vMAJOR.MINOR.PATCH`; alpha tags use
`vMAJOR.MINOR.PATCH-alpha.DATE.NUMBER` (`vMAJOR.MINOR.PATCH-alpha.NUMBER` is the
older shape and still reads), and a dev-branch build is that tag with a trailing
`.dev`. Every prerelease tag must have GitHub's prerelease flag set. Other
prerelease channels are not silently treated as alpha, and a dev build is listed
on the `dev` channel rather than with the alphas, the way the remote's own
updater separates them. Selection is sorted by semantic-version precedence and
pinned to the release ID, tag, asset ID, exact versioned URL, byte size and
SHA-256. It never follows a floating `latest` reference later.
GitHub's [release API documentation](https://docs.github.com/en/rest/releases/releases)
describes the list endpoint and asset digests; `/latest` excludes prereleases.

## Discovery descriptor

For `v0.1.0-alpha.1`, the asset name is exactly
`couch-v0.1.0-alpha.1-sanytron-ha100.json`. The uploaded asset must have a SHA-256
digest reported by GitHub and be at most 256 KiB. Explicit inspection verifies
its downloaded size and hash against the selection before parsing:

```json
{
  "schema": 1,
  "model": "sanytron-ha100",
  "version": "v0.1.0-alpha.1",
  "installable": false,
  "files": []
}
```

Each listed file requires `name`, positive byte `size`, lowercase `sha256` and
an exact versioned repository release `url`. Names are unique safe basenames
containing the selected version, such as
`couch-installer-v0.1.0-alpha.1-linux-x86_64.tar.gz`. Empty preparation descriptors
must state `installable: false`. Releases without the named descriptor are not
selectable installation releases.

This **discovery descriptor is distinct from the raw partition flash manifest**
whose schema uses `partitions` and `images`. The browser never converts one into
the other. Even a descriptor claiming `installable: true` cannot authorize
installation or override the reviewed bootstrap version/checksum pins.

GitHub asset hashes detect changed bytes; hashes from the same hosting account
are **not an independently verified publisher signature**. Signing, public
artifact approval and physical installation validation remain separate gates.

Validation:
`python3 -m unittest discover -s couch-installer/tools/installer -p 'test_release_discovery.py'`.
