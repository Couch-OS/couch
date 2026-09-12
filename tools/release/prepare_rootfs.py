#!/usr/bin/env python3
"""Install a verified offline APK closure into clean staging, never a device."""
import argparse
import gzip
import io
import json
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import tarfile
import tempfile

from clean_stage import (GENERATED, StageError, archive_name, build, checksum,
                         require, secret_path)
from package_closure import verify
from os_baseline import seed as seed_os_baseline


def normalize(data, epoch, private_files=None):
    """Validate without host extraction and normalize order/times, retaining IDs."""
    private_files = private_files or {}
    if private_files:
        from private_vendor import ALLOWED
        require(set(private_files) == {'opt/couch/' + p for p in ALLOWED}, 'Incomplete private vendor allowlist')
        require(all(re.fullmatch('[0-9a-f]{64}', h) for h in private_files.values()), 'Invalid private vendor digest')
    private_dirs = {str(parent) for name in private_files for parent in PurePosixPath(name).parents}
    entries = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            name = archive_name(member.name)
            if name == '.':
                continue
            require(name not in entries, 'Duplicate assembled path')
            require(not secret_path(name) or name in private_files or (member.isdir() and name in private_dirs), f'Private runtime state in assembled rootfs: {name}')
            require(member.isdir() or member.isreg() or member.issym() or member.islnk(),
                    'Special filesystem entry after package installation')
            require(not member.mode & 0o6000, 'Set-ID package file requires separate review')
            require(not name.startswith('dev/'), 'Runtime /dev entry in assembled rootfs')
            if member.issym() or member.islnk():
                require('\\' not in member.linkname, 'Invalid assembled link')
                target = posixpath.normpath(posixpath.join(
                    posixpath.dirname(name) if member.issym() else '', member.linkname))
                require(target != '..' and not target.startswith('../'), 'Assembled link escape')
            content = archive.extractfile(member).read() if member.isreg() else b''
            if name in private_files:
                require(member.isreg() and checksum(content) == private_files[name], 'Private vendor member mismatch')
            if name == 'etc/shadow':
                require(all(len(line.split(b':')) >= 2 and line.split(b':')[1] in (b'!', b'*', b'!!')
                            for line in content.splitlines() if line), 'Assembled password credentials')
            entries[name] = (member, content)
    require(set(private_files) <= set(entries), 'Missing private vendor member')
    for name, content in GENERATED.items():
        require(name in entries and entries[name][0].isreg() and entries[name][1] == content,
                f'Package installation changed clean defaults: {name}')
    for name, (member, _) in entries.items():
        for parent in PurePosixPath(name).parents:
            if str(parent) in entries:
                require(entries[str(parent)][0].isdir(), 'Assembled path traverses a link')
        if member.islnk():
            target = archive_name(member.linkname)
            require(target in entries and entries[target][0].isreg(), 'Invalid assembled hardlink')
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode='wb', filename='', mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, (member, content) in sorted(entries.items()):
                item = tarfile.TarInfo(name)
                item.type = tarfile.REGTYPE if member.isreg() else member.type
                item.mode = member.mode & 0o777
                item.uid, item.gid = member.uid, member.gid
                item.linkname, item.mtime = member.linkname, epoch
                item.size = len(content) if member.isreg() else 0
                archive.addfile(item, io.BytesIO(content) if item.size else None)
    return output.getvalue(), len(entries)


def prepare(spec, closure, output):
    require(not output.exists(), 'Output directory must be new')
    closure = closure.resolve()
    manifest = verify(closure)
    require(manifest['architecture'] == 'armv7', 'Wrong package architecture')
    require(spec['alpine']['version'].startswith(manifest['branch'][1:] + '.'),
            'Alpine base and package branch differ')
    image = manifest['builder_image']
    require(re.fullmatch(r'alpine@sha256:[0-9a-f]{64}', image), 'Unpinned builder image')
    staged, staging = build(spec)
    output.mkdir()
    helper = Path(__file__).with_name('assemble_packages.sh').resolve()
    with tempfile.TemporaryDirectory(prefix='couch-rootfs-input-') as temp:
        inputs = Path(temp)
        (inputs / 'rootfs-staging.tar.gz').write_bytes(staged)
        # Verify package bytes again inside the isolated assembler before use.
        (inputs / 'package-hashes.txt').write_text(''.join(
            f"{package['sha256']}  packages/{package['filename']}\n"
            for package in manifest['packages']))
        subprocess.run(['docker', 'run', '--rm', '--platform=linux/amd64', '--network=none',
                        '--read-only', '--cap-drop=ALL', '--cap-add=SYS_CHROOT',
                        '--cap-add=CHOWN', '--cap-add=FOWNER', '--cap-add=DAC_OVERRIDE',
                        '--security-opt=no-new-privileges',
                        '--tmpfs', '/tmp:rw,exec,nosuid,nodev,size=512m',
                        '--mount', f'type=bind,src={inputs},dst=/input,readonly',
                        '--mount', f'type=bind,src={closure},dst=/packages,readonly',
                        '--mount', f'type=bind,src={helper},dst=/assemble.sh,readonly',
                        '--mount', f'type=bind,src={output.resolve()},dst=/out',
                        image, 'sh', '/assemble.sh'], check=True)
    raw = output / 'package-rootfs.raw.tar.gz'
    seeded, baseline = seed_os_baseline(raw.read_bytes(), checksum((closure / 'closure.json').read_bytes()))
    archive, count = normalize(seeded, spec['source_date_epoch'])
    result = {**staging, 'kind': 'couch-packaged-staging', 'installable': False,
              'os_baseline': baseline,
              'package_closure_sha256': checksum((closure / 'closure.json').read_bytes()),
              'packages': manifest['packages'], 'builder_image': image,
              'package_scripts': 'executed under existing ARM binfmt in isolated container',
              'archive_sha256': checksum(archive), 'entries': count,
              'pending': ['reviewed per-model vendor inputs', 'raw partition images and observed layout',
                          'signed release manifest', 'hardware recovery and boot validation']}
    (output / 'rootfs-staging.tar.gz').write_bytes(archive)
    (output / 'staging.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    raw.unlink()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('spec', type=Path)
    parser.add_argument('closure', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    result = prepare(json.loads(args.spec.read_text()), args.closure, args.output)
    print(f"Prepared noninstallable packaged rootfs: {result['entries']} entries")


if __name__ == '__main__':
    main()
