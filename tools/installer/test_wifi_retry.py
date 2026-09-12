import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import contextmanager
from types import SimpleNamespace
from unittest.mock import Mock, patch

import wifi_trial  # Establish the Linux-stage import path used by the wizard.
import wifi_retry
import wifi_install


class WifiRetryTests(unittest.TestCase):
    def fixture(self, root):
        names = {'boot', 'recovery', 'userdata'} | wifi_install.IDENTITY
        partitions = {n: {'offset': (i+1)*4096, 'size': 4096} for i, n in enumerate(sorted(names))}
        original = b'O'*4096
        checksum = hashlib.sha256(original).hexdigest()
        baseline = {'cid': '12'*16, 'capacity': 1024*1024, 'partitions': partitions,
                    'identity_sha256': {n: checksum for n in wifi_install.IDENTITY}}
        plan = {**baseline, 'stage_sha256': 'a'*64, 'original_boot_sha256': checksum,
                'images': {n: {'sha256': 'b'*64} for n in ('boot', 'recovery', 'userdata')}}
        for name in ('bootstrap', 'originals', 'private-image'):
            (root/name).mkdir()
        def save(name, value): (root/name).write_text(json.dumps(value))
        save('bootstrap/baseline.json', baseline)
        save('bootstrap/journal.json', {'complete': True, 'stage_sha256': 'a'*64})
        save('bootstrap/backup-receipt.json', {'complete': True, 'originals': {'boot': checksum},
             'journal_sha256': wifi_install.digest(root/'bootstrap/journal.json')})
        save('private-image/plan.json', plan)
        journal = {'complete': False, 'backup_complete': True, 'writes': {'userdata': 'writing'},
                   'plan_sha256': hashlib.sha256(wifi_install.json_bytes(plan)).hexdigest(),
                   'bootstrap_receipt_sha256': wifi_install.digest(root/'bootstrap/backup-receipt.json'),
                   'backups': {n: checksum for n in names}}
        save('originals/journal.json', journal)
        for name in names: (root/'originals'/(name+'.img')).write_bytes(original)
        return {'partitions': partitions, 'images': plan['images']}, journal

    def test_restore_rejects_receipt_verified_couch_backups(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release, journal = self.fixture(root)
            original = json.loads((root/'bootstrap/journal.json').read_text())
            original['original_os'] = 'Couch'
            (root/'bootstrap/journal.json').write_text(json.dumps(original))
            receipt = json.loads((root/'bootstrap/backup-receipt.json').read_text())
            receipt['journal_sha256'] = wifi_install.digest(root/'bootstrap/journal.json')
            (root/'bootstrap/backup-receipt.json').write_text(json.dumps(receipt))
            journal['bootstrap_receipt_sha256'] = wifi_install.digest(root/'bootstrap/backup-receipt.json')
            (root/'originals/journal.json').write_text(json.dumps(journal))
            prior = wifi_retry.validate(root, release, lambda _: None)
            self.assertEqual(prior['original_os'], 'Couch')
            with self.assertRaises(wifi_retry.core.InstallError):
                wifi_retry.require_android_restore(prior)
            wifi_retry.require_android_restore({'original_os': 'Android'})

    def test_only_complete_unchanged_originals_can_restart(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release, journal = self.fixture(root)
            prior = wifi_retry.validate(root, release, lambda _: None)
            self.assertEqual(prior['journal'], journal)
            (root/'originals/userdata.img').write_bytes(b'X'*4096)
            with self.assertRaises(wifi_retry.core.InstallError):
                wifi_retry.validate(root, release, lambda _: None)

    def test_boot_write_or_missing_backup_completion_refuses_restart(self):
        for change in ({'writes': {'boot': 'writing'}}, {'backup_complete': False}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                release, journal = self.fixture(root)
                journal.update(change)
                (root/'originals/journal.json').write_text(json.dumps(journal))
                with self.assertRaises(wifi_retry.core.InstallError):
                    wifi_retry.validate(root, release, lambda _: None)

    def exercise_bootstrap(self, changed_live_calibration=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release, _ = self.fixture(root)
            prior = wifi_retry.validate(root, release, lambda _: None)
            destination = root/'restart-bootstrap'
            args = SimpleNamespace(stage_sha256='c'*64, stage_image=root/'new-stage.img',
                bus=1, timeout=30, checkout=root, loader=root/'loader', loader_sha256='d'*64,
                lock_dir=root/'locks', boot_after_capture=True, confirm_cid_sha256='e'*64)
            originals = prior['journal']['backups']
            description = {'capacity': prior['baseline']['capacity'], 'partitions': release['partitions'],
                'cid_encoding': 'mt6580-legacy-le32-registers',
                'runtime_cid_sha256': hashlib.sha256(bytes.fromhex(prior['baseline']['cid'])).hexdigest()}
            reader = Mock(description=description)
            live_hashes = {**originals, 'boot': 'a'*64, 'odmdtbo': 'f'*64}
            if changed_live_calibration: live_hashes['nvdata'] = '0'*64
            reader.hash.side_effect = lambda n: live_hashes[n]
            writer = Mock(description=description)
            writer.hash.side_effect = lambda n: {**originals, 'boot': 'c'*64, 'odmdtbo': 'f'*64}[n]
            class Device:
                def __getattr__(self, name): return getattr(writer, name)
                def __enter__(self): return writer
                def __exit__(self, *args): pass
            @contextmanager
            def session(*args, **kwargs):
                self.assertIs(kwargs['device_factory'](None, 'fixture'), writer)
                yield Device()
            package_baseline = copy.deepcopy(prior['baseline'])
            package_baseline['identity_sha256']['nvdata'] = '9'*64
            validated = (package_baseline, release, root, destination, (1,), {'odmdtbo': 'f'*64})
            with patch.object(wifi_retry.bootstrap, 'validate', return_value=validated):
                if changed_live_calibration:
                    with self.assertRaisesRegex(wifi_retry.core.InstallError, 'Runtime identity baseline differs: nvdata'):
                        wifi_retry.bootstrap_stage(args, prior, session=session,
                            reader_factory=lambda *_: reader, writer_factory=lambda *a, **k: writer)
                    writer.write.assert_not_called()
                    self.assertFalse((destination/'backup-receipt.json').exists())
                    return
                wifi_retry.bootstrap_stage(args, prior, session=session,
                    reader_factory=lambda *_: reader, writer_factory=lambda *a, **k: writer)
            writer.write.assert_called_once_with('boot', args.stage_image)
            for name in wifi_install.IDENTITY:
                self.assertIn((name,), [c.args for c in reader.hash.call_args_list])
                self.assertIn((name,), [c.args for c in writer.hash.call_args_list])
            receipt = json.loads((destination/'backup-receipt.json').read_text())
            self.assertTrue(receipt['complete'])
            self.assertEqual(receipt['originals']['boot'], originals['boot'])
            self.assertEqual((destination/'boot.img').read_bytes(), b'O'*4096)

    def test_stale_package_calibration_uses_verified_capture_and_checks_live_device(self):
        self.exercise_bootstrap()

    def test_changed_live_calibration_still_prevents_every_write(self):
        self.exercise_bootstrap(changed_live_calibration=True)

    def test_restart_tracks_the_completed_stage_after_a_wifi_entry_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            release,journal=self.fixture(root)
            prior=wifi_retry.validate(root,release,lambda _:None)
            latest=root/'wifi-later/bootstrap';latest.mkdir(parents=True)
            record={'purpose':'explicit-wifi-restart','complete':True,'write':'verified',
                'predecessor':str(root.resolve()),'stage_sha256':'c'*64,
                'originals':{n:journal['backups'][n] for n in ('boot','recovery')}}
            (latest/'journal.json').write_text(json.dumps(record))
            (latest/'baseline.json').write_text(json.dumps(prior['baseline']))
            (latest/'backup-receipt.json').write_text(json.dumps({'complete':True,
                'journal_sha256':wifi_install.digest(latest/'journal.json'),'originals':record['originals']}))
            self.assertEqual(wifi_retry.latest_completed_stage(prior,root),'c'*64)
            record['stage_sha256']='d'*64
            (latest/'journal.json').write_text(json.dumps(record))
            with self.assertRaisesRegex(wifi_retry.core.InstallError,'Latest RAM-stage receipt changed'):
                wifi_retry.latest_completed_stage(prior,root)

    def test_restore_plan_uses_exact_android_originals_without_customization_or_shrinking(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release, journal = self.fixture(root)
            (root/'bootstrap/boot.img').write_bytes((root/'originals/boot.img').read_bytes())
            with patch.object(wifi_install, 'customize_userdata') as customize, \
                    patch.object(wifi_install, 'compact_userdata') as compact:
                plan, paths = wifi_install.make_plan(release, root, root/'bootstrap', 'a'*64,
                    None, root/'restore-plan', lambda _: None,
                    reused_backups=journal['backups'], restore_originals=root/'originals')
                customize.assert_not_called(); compact.assert_not_called()
            self.assertFalse(plan['skip_userdata_backup'])
            for name, image in plan['images'].items():
                self.assertEqual(paths[name], root/'originals'/(name+'.img'))
                self.assertEqual(image['sha256'], journal['backups'][name])
                self.assertEqual(image['size'], release['partitions'][name]['size'])
            self.assertEqual(json.loads((root/'restore-plan/operation.json').read_text())['operation'],
                             'restore-original-android')
            (root/'originals/userdata.img').write_bytes(b'X'*4096)
            with self.assertRaisesRegex(ValueError, 'Android original changed'):
                wifi_install.make_plan(release, root, root/'bootstrap', 'a'*64,
                    None, root/'bad-restore-plan', lambda _: None,
                    reused_backups=journal['backups'], restore_originals=root/'originals')


if __name__ == '__main__':
    unittest.main()
