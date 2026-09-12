#!/usr/bin/env python3
"""First capture from stock Android: read-only enrollment, never a flash writer."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

from capture_readonly import wait_preloader
from couch_install import (CHUNK, IDENTITY_PARTITIONS, MODEL, REPO, InstallError, digest, layout,
                           regular, require, save_json, sync_directory, identity_record)
from mtk_session import loader_bytes, read_session, source_pin
from mtk_usb import ExactUsbBackend

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'release'))
from prepare_official_inputs import BOOTSTRAP_MEMBERS
from official_runtime import PIN


def android_identity(serial, bus, ports, *, run=subprocess.run, enumerate_usb=None):
    """Bind an authorized ADB device to an explicit physical USB port, without reboot."""
    require(re.fullmatch(r'[A-Za-z0-9._:-]{1,128}', serial) is not None, 'Invalid ADB serial selection')
    if enumerate_usb is None:
        import usb.core
        enumerate_usb = lambda: usb.core.find(find_all=True)
    matches = [d for d in enumerate_usb() if d.bus == bus and tuple(d.port_numbers or ()) == ports]
    require(len(matches) == 1 and matches[0].serial_number == serial, 'Android USB port/serial selection differs')
    def adb(*words):
        result = run(['adb', *words], capture_output=True, text=True, timeout=15)
        require(result.returncode == 0 and len(result.stdout) <= 65536, 'Android identity query failed')
        return result.stdout
    listing = adb('devices')
    require(sum(line.split() == [serial, 'device'] for line in listing.splitlines()) == 1,
            'Enable USB debugging and authorize the selected Android device')
    cid = adb('-s', serial, 'shell', 'cat', '/sys/block/mmcblk0/device/cid').strip().lower()
    require(re.fullmatch(r'[0-9a-f]{32}', cid) is not None and cid not in ('0'*32, 'f'*32),
            'Android did not expose a valid canonical storage identity')
    return {'cid': cid, 'bus': bus, 'ports': '.'.join(map(str, ports)),
            'android_serial_sha256': hashlib.sha256(serial.encode()).hexdigest()}


def official_inputs(directory):
    directory = Path(directory)
    receipt = json.loads(regular(directory / 'inputs.json').read_text())
    pin = json.loads(PIN.read_text())
    require(receipt.get('kind') == 'owner-official-install-inputs'
            and receipt.get('source_archive_sha256') == pin['sha256']
            and receipt.get('original_device_backup') is False, 'Expected verified official owner inputs')
    result = {}
    for name, (size, checksum) in BOOTSTRAP_MEMBERS.items():
        path = regular(directory / 'bootstrap' / name)
        require(path.stat().st_size == size and digest(path) == checksum, 'Official bootstrap input changed')
        result[name] = path
    offsets = {}
    for line in result['scatter.txt'].read_text().splitlines():
        name, raw = line.split()
        offset = int(raw, 16)
        if name not in ('preloader', 'pgpt', 'sgpt', 'flashinfo'):
            offsets[name] = offset
    return result, offsets


def check_layout(observed, offsets):
    partitions = layout(observed['partitions'])
    require(set(partitions) == set(offsets) | {'flashinfo'}, 'Unexpected stock partition inventory')
    require(all(partitions[name]['offset'] == offset for name, offset in offsets.items()),
            'Stock partition offsets differ from official HA100 profile')
    # The pinned official scatter defines contiguous fixed partitions through
    # userdata. Matching starts alone could admit a truncated calibration range.
    ordered = sorted(offsets, key=offsets.__getitem__)
    require(ordered and ordered[-1] == 'userdata', 'Unexpected official partition ordering')
    require(all(partitions[name]['size'] == offsets[next_name] - offsets[name]
                for name, next_name in zip(ordered, ordered[1:])),
            'Stock partition sizes differ from official HA100 boundaries')
    require(all(r['offset'] + r['size'] <= observed['capacity'] for r in partitions.values()),
            'Partition exceeds observed storage')
    require(partitions['userdata']['offset'] + partitions['userdata']['size'] == partitions['flashinfo']['offset'],
            'Unexpected userdata boundary')
    return partitions


def image_prefix(reader, name, expected):
    wanted = expected.read_bytes()
    remaining = len(wanted)
    offset = 0
    for chunk in reader.chunks(name):
        count = min(len(chunk), remaining)
        require(chunk[:count] == wanted[offset:offset+count], 'Stock image differs from official HA100 firmware')
        remaining -= count
        offset += count
        if not remaining:
            return
    require(False, 'Stock image is truncated')


def enroll(args, *, session=read_session, android=android_identity):
    saved_identity = identity_record(args.identity)
    destination = Path(args.backup_dir).absolute()
    require(not destination.exists() and not destination.is_symlink()
            and not destination.resolve().is_relative_to(REPO), 'Use a new private enrollment directory outside Git')
    ports = tuple(int(v) for v in args.ports.split('.'))
    require(args.bus > 0 and ports and all(v > 0 for v in ports), 'Invalid USB topology')
    require(0 < args.timeout <= 600, 'Invalid enrollment timeout')
    files, offsets = official_inputs(args.official_inputs)
    source_pin(args.checkout)
    loader_bytes(args.loader, args.loader_sha256)
    # Never scan for another preloader or write these distribution bytes.
    preloader_hash = BOOTSTRAP_MEMBERS['preloader.img'][1]
    binding = android(args.serial, args.bus, ports)
    expected_cid = hashlib.sha256(bytes.fromhex(binding['cid'])).hexdigest()
    record = None
    def choose(enumerate_devices):
        print('Android identity captured. Restart the selected remote into preloader; no partition writes will be sent.', flush=True)
        return wait_preloader(enumerate_devices, args.bus, ports, args.timeout)
    backend = lambda checkout: ExactUsbBackend(checkout, preloader=files['preloader.img'],
                                                preloader_sha256=preloader_hash)
    with session(args.checkout, args.loader, args.loader_sha256, args.lock_dir, None, backend,
                 candidate_provider=choose, boot_after_capture=False) as reader:
        observed = reader.description
        require(observed.get('hwcode') == 0x6580
                and observed.get('cid_encoding') == 'mt6580-legacy-le32-registers'
                and observed.get('runtime_cid_sha256') == expected_cid,
                'Download-mode device differs from selected Android storage')
        partitions = check_layout(observed, offsets)
        for name in ('boot', 'odmdtbo'):
            require(partitions[name]['size'] >= files[name+'.img'].stat().st_size, 'Stock partition is too small')
            image_prefix(reader, name, files[name+'.img'])
        # Model evidence is independent official firmware plus layout, not an
        # assumption derived from the just-observed CID or generic MT6580 chip.
        needed = sum(partitions[n]['size'] for n in IDENTITY_PARTITIONS | {'boot', 'recovery', 'odmdtbo'}) + CHUNK
        destination.parent.mkdir(parents=True, exist_ok=True)
        require(shutil.disk_usage(destination.parent).free >= needed, 'Insufficient space for enrollment originals')
        report = reader.backup_identity(destination, observed['storage_id'])
        require(report.get('complete') is True and set(report['backups']) == IDENTITY_PARTITIONS,
                'Independent calibration backup did not complete')
        originals = {}
        for name in ('boot', 'recovery', 'odmdtbo'):
            path = destination / (name+'.img')
            with path.open('xb') as output:
                for chunk in reader.chunks(name):
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            path.chmod(0o600)
            checksum = digest(path)
            require(path.stat().st_size == partitions[name]['size'] and checksum == reader.hash(name),
                    'Independent original image readback mismatch')
            originals[name] = checksum
        for name, checksum in report['backups'].items():
            require(digest(regular(destination/(name+'.img'))) == checksum == reader.hash(name),
                    'Calibration changed during enrollment')
        save_json(destination/'android-identity.json', saved_identity)
        identity_checksum = digest(destination/'android-identity.json')
        record = {'schema': 1, 'cid': binding['cid'], 'capacity': observed['capacity'],
                  'partitions': partitions, 'identity_sha256': report['backups'],
                  'source': 'first-stock-android-enrollment', 'prior_baseline': False,
                  'identity_decoded': False, 'normal_os_verified': False,
                  'model_evidence': 'official-stock-boot-and-odmdtbo-prefixes-and-fixed-partition-boundaries',
                  'android_serial_sha256': binding['android_serial_sha256'],
                  'android_identity_record_sha256': identity_checksum,
                  'official_archive_sha256': json.loads(PIN.read_text())['sha256']}
        save_json(destination/'enrollment-journal.json', {'schema': 1, 'complete': False,
                  'originals': originals, 'identity_sha256': report['backups']})
    # Publish a usable baseline only after session cleanup, never on partial capture.
    save_json(destination/'stock-manifest.json', {'schema': 1, 'model': MODEL, 'purpose': 'stock-baseline',
              'private_candidate': True, 'installable': False, 'partitions': partitions,
              'images': {name: {'file': name+'.img', 'sha256': checksum} for name, checksum in originals.items()}})
    save_json(destination/'baseline.json', record)
    save_json(destination/'enrollment-journal.json', {'schema': 1, 'complete': True,
              'usb_cleanup_verified': True, 'originals': originals, 'identity_sha256': report['backups'],
              'android_identity_record_sha256': identity_checksum,
              'baseline_sha256': digest(destination/'baseline.json')})
    sync_directory(destination)
    print('First enrollment captured and independently verified. No flash writes or reboot requested.')
    return record


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('official-inputs', 'checkout', 'loader', 'backup-dir', 'lock-dir', 'identity'):
        parser.add_argument('--'+name, type=Path, required=True)
    for name in ('loader-sha256', 'serial', 'ports'):
        parser.add_argument('--'+name, required=True)
    parser.add_argument('--bus', type=int, required=True)
    parser.add_argument('--timeout', type=float, default=120)
    try:
        enroll(parser.parse_args())
    except (InstallError, ValueError, OSError, RuntimeError) as error:
        parser.exit(1, f'Enrollment stopped: {error}\n')
