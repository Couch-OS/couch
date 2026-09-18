#!/usr/bin/env python3
"""Prepare, retain or verify a pinned Alpine package inventory; never install it."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
from urllib.parse import urlsplit

# Runtime Wi-Fi, enrollment-gated SSH, clock zones and optional recovery portal.
DEFAULT_PACKAGES = ('wpa_supplicant', 'openssh', 'iw', 'tzdata', 'hostapd', 'dnsmasq')
IMAGE = 'alpine@sha256:48b0309ca019d89d40f670aa1bc06e426dc0931948452e8491e3d65087abc07d'
PREFIX = 'https://dl-cdn.alpinelinux.org/alpine/v3.21/'
# Bounds for a retained archive received from a release asset, applied before
# any of its bytes reach the filesystem. The reviewed closure is about 47 MiB.
ARCHIVE_LIMIT = 512 * 1024 * 1024
MEMBER_LIMIT = 128 * 1024 * 1024
MEMBER_COUNT = 4096
MEMBER_PART = re.compile(r'[A-Za-z0-9+_.@~-]+')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def files(root):
    result = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('Symlinks are not package inputs')
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError('Nonregular package input')
        name = path.relative_to(root).as_posix()
        if name != 'closure.json':
            result[name] = digest(path)
    return result


def inventory(root, requested, image, architecture="armv7"):
    if architecture not in ("armv7", "x86_64"):
        raise ValueError("Unsupported package architecture")
    urls = {}
    for url in (root / 'package-urls.txt').read_text().splitlines():
        parsed = urlsplit(url)
        if not url.startswith(PREFIX) or parsed.query or parsed.fragment:
            raise ValueError('Unexpected package source URL')
        relative = url[len(PREFIX):]
        if not re.fullmatch(rf'(main|community)/{architecture}/[A-Za-z0-9+_.-]+\.apk', relative):
            raise ValueError('Unexpected package source path')
        name = relative.rsplit('/', 1)[1]
        if name in urls and urls[name] != url:
            raise ValueError('Ambiguous package source')
        urls[name] = url
    packages = {p.name for p in (root / 'packages').iterdir()}
    if not packages or packages != set(urls):
        raise ValueError('Downloaded package set differs from resolved source URLs')
    hashes = files(root)
    if not any(name.startswith('indexes/') for name in hashes):
        raise ValueError('Missing signed repository indexes')
    return {'schema': 1, 'kind': 'couch-offline-package-closure', 'installable': False,
            'architecture': architecture, 'branch': 'v3.21', 'builder_image': image,
            'requested': list(requested), 'files': hashes,
            'packages': [{'filename': name, 'url': urls[name],
                          'sha256': hashes['packages/' + name]} for name in sorted(packages)],
            'validation': ['apk signature verification', 'offline dependency simulation'],
            'limitations': ['not a rootfs installation or boot test',
                            'retain this cache: upstream package versions can disappear']}


def verify(root):
    manifest = json.loads((root / 'closure.json').read_text())
    if manifest.get('schema') != 1 or manifest.get('kind') != 'couch-offline-package-closure':
        raise ValueError('Unsupported closure manifest')
    if files(root) != manifest['files']:
        raise ValueError('Closure missing, changed, or unexpected files')
    expected = inventory(root, manifest['requested'], manifest['builder_image'], manifest['architecture'])
    if manifest != expected:
        raise ValueError('Closure inventory metadata mismatch')
    return manifest


def member_name(name):
    """A relative closure path: no root, no traversal, no link or device tricks."""
    parts = name.split('/')
    return (0 < len(name) <= 200 and '\\' not in name
            and all(part not in ('.', '..') and MEMBER_PART.fullmatch(part) for part in parts))


def archive(root, output):
    """Retain a verified closure directory as byte-deterministic archive bytes.

    The archive, not the version list in `requested`, is the reproducible
    release input. Re-resolving a closure against the live Alpine mirror
    returns whatever it serves that day, and superseded package revisions are
    deleted upstream, so the missing bytes cannot be fetched back by any pin.

    Entries are the verified manifest file set, sorted, with normalized mode,
    ownership and timestamps, so the same closure always archives identically.
    """
    manifest = verify(root)
    raw = output.open('xb')  # Never overwrite an earlier retained archive.
    try:
        with raw:
            with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0, compresslevel=9) as compressed:
                with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as bundle:
                    for name in sorted({*manifest['files'], 'closure.json'}):
                        path = root / name
                        item = tarfile.TarInfo(name)
                        item.mode, item.uid, item.gid, item.mtime = 0o644, 0, 0, 0
                        item.uname = item.gname = ''
                        item.size = path.stat().st_size
                        with path.open('rb') as source:
                            bundle.addfile(item, source)
    except BaseException:
        output.unlink(missing_ok=True)  # Never leave a half-written archive behind.
        raise
    return manifest


def restore(source, output):
    """Extract a retained closure archive, then verify it as a prepared directory.

    `closure.json` already hashes every other entry, so the extracted tree is
    self-verifying: this deliberately ends in the same `verify` a prepared
    directory takes rather than trusting the archive through a second path.
    """
    if not source.is_file() or source.is_symlink():
        raise ValueError('Retained closure archive must be a regular file')
    if source.stat().st_size > ARCHIVE_LIMIT:
        raise ValueError('Retained closure archive is too large')
    if output.exists() or output.is_symlink():
        raise ValueError('Restored closure directory must be new')
    output.mkdir(parents=True)
    seen, total = set(), 0
    with tarfile.open(source, mode='r:gz') as bundle:
        for count, member in enumerate(bundle, start=1):
            if count > MEMBER_COUNT:
                raise ValueError('Retained closure archive holds too many entries')
            if not member_name(member.name) or member.name in seen:
                raise ValueError('Unexpected retained closure archive entry')
            if not member.isreg():
                raise ValueError('Retained closure archive entries must be regular files')
            total += member.size
            if member.size > MEMBER_LIMIT or total > ARCHIVE_LIMIT:
                raise ValueError('Retained closure archive is too large')
            seen.add(member.name)
            path = output / member.name
            path.parent.mkdir(parents=True, exist_ok=True)
            with bundle.extractfile(member) as stream, path.open('xb') as target:
                shutil.copyfileobj(stream, target)
            path.chmod(0o644)
    return verify(output)


def prepare(output, image, requested, architecture="armv7"):
    if architecture not in ("armv7", "x86_64"):
        raise ValueError("Unsupported package architecture")
    if not re.fullmatch(r'alpine@sha256:[0-9a-f]{64}', image):
        raise ValueError('Builder must be an explicit Alpine image digest')
    if not requested or any(not re.fullmatch(r'[a-z0-9][a-z0-9+_.-]*(=[a-zA-Z0-9+_.~-]+)?', p) for p in requested):
        raise ValueError('Use package names or exact name=version constraints')
    output.mkdir()  # Never overwrite an earlier cache.
    helper = Path(__file__).with_name('prepare_packages.sh').resolve()
    # Read-only builder, no privileges, and only the new output directory is
    # writable on the host. Never mount a home directory or Docker socket.
    command = ['docker', 'run', '--rm', '--platform=linux/amd64', '--read-only', '--cap-drop=ALL',
               '--security-opt=no-new-privileges', '--env', f'APK_ARCH={architecture}', '--user', f'{os.getuid()}:{os.getgid()}',
               '--tmpfs', '/tmp:rw,nosuid,nodev,size=128m',
               '--mount', f'type=bind,src={output.resolve()},dst=/out',
               '--mount', f'type=bind,src={helper},dst=/prepare.sh,readonly',
               '--mount', f'type=bind,src={helper.with_name("check_packages.sh")},dst=/check.sh,readonly',
               image, 'sh', '/prepare.sh', *requested]
    subprocess.run(command, check=True)
    manifest = inventory(output, requested, image, architecture)
    (output / 'closure.json').write_text(json.dumps(manifest, sort_keys=True, indent=2) + '\n')
    verify(output)
    return manifest


def authenticate(root, manifest):
    image = manifest['builder_image']
    if not re.fullmatch(r'alpine@sha256:[0-9a-f]{64}', image):
        raise ValueError('Unpinned builder image')
    architecture = manifest['architecture']
    if architecture not in ('armv7', 'x86_64'):
        raise ValueError('Unsupported package architecture')
    helper = Path(__file__).with_name('check_packages.sh').resolve()
    subprocess.run(['docker', 'run', '--rm', '--platform=linux/amd64', '--network=none',
                    '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges',
                    '--env', f'APK_ARCH={architecture}', '--user', f'{os.getuid()}:{os.getgid()}',
                    '--tmpfs', '/tmp:rw,nosuid,nodev,size=128m',
                    '--mount', f'type=bind,src={root.resolve()},dst=/out,readonly',
                    '--mount', f'type=bind,src={helper},dst=/check.sh,readonly',
                    image, 'sh', '/check.sh'], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('prepare', 'verify', 'archive', 'restore'))
    parser.add_argument('directory', type=Path, help='Prepared closure directory; "restore" writes a new one')
    parser.add_argument('--image', default=IMAGE)
    parser.add_argument('--architecture', choices=('armv7', 'x86_64'), default='armv7')
    parser.add_argument('--authenticate', action='store_true', help='Repeat signature/closure checks with Docker networking disabled')
    parser.add_argument('--package', action='append', help='Explicit replacement root set; optional name=version pin')
    parser.add_argument('--archive', type=Path, help='Retained closure archive: written by "archive", read by "restore"')
    args = parser.parse_args()
    if args.operation in ('archive', 'restore') and args.archive is None:
        parser.error('--archive names the retained closure archive')
    if args.operation == 'prepare':
        manifest = prepare(args.directory, args.image, args.package or DEFAULT_PACKAGES, args.architecture)
    elif args.operation == 'archive':
        manifest = archive(args.directory, args.archive)
    elif args.operation == 'restore':
        manifest = restore(args.archive, args.directory)
    else:
        manifest = verify(args.directory)
    if args.authenticate:
        authenticate(args.directory, manifest)
    print(f"Verified inventory: {len(manifest['packages'])} {manifest['architecture']} packages; not installable.")


if __name__ == '__main__':
    main()
