#!/usr/bin/env python3
"""Build compact vendor-free userdata using pinned offline filesystem tools."""
import argparse
import io
import json
import tarfile
from pathlib import Path

from clean_stage import checksum, require
from installer_pins import installer_path
from prepare_ext4 import prepare as build_ext4
from fresh_os import validate_binding


def prepare(staging, tools, geometry, output, fresh_core=None):
    manifest = json.loads((staging / 'staging.json').read_text())
    require(manifest.get('kind') == 'couch-packaged-staging' and manifest.get('installable') is False,
            'Expected clean, noninstallable packaged staging')
    data = (staging / 'rootfs-staging.tar.gz').read_bytes()
    require(checksum(data) == manifest['archive_sha256'], 'Staging archive changed')
    require(fresh_core is not None, 'Public userdata requires --fresh-core from fresh_os.py')
    binding = json.loads(fresh_core.read_text())
    validate_binding(binding, manifest['source_commit'], checksum(data))
    pin = json.loads(installer_path('pins', 'ha100_official_runtime.json').read_text())
    names = {'opt/couch/' + entry['path'] for entry in pin['files']}
    hashes = {entry['sha256'] for entry in pin['files']}
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            require(member.name not in names, 'Owner vendor path in public userdata')
            if member.isreg():
                require(checksum(archive.extractfile(member).read()) not in hashes,
                        'Owner vendor bytes in public userdata')
    result = build_ext4(staging, tools, geometry, output, compact=True)
    validate_binding(binding, manifest['source_commit'], result['rootfs_archive_sha256'])
    result['kind'] = 'couch-owner-neutral-userdata'
    result['owner_vendor_source_sha256'] = pin['sha256']
    result['fresh_core'] = binding
    result['pending'] = ['signed release inventory', 'owner-local vendor insertion', 'physical boot and recovery validation']
    (output / 'image.json').write_text(json.dumps(result, sort_keys=True, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('staging', 'tools', 'geometry', 'output'):
        parser.add_argument(name, type=Path)
    parser.add_argument('--fresh-core', type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.staging, args.tools, json.loads(args.geometry.read_text()), args.output, args.fresh_core)
    print(f"Prepared vendor-free compact userdata: {result['image']['size']} bytes")
