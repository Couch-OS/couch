"""Explicit restart after interrupted Wi-Fi writes, retaining verified originals."""
import hashlib
import os
from pathlib import Path
import shutil

import bootstrap_linux_stage as bootstrap
import couch_install as core


def validate(directory, release, progress=print):
    directory = Path(directory).resolve()
    core.require(not directory.is_relative_to(core.REPO), 'Keep retry originals outside Git')
    plan_path = core.regular(directory / 'private-image/plan.json')
    plan = core.read_json(plan_path)
    journal = core.read_json(core.regular(directory / 'originals/journal.json'))
    # Canonical plan bytes are what USB bound and the transaction journal pins.
    import wifi_install as install
    core.require(journal.get('plan_sha256') == hashlib.sha256(install.json_bytes(plan)).hexdigest(),
                 'Previous installation plan changed')
    core.require(journal.get('backup_complete') is True and journal.get('complete') is False
                 and 'boot' not in journal.get('writes', {}),
                 'Restart requires complete original backups and no final boot write')
    core.require(plan['partitions'] == release['partitions'], 'Retry partition layout changed')
    names = set(release['images']) | core.IDENTITY_PARTITIONS
    core.require(set(journal['backups']) == names, 'Restart requires the full original backup set')
    old_bootstrap = directory / 'bootstrap'
    receipt = core.read_json(core.regular(old_bootstrap / 'backup-receipt.json'))
    old_journal = core.read_json(core.regular(old_bootstrap / 'journal.json'))
    baseline = core.read_json(core.regular(old_bootstrap / 'baseline.json'))
    core.require(journal['bootstrap_receipt_sha256'] == core.digest(old_bootstrap / 'backup-receipt.json')
                 and receipt.get('complete') is True and old_journal.get('complete') is True
                 and receipt['journal_sha256'] == core.digest(old_bootstrap / 'journal.json')
                 and old_journal['stage_sha256'] == plan['stage_sha256'], 'Original bootstrap proof changed')
    for key in ('cid', 'capacity', 'partitions', 'identity_sha256'):
        core.require(baseline[key] == plan[key], 'Retry identity differs from original bootstrap')
    core.require(receipt['originals']['boot'] == plan['original_boot_sha256'] == journal['backups']['boot'],
                 'Original boot proof changed')
    for name in sorted(names):
        source = core.regular(directory / 'originals' / (name + '.img'))
        core.require(source.stat().st_size == plan['partitions'][name]['size']
                     and install.digest(source, progress) == journal['backups'][name],
                     'Saved original backup changed: ' + name)
        if name in core.IDENTITY_PARTITIONS:
            core.require(journal['backups'][name] == plan['identity_sha256'][name], 'Calibration backup changed')
    original_os = old_journal.get('original_os', 'Android')
    core.require(original_os in ('Android', 'Couch'), 'Unknown original OS in bootstrap proof')
    core.require(journal.get('original_os', original_os) == original_os, 'Backup OS provenance changed')
    return {'directory': directory, 'plan': plan, 'journal': journal, 'baseline': baseline,
            'original_os': original_os}


def require_android_restore(prior):
    core.require(prior.get('original_os', 'Android') == 'Android',
                 'These backups contain Couch. Select your earlier Android backup set to restore Android.')


def latest_completed_stage(prior, state_root):
    """Follow verified stage receipts after an earlier attempt stopped at credentials."""
    candidates = []
    for path in Path(state_root).glob('wifi-*/bootstrap/journal.json'):
        record = core.read_json(core.regular(path))
        if (record.get('purpose') == 'explicit-wifi-restart' and record.get('complete') is True
                and record.get('predecessor') == str(prior['directory'])):
            candidates.append((path.stat().st_mtime_ns, path, record))
    if not candidates:
        return prior['plan']['stage_sha256']
    _, path, record = max(candidates, key=lambda item: item[0])
    root = path.parent
    receipt = core.read_json(core.regular(root/'backup-receipt.json'))
    baseline = core.read_json(core.regular(root/'baseline.json'))
    originals = {n: prior['journal']['backups'][n] for n in ('boot', 'recovery')}
    core.require(receipt.get('complete') is True and record.get('write') == 'verified'
                 and receipt['journal_sha256'] == core.digest(path)
                 and receipt['originals'] == record['originals'] == originals,
                 'Latest RAM-stage receipt changed')
    for key in ('cid', 'capacity', 'partitions', 'identity_sha256'):
        core.require(baseline[key] == prior['baseline'][key], 'Latest RAM-stage identity changed')
    return record['stage_sha256']


def bootstrap_stage(args, prior, *, session=bootstrap.connected_session,
                    reader_factory=bootstrap.ConnectedMtkReader, writer_factory=bootstrap.ConnectedMtkWriter):
    baseline, release, bundle, destination, ports, stock = bootstrap.validate(args)
    old = prior['plan']
    for key in ('cid', 'capacity', 'partitions'):
        core.require(baseline[key] == old[key], 'Restart target differs from package')
    # The package snapshot predates Android startup. Normal bootstrap captures
    # fresh calibration after binding CID/layout; retry must use that receipt-
    # verified capture too. Both live comparisons below enforce its exact hashes.
    core.require(set(old['identity_sha256']) == core.IDENTITY_PARTITIONS
                 and prior['baseline']['identity_sha256'] == old['identity_sha256'],
                 'Restart calibration differs from saved bootstrap')
    # Boot still contains the prior RAM stage. Recovery may already be verified Couch.
    expected = {'boot': latest_completed_stage(prior, destination.parent.parent), 'odmdtbo': stock['odmdtbo']}
    for name in ('recovery', 'logo'):
        if name not in old['images']:
            continue
        phase = prior['journal']['writes'].get(name)
        core.require(phase in (None, 'verified'), 'Ambiguous non-userdata write requires recovery diagnosis')
        expected[name] = old['images'][name]['sha256'] if phase else prior['journal']['backups'][name]
    destination.mkdir(mode=0o700)
    # Preserve the original OS backups, never recapture partially installed Couch.
    for name in ('boot', 'recovery'):
        source = prior['directory'] / 'originals' / (name + '.img')
        shutil.copyfile(source, destination / (name + '.img'))
        with (destination / (name + '.img')).open('rb') as file:
            os.fsync(file.fileno())
    record = {'schema': 1, 'purpose': 'explicit-wifi-restart', 'complete': False,
              'original_os': prior.get('original_os', 'Android'),
              'stage_sha256': args.stage_sha256, 'predecessor': str(prior['directory']),
              'originals': {n: prior['journal']['backups'][n] for n in ('boot', 'recovery')},
              'identity_sha256': old['identity_sha256'], 'write': 'pending'}
    core.save_json(destination / 'journal.json', record)

    def choose(enumerate_devices):
        print('Restart prepared. Hold side Power until the screen goes off, then release. Keep USB connected.', flush=True)
        return bootstrap.wait_preloader(enumerate_devices, args.bus, ports, args.timeout)

    def factory(mtk, revision):
        reader = reader_factory(mtk, revision)
        bootstrap.compare_baseline(reader, prior['baseline'])
        for name, checksum in expected.items():
            core.require(reader.hash(name) == checksum, 'Unexpected interrupted-install partition: ' + name)
        binding = {**reader.description, 'model': core.MODEL, 'model_verified': True,
                   'identity_sha256': core.fingerprint(old['identity_sha256'])}
        return writer_factory(mtk, revision, release=release, bundle=bundle, binding=binding)

    backend = lambda checkout: bootstrap.ExactUsbBackend(checkout, preloader=args.preloader,
                                                         preloader_sha256=args.preloader_sha256)
    try:
        with session(args.checkout, args.loader, args.loader_sha256, args.lock_dir, None, backend,
                     candidate_provider=choose, device_factory=factory,
                     boot_after_capture=args.boot_after_capture) as device:
            with device:
                record['write'] = 'started'; core.save_json(destination / 'journal.json', record)
                device.write('boot', args.stage_image)
                core.require(device.hash('boot') == args.stage_sha256, 'Restart RAM stage readback mismatch')
                for name, checksum in expected.items():
                    if name != 'boot':
                        core.require(device.hash(name) == checksum, 'Retained partition changed: ' + name)
                bootstrap.compare_baseline(device, prior['baseline'])
                record['write'] = 'verified'; core.save_json(destination / 'journal.json', record)
        record.update(complete=True, usb_cleanup_verified=True, boot_acknowledged=args.boot_after_capture)
        core.save_json(destination / 'journal.json', record)
        core.save_json(destination / 'baseline.json', prior['baseline'])
        core.save_json(destination / 'backup-receipt.json', {'schema': 1, 'complete': True,
            'identity_sha256': old['identity_sha256'], 'originals': record['originals'],
            'journal_sha256': core.digest(destination / 'journal.json'), 'cid_sha256': args.confirm_cid_sha256})
    except BaseException as error:
        record.update(complete=False, stopped=type(error).__name__)
        core.save_json(destination / 'journal.json', record)
        raise
