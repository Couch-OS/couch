#!/usr/bin/env python3
"""Build a private RAM-only WiFi bootstrap; never access hardware."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import struct
import subprocess
import sys
import tarfile
import tempfile

from clean_stage import require
from kernel_provenance import PIN, verify
from prepare_boot_candidates import kernel
from prepare_probe_ramdisk import LIMIT, REPO
from installer_pins import installer_path, load_neutral_ramdisk
from installer_pins import load as load_installer_pin
neutral_ramdisk = load_neutral_ramdisk()
cpio_files = neutral_ramdisk.cpio_files

verify_bundle = load_installer_pin('private_vendor').verify_bundle
from runtime_inventory import regular
from audit_vendor_elf import audit
from pack import repack

FIRMWARE = {'WMT_SOC.cfg', 'WIFI_RAM_CODE_6580', 'ROMv2_lm_patch_1_0_hdr.bin',
            'ROMv2_lm_patch_1_1_hdr.bin', 'pcm_sodi.bin', 'pcm_suspend.bin', 'pcm_deepidle.bin'}

PACKAGES = ('wpa_supplicant', 'musl', 'libcrypto3', 'libssl3', 'dbus-libs', 'libnl3', 'pcsc-lite-libs')


sha = neutral_ramdisk.sha
alpine_files = neutral_ramdisk.alpine_files


def vendor_files(bundle):
    manifest = verify_bundle(bundle)
    pin = json.loads(regular(installer_path('pins', 'ha100_official_runtime.json')))
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
    require(set(selected) == set(json.loads(regular(installer_path('pins', 'ha100_ram_runtime.json')))),
            "Audited WMT closure differs from native compiled RAM subset")
    require('vendor/bin/wmt_loader' in selected and 'vendor/bin/wmt_launcher' in selected,
            'Missing WMT executables')
    return selected


ramdisk = neutral_ramdisk.ramdisk
neutral_files = neutral_ramdisk.neutral_files


def prepare(template, kernel_manifest, busybox, service, vendor_bundle, apk_cache, output, installer=False, debug=False, display=None, wmt_properties=None, filesystem_cache=None):
    require(not output.exists() and not output.resolve().is_relative_to(REPO), 'New private output required')
    original = regular(template)
    metadata, pin = json.loads(regular(kernel_manifest)), json.loads(regular(PIN))
    verify(original, metadata, pin)
    files, apk_hash, fs_hash = neutral_files(busybox, service, apk_cache, installer, debug, display, wmt_properties, filesystem_cache)
    files.update(vendor_files(vendor_bundle))
    raw = ramdisk(files, include_recovery=not debug)
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
    result = {'schema': 1, 'kind': ('private-ram-wifi-installer' if installer else
                                    'private-ram-wifi-debug-stage' if debug else
                                    'private-ram-wifi-stage'), 'private_only': True,
              'installable': False, 'redistribution_authorized': False, 'physical_boot_verified': False,
              'wifi_verified': False, 'file': 'wifi-stage.img', 'size': len(image),
              'used_boot_bytes': unpacked_size, 'ramdisk_raw_bytes': len(raw),
              'ramdisk_compressed_bytes': len(compressed), 'sha256': sha(image),
              'payload_sha256': hashes, 'apk_inventory_sha256': apk_hash,
              'filesystem_inventory_sha256': fs_hash,
              'files': {name: {'size': len(data), 'sha256': sha(data)} for name, data in sorted(files.items())},
              'credentials': ('Not accepted by the debug protocol' if debug else
                              'USB provisioned into RAM only; not included'),
              'storage_operations': (['USB-bound TLS backups', 'verified OS partition transaction'] if installer else
                                     [] if debug else ['read-only recovery SHA-256']),
              'pending': (['Physical WiFi startup/retry diagnostics', 'Calibration/identity review',
                           'Separate debug-stage transition review'] if debug else
                          ['Physical WiFi association and TLS benchmark', 'Calibration/identity review',
                           'Separate installer transaction and write-service review'])}
    output.mkdir(parents=True, mode=0o700)
    (output / 'wifi-stage.img').write_bytes(image)
    (output / 'wifi-stage.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('template', 'kernel-manifest', 'busybox', 'service', 'vendor-bundle', 'apk-cache', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--installer', action='store_true')
    parser.add_argument('--debug', action='store_true', help='Build the pre-credential Wi-Fi debug stage')
    parser.add_argument('--display', type=Path)
    parser.add_argument('--wmt-properties', type=Path)
    parser.add_argument('--filesystem-cache', type=Path)
    args = parser.parse_args()
    result = prepare(**vars(args))
    print(f"Private WiFi stage: {result['used_boot_bytes']}/{LIMIT} boot bytes; not hardware validated")
