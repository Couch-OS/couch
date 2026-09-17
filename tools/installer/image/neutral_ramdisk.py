#!/usr/bin/env python3
"""Build the public, vendor-free installer RAM payload from explicit inputs.

This source is intentionally independent of ``tools/release``. It validates a
pinned APK closure, selects its ARM runtime dependency graph without running
target code, and writes a deterministic newc/gzip payload.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import struct
import subprocess
import tarfile
import tempfile


INSTALLER = Path(__file__).resolve().parents[1]
PACKAGES = ('wpa_supplicant', 'musl', 'libcrypto3', 'libssl3', 'dbus-libs', 'libnl3', 'pcsc-lite-libs')
FILESYSTEM_PACKAGES = ('e2fsprogs', 'e2fsprogs-extra', 'e2fsprogs-libs', 'libblkid', 'libcom_err',
                       'libeconf', 'libgcc', 'libuuid', 'musl')


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def regular(path):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), f'Missing regular input: {path.name}')
    return path.read_bytes()


def arm_static(data):
    require(len(data) >= 52 and data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\x00',
            'Expected little-endian ARM32 ELF')
    offset = struct.unpack_from('<I', data, 28)[0]
    stride, count = struct.unpack_from('<HH', data, 42)
    require(count > 0 and stride >= 32 and offset + stride * count <= len(data),
            'Invalid executable program headers')
    require(all(struct.unpack_from('<I', data, offset + i * stride)[0] not in (2, 3)
                for i in range(count)), 'Runtime executable requires dynamic loader/libraries')


def archive_name(name):
    require(isinstance(name, str) and '\\' not in name and not name.startswith('/'), 'Invalid initramfs path')
    parts = PurePosixPath(name).parts
    require('..' not in parts, 'Initramfs path traversal')
    return str(PurePosixPath(name))


def private_name(name):
    parts = PurePosixPath(name).parts
    return ('.ssh' in parts or name.startswith(('home/', 'data/nvram/', 'opt/couch/vendor/', 'opt/couch/system/'))
            or name in ('etc/machine-id', 'var/lib/dbus/machine-id', 'etc/wpa_supplicant/wpa_supplicant.conf')
            or PurePosixPath(name).name in ('authorized_keys', 'networks.conf', 'settings.conf', 'props.tar.gz',
                                             '.ash_history', '.bash_history')
            or PurePosixPath(name).name.startswith('ssh_host_'))


def cpio_files(data):
    """Inspect a newc payload in memory; never extract it to the host."""
    offset, entries = 0, {}
    while True:
        require(offset + 110 <= len(data) and data[offset:offset + 6] == b'070701',
                'Unsupported or truncated initramfs')
        try:
            fields = [int(data[offset + 6 + index * 8:offset + 14 + index * 8], 16) for index in range(13)]
        except ValueError as error:
            raise ValueError('Malformed newc header') from error
        size, length = fields[6], fields[11]
        begin = offset + 110
        require(length > 0 and begin + length <= len(data) and data[begin + length - 1] == 0,
                'Invalid initramfs filename')
        name = archive_name(data[begin:begin + length - 1].decode())
        start = (begin + length + 3) & ~3
        require(start + size <= len(data), 'Truncated initramfs member')
        if name == 'TRAILER!!!':
            require(size == 0 and not any(data[start:]), 'Unexpected trailing initramfs payload')
            return entries
        require(name not in entries and not private_name(name), 'Private or duplicate initramfs member')
        require(name not in ('extra/props.tar.gz', 'opt/couch/config.json'), 'Private initramfs state')
        entries[name] = data[start:start + size]
        offset = (start + size + 3) & ~3


def alpine_files(cache, filesystem=False):
    provenance = regular(Path(cache) / 'closure.json')
    manifest = json.loads(provenance)
    require(manifest['architecture'] == 'armv7' and manifest['kind'] == 'couch-offline-package-closure',
            'Expected inventoried ARMv7 APK closure')
    files, links = {}, {}
    packages = FILESYSTEM_PACKAGES if filesystem else PACKAGES
    binaries = {'sbin/e2fsck', 'usr/sbin/resize2fs', 'usr/sbin/debugfs'} if filesystem else {'sbin/wpa_supplicant'}
    for package in packages:
        candidates = [name for name in manifest['files'] if re.fullmatch(
            'packages/' + re.escape(package) + r'-[0-9][^/]*\.apk', name)]
        require(len(candidates) == 1, f'Ambiguous/missing package: {package}')
        name = candidates[0]
        data = regular(Path(cache) / name)
        require(sha(data) == manifest['files'][name], 'APK cache hash mismatch')
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            for item in archive:
                name = item.name.removeprefix('./')
                if not (name in binaries or re.fullmatch(r'(usr/)?lib/[^/]+\.so(?:\.[0-9]+)*', name)):
                    continue
                require(name not in files and name not in links, 'Duplicate runtime path')
                if item.isfile():
                    require(0 < item.size <= 16 * 1024**2, 'Unexpected library size')
                    files[name] = archive.extractfile(item).read()
                elif item.issym():
                    target = item.linkname
                    require('/' not in target and target not in ('.', '..'), 'Unsafe APK library link')
                    links[name] = (PurePosixPath(name).parent / target).as_posix()
                else:
                    raise ValueError('Unsupported APK runtime member')
    for name, target in links.items():
        seen = {name}
        while target in links:
            require(target not in seen, 'Cyclic APK library link')
            seen.add(target)
            target = links[target]
        require(target in files, 'Missing APK library target')
        files[name] = files[target]
    require(binaries <= files.keys() and 'lib/ld-musl-armhf.so.1' in files,
            'Missing supplicant or ARM musl loader')
    dependencies = {}
    with tempfile.TemporaryDirectory(prefix='couch-wifi-elf-') as temporary:
        path = Path(temporary) / 'elf'
        for name, data in files.items():
            require(data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\0', 'Non-ARM runtime ELF')
            path.write_bytes(data)
            result = subprocess.run(['readelf', '-d', str(path)], capture_output=True, text=True,
                                    check=True, timeout=20)
            dependencies[name] = set(re.findall(r'Shared library: \[([^]]+)\]', result.stdout))
    selected, pending = set(), [*binaries, 'lib/ld-musl-armhf.so.1']
    while pending:
        name = pending.pop()
        if name in selected:
            continue
        selected.add(name)
        for needed in dependencies[name]:
            matches = [path for path in files if PurePosixPath(path).name == needed]
            require(len(matches) == 1, f'Missing/ambiguous Alpine dependency: {needed}')
            pending.extend(matches)
    return {name: files[name] for name in selected}, sha(provenance)


def ramdisk(files, include_recovery=True):
    records = {}
    def add(name, mode, data=b'', major=0, minor=0):
        require(name not in records, 'Duplicate cpio entry')
        records[name] = (mode, data, major, minor)
    directories = {'.', 'dev', 'proc', 'sys', 'tmp', 'run', 'etc', 'system/etc', 'dev/__properties__'}
    for name in files:
        directories.update(str(parent) for parent in PurePosixPath(name).parents)
    for name in sorted(directories):
        add(name, 0o040755)
    shared = {}
    for name, data in sorted(files.items()):
        digest = sha(data)
        if data.startswith(b'\x7fELF') and digest in shared:
            add(name, 0o120777, ('/' + shared[digest]).encode())
        else:
            add(name, 0o100755, data)
            shared[digest] = name
    for name, target in (('system/vendor', '/vendor'), ('system/etc/firmware', '/vendor/firmware'),
                         ('etc/firmware', '/vendor/firmware')):
        add(name, 0o120777, target.encode())
    for name, major, minor in (('null', 1, 3), ('zero', 1, 5), ('urandom', 1, 9), ('console', 5, 1)):
        add('dev/' + name, 0o020600, major=major, minor=minor)
    if include_recovery:
        add('dev/mmcblk0p9', 0o060400, major=179, minor=9)
    add('TRAILER!!!', 0)
    output = bytearray()
    for inode, (name, (mode, content, major, minor)) in enumerate(records.items(), 1):
        encoded = name.encode() + b'\0'
        fields = (inode, mode, 0, 0, 1, 0, len(content), 0, 0, major, minor, len(encoded), 0)
        output.extend(('070701' + ''.join(f'{value:08x}' for value in fields)).encode())
        output.extend(encoded); output.extend(b'\0' * (-len(output) % 4))
        output.extend(content); output.extend(b'\0' * (-len(output) % 4))
    output.extend(b'\0' * (-len(output) % 512))
    return bytes(output)


def neutral_files(busybox, service, apk_cache, installer=False, debug=False, display=None,
                  wmt_properties=None, filesystem_cache=None):
    files, apk_hash = alpine_files(apk_cache)
    fs_hash = None
    if filesystem_cache is not None:
        require(installer and not debug, 'Filesystem tools are installer-only')
        fs_files, fs_hash = alpine_files(filesystem_cache, filesystem=True)
        for name, data in fs_files.items():
            require(name not in files or files[name] == data, 'Conflicting RAM runtime libraries')
            files[name] = data
    if installer:
        require(filesystem_cache is not None, 'Installer requires offline filesystem expansion tools')
    require(not (installer and debug), 'Installer and debug stages must be separate')
    bb, binary = regular(busybox), regular(service)
    arm_static(bb); arm_static(binary)
    capability = (b'COUCH_PRIVATE_WIFI_INSTALLER_V1' if installer else
                  b'COUCH_PRIVATE_WIFI_DEBUG_STAGE_V1' if debug else b'COUCH_READONLY_RAM_PROBE_V1')
    require(capability in binary, 'Service binary capabilities differ from requested stage mode')
    files.update({'bin/busybox': bb, 'bin/couch-installer-probe': binary})
    if wmt_properties is not None:
        bridge = regular(wmt_properties)
        require(bridge[:6] == b'\x7fELF\x01\x01' and bridge[18:20] == b'\x28\0',
                'Expected ARM WMT property bridge')
        files['lib/couch-wmt-properties.so'] = bridge
    if installer:
        files['etc/couch-installer-mode'] = b'private-install\n'
    if debug:
        files['etc/couch-wifi-debug-mode'] = b'precredential-only\n'
    if display is not None:
        pixels = regular(display)
        arm_static(pixels)
        files['bin/couch-installer-display'] = pixels
    for source, target in (('init', 'init'), ('wifi-init', 'bin/couch-wifi-init'), ('dhcp', 'bin/couch-dhcp')):
        files[target] = regular(INSTALLER / 'wifi-stage' / source)
    if debug:
        files['bin/couch-wifi-debug-supervisor'] = regular(INSTALLER / 'wifi-stage' / 'debug-supervisor')
    return files, apk_hash, fs_hash


def prepare(busybox, service, apk_cache, filesystem_cache, display, wmt_properties, output):
    output = Path(output)
    require(not output.exists(), 'Public RAM output must be new')
    files, apk_hash, fs_hash = neutral_files(
        busybox, service, apk_cache, installer=True, display=display,
        wmt_properties=wmt_properties, filesystem_cache=filesystem_cache)
    pin = json.loads((INSTALLER / 'pins/ha100_official_runtime.json').read_text())
    owner_names = {record['path'] for record in pin['files']}
    owner_hashes = {record['sha256'] for record in pin['files']}
    require(not owner_names.intersection(files), 'Owner path in neutral RAM')
    require(not owner_hashes.intersection(map(sha, files.values())), 'Owner content in neutral RAM')
    raw = ramdisk(files)
    cpio_files(raw)
    data = gzip.compress(raw, mtime=0)
    require(len(raw) <= 64 * 1024**2 and len(data) <= 16 * 1024**2, 'RAM payload exceeds bounds')
    result = {'schema': 1, 'kind': 'couch-owner-neutral-ramdisk',
              'file': 'installer.cpio.gz', 'size': len(data), 'sha256': sha(data),
              'owner_vendor_source_sha256': pin['sha256'],
              'apk_inventory_sha256': apk_hash, 'filesystem_inventory_sha256': fs_hash,
              'files': {name: {'size': len(value), 'sha256': sha(value)} for name, value in sorted(files.items())},
              'bootable': False, 'physical_boot_verified': False,
              'assembly': 'Native host inserts pinned owner files and assembles an owner-local Android header/kernel/DTB image.'}
    output.mkdir(parents=True, mode=0o700)
    (output / 'installer.cpio.gz').write_bytes(data)
    (output / 'ramdisk.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('busybox', 'service', 'apk-cache', 'filesystem-cache', 'display', 'wmt-properties', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.busybox, args.service, args.apk_cache, args.filesystem_cache,
                     args.display, args.wmt_properties, args.output)
    print(f"Prepared vendor-free RAM payload: {result['size']} bytes")


if __name__ == '__main__':
    main()
