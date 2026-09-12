from contextlib import contextmanager, redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch

from couch_install import IDENTITY_PARTITIONS, InstallError, read_json
import enroll_android as enrollment


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.cid = '1234567890abcdef1234567890abcdef'
        self.data = {name: (name.encode()*4096)[:4096]
                     for name in IDENTITY_PARTITIONS | {'boot', 'recovery', 'odmdtbo', 'userdata', 'flashinfo'}}
        # Keep userdata immediately before the trailing reserve.
        names = sorted(set(self.data)-{'userdata', 'flashinfo'}) + ['userdata', 'flashinfo']
        self.parts = {n: {'offset': (i+1)*4096, 'size': 4096} for i, n in enumerate(names)}
        self.offsets = {n: p['offset'] for n, p in self.parts.items() if n != 'flashinfo'}
        self.files = {}
        for name in ('boot', 'odmdtbo', 'preloader'):
            self.files[name+'.img'] = self.root/(name+'.img')
            self.files[name+'.img'].write_bytes(self.data.get(name, b'EMI')[:64])
        identity = self.root/'identity.json'
        identity.write_text(json.dumps({'device_id':'FIXTURE1234', 'wifi_mac':'02:00:00:00:00:01', 'bluetooth_mac':'02:00:00:00:00:02'}))
        self.args = NS(identity=identity, backup_dir=self.root/'capture', bus=1, ports='2',
                       timeout=10, official_inputs=self.root, checkout=self.root, loader=self.root/'loader',
                       loader_sha256='1'*64, lock_dir=self.root/'locks', serial='fixture')
        self.cleanup_failure = False
        self.corrupt_readback = False
        outer = self
        class Reader:
            description = {'hwcode': 0x6580, 'cid_encoding': 'mt6580-legacy-le32-registers',
                           'runtime_cid_sha256': hashlib.sha256(bytes.fromhex(outer.cid)).hexdigest(),
                           'capacity': 1024*1024, 'partitions': outer.parts, 'storage_id': 'fixture'}
            def chunks(self, name):
                yield outer.data[name]
            def hash(self, name):
                return '0'*64 if outer.corrupt_readback and name == 'recovery' else hashlib.sha256(outer.data[name]).hexdigest()
            def backup_identity(self, destination, confirm):
                destination.mkdir(mode=0o700)
                for name in IDENTITY_PARTITIONS:
                    (destination/(name+'.img')).write_bytes(outer.data[name])
                return {'complete': True, 'backups': {n: self.hash(n) for n in IDENTITY_PARTITIONS}}
        self.reader = Reader()

    @contextmanager
    def session(self, *args, **kwargs):
        self.assertFalse(kwargs['boot_after_capture'])
        yield self.reader
        if self.cleanup_failure:
            raise InstallError('cleanup failed')

    def run_capture(self):
        with patch.object(enrollment, 'official_inputs', return_value=(self.files, self.offsets)), \
             patch.object(enrollment, 'source_pin'), patch.object(enrollment, 'loader_bytes'), redirect_stdout(io.StringIO()):
            return enrollment.enroll(self.args, session=self.session,
                    android=lambda *a: {'cid': self.cid, 'android_serial_sha256': '2'*64})

    def test_first_capture_records_observed_provenance_and_retains_original_recovery(self):
        result = self.run_capture()
        self.assertFalse(result['prior_baseline'])
        self.assertFalse(result['identity_decoded'])
        saved = self.args.backup_dir/'android-identity.json'
        self.assertEqual(read_json(saved), read_json(self.args.identity))
        self.assertEqual(result['android_identity_record_sha256'], hashlib.sha256(saved.read_bytes()).hexdigest())
        self.assertEqual((self.args.backup_dir/'recovery.img').read_bytes(), self.data['recovery'])
        self.assertTrue(read_json(self.args.backup_dir/'enrollment-journal.json')['complete'])
        self.assertEqual(read_json(self.args.backup_dir/'baseline.json')['identity_sha256'],
                         {n: self.reader.hash(n) for n in IDENTITY_PARTITIONS})

    def test_mismatched_android_cid_stops_before_backup(self):
        self.reader.description = {**self.reader.description, 'runtime_cid_sha256': '0'*64}
        with self.assertRaisesRegex(InstallError, 'differs from selected Android'):
            self.run_capture()
        self.assertFalse(self.args.backup_dir.exists())

    def test_generic_layout_or_modified_stock_image_does_not_establish_model(self):
        self.files['boot.img'].write_bytes(b'wrong stock')
        with self.assertRaisesRegex(InstallError, 'differs from official'):
            self.run_capture()
        self.assertFalse(self.args.backup_dir.exists())
        self.reader.description = {**self.reader.description, 'partitions': {**self.parts, 'unknown': {'offset': 900000, 'size': 512}}}
        with self.assertRaises(InstallError):
            self.run_capture()

    def test_shortened_calibration_partition_rejected_before_backup(self):
        parts = {name: dict(value) for name, value in self.parts.items()}
        parts['nvram']['size'] -= 512
        self.reader.description = {**self.reader.description, 'partitions': parts}
        with self.assertRaisesRegex(InstallError, 'sizes differ from official'):
            self.run_capture()
        self.assertFalse(self.args.backup_dir.exists())

    def test_failed_readback_or_cleanup_never_publishes_baseline(self):
        self.corrupt_readback = True
        with self.assertRaisesRegex(InstallError, 'readback mismatch'):
            self.run_capture()
        self.assertFalse((self.args.backup_dir/'baseline.json').exists())
        self.args.backup_dir = self.root/'cleanup-failure'
        self.corrupt_readback = False
        self.cleanup_failure = True
        with self.assertRaisesRegex(InstallError, 'cleanup failed'):
            self.run_capture()
        self.assertFalse((self.args.backup_dir/'baseline.json').exists())

    def test_existing_output_or_invalid_saved_identity_rejected(self):
        saved = self.args.identity.read_bytes()
        self.args.identity.write_text('{}')
        with self.assertRaises(InstallError):
            self.run_capture()
        self.args.identity.write_bytes(saved)
        self.args.backup_dir.mkdir()
        with self.assertRaises(InstallError):
            self.run_capture()

    def test_android_query_requires_authorized_exact_physical_device_without_reboot(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            return NS(returncode=0, stdout='List of devices attached\nfixture\tdevice\n' if argv[1:] == ['devices'] else self.cid+'\n')
        usb = lambda: [NS(bus=1, port_numbers=(2,), serial_number='fixture')]
        value = enrollment.android_identity('fixture', 1, (2,), run=run, enumerate_usb=usb)
        self.assertEqual(value['cid'], self.cid)
        self.assertEqual(len(calls), 2)
        self.assertFalse(any('reboot' in c for c in calls))
        with self.assertRaises(InstallError):
            enrollment.android_identity('other', 1, (2,), run=run, enumerate_usb=usb)
        self.assertEqual(len(calls), 2)
