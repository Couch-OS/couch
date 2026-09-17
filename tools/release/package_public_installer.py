#!/usr/bin/env python3
"""Package attested neutral inputs; never discover or include owner-local files."""
import argparse
from contextlib import ExitStack
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import tarfile

from installer_pins import installer_path

ROOT = Path(__file__).resolve().parents[2]
FILES = ('userdata.ext4', 'installer.cpio.gz', 'boot.cpio.gz', 'recovery.cpio.gz', 'zImage', 'logo.bgra')
MAX_TOTAL = 2 * 1024**3
MAX_ARCHIVE = 1024**3  # Also accepted by the immutable launcher generator.


def require(value, message):
    if not value:
        raise ValueError(message)


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode() + b'\n'


def hex_value(value, size):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(size) + '}', value)


def pin(value):
    require(isinstance(value, dict) and set(value) == {'size', 'sha256'}
            and type(value['size']) is int and 0 < value['size'] <= MAX_TOTAL
            and hex_value(value['sha256'], 64), 'Invalid public file pin')
    return value


def open_regular(path, stack, limit):
    require(stat.S_ISREG(path.lstat().st_mode), 'Public input must be a regular non-symlink file')
    fd = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0) | getattr(os, 'O_BINARY', 0))
    source = stack.enter_context(os.fdopen(fd, 'rb'))
    meta = os.fstat(source.fileno())
    require(stat.S_ISREG(meta.st_mode) and 0 < meta.st_size <= limit, 'Invalid public input size/type')
    return source, meta.st_size


def read_json(path, stack):
    source, size = open_regular(path, stack, 1024**2)
    data = source.read(size + 1)
    require(len(data) == size, 'Builder receipt changed while reading')
    return json.loads(data), hashlib.sha256(data).hexdigest()


class CheckedReader:
    def __init__(self, source, expected):
        self.source, self.expected = source, expected
        self.count, self.hash = 0, hashlib.sha256()

    def read(self, size=-1):
        require(size >= 0, 'Unbounded public file read')
        data = self.source.read(size)
        self.count += len(data)
        require(self.count <= self.expected['size'], 'Public input grew during packaging')
        self.hash.update(data)
        return data

    def finish(self):
        require(self.count == self.expected['size'] and not self.source.read(1)
                and self.hash.hexdigest() == self.expected['sha256'], 'Public input checksum changed')


def verify_input(source, expected):
    reader = CheckedReader(source, expected)
    while reader.read(1024**2):
        pass
    reader.finish()
    source.seek(0)


def verify_source_commit(source, expected):
    # git archive records the committed tree ID in a global PAX comment. Read
    # only bounded metadata here; the whole compressed file was already hashed.
    # This distinguishes the exact Couch build input from a corresponding-source
    # collection, whose commit is stored in SOURCE-MANIFEST.json instead.
    try:
        with gzip.GzipFile(fileobj=source, mode='rb') as compressed:
            header = tarfile.TarInfo.frombuf(compressed.read(512), 'utf-8', 'strict')
            # Inspect the fixed header before reading any declared PAX payload.
            # This SHA-1 Git repository emits one 52-byte global comment record.
            require(header.type == tarfile.XGLTYPE and header.size == 52,
                    'Expected bounded Git archive global commit metadata')
            metadata = compressed.read(header.size)
            require(metadata == f'52 comment={expected}\n'.encode('ascii'),
                    'Source archive Git commit differs from build attestation')

    except (OSError, EOFError, tarfile.TarError) as error:
        raise ValueError('Expected a gzip Git archive with bounded commit metadata') from error
    finally:
        source.seek(0)


def durable_write(path, data):
    with path.open('xb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


class BoundedWriter:
    def __init__(self, target):
        self.target, self.count = target, 0

    def write(self, data):
        self.count += len(data)
        require(self.count <= MAX_ARCHIVE, 'Compressed public payload exceeds launcher bound')
        return self.target.write(data)

    def flush(self):
        self.target.flush()


def prepare(attestation, source_archive, userdata, ramdisk, boot, logo, output, version):
    from fresh_os import validate_binding
    require(isinstance(version, str) and len(version) <= 128 and re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?', version), 'Invalid release version')
    require(not output.exists() and not output.is_symlink(), 'Public output must be new')
    with ExitStack() as stack:
        build, build_hash = read_json(attestation, stack)
        require(set(build) == {'schema', 'kind', 'source_commit', 'source_archive_sha256',
                               'complete', 'private_inputs', 'files', 'builder_receipts'}
                and type(build['schema']) is int and build['schema'] == 1 and build['kind'] == 'couch-public-os-build'
                and build['complete'] is True and build['private_inputs'] is False
                and hex_value(build['source_commit'], 40) and hex_value(build['source_archive_sha256'], 64),
                'Expected exact-source public build attestation')
        source, source_size = open_regular(source_archive, stack, MAX_TOTAL)
        verify_input(source, {'size': source_size, 'sha256': build['source_archive_sha256']})
        verify_source_commit(source, build['source_commit'])
        require(set(build['files']) == set(FILES), 'Attestation must contain exactly six public files')
        require(set(build['builder_receipts']) == {'userdata', 'ramdisk', 'boot', 'logo'},
                'Missing or extra builder receipt pins')
        locations = {'userdata': (userdata, 'image.json'), 'ramdisk': (ramdisk, 'ramdisk.json'),
                     'boot': (boot, 'boot.json'), 'logo': (logo, 'logo.json')}
        receipts = {}
        for kind, (directory, name) in locations.items():
            receipt, digest = read_json(directory / name, stack)
            require(build['builder_receipts'][kind] == {'sha256': digest}, 'Builder receipt differs from attestation')
            require(type(receipt.get('schema')) is int and receipt.get('schema') == 1 and receipt.get('private_only') is not True,
                    'Private or unsupported builder receipt')
            receipts[kind] = receipt
        u, r, b, l = (receipts[kind] for kind in ('userdata', 'ramdisk', 'boot', 'logo'))
        validate_binding(u.get('fresh_core'), build['source_commit'], u.get('rootfs_archive_sha256'))
        official = json.loads(installer_path('pins', 'ha100_official_runtime.json', root=ROOT).read_text())
        kernel = json.loads((ROOT / 'kernel/release-pin.json').read_text())
        require(u['kind'] == 'couch-owner-neutral-userdata' and u.get('installable') is False
                and u.get('private_only') is False and u['image']['path'] == 'userdata.ext4'
                and u['image']['format'] == 'raw-ext4' and u['image']['size'] < u['geometry']['size']
                and u['owner_vendor_source_sha256'] == official['sha256'], 'Expected compact vendor-free userdata receipt')
        require(r['kind'] == 'couch-owner-neutral-ramdisk' and r.get('bootable') is False
                and r['file'] == 'installer.cpio.gz' and r['owner_vendor_source_sha256'] == official['sha256'],
                'Expected neutral RAM receipt')
        require(b['kind'] == 'couch-owner-neutral-boot-payloads' and b.get('bootable') is False
                and b['source_kernel_commit'] == kernel['source_commit']
                and set(b['files']) == {'zImage', 'boot.cpio.gz', 'recovery.cpio.gz'}
                and b['files']['zImage']['sha256'] == kernel['zimage_sha256'], 'Expected source-built neutral boot receipt')
        require(l['kind'] == 'couch-public-logo-frame' and l['file'] == 'logo.bgra'
                and l['size'] == 480 * 800 * 4
                and l['source_sha256'] == hashlib.sha256((ROOT / 'assets/boot/couch.png').read_bytes()).hexdigest(),
                'Expected Couch-authored logo frame receipt')
        expected = {'userdata.ext4': {key: u['image'][key] for key in ('size', 'sha256')},
                    'installer.cpio.gz': {key: r[key] for key in ('size', 'sha256')},
                    **b['files'], 'logo.bgra': {key: l[key] for key in ('size', 'sha256')}}
        sources, total = {}, 0
        for name in FILES:
            item = pin(expected[name])
            require(pin(build['files'][name]) == item, 'Mixed build artifact inventory')
            directory = userdata if name == 'userdata.ext4' else ramdisk if name == 'installer.cpio.gz' else logo if name == 'logo.bgra' else boot
            source, size = open_regular(directory / name, stack, MAX_TOTAL)
            require(size == item['size'], 'Public file size differs from builder receipt')
            verify_input(source, item)
            sources[name] = source
            total += size
        require(total < MAX_TOTAL - 65536, 'Public payload exceeds native admission bound')
        manifest = encoded({'schema': 1, 'kind': 'couch-public-os-inputs', 'version': version,
                            'source_commit': build['source_commit'], 'files': expected})
        require(len(manifest) <= 65536, 'Public manifest exceeds native bound')
        output.mkdir(parents=True, mode=0o700)
        filename = f'couch-{version}-ha100-public-inputs.tar.gz'
        partial = output / (filename + '.partial')
        target = partial.open('xb')  # Acquire before finally: never unlink another process's file.
        try:
            with target:
                with gzip.GzipFile(fileobj=BoundedWriter(target), mode='wb', filename='', mtime=0) as compressed:
                    with tarfile.open(fileobj=compressed, mode='w|', format=tarfile.USTAR_FORMAT) as archive:
                        for name in sorted([*FILES, 'manifest.json']):
                            item = tarfile.TarInfo(name)
                            item.mode, item.uid, item.gid, item.mtime = 0o644, 0, 0, 0
                            item.size = len(manifest) if name == 'manifest.json' else expected[name]['size']
                            reader = io.BytesIO(manifest) if name == 'manifest.json' else CheckedReader(sources[name], expected[name])
                            archive.addfile(item, reader)
                            if name != 'manifest.json':
                                reader.finish()
                target.flush()
                os.fsync(target.fileno())
            # Publish exclusively even if another process inserts a destination.
            os.link(partial, output / filename)
        finally:
            partial.unlink(missing_ok=True)
        asset = output / filename
        with asset.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        descriptor = {'schema': 1, 'kind': 'couch-native-installer-release', 'model': 'sanytron-ha100',
                      'version': version, 'source_commit': build['source_commit'],
                      'payload': {'url': f'https://github.com/dangerouslaser/couch/releases/download/{version}/{filename}',
                                  'size': asset.stat().st_size, 'sha256': digest, 'format': 'tar.gz'}}
        durable_write(output / 'installer.json', encoded(descriptor))
        receipt = {'schema': 1, 'kind': 'couch-public-installer-package', 'source_commit': build['source_commit'],
                   'source_archive_sha256': build['source_archive_sha256'], 'source_kernel_commit': b['source_kernel_commit'],
                   'build_attestation_sha256': build_hash, 'builder_receipts': build['builder_receipts'],
                   'fresh_core': u['fresh_core'],
                   'files': expected, 'payload': descriptor['payload'],
                   'installer_json_sha256': hashlib.sha256(encoded(descriptor)).hexdigest(), 'published': False}
        durable_write(output / 'package.json', encoded(receipt))
        return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('attestation', 'source-archive', 'userdata', 'ramdisk', 'boot', 'logo', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    result = prepare(args.attestation, args.source_archive, args.userdata, args.ramdisk, args.boot, args.logo, args.output, args.version)
    print(json.dumps({'payload': result['payload'], 'published': False}))
