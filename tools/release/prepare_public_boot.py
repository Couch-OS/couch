#!/usr/bin/env python3
"""Export source-built zImage and clean ramdisks without owner boot headers/DTBs."""
import argparse
import json
from pathlib import Path

from clean_stage import require
from kernel_provenance import PIN, sha, verify_manifest
from prepare_boot_candidates import REPO, clean_ramdisk, verified_busybox
from runtime_inventory import arm_static, regular


def prepare(zimage, kernel_manifest, output, root=REPO):
    require(not output.exists(), 'Public boot output must be new')
    pin = json.loads(regular(PIN))
    verify_manifest(json.loads(regular(kernel_manifest)), pin)
    kernel = regular(zimage)
    require(sha(kernel) == pin['zimage_sha256'], 'Public kernel differs from source-built pin')
    arm_static(regular(verified_busybox(root)))
    arm_static(regular(root / 'build/fbcon'))
    payloads = {'zImage': kernel}
    for role in ('boot', 'recovery'):
        payloads[role + '.cpio.gz'] = clean_ramdisk(root, role)[0]
    result = {'schema': 1, 'kind': 'couch-owner-neutral-boot-payloads', 'bootable': False,
              'source_kernel_commit': pin['source_commit'],
              'files': {name: {'size': len(data), 'sha256': sha(data)} for name, data in payloads.items()},
              'recovery': 'Owner-local pinned stock OTA kernel plus public recovery ramdisk; physical acceptance pending.'}
    output.mkdir(parents=True, mode=0o700)
    for name, data in payloads.items():
        (output / name).write_bytes(data)
    (output / 'boot.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('zimage', 'kernel-manifest', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--root', type=Path, default=REPO)
    args = parser.parse_args()
    prepare(args.zimage, args.kernel_manifest, args.output, args.root)
