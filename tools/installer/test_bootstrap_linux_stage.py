from contextlib import contextmanager, redirect_stdout
import io
import json
from unittest.mock import patch
import unittest

import test_finalize_stock_recovery as fixtures
from bootstrap_linux_stage import run
from couch_install import InstallError, digest, read_json, save_json


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.FinalizeTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.args, self.reader = self.fixture.args, self.fixture.reader
        self.original_hash = self.reader.hash
        self.reader.hash = lambda name: self.args.recovery_sha256 if name == 'recovery' else self.original_hash(name)
        self.args.stage_image = self.args.recovery.parent / 'wifi-stage.img'
        self.size = self.reader.description['partitions']['boot']['size']
        self.args.stage_image.write_bytes(b'ANDROID!' + b'Z' * (self.size - 8))
        self.args.stage_sha256 = digest(self.args.stage_image)
        self.args.stage_metadata = self.args.stage_image.with_suffix('.json')
        save_json(self.args.stage_metadata, {'schema': 1, 'kind': 'private-ram-wifi-stage',
            'private_only': True, 'installable': False, 'redistribution_authorized': False,
            'file': 'wifi-stage.img', 'size': self.size, 'sha256': self.args.stage_sha256,
            'storage_operations': ['read-only recovery SHA-256']})
        self.args.stage_metadata_sha256 = digest(self.args.stage_metadata)
        self.writes = []
        self.backups = []
        self.exits = 0
        self.corrupt_backup = self.corrupt_readback = self.cleanup_fails = False

    @contextmanager
    def session(self, *args, **kwargs):
        device = kwargs['device_factory'](None, None)
        yield device
        if kwargs['boot_after_capture']:
            self.exits += 1
        if self.cleanup_fails:
            raise InstallError('cleanup failed')

    def writer(self, mtk, revision, **kwargs):
        self.assertEqual(set(kwargs['release']['images']), {'boot'})
        parent = self
        class Writer:
            description = parent.reader.description
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def backup(self, name, path):
                parent.backups.append(name)
                data = parent.args.recovery.read_bytes() if name == 'recovery' else b''.join(parent.reader.chunks(name))
                if parent.corrupt_backup: data = b'X' * len(data)
                path.write_bytes(data)
            def write(self, name, path):
                record = read_json(parent.args.backup_dir / 'journal.json')
                parent.assertTrue(record['backup_complete'])
                parent.assertEqual(record['write'], 'started')
                parent.assertEqual(set(record['originals']), {'boot', 'recovery'})
                parent.writes.append(name)
            def hash(self, name):
                parent.assertNotEqual(name, 'userdata')
                if name == 'boot' and parent.writes:
                    return '0'*64 if parent.corrupt_readback else parent.args.stage_sha256
                return parent.reader.hash(name)
        return Writer()

    def execute(self):
        with patch('finalize_stock_recovery.source_pin', return_value={}), \
             patch('bootstrap_linux_stage.BOOT_BYTES', self.size), redirect_stdout(io.StringIO()):
            run(self.args, session=self.session, reader_factory=lambda *a: self.reader, writer_factory=self.writer)

    def test_only_boot_written_after_two_verified_originals(self):
        self.execute()
        self.assertEqual(self.backups, ['boot', 'recovery'])
        self.assertEqual(self.writes, ['boot'])
        self.assertEqual(self.exits, 1)
        self.assertTrue(read_json(self.args.backup_dir / 'journal.json')['complete'])
        self.assertTrue(read_json(self.args.backup_dir / 'backup-receipt.json')['complete'])
        self.assertTrue((self.args.backup_dir / 'baseline.json').exists())

    def test_bad_original_prevents_any_write_and_exit(self):
        self.corrupt_backup = True
        with self.assertRaisesRegex(InstallError, 'Independent original boot'):
            self.execute()
        self.assertEqual(self.writes, [])
        self.assertEqual(self.exits, 0)
        self.assertFalse((self.args.backup_dir / 'baseline.json').exists())

    def test_bad_stage_readback_or_cleanup_never_publishes_receipt(self):
        self.corrupt_readback = True
        with self.assertRaisesRegex(InstallError, 'boot readback mismatch'):
            self.execute()
        self.assertEqual(self.exits, 0)
        self.assertFalse((self.args.backup_dir / 'backup-receipt.json').exists())

    def test_calibration_change_after_boot_write_prevents_exit(self):
        old = self.reader.hash
        self.reader.hash = lambda name: '0'*64 if name == 'nvdata' and self.writes else old(name)
        with self.assertRaisesRegex(InstallError, 'identity baseline differs: nvdata'):
            self.execute()
        self.assertEqual(self.writes, ['boot'])
        self.assertEqual(self.exits, 0)
        self.assertFalse((self.args.backup_dir / 'backup-receipt.json').exists())

    def test_cleanup_failure_blocks_final_baseline(self):
        self.cleanup_fails = True
        with self.assertRaisesRegex(InstallError, 'cleanup failed'):
            self.execute()
        self.assertFalse((self.args.backup_dir / 'baseline.json').exists())
        self.assertFalse(read_json(self.args.backup_dir / 'journal.json')['complete'])

    def test_wrong_metadata_purpose_or_pin_is_rejected_before_capture(self):
        original = self.args.stage_metadata_sha256
        self.args.stage_metadata_sha256 = '0'*64
        with self.assertRaisesRegex(InstallError, 'provenance pin'):
            self.execute()
        self.args.stage_metadata_sha256 = original
        metadata = read_json(self.args.stage_metadata)
        metadata['storage_operations'] = ['write userdata']
        save_json(self.args.stage_metadata, metadata)
        self.args.stage_metadata_sha256 = digest(self.args.stage_metadata)
        with self.assertRaisesRegex(InstallError, 'read-only RAM-stage'):
            self.execute()
        self.assertFalse(self.args.backup_dir.exists())

    def test_stock_recovery_mismatch_and_check_only_do_not_write(self):
        old = self.reader.hash
        self.reader.hash = lambda name: '0'*64 if name == 'recovery' else old(name)
        with self.assertRaisesRegex(InstallError, 'recovery differs'):
            self.execute()
        self.assertFalse(self.args.backup_dir.exists())
        self.args.check_only = True
        self.execute()
        self.assertEqual(self.writes, [])
        self.assertEqual(self.exits, 0)


if __name__ == '__main__': unittest.main()
