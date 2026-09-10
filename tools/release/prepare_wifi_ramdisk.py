#!/usr/bin/env python3
"""Build a private RAM-only WiFi bootstrap on Ollie; never access hardware."""
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

from clean_stage import require
from kernel_provenance import PIN, verify
from prepare_boot_candidates import kernel
from prepare_probe_ramdisk import LIMIT, REPO
from private_vendor import verify_bundle
from runtime_inventory import arm_static, regular, cpio_files
from audit_vendor_elf import audit
from pack import repack

FIRMWARE = {'WMT_SOC.cfg', 'WIFI_RAM_CODE_6580', 'ROMv2_lm_patch_1_0_hdr.bin',
            'ROMv2_lm_patch_1_1_hdr.bin', 'pcm_sodi.bin', 'pcm_suspend.bin', 'pcm_deepidle.bin'}

PACKAGES = ('wpa_supplicant', 'musl', 'libcrypto3', 'libssl3', 'dbus-libs', 'libnl3', 'pcsc-lite-libs')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def alpine_files(cache):
    provenance = regular(cache / 'closure.json')
    manifest = json.loads(provenance)
    require(manifest['architecture'] == 'armv7' and manifest['kind'] == 'couch-offline-package-closure',
            'Expected inventoried ARMv7 APK closure')
    files, links = {}, {}
    for package in PACKAGES:
        candidates = [name for name in manifest['files'] if re.fullmatch(
            'packages/' + re.escape(package) + r'-[0-9][^/]*\.apk', name)]
        require(len(candidates) == 1, f'Ambiguous/missing package: {package}')
        name = candidates[0]
        data = regular(cache / name)
        require(sha(data) == manifest['files'][name], 'APK cache hash mismatch')
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            for item in archive:
                name = item.name.removeprefix('./')
                if not (name == 'sbin/wpa_supplicant' or
                        re.fullmatch(r'(usr/)?lib/[^/]+\.so(?:\.[0-9]+)*', name)):
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
    # Materialize library aliases inside cpio; never follow host filesystem links.
    for name, target in links.items():
        seen = {name}
        while target in links:
            require(target not in seen, 'Cyclic APK library link')
            seen.add(target)
            target = links[target]
        require(target in files, 'Missing APK library target')
        files[name] = files[target]
    require('sbin/wpa_supplicant' in files and 'lib/ld-musl-armhf.so.1' in files,
            'Missing supplicant or ARM musl loader')
    # Verify DT_NEEDED for every selected ELF without executing target code.
    dependencies = {}
    with tempfile.TemporaryDirectory(prefix='couch-wifi-elf-') as temporary:
        path = Path(temporary) / 'elf'
        for name, data in files.items():
            require(data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\0', 'Non-ARM runtime ELF')
            path.write_bytes(data)
            result = subprocess.run(['readelf', '-d', str(path)], capture_output=True, text=True,
                                    check=True, timeout=20)
            needed = set(re.findall(r'Shared library: \[([^]]+)\]', result.stdout))
            dependencies[name] = needed
    selected, pending = set(), ['sbin/wpa_supplicant', 'lib/ld-musl-armhf.so.1']
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


def vendor_files(bundle):
    manifest = verify_bundle(bundle)
    pin = json.loads(regular(REPO / 'tools/release/ha100_official_runtime.json'))
    require(manifest.get('source_images') == pin['images'], 'Vendor source is not pinned official runtime')
    pinned_files = {record['path']: (record['size'], record['sha256']) for record in pin['files']}
    observed_files = {record['path']: (record['size'], record['sha256']) for record in manifest['files']}
    require(observed_files == pinned_files, 'Vendor file bytes differ from pinned official runtime')
    report = audit(bundle)
    require(not report['missing_required'], 'Incomplete Bionic WMT dependency closure')
    names = set(report['wmt_dependency_names'])
    selected = {}
    for record in manifest['files']:
        path = record['path']
        if (PurePosixPath(path).name in names or path == 'system/bin/linker' or
                (path.startswith('vendor/firmware/') and PurePosixPath(path).name in FIRMWARE) or path.endswith('property_contexts') or
                path == 'system/etc/ld.config.txt'):
            selected[path] = regular(bundle / path)
    require('vendor/bin/wmt_loader' in selected and 'vendor/bin/wmt_launcher' in selected,
            'Missing WMT executables')
    return selected


def ramdisk(files):
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


def prepare(template, kernel_manifest, busybox, service, vendor_bundle, apk_cache, output):
    require(not output.exists() and not output.resolve().is_relative_to(REPO), 'New private output required')
    original = regular(template)
    metadata, pin = json.loads(regular(kernel_manifest)), json.loads(regular(PIN))
    verify(original, metadata, pin)
    files, apk_hash = alpine_files(apk_cache)
    files.update(vendor_files(vendor_bundle))
    bb, binary = regular(busybox), regular(service)
    arm_static(bb); arm_static(binary)
    files.update({'bin/busybox': bb, 'bin/couch-installer-probe': binary})
    for source, target in (('init', 'init'), ('wifi-init', 'bin/couch-wifi-init'), ('dhcp', 'bin/couch-dhcp')):
        files[target] = regular(REPO / 'tools/installer/wifi-stage' / source)
    raw = ramdisk(files)
    cpio_files(raw)
    compressed = gzip.compress(raw, mtime=0)
    estimated = 2048 + ((len(kernel(original)) + 2047) // 2048) * 2048 + ((len(compressed) + 2047) // 2048) * 2048
    require(estimated <= LIMIT, f'WiFi image exceeds 16 MiB: kernel={len(kernel(original))}, ramdisk={len(compressed)}, raw={len(raw)}; do not truncate')
    image, hashes = repack(original, kernel(original), compressed)
    require(len(image) <= LIMIT, f'WiFi boot image exceeds 16 MiB: {len(image)} bytes; do not truncate')
    unpacked_size = len(image)
    image = image.ljust(LIMIT, b'\0')
    verify(image, metadata, pin)
    page = struct.unpack_from('<I', image, 36)[0]
    kernel_size = struct.unpack_from('<I', image, 8)[0]
    ramdisk_size = struct.unpack_from('<I', image, 16)[0]
    start = page + ((kernel_size + page - 1) // page) * page
    require(gzip.decompress(image[start:start + ramdisk_size]) == raw, 'Packaged WiFi ramdisk mismatch')
    result = {'schema': 1, 'kind': 'private-ram-wifi-stage', 'private_only': True,
              'installable': False, 'redistribution_authorized': False, 'physical_boot_verified': False,
              'wifi_verified': False, 'file': 'wifi-stage.img', 'size': len(image),
              'used_boot_bytes': unpacked_size, 'ramdisk_raw_bytes': len(raw),
              'ramdisk_compressed_bytes': len(compressed), 'sha256': sha(image),
              'payload_sha256': hashes, 'apk_inventory_sha256': apk_hash,
              'files': {name: {'size': len(data), 'sha256': sha(data)} for name, data in sorted(files.items())},
              'credentials': 'USB provisioned into RAM only; not included',
              'storage_operations': ['read-only recovery SHA-256'],
              'pending': ['Physical WiFi association and TLS benchmark', 'Calibration/identity review',
                          'Separate installer transaction and write-service review']}
    output.mkdir(parents=True, mode=0o700)
    (output / 'wifi-stage.img').write_bytes(image)
    (output / 'wifi-stage.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('template', 'kernel-manifest', 'busybox', 'service', 'vendor-bundle', 'apk-cache', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    result = prepare(**vars(args))
    print(f"Private WiFi stage: {result['used_boot_bytes']}/{LIMIT} boot bytes; not hardware validated")
