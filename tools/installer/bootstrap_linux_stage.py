#!/usr/bin/env python3
"""Private stock-to-RAM-stage bootstrap. Backs up boot/recovery; never touches userdata."""
import argparse
import hashlib
from pathlib import Path
import shutil
import sys

from capture_readonly import compare_baseline, wait_preloader
from couch_install import CHUNK, MODEL, InstallError, digest, fingerprint, read_json, regular, require, save_json, sync_directory
from finalize_stock_recovery import validate as validate_stock
from mtk_readonly import ConnectedMtkReader
from mtk_session import connected_session
from mtk_usb import ExactUsbBackend
from mtk_writer import ConnectedMtkWriter

BOOT_BYTES = 16 * 1024 * 1024


def validate(args):
    baseline, stock, _, destination, ports = validate_stock(args)
    metadata_path = regular(args.stage_metadata)
    require(digest(metadata_path) == args.stage_metadata_sha256, 'RAM-stage provenance pin mismatch')
    metadata = read_json(metadata_path)
    readonly = (metadata.get('kind') in ('private-ram-wifi-stage', 'private-readonly-ram-probe')
                and metadata.get('storage_operations') == ['read-only recovery SHA-256'])
    installer = (getattr(args, 'allow_private_wifi_install', False)
                 and metadata.get('kind') == 'private-ram-wifi-installer'
                 and metadata.get('storage_operations') == ['USB-bound TLS backups', 'verified OS partition transaction'])
    require(metadata.get('schema') == 1 and metadata.get('private_only') is True
            and metadata.get('installable') is False and metadata.get('redistribution_authorized') is False
            and (readonly or installer),
            'RAM-stage kind/capabilities require explicit matching private authorization')
    image = regular(args.stage_image).resolve()
    require(metadata.get('file') == image.name and Path(metadata['file']).name == metadata['file'],
            'RAM-stage image differs from provenance filename')
    require(image.stat().st_size == metadata.get('size') == baseline['partitions']['boot']['size'] == BOOT_BYTES,
            'RAM-stage image must be exactly one 16MiB boot partition')
    require(metadata.get('sha256') == args.stage_sha256 == digest(image), 'RAM-stage image hash mismatch')
    with image.open('rb') as stream:
        require(stream.read(8) == b'ANDROID!', 'RAM-stage image lacks Android boot image magic')
    release = {'schema': 1, 'model': MODEL, 'partitions': baseline['partitions'],
               'images': {'boot': {'file': image.name, 'sha256': args.stage_sha256}}}
    expected = {**stock['retained_stock'], 'recovery': args.recovery_sha256}
    return baseline, release, image.parent, destination, ports, expected


def run(args, *, session=connected_session, reader_factory=ConnectedMtkReader, writer_factory=ConnectedMtkWriter,
        before_wait=None):
    if getattr(args, 'restore_boot_from', None) is not None:
        return restore_boot(args, session=session, reader_factory=reader_factory,
                            writer_factory=writer_factory)
    baseline, release, bundle, destination, ports, expected = validate(args)
    if args.check_only:
        print('Stock target, original recovery and private read-only stage provenance/bytes verified. No USB operation performed.')
        return
    binding_baseline = {key: baseline[key] for key in ('schema', 'cid', 'capacity', 'partitions')}
    journal = None
    fresh = None

    def choose(enumerate_devices):
        # The session has prepared and verified its loader before this hook.
        # An authorized Android reboot must happen here, not during preflight.
        if before_wait is not None:
            before_wait()
        print('Waiting for selected preloader. USB is used only to bootstrap the RAM stage.', flush=True)
        return wait_preloader(enumerate_devices, args.bus, ports, args.timeout)

    def device_factory(mtk, revision):
        nonlocal journal, fresh
        reader = reader_factory(mtk, revision)
        compare_baseline(reader, binding_baseline)
        if getattr(args, 'reinstall_couch', False):
            # Explicit reinstall backs up the currently installed OS. Device CID,
            # partition layout and retained DTB are still checked independently.
            # Never substitute these captures for the user's Android originals.
            for name in ('boot', 'recovery'):
                expected[name] = reader.hash(name)
        for name, checksum in expected.items():
            require(reader.hash(name) == checksum, f'Observed {name} differs from pinned stock image')
        report = reader.backup_identity(destination, reader.description['storage_id'],
                                        progress=lambda text: print(text, flush=True))
        require(report.get('complete') is True, 'Fresh identity capture incomplete')
        sync_directory(destination.parent)
        fresh = {**binding_baseline, 'identity_sha256': report['backups'],
                 'source': 'private-stock-to-ram-bootstrap', 'predecessor_sha256': digest(args.baseline),
                 'identity_decoded': False, 'normal_os_verified': False}
        compare_baseline(reader, fresh)
        needed = sum(baseline['partitions'][name]['size'] for name in ('boot', 'recovery')) + CHUNK
        require(shutil.disk_usage(destination).free >= needed, 'Insufficient space for boot/recovery originals')
        journal = {'schema': 1, 'purpose': 'private-stock-to-ram-bootstrap', 'complete': False,
                   'device': reader.description, 'identity_sha256': report['backups'], 'originals': {},
                   'stock_hashes': expected, 'original_os': 'Couch' if getattr(args, 'reinstall_couch', False) else 'Android',
                   'stage_sha256': args.stage_sha256,
                   'stage_metadata_sha256': args.stage_metadata_sha256,
                   'stock_manifest_sha256': args.stock_manifest_sha256,
                   'loader_sha256': args.loader_sha256, 'board_data_sha256': args.preloader_sha256,
                   'predecessor_sha256': fresh['predecessor_sha256'], 'backup_complete': False,
                   'write': 'pending', 'boot_requested': args.boot_after_capture,
                   'boot_acknowledged': False, 'usb_cleanup_verified': False}
        save_json(destination / 'journal.json', journal)
        binding = {**reader.description, 'model': MODEL, 'model_verified': True,
                   'identity_sha256': fingerprint(report['backups'])}
        return writer_factory(mtk, revision, release=release, bundle=bundle, binding=binding)

    factory = lambda checkout: ExactUsbBackend(checkout, preloader=args.preloader,
                                               preloader_sha256=args.preloader_sha256)
    try:
        with session(args.checkout, args.loader, args.loader_sha256, args.lock_dir, None, factory,
                     candidate_provider=choose, device_factory=device_factory,
                     boot_after_capture=args.boot_after_capture) as device:
            with device:
                for name in ('boot', 'recovery'):
                    path = destination / f'{name}.img'
                    device.backup(name, path)
                    checksum = digest(regular(path))
                    require(path.stat().st_size == baseline['partitions'][name]['size']
                            and checksum == device.hash(name) == expected[name], f'Independent original {name} backup mismatch')
                    journal['originals'][name] = checksum
                    save_json(destination / 'journal.json', journal)
                compare_baseline(device, fresh)
                journal.update(backup_complete=True, write='started')
                save_json(destination / 'journal.json', journal)
                device.write('boot', bundle / release['images']['boot']['file'])
                require(device.hash('boot') == args.stage_sha256, 'RAM-stage boot readback mismatch')
                for name in ('recovery', 'odmdtbo'):
                    require(device.hash(name) == expected[name], f'Retained {name} changed')
                compare_baseline(device, fresh)
                journal.update(write='verified', identity_verified_after=True, retained_verified_after=True)
                save_json(destination / 'journal.json', journal)
        journal.update(complete=True, usb_cleanup_verified=True, boot_acknowledged=args.boot_after_capture)
        save_json(destination / 'journal.json', journal)
        save_json(destination / 'baseline.json', fresh)
        save_json(destination / 'backup-receipt.json', {'schema': 1, 'complete': True,
            'identity_sha256': fresh['identity_sha256'], 'originals': journal['originals'],
            'journal_sha256': digest(destination / 'journal.json'), 'cid_sha256': args.confirm_cid_sha256})
        print('Boot stage readback and retained stock recovery/identity verified. Original boot and recovery are backed up; userdata untouched.')
        if args.boot_after_capture:
            print('DA exit acknowledged; hold Power if off. RAM-stage startup remains unverified.')
    except BaseException as error:
        if journal is not None:
            journal.update(complete=False, stopped=type(error).__name__)
            save_json(destination / 'journal.json', journal)
        raise


def restore_boot(args, *, session=connected_session, reader_factory=ConnectedMtkReader,
                 writer_factory=ConnectedMtkWriter):
    """Undo only a completed, receipt-pinned bootstrap; never restore userdata."""
    _, release, _, destination, ports, expected = validate(args)
    original = args.restore_boot_from.resolve()
    receipt_path = regular(original / 'backup-receipt.json')
    require(digest(receipt_path) == args.restore_receipt_sha256, 'Bootstrap receipt pin mismatch')
    receipt = read_json(receipt_path)
    journal_path = regular(original / 'journal.json')
    require(receipt.get('complete') is True and digest(journal_path) == receipt.get('journal_sha256'),
            'Incomplete or changed bootstrap journal')
    previous = read_json(journal_path)
    require(previous.get('purpose') == 'private-stock-to-ram-bootstrap'
            and previous.get('complete') is True and previous.get('write') == 'verified'
            and previous.get('backup_complete') is True
            and previous.get('stage_sha256') == args.stage_sha256
            and previous.get('stock_hashes') == expected,
            'Receipt does not describe this completed stock-to-RAM bootstrap')
    baseline = read_json(regular(original / 'baseline.json'))
    require(baseline.get('identity_sha256') == receipt.get('identity_sha256')
            == previous.get('identity_sha256')
            and receipt.get('cid_sha256') == args.confirm_cid_sha256,
            'Bootstrap identity receipt mismatch')
    # The pinned journal binds these independently observed device fields.
    observed = previous['device']
    require(baseline['partitions'] == release['partitions'] == observed['partitions']
            and baseline['capacity'] == observed['capacity'], 'Bootstrap layout changed')
    require(hashlib.sha256(bytes.fromhex(baseline['cid'])).hexdigest()
            == observed['runtime_cid_sha256'] == args.confirm_cid_sha256,
            'Bootstrap CID changed')
    for name in ('boot', 'recovery'):
        path = regular(original / f'{name}.img')
        require(path.stat().st_size == baseline['partitions'][name]['size']
                and digest(path) == expected[name] == receipt['originals'][name]
                == previous['originals'][name], f'Original {name} backup mismatch')
    if args.check_only:
        print('Pinned bootstrap receipt, original boot/recovery, identity and loader inputs verified. Only boot will be restored; no USB operation performed.')
        return
    record = {'schema': 1, 'purpose': 'restore-original-boot-after-ram-bootstrap',
              'complete': False, 'receipt_sha256': args.restore_receipt_sha256,
              'write': 'pending', 'boot_acknowledged': False}
    def choose(enumerate_devices):
        print('Ready to restore only original boot. Reboot the remote on the selected USB port now.', flush=True)
        return wait_preloader(enumerate_devices, args.bus, ports, args.timeout)
    def device_factory(mtk, revision):
        reader = reader_factory(mtk, revision)
        compare_baseline(reader, baseline, progress=lambda text: print(text, flush=True))
        require(reader.hash('boot') == args.stage_sha256, 'Current boot differs from journaled RAM stage')
        for name in ('recovery', 'odmdtbo'):
            require(reader.hash(name) == expected[name], f'Retained {name} changed')
        binding = {**reader.description, 'model': MODEL, 'model_verified': True,
                   'identity_sha256': fingerprint(baseline['identity_sha256'])}
        return writer_factory(mtk, revision, release={**release, 'images': {
            'boot': {'file': 'boot.img', 'sha256': expected['boot']}}}, bundle=original, binding=binding)
    factory = lambda checkout: ExactUsbBackend(checkout, preloader=args.preloader,
                                               preloader_sha256=args.preloader_sha256)
    destination.mkdir(parents=True, mode=0o700)
    sync_directory(destination.parent)
    path = destination / 'journal.json'
    save_json(path, record)
    try:
        with session(args.checkout, args.loader, args.loader_sha256, args.lock_dir, None, factory,
                     candidate_provider=choose, device_factory=device_factory,
                     boot_after_capture=args.boot_after_capture) as device:
            with device:
                record['write'] = 'started'
                save_json(path, record)
                device.write('boot', original / 'boot.img')
                require(device.hash('boot') == expected['boot'], 'Original boot readback mismatch')
                for name in ('recovery', 'odmdtbo'):
                    require(device.hash(name) == expected[name], f'Retained {name} changed')
                compare_baseline(device, baseline, progress=lambda text: print(text, flush=True))
                record['write'] = 'verified'
                save_json(path, record)
        record.update(complete=True, boot_acknowledged=args.boot_after_capture,
                      identity_verified_after=True, retained_verified_after=True)
        save_json(path, record)
        print('Original Android boot restored and independently verified; userdata untouched. Hold Power if off; Android startup still needs verification.')
    except BaseException as error:
        record['stopped'] = type(error).__name__
        save_json(path, record)
        raise


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('baseline', 'stock-manifest', 'recovery', 'stage-image', 'stage-metadata', 'checkout', 'loader', 'preloader', 'backup-dir'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('stock-manifest-sha256', 'recovery-sha256', 'stage-sha256', 'stage-metadata-sha256', 'confirm-cid-sha256', 'loader-sha256', 'preloader-sha256', 'ports'):
        parser.add_argument('--' + name, required=True)
    for name in ('allow-private-flash', 'stock-boot-confirmed', 'boot-after-capture', 'check-only'):
        parser.add_argument('--' + name, action='store_true')
    parser.add_argument('--bus', type=int, required=True)
    parser.add_argument('--timeout', type=float, default=120)
    parser.add_argument('--lock-dir', type=Path, default=Path.home() / '.local/state/couch-installer/locks')
    parser.add_argument('--restore-boot-from', type=Path, help='Undo a completed RAM bootstrap using its verified original boot backup')
    parser.add_argument('--restore-receipt-sha256', help='Explicit SHA-256 pin for that bootstrap backup receipt')
    parser.add_argument('--allow-private-wifi-install', action='store_true', help='Explicitly permit the private USB-bound Wi-Fi installer stage')
    try:
        run(parser.parse_args(argv))
        return 0
    except (InstallError, OSError, ValueError, KeyError, TypeError) as error:
        print(f'RAM bootstrap stopped: {error}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('Interrupted; retain originals and journal. No automatic retry/reset.', file=sys.stderr)
        return 130


if __name__ == '__main__':
    sys.exit(main())
