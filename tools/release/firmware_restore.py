#!/usr/bin/env python3
"""Firmware restore pin: cross-checks, and change detection against the vendor OTA.

This never redistributes firmware. It loads the repo-pinned restore manifest
(`ha100_firmware_restore.json`), confirms it agrees byte-for-byte with the
official runtime pin (`ha100_official_runtime.json`), and can verify/derive the
restore metadata from a locally supplied OTA ZIP. The scheduled firmware watcher
uses `--derive` to build a pull-request body when the vendor archive hash drifts.
No device access and no installation authorization.
"""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile

from installer_pins import load as load_installer_pin

official_runtime = load_installer_pin('official_runtime')
BOOTSTRAP_MEMBERS = load_installer_pin('prepare_official_inputs').BOOTSTRAP_MEMBERS
file_sha = load_installer_pin('private_vendor').file_sha
require = load_installer_pin('private_vendor').require

PIN = Path(__file__).resolve().parents[1] / 'installer/pins/ha100_firmware_restore.json'
OFFICIAL = official_runtime.PIN


def load(pin=None, official=None):
    pin = json.loads(PIN.read_text()) if pin is None else pin
    official = json.loads(OFFICIAL.read_text()) if official is None else official
    require(pin.get('schema') == 1 and pin.get('kind') == 'couch-firmware-restore-pin'
            and pin.get('model') == 'sanytron-ha100' and pin.get('purpose') == 'restore-original-android'
            and pin.get('installable') is False and pin.get('redistribution_authorized') is False,
            'Not a reviewed firmware restore pin')
    archive = pin['archive']
    require(archive['sha256'] == official['sha256'] and archive['size'] == official['size']
            and archive['version'] == official['version'],
            'Restore pin archive disagrees with the official runtime pin')
    for name, image in pin['images'].items():
        member = image['member']
        expected = BOOTSTRAP_MEMBERS.get(member)
        require(expected is not None and expected[0] == image['size'] and expected[1] == image['sha256'],
                f'Restore image {name} disagrees with the pinned official bootstrap member')
    require(pin['userdata']['partition'] == 'userdata' and pin['userdata']['filesystem'] == 'f2fs'
            and pin['userdata']['reproducible_hash'] is False
            and pin['userdata']['superblock_offsets'] == [1024, 5120],
            'Unexpected restore userdata geometry')
    return pin


def verify_archive(ota, pin=None):
    # When the caller supplies a pin it is trusted as already validated; otherwise
    # the repo pin is loaded and cross-checked against the official runtime pin.
    pin = load() if pin is None else pin
    ota = Path(ota)
    archive = pin['archive']
    require(ota.is_file() and not ota.is_symlink() and ota.stat().st_size == archive['size'],
            'Wrong regular OTA file or size')
    require(file_sha(ota) == archive['sha256'], 'OTA archive hash differs from the pinned value')
    return archive['sha256']


def derive(ota, *, with_formatter=True, official=None):
    """Derive restore metadata from a supplied OTA. Used in check/derive modes."""
    ota = Path(ota)
    official = json.loads(OFFICIAL.read_text()) if official is None else official
    digest = file_sha(ota)
    size = ota.stat().st_size
    result = {'archive': {'sha256': digest, 'size': size, 'version': official['version']},
              'images': {}, 'formatter': None, 'checker': None}
    with zipfile.ZipFile(ota) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)), 'Duplicate OTA members')
        for name, member in (('boot', 'boot.img'), ('odmdtbo', 'odmdtbo.img')):
            info = archive.getinfo(member)
            require(not info.is_dir() and (info.external_attr >> 16) & 0o170000 != 0o120000,
                    f'{member} is a directory or symlink')
            sha = hashlib.sha256()
            with archive.open(info) as source:
                for block in iter(lambda: source.read(65536), b''):
                    sha.update(block)
            result['images'][name] = {'member': member, 'size': info.file_size, 'sha256': sha.hexdigest()}
    if with_formatter:
        # The formatter/checker live in the reconstructed system image. This needs
        # brotli + debugfs; it is best effort so the watcher still works without them.
        try:
            import tempfile, os
            from private_vendor import read_image
            with tempfile.TemporaryDirectory(prefix='.couch-fw-restore-') as scratch:
                scratch = Path(scratch)
                os.umask(0o077)
                with zipfile.ZipFile(ota) as archive:
                    image = scratch / 'system.img'
                    official_runtime.reconstruct(archive, 'system', official, image)
                    for key, path in (('formatter', 'system/bin/make_f2fs'),
                                      ('checker', 'system/bin/fsck.f2fs')):
                        content = read_image(image, path)
                        result[key] = {'source_path': path, 'size': len(content),
                                       'sha256': hashlib.sha256(content).hexdigest()}
        except Exception as error:  # noqa: BLE001 - best-effort derivation
            result['formatter_note'] = f'formatter/checker not derived: {error}'
    return result


def check(ota, pin=None):
    """Confirm a supplied OTA reproduces every pinned restore value. Raises on drift."""
    pin = load() if pin is None else pin
    verify_archive(ota, pin)
    derived = derive(ota, with_formatter=True)
    for name, image in pin['images'].items():
        got = derived['images'].get(name)
        require(got and got['size'] == image['size'] and got['sha256'] == image['sha256'],
                f'Derived {name} differs from the pinned restore image')
    if derived.get('formatter'):
        require(derived['formatter']['sha256'] == pin['formatter']['sha256'],
                'Derived make_f2fs differs from the pinned formatter')
    if derived.get('checker'):
        require(derived['checker']['sha256'] == pin['checker']['sha256'],
                'Derived fsck.f2fs differs from the pinned checker')
    return derived


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', type=Path, help='Verify an OTA reproduces the pinned restore values')
    parser.add_argument('--derive', type=Path, help='Print restore metadata derived from an OTA as JSON')
    parser.add_argument('--no-formatter', action='store_true', help='Skip the debugfs/brotli formatter derivation')
    args = parser.parse_args()
    if args.derive is not None:
        print(json.dumps(derive(args.derive, with_formatter=not args.no_formatter), indent=2, sort_keys=True))
    elif args.check is not None:
        check(args.check)
        print('OTA reproduces every pinned firmware restore value.')
    else:
        load()
        print('Firmware restore pin is internally consistent with the official runtime pin.')
