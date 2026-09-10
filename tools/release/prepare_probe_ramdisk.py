#!/usr/bin/env python3
"""Package a private RAM-only probe on Ollie; no device or USB operations."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import struct

from clean_stage import require
from kernel_provenance import PIN, verify
from prepare_boot_candidates import kernel
from runtime_inventory import arm_static, cpio_files, regular
from pack import repack

REPO = Path(__file__).resolve().parents[2]
LIMIT = 16 * 1024 * 1024


def ramdisk(init, busybox, service):
    """Exact newc inventory: no normal-init extras or writable storage nodes."""
    output = bytearray()
    inode = 0

    def entry(name, mode, content=b'', major=0, minor=0):
        nonlocal inode
        inode += 1
        filename = name.encode() + b'\0'
        fields = (inode, mode, 0, 0, 1, 0, len(content), 0, 0, major, minor, len(filename), 0)
        output.extend(('070701' + ''.join(f'{value:08x}' for value in fields)).encode())
        output.extend(filename)
        output.extend(b'\0' * (-len(output) % 4))
        output.extend(content)
        output.extend(b'\0' * (-len(output) % 4))

    for name in ('.', 'bin', 'dev', 'proc', 'sys', 'tmp'):
        entry(name, 0o040755)
    for name, content in (('init', init), ('bin/busybox', busybox), ('bin/couch-installer-probe', service)):
        entry(name, 0o100755, content)
    for name, major, minor in (('null', 1, 3), ('zero', 1, 5), ('urandom', 1, 9),
                                ('console', 5, 1)):
        entry('dev/' + name, 0o020600, major=major, minor=minor)
    # Protocol exposes only a read-only recovery hash; never the whole eMMC.
    entry('dev/mmcblk0p9', 0o060400, major=179, minor=9)
    entry('TRAILER!!!', 0)
    output.extend(b'\0' * (-len(output) % 512))
    return bytes(output)


def prepare(template, manifest, busybox, service, output, root=REPO):
    require(not output.exists(), 'Use a new private output directory')
    require(not output.resolve().is_relative_to(REPO), 'Keep private boot artifacts outside Git')
    template_data = regular(template)
    metadata = json.loads(regular(manifest))
    pin = json.loads(PIN.read_text())
    verify(template_data, metadata, pin)
    bb, probe = regular(busybox), regular(service)
    arm_static(bb)
    arm_static(probe)
    init = regular(root / 'tools/installer/probe/init')
    raw = ramdisk(init, bb, probe)
    entries = cpio_files(raw)
    require({name for name, value in entries.items() if value} ==
            {'init', 'bin/busybox', 'bin/couch-installer-probe'}, 'Unexpected probe payload')
    image, payload_hashes = repack(template_data, kernel(template_data), gzip.compress(raw, mtime=0))
    require(len(image) <= LIMIT, 'Probe image exceeds 16 MiB boot partition')
    image = image.ljust(LIMIT, b'\0')
    verify(image, metadata, pin)
    page = struct.unpack_from('<I', image, 36)[0]
    kernel_size, ramdisk_size = struct.unpack_from('<I', image, 8)[0], struct.unpack_from('<I', image, 16)[0]
    start = page + ((kernel_size + page - 1) // page) * page
    require(gzip.decompress(image[start:start+ramdisk_size]) == raw, 'Packaged ramdisk verification failed')
    result = {'schema': 1, 'kind': 'private-readonly-ram-probe', 'installable': False,
              'private_only': True, 'redistribution_authorized': False,
              'physical_boot_verified': False, 'functionfs_verified': False,
              'kernel_commit': pin['source_commit'], 'file': 'probe.img', 'size': len(image),
              'sha256': hashlib.sha256(image).hexdigest(), 'payload_sha256': payload_hashes,
              'busybox_sha256': hashlib.sha256(bb).hexdigest(),
              'probe_sha256': hashlib.sha256(probe).hexdigest(),
              'init_sha256': hashlib.sha256(init).hexdigest(),
              'storage_operations': ['read-only recovery SHA-256'],
              'pending': ['Controlled RAM boot and USB enumeration', 'Measured throughput and RAM footprint',
                          'Independent protocol review; no installer write service included']}
    output.mkdir(parents=True, mode=0o700)
    (output / 'probe.img').write_bytes(image)
    (output / 'probe.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('template', 'kernel-manifest', 'busybox', 'service', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(args.template, args.kernel_manifest, args.busybox, args.service, args.output), indent=2))
