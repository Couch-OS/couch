#!/usr/bin/env python3
"""Read-only GitHub release metadata. Selection never authorizes installation."""
from dataclasses import dataclass, asdict
import hashlib
import json
import re
from urllib.error import URLError
from urllib.parse import urlparse
from urllib.request import Request, HTTPRedirectHandler, build_opener

from couch_install import MODEL, InstallError, require

REPOSITORY = 'dangerouslaser/couch'
API = f'https://api.github.com/repos/{REPOSITORY}/releases'
DOWNLOAD = f'https://github.com/{REPOSITORY}/releases/download/'
MAX_MANIFEST = 256 * 1024
MAX_LIST = 4 * 1024 * 1024
VERSION = re.compile(r'^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-alpha\.([1-9][0-9]*))?$')
SHA = re.compile(r'^[0-9a-f]{64}$')


class MetadataRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        parsed = urlparse(newurl)
        require(request.full_url.startswith(DOWNLOAD)
                and parsed.scheme == 'https' and parsed.hostname == 'release-assets.githubusercontent.com'
                and parsed.username is None and parsed.password is None and parsed.port in (None, 443),
                'Unexpected release metadata redirect')
        return super().redirect_request(request, fp, code, msg, headers, newurl)


def fetch_bytes(url, limit):
    """Bounded metadata request only; callers never pass an executable asset."""
    require(url.startswith(API + '?') or url.startswith(DOWNLOAD), 'Unsupported metadata origin')
    request = Request(url, headers={'Accept': 'application/vnd.github+json',
                                   'X-GitHub-Api-Version': '2026-03-10',
                                   'User-Agent': 'couch-release-discovery'})
    try:
        with build_opener(MetadataRedirects()).open(request, timeout=15) as response:
            data = response.read(limit + 1)
    except URLError as error:
        raise InstallError('GitHub release metadata unavailable; check connectivity or API rate limits') from error
    require(len(data) <= limit, 'Release metadata exceeds size limit')
    return data


@dataclass(frozen=True)
class Selection:
    tag: str
    channel: str
    release_id: int
    asset_id: int
    name: str
    url: str
    size: int
    sha256: str

    def record(self):
        return {**asdict(self), 'repository': REPOSITORY, 'model': MODEL,
                'publisher_signature_verified': False, 'installation_authorized': False}


def selections(releases, channel='stable', exact=None):
    require(channel in ('stable', 'alpha'), 'Choose stable or alpha channel')
    require(exact is None or isinstance(exact, str) and VERSION.fullmatch(exact), 'Invalid exact release version')
    require(isinstance(releases, list), 'Invalid GitHub release listing')
    result = []
    seen = set()
    for release in releases:
        require(isinstance(release, dict), 'Invalid release entry')
        if release.get('draft') is not False:
            continue
        tag = release.get('tag_name')
        match = VERSION.fullmatch(tag) if isinstance(tag, str) else None
        if not match:
            continue
        alpha = match[4] is not None
        if release.get('prerelease') is not alpha or channel != ('alpha' if alpha else 'stable') or exact and tag != exact:
            continue
        name = f'couch-{tag}-{MODEL}.json'
        assets = release.get('assets')
        require(isinstance(assets, list), 'Invalid release assets')
        candidates = [asset for asset in assets if isinstance(asset, dict) and asset.get('name') == name]
        if not candidates:  # Source-only releases are not installation manifests.
            continue
        require(len(candidates) == 1 and tag not in seen, 'Ambiguous release manifest')
        asset = candidates[0]
        expected_url = DOWNLOAD + tag + '/' + name
        digest = asset.get('digest', '')
        require(type(release.get('id')) is int and release['id'] > 0
                and type(asset.get('id')) is int and asset['id'] > 0, 'Invalid release/asset identifier')
        require(asset.get('state') == 'uploaded' and asset.get('browser_download_url') == expected_url,
                'Manifest must be an uploaded version-specific repository asset')
        require(type(asset.get('size')) is int and 0 < asset['size'] <= MAX_MANIFEST, 'Invalid manifest size')
        require(isinstance(digest, str) and digest.startswith('sha256:') and SHA.fullmatch(digest[7:]),
                'Manifest asset has no usable SHA-256 metadata')
        result.append(Selection(tag, channel, release['id'], asset['id'], name, expected_url, asset['size'], digest[7:]))
        seen.add(tag)
    return sorted(result, key=lambda selected: tuple(int(part or 0) for part in VERSION.fullmatch(selected.tag).groups()), reverse=True)


def discover(channel='stable', exact=None, fetch=fetch_bytes):
    selections([], channel, exact)  # Reject invalid filters before networking.
    # Include prereleases: /latest excludes them. Stop after a bounded 1,000
    # releases; never silently claim a complete listing when pagination remains.
    releases = []
    for page in range(1, 11):
        values = json.loads(fetch(f'{API}?per_page=100&page={page}', MAX_LIST))
        require(isinstance(values, list), 'Invalid GitHub release response')
        releases.extend(values)
        if len(values) < 100:
            return selections(releases, channel, exact)
    raise InstallError('Release history exceeds discovery limit; no selection made')


def inspect_manifest(selected, fetch=fetch_bytes):
    """Explicit metadata inspection uses the pinned selection, never /latest."""
    data = fetch(selected.url, selected.size)
    require(len(data) == selected.size and hashlib.sha256(data).hexdigest() == selected.sha256,
            'Selected manifest changed or is incomplete; select again explicitly')
    value = json.loads(data)
    require(isinstance(value, dict) and value.get('schema') == 1 and value.get('model') == MODEL
            and value.get('version') == selected.tag, 'Manifest version/model does not match selection')
    require(type(value.get('installable')) is bool, 'Manifest must state installable status')
    files = value.get('files')
    require(isinstance(files, list), 'Manifest must declare file metadata')
    require(not value['installable'] or bool(files), 'An empty release cannot claim to be installable')
    seen = set()
    for entry in files:
        require(isinstance(entry, dict), 'Invalid manifest file entry')
        name = entry.get('name')
        require(isinstance(name, str) and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name)
                and name.startswith('couch-') and f'-{selected.tag}-' in name and name not in seen,
                'Manifest files require unique version-specific basenames')
        require(type(entry.get('size')) is int and 0 < entry['size'] <= 8 * 1024**3,
                'Invalid manifest file size')
        require(isinstance(entry.get('sha256'), str) and SHA.fullmatch(entry['sha256']), 'Invalid manifest file hash')
        require(entry.get('url') == DOWNLOAD + selected.tag + '/' + name, 'Invalid manifest file URL')
        seen.add(name)
    # A remotely asserted installable:true never overrides local policy.
    return value
