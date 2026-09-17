#!/usr/bin/env python3
"""Prepare or verify a pinned Alpine package inventory; never install it."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from urllib.parse import urlsplit

# Runtime Wi-Fi, enrollment-gated SSH, clock zones and optional recovery portal.
DEFAULT_PACKAGES = ('wpa_supplicant', 'openssh', 'iw', 'tzdata', 'hostapd', 'dnsmasq')
IMAGE = 'alpine@sha256:48b0309ca019d89d40f670aa1bc06e426dc0931948452e8491e3d65087abc07d'
PREFIX = 'https://dl-cdn.alpinelinux.org/alpine/v3.21/'


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


def validate_requested(requested):
    if not isinstance(requested, (list, tuple)) or not requested or any(
            not isinstance(p, str) or not re.fullmatch(
                r'[a-z0-9][a-z0-9+_.-]*(=[a-zA-Z0-9+_.~-]+)?', p) for p in requested):
        raise ValueError('Use package names or exact name=version constraints')


def inventory(root, requested, image, architecture="armv7"):
    if architecture not in ("armv7", "x86_64"):
        raise ValueError("Unsupported package architecture")
    validate_requested(requested)
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
    # Independently bind concrete pinned roots to the resolved/downloaded APKs.
    # This also fails closed if a helper accidentally drops the WORLD constraints.
    for request in requested:
        if '=' in request:
            name, version = request.split('=', 1)
            resolved = {package for package in packages if package.rsplit('-', 2)[0] == name}
            if resolved != {f'{name}-{version}.apk'}:
                raise ValueError('Requested exact package version differs from closure: ' + request)
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


def prepare(output, image, requested, architecture="armv7"):
    if architecture not in ("armv7", "x86_64"):
        raise ValueError("Unsupported package architecture")
    if not re.fullmatch(r'alpine@sha256:[0-9a-f]{64}', image):
        raise ValueError('Builder must be an explicit Alpine image digest')
    validate_requested(requested)
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
    parser.add_argument('operation', choices=('prepare', 'verify'))
    parser.add_argument('directory', type=Path)
    parser.add_argument('--image', default=IMAGE)
    parser.add_argument('--architecture', choices=('armv7', 'x86_64'), default='armv7')
    parser.add_argument('--authenticate', action='store_true', help='Repeat signature/closure checks with Docker networking disabled')
    parser.add_argument('--package', action='append', help='Explicit replacement root set; optional name=version pin')
    args = parser.parse_args()
    manifest = (prepare(args.directory, args.image, args.package or DEFAULT_PACKAGES, args.architecture)
                if args.operation == 'prepare' else verify(args.directory))
    if args.authenticate:
        authenticate(args.directory, manifest)
    print(f"Verified inventory: {len(manifest['packages'])} {manifest['architecture']} packages; not installable.")


if __name__ == '__main__':
    main()
