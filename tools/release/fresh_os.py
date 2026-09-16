#!/usr/bin/env python3
"""Bind a clean packaged rootfs to the exact integration-capable core inputs.

This is offline build evidence, not a signature or source-to-binary proof. It
never reads owner state, opens a device, or installs an integration package.
"""
import argparse
import io
import json
from pathlib import Path
import re
import tarfile

from clean_stage import archive_name, checksum, require
from os_baseline import MARKER, PIN
from prepare_rootfs import normalize
from runtime_inventory import RUNTIME, regular
from verify_integration_set import DEFAULT, verify_receipt

CORE = {name: path for name, path in RUNTIME.items() if name != 'fbcon'}


def validate_binding(binding, source_commit, rootfs_sha256, *, baseline=None, key_sha256=None):
    """Admission at each builder boundary; the enclosing receipt is hash-pinned."""
    baseline = json.loads(PIN.read_text()) if baseline is None else baseline
    if key_sha256 is None:
        key_sha256 = json.loads(DEFAULT.read_text())['feed']['public_key']['sha256']
    require(isinstance(binding, dict) and binding.get('schema') == 1
            and binding.get('kind') == 'couch-fresh-os-core-binding'
            and binding.get('installable') is False,
            'Fresh OS requires a verified core binding; old userdata cannot be relabeled')
    require(binding.get('source_commit') == source_commit
            and re.fullmatch('[0-9a-f]{40}', source_commit or ''), 'Fresh OS core source differs')
    require(binding.get('rootfs_archive_sha256') == rootfs_sha256
            and re.fullmatch('[0-9a-f]{64}', rootfs_sha256 or ''), 'Fresh OS rootfs binding differs')
    require(binding.get('runtime_boot_sha256') == baseline['runtime_boot_sha256']
            and binding.get('package_closure_sha256') == baseline['package_closure_sha256']
            and binding.get('os_baseline') == {key: baseline[key] for key in ('schema', 'model', 'id')},
            'Fresh OS requires the reviewed bootstrap and package baseline')
    require(binding.get('official_integration_key_sha256') == key_sha256,
            'Fresh OS official integration key differs')
    tested = binding.get('tested_integration_set', {})
    require(tested.get('candidate_commit') == source_commit
            and tested.get('artifact_bytes_verified') is True and tested.get('protocol_version') == 1
            and tested.get('rollout') == {
                'bundle_packages_in_runtime': False, 'bundle_packages_in_installer': False,
                'automatic_install': False, 'automatic_configuration_migration': False},
            'Fresh OS needs the verified same-source unbundled integration set')
    files = binding.get('core_files', {})
    require(set(files) == set(CORE), 'Fresh OS core inventory is incomplete')
    for item in files.values():
        require(isinstance(item, dict) and set(item) == {'size', 'sha256'}
                and type(item['size']) is int and item['size'] > 0
                and re.fullmatch('[0-9a-f]{64}', item['sha256'] or ''), 'Invalid fresh core file pin')
    for key in ('runtime_inventory_sha256', 'runtime_build_receipt_sha256', 'integration_receipt_sha256'):
        require(re.fullmatch('[0-9a-f]{64}', binding.get(key, '')), 'Missing fresh OS input receipt digest')
    return binding


def bind(staging, inventory_path, build_path, integration_path, *, integration_set=DEFAULT):
    inventory_bytes, build_bytes = regular(inventory_path), regular(build_path)
    inventory, build = json.loads(inventory_bytes), json.loads(build_bytes)
    tested = verify_receipt(integration_set, integration_path, require_artifacts=True)
    manifest = json.loads(regular(staging / 'staging.json'))
    data = regular(staging / 'rootfs-staging.tar.gz')
    commit = tested['candidate_commit']
    require(manifest.get('kind') == 'couch-packaged-staging' and manifest.get('installable') is False,
            'Fresh OS requires clean packaged staging')
    require(manifest.get('source_commit') == inventory.get('inventory_source_commit') == build.get('source_commit') == commit,
            'Fresh OS inputs must describe the same frozen core commit')
    require(inventory.get('kind') == 'couch-runtime-payload-inventory'
            and inventory.get('clean_runtime_ready') is True
            and inventory.get('tracked_payload_worktree_clean') is True
            and inventory.get('tested_integration_set') == tested,
            'Fresh OS requires clean runtime inventory with the verified integration set')
    require(build.get('kind') == 'couch-unsigned-runtime-build' and build.get('schema') == 1
            and build.get('target') == 'armv7-unknown-linux-musleabihf',
            'Expected original ARM core build receipt')
    require(checksum(data) == manifest['archive_sha256'], 'Packaged rootfs bytes changed')
    normalize(data, manifest['source_date_epoch'])  # Link/private-state/default checks, without extraction.
    baseline = json.loads(PIN.read_text())
    require(manifest.get('package_closure_sha256') == baseline['package_closure_sha256'],
            'Fresh OS package closure differs from baseline')
    artifacts = inventory['artifacts']
    by_destination = {item['destination']: item for item in artifacts}
    require(len(by_destination) == len(artifacts), 'Duplicate runtime inventory path')
    built = {item['path']: item for item in build['files']}
    require(len(built) == len(build['files']) and set(built) == set(CORE.values()),
            'Core build receipt must contain exactly the five runtime executables')
    key = regular(DEFAULT.parents[2] / 'daemon/couch-integrations/src/official.rsa.pub')
    key_pin = json.loads(regular(integration_set))['feed']['public_key']
    require(len(key) == key_pin['size'] and checksum(key) == key_pin['sha256'], 'Official key bytes changed')
    core_files = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        members = {archive_name(item.name): item for item in archive.getmembers()}
        # No integration slots/APKs or previously activated runtime may ride in a fresh image.
        require(not any(name.endswith('.apk') or name.startswith((
            'opt/couch/integrations/', 'opt/couch/runtime/', 'opt/couch/updates/',
            'usr/lib/couch/integrations/')) for name in members),
            'Fresh OS must not contain installed integration packages or runtime slots')
        for destination, item in by_destination.items():
            member = members.get(destination)
            require(member is not None and member.isreg() and member.mode == item['mode'],
                    'Missing or nonregular fresh runtime artifact: ' + destination)
            content = archive.extractfile(member).read()
            require(checksum(content) == item['sha256'], 'Stale rootfs runtime artifact: ' + destination)
        for name, path in CORE.items():
            destination = 'opt/couch/' + name
            require(destination in by_destination, 'Missing core runtime inventory member')
            content = archive.extractfile(members[destination]).read()
            expected = {'size': len(content), 'sha256': checksum(content)}
            require({field: built[path][field] for field in expected} == expected,
                    'Rootfs core differs from original build receipt: ' + name)
            if name == 'couch-confd':
                require(key in content, 'Rootfs core does not embed the tested official integration key')
            core_files[name] = expected
        bootstrap = members.get('opt/couch/runtime-boot.sh')
        require(bootstrap is not None and bootstrap.isreg() and bootstrap.mode & 0o111
                and checksum(archive.extractfile(bootstrap).read()) == baseline['runtime_boot_sha256'],
                'Fresh OS requires corrected stable bootstrap bytes, not just its baseline ID')
        marker = members.get(MARKER)
        expected_marker = {field: baseline[field] for field in ('schema', 'model', 'id')}
        require(marker is not None and marker.isreg()
                and json.load(archive.extractfile(marker)) == expected_marker,
                'Fresh OS baseline marker differs')
    binding = dict(schema=1, kind='couch-fresh-os-core-binding', installable=False,
                   source_commit=commit, rootfs_archive_sha256=checksum(data),
                   runtime_inventory_sha256=checksum(inventory_bytes),
                   runtime_build_receipt_sha256=checksum(build_bytes),
                   integration_receipt_sha256=checksum(regular(integration_path)),
                   tested_integration_set=tested, core_files=core_files,
                   official_integration_key_sha256=checksum(key), os_baseline=expected_marker,
                   runtime_boot_sha256=baseline['runtime_boot_sha256'],
                   package_closure_sha256=baseline['package_closure_sha256'])
    return validate_binding(binding, commit, checksum(data))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('staging', 'runtime-inventory', 'runtime-build', 'integration-receipt', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    result = bind(args.staging, args.runtime_inventory, args.runtime_build, args.integration_receipt)
    with args.output.open('x') as output:
        output.write(json.dumps(result, sort_keys=True, indent=2) + '\n')
