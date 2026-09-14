#!/usr/bin/env python3
"""Prepare private full-partition boot/recovery candidates; never access devices."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

from kernel_provenance import PIN, verify
from clean_stage import require
from runtime_inventory import arm_static, cpio_files, regular
from pack import repack, split_dtb

REPO = Path(__file__).resolve().parents[2]


def kernel(image):
    require(len(image) >= 2048 and image[:8] == b'ANDROID!', 'Invalid template')
    size = struct.unpack_from('<I', image, 8)[0]
    page = struct.unpack_from('<I', image, 36)[0]
    require(page in (2048, 4096, 8192, 16384) and page + size <= len(image), 'Invalid kernel bounds')
    return split_dtb(image[page:page+size])[0]


MODULES = ('compat.ko', 'bluetooth.ko', 'hci_vhci.ko', 'hci_stp.ko')


def clean_ramdisk(root, role):
    require(role in ("boot", "recovery"), "Unknown ramdisk role")
    init = 'initramfs/init' if role == 'boot' else 'recovery/init'
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch)
        (tree / 'extra').mkdir()
        shutil.copyfile(root / init, tree / 'init')
        shutil.copyfile(root / 'build/busybox-armv7l', tree / 'busybox')
        shutil.copyfile(root / 'build/fbcon', tree / 'extra/fbcon')
        if role == 'boot':
            shutil.copyfile(root / 'initramfs/boot-health.sh', tree / 'extra/boot-health.sh')
            # The Bluetooth binaries travel with the kernel that gives them
            # /dev/vhci: runtime updaters before .142 refuse bundles with new
            # binaries, so until every remote runs a tolerant updater the boot
            # payload is the one thing only Bluetooth-capable images get, and
            # couch-system falls back to /extra when the runtime lacks them.
            for name in ('couch-bt-bridge', 'couch-bt-hid'):
                binary = root / 'clients/target/armv7-unknown-linux-musleabihf/release' / name
                arm_static(regular(binary))
                shutil.copyfile(binary, tree / 'extra' / name)
                (tree / 'extra' / name).chmod(0o755)
            # A kernel built without the in-tree Bluetooth core carries the
            # backported 4.4 core as modules; they must match this exact
            # kernel (MODVERSIONS), so they travel in the same boot payload.
            for name in MODULES:
                module = root / 'build/backports' / name
                if module.is_file():
                    shutil.copyfile(module, tree / 'extra' / name)
        subprocess.run([sys.executable, str(root / 'tools/mkcpio.py'), str(tree), str(tree / 'ramdisk.cpio')], check=True, stdout=subprocess.DEVNULL)
        raw = (tree / 'ramdisk.cpio').read_bytes()
        entries = cpio_files(raw)
        expected = {'init', 'bin/busybox', 'extra/fbcon'}
        if role == 'boot':
            expected.add('extra/boot-health.sh')
            expected.add('extra/couch-bt-bridge')
            expected.add('extra/couch-bt-hid')
            for name in MODULES:
                if (root / 'build/backports' / name).is_file():
                    expected.add('extra/' + name)
        payloads = {name for name, content in entries.items() if content}
        require(payloads == expected, 'Unexpected payload file in clean ramdisk')
        ramdisk = gzip.compress(raw, compresslevel=9, mtime=0)
    return ramdisk, payloads


def prepare(normal, recovery, manifest, output, root=REPO):
    require(not output.exists(), 'Output directory must be new')
    normal_data, recovery_data = regular(normal), regular(recovery)
    pin = json.loads(PIN.read_text())
    verify(normal_data, json.loads(regular(manifest)), pin)
    require(kernel(recovery_data) != kernel(normal_data), 'Recovery must retain independent stock kernel')
    for binary in ('build/busybox-armv7l', 'build/fbcon'):
        arm_static(regular(root / binary))
    output.mkdir(parents=True, mode=0o700)
    results = {}
    for role, template, init in [('boot', normal_data, 'initramfs/init'),
                                  ('recovery', recovery_data, 'recovery/init')]:
        ramdisk, payloads = clean_ramdisk(root, role)
        image, hashes = repack(template, kernel(template), ramdisk)
        require(len(image) <= 16 * 1024 * 1024, 'Boot partition overflow')
        full = image.ljust(16 * 1024 * 1024, b'\0')
        (output / (role + '.img')).write_bytes(full)
        if role == 'boot':
            verify(full, json.loads(regular(manifest)), pin)
        results[role] = {'file': role + '.img', 'size': len(full),
                         'sha256': hashlib.sha256(full).hexdigest(),
                         'payload_sha256': hashes, 'ramdisk_payload_files': sorted(payloads)}
    result = {'schema': 1, 'kind': 'couch-private-boot-candidates', 'private_only': True,
              'installable': False, 'redistribution_authorized': False,
              'kernel_commit': pin['source_commit'], 'images': results,
              'pending': ['Physical clean-ramdisk boot and recovery validation',
                          'BusyBox and stock recovery corresponding-source provenance',
                          'Release signing and vendor redistribution review']}
    (output / 'boot-candidates.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--normal-template', type=Path, required=True)
    parser.add_argument('--stock-recovery-template', type=Path, required=True)
    parser.add_argument('--kernel-manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.normal_template, args.stock_recovery_template, args.kernel_manifest, args.output)
    print(json.dumps(result, indent=2))
