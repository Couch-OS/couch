#!/usr/bin/env python3
"""Factory firmware restore pin: self-consistency and change detection.

This never redistributes firmware. It loads the repo-pinned restore manifest
(`ha100_factory_firmware.json`), confirms it is internally consistent, and can
verify/derive the restore metadata from a locally supplied vendor factory ZIP
(an SP Flash Tool `binpackage`). The scheduled firmware watcher uses `--derive`
to build a pull-request body when the vendor archive changes. No device access
and no installation authorization.

Unlike an Android block OTA, the factory binpackage ships direct partition
images (boot.img, system.img [Android-sparse], lk.img, ...), so no brotli or
dat-transfer reconstruction is needed: every pinned source value is a plain ZIP
member hash. The full-partition images that the restore actually flashes
(`restore_images`) are derived from these members by the bundle builder
(tools, factory_bundle) and verified on-device by independent readback.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import zipfile

from installer_pins import load as load_installer_pin

_vendor = load_installer_pin('private_vendor')
file_sha, require = _vendor.file_sha, _vendor.require

PIN = Path(__file__).with_name('ha100_factory_firmware.json')
_SHA = re.compile(r'[0-9a-f]{64}')


def _member(archive, member):
    info = archive.getinfo(member)
    require(not info.is_dir() and (info.external_attr >> 16) & 0o170000 != 0o120000,
            f'{member} is a directory or symlink')
    sha = hashlib.sha256()
    with archive.open(info) as source:
        for block in iter(lambda: source.read(65536), b''):
            sha.update(block)
    return info.file_size, sha.hexdigest()


def load(pin=None):
    pin = json.loads(PIN.read_text()) if pin is None else pin
    require(pin.get('schema') == 1 and pin.get('kind') == 'couch-firmware-restore-pin'
            and pin.get('model') == 'sanytron-ha100' and pin.get('purpose') == 'restore-factory-firmware'
            and pin.get('installable') is False and pin.get('redistribution_authorized') is False,
            'Not a reviewed factory firmware restore pin')
    archive = pin['archive']
    require(isinstance(archive.get('version'), str) and isinstance(archive.get('size'), int)
            and archive['size'] > 0 and _SHA.fullmatch(archive.get('sha256', '')),
            'Invalid pinned archive metadata')
    root = pin['source']['member_root']
    require(isinstance(root, str) and root.endswith('/'), 'Invalid binpackage member root')
    for section in ('scatter', 'preloader'):
        entry = pin[section]
        require(entry['member'].startswith(root) and isinstance(entry['size'], int)
                and _SHA.fullmatch(entry['sha256']), f'Invalid pinned {section} entry')
    require(pin['preloader'].get('flashed') is False, 'The factory restore must preserve the preloader')
    members = pin['source_members']
    require(isinstance(members, dict) and {'boot', 'system', 'vendor'} <= members.keys(),
            'Factory source members must include boot, system and vendor')
    for name, image in members.items():
        require(image['member'].startswith(root) and isinstance(image['size'], int)
                and _SHA.fullmatch(image['sha256']), f'Invalid source member {name}')
    restore = pin['restore_images']
    require(isinstance(restore, dict) and {'system', 'vendor', 'boot'} <= restore.keys(),
            'restore_images must include system, vendor and boot')
    for name, image in restore.items():
        require(image['partition'] == name and isinstance(image['size'], int)
                and _SHA.fullmatch(image['sha256']), f'Invalid restore image {name}')
    require(pin['userdata']['partition'] == 'userdata' and pin['userdata']['filesystem'] == 'f2fs'
            and pin['userdata']['reproducible_hash'] is False
            and pin['userdata']['superblock_offsets'] == [1024, 5120],
            'Unexpected restore userdata geometry')
    require('preloader' in pin['preserved'] and {'proinfo', 'nvram', 'nvdata', 'protect1', 'protect2'}
            <= set(pin['preserved']), 'Preserved set must keep the preloader and calibration partitions')
    return pin


def verify_archive(ota, pin=None):
    pin = load() if pin is None else pin
    ota = Path(ota)
    archive = pin['archive']
    require(ota.is_file() and not ota.is_symlink() and ota.stat().st_size == archive['size'],
            'Wrong regular factory ZIP or size')
    require(file_sha(ota) == archive['sha256'], 'Factory archive hash differs from the pinned value')
    return archive['sha256']


def derive(ota, *, pin=None):
    """Derive factory restore metadata from a supplied vendor ZIP. The member set
    mirrors the repo pin so a changed archive is reported field by field."""
    pin = load() if pin is None else pin
    ota = Path(ota)
    result = {'archive': {'sha256': file_sha(ota), 'size': ota.stat().st_size,
                          'version': pin['archive']['version']},
              'scatter': {}, 'preloader': {}, 'source_members': {}}
    with zipfile.ZipFile(ota) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)), 'Duplicate ZIP members')
        for section in ('scatter', 'preloader'):
            size, sha = _member(archive, pin[section]['member'])
            result[section] = {'member': pin[section]['member'], 'size': size, 'sha256': sha}
        for name, image in pin['source_members'].items():
            size, sha = _member(archive, image['member'])
            result['source_members'][name] = {'member': image['member'], 'size': size, 'sha256': sha}
    return result


def check(ota, pin=None):
    """Confirm a supplied ZIP reproduces every pinned factory value. Raises on drift."""
    pin = load() if pin is None else pin
    verify_archive(ota, pin)
    derived = derive(ota, pin=pin)
    for section in ('scatter', 'preloader'):
        got = derived[section]
        require(got['size'] == pin[section]['size'] and got['sha256'] == pin[section]['sha256'],
                f'Derived {section} differs from the pinned value')
    for name, image in pin['source_members'].items():
        got = derived['source_members'].get(name)
        require(got and got['size'] == image['size'] and got['sha256'] == image['sha256'],
                f'Derived source member {name} differs from the pinned value')
    return derived


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', type=Path, help='Verify a factory ZIP reproduces the pinned values')
    parser.add_argument('--derive', type=Path, help='Print restore metadata derived from a factory ZIP as JSON')
    args = parser.parse_args()
    if args.derive is not None:
        print(json.dumps(derive(args.derive), indent=2, sort_keys=True))
    elif args.check is not None:
        check(args.check)
        print('Factory ZIP reproduces every pinned firmware restore value.')
    else:
        load()
        print('Factory firmware restore pin is internally consistent.')
