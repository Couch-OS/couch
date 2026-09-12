#!/usr/bin/env python3
"""Prepare pinned owner-local ADB dependencies; never contact USB devices."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import stat
import subprocess
import tempfile
import time
import urllib.request
import zipfile

PIN = Path(__file__).with_suffix('.json')
REPO = Path(__file__).resolve().parents[2]
MAX_EXPANDED = 128 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def host_platform():
    system, machine = platform.system(), platform.machine().lower()
    if system == 'Darwin' and machine in ('arm64', 'x86_64'):
        return 'macos-universal'
    if machine in ('x86_64', 'amd64'):
        return {'Linux': 'linux-x86_64', 'Windows': 'windows-x86_64'}[system]
    raise ValueError('No verified ADB package for this host architecture')


def hash_stream(stream, size):
    checksum, count = hashlib.sha256(), 0
    while block := stream.read(min(1024 * 1024, size + 1 - count)):
        count += len(block)
        require(count <= size, 'Archive exceeds pinned size')
        checksum.update(block)
    require(count == size, 'Archive size mismatch')
    return checksum.hexdigest()


class GoogleRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        require(newurl.startswith('https://dl.google.com/android/repository/'),
                'Unexpected dependency download redirect')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(pin, target):
    require(pin['url'].startswith('https://dl.google.com/android/repository/'),
            'Unexpected dependency source')
    deadline, count = time.monotonic() + 180, 0
    opener = urllib.request.build_opener(GoogleRedirect())
    with opener.open(pin['url'], timeout=15) as response, target.open('xb') as output:
        while True:
            require(time.monotonic() < deadline, 'Dependency download deadline exceeded')
            # HTTPResponse.read(n) may perform many socket reads while a peer
            # trickles bytes. read1 does at most one underlying read, bounded by
            # the socket timeout, so the overall deadline is checked promptly.
            block = response.read1(64 * 1024)
            require(time.monotonic() < deadline, 'Dependency download deadline exceeded')
            if not block:
                break
            count += len(block)
            require(count <= pin['size'], 'Download exceeds pinned size')
            output.write(block)
        output.flush()
        os.fsync(output.fileno())


def prepare(archive, destination, selected, metadata):
    pin = metadata['platforms'][selected]
    destination = Path(destination).absolute()
    require(not destination.exists() and not destination.is_symlink(), 'Use a new output directory')
    require(destination.parent.is_dir() and not destination.resolve().is_relative_to(REPO),
            'Output must be outside the source checkout with an existing parent')
    require(not Path(archive).is_symlink(), 'Archive must not be a symlink')
    # Hash and parse the same open file; never re-open an unverified path for ZIP extraction.
    with Path(archive).open('rb') as source:
        require(stat.S_ISREG(os.fstat(source.fileno()).st_mode), 'Archive must be a regular file')
        require(hash_stream(source, pin['size']) == pin['sha256'], 'Archive SHA-256 mismatch')
        source.seek(0)
        with zipfile.ZipFile(source) as bundle:
            members = bundle.infolist()
            require(len(members) <= 100, 'Too many archive members')
            seen, expanded = set(), 0
            for member in members:
                name = member.filename
                path = PurePosixPath(name)
                require(name == member.orig_filename and name not in seen and not path.is_absolute()
                        and '..' not in path.parts and '\\' not in name and ':' not in name
                        and name.startswith('platform-tools/'), 'Invalid or duplicate archive path')
                require(not stat.S_ISLNK(member.external_attr >> 16), 'Archive symlink rejected')
                seen.add(name)
                expanded += member.file_size
            require(expanded <= MAX_EXPANDED and pin['files'].keys() <= seen,
                    'Archive inventory mismatch or expansion limit exceeded')
            # Verify all selected bytes before creating an executable output directory.
            verified = {}
            for name, expected in pin['files'].items():
                member = bundle.getinfo(name)
                require(member.file_size == expected['size'], 'Member size mismatch')
                data = bundle.read(member)
                require(hashlib.sha256(data).hexdigest() == expected['sha256'], 'Member hash mismatch')
                verified[name] = data
            destination.mkdir(mode=0o700)
            for name, data in verified.items():
                path = destination / name
                path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
                with path.open('xb') as output:
                    os.chmod(path, 0o700 if name == pin['adb'] else 0o600)
                    output.write(data)
                    output.flush()
                    os.fsync(output.fileno())
    receipt = {'schema': 1, 'platform': selected, 'component': metadata['component'],
               'version': metadata['version'], 'files': pin['files'],
               'executables': {'adb': pin['adb']}, 'mtk_revision': metadata['mtk_revision'],
               'archive_sha256': pin['sha256'], 'complete_mtk_runtime': False}
    with (destination / 'receipt.json').open('x') as output:
        json.dump(receipt, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    return receipt


def smoke(destination, selected, metadata):
    require(selected == host_platform(), 'Cannot execute dependencies for a different host')
    pin = metadata['platforms'][selected]
    destination = Path(destination).resolve()
    for name, expected in pin['files'].items():
        path = destination / name
        require(not path.is_symlink(), 'Dependency symlink rejected')
        with path.open('rb') as source:
            require(hash_stream(source, expected['size']) == expected['sha256'], 'Dependency changed')
    # `version` does not start an ADB server, enumerate devices or issue a USB command.
    env = {key: value for key, value in os.environ.items()
           if key in ('SYSTEMROOT', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'HOME')}
    result = subprocess.run([str(destination / pin['adb']), 'version'], check=True,
                            capture_output=True, text=True, timeout=15, cwd=destination, env=env)
    require('Android Debug Bridge version 1.0.41' in result.stdout
            and '\nVersion 37.0.1-' in result.stdout, 'Unexpected ADB version output')
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=json.loads(PIN.read_text())['platforms'])
    parser.add_argument('--archive', type=Path, help='Use an already downloaded official archive')
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--smoke', action='store_true', help='Execute only adb version after verification')
    args = parser.parse_args()
    metadata = json.loads(PIN.read_text())
    selected = args.platform or host_platform()
    with tempfile.TemporaryDirectory(prefix='couch-adb-download-') as temporary:
        archive = args.archive or Path(temporary) / 'platform-tools.zip'
        if args.archive is None:
            download(metadata['platforms'][selected], archive)
        prepare(archive, args.output, selected, metadata)
    if args.smoke:
        print(smoke(args.output, selected, metadata), end='')
    print('Verified owner-local ADB dependencies; MTK runtime is not included.')


if __name__ == '__main__':
    main()
