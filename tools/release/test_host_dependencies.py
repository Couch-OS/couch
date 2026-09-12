import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch
import zipfile

from host_dependencies import download, prepare, smoke


class DependenciesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.archive = self.root / 'input.zip'
        self.output = self.root / 'output'
        self.data = b'fixture; never executable'
        self.metadata = {'schema': 1, 'component': 'fixture', 'version': '37.0.1',
                         'mtk_revision': '1' * 40, 'platforms': {'fixture': {}}}

    def fixture(self, extra=None):
        with zipfile.ZipFile(self.archive, 'w') as bundle:
            bundle.writestr('platform-tools/adb', self.data)
            if extra:
                name, data = extra
                if isinstance(name, str):
                    member = zipfile.ZipInfo()
                    # ZipInfo's constructor normalizes host separators on Windows.
                    # Preserve the malicious raw member name in this fixture.
                    member.filename = name
                else:
                    member = name
                bundle.writestr(member, data)
        raw = self.archive.read_bytes()
        self.metadata['platforms']['fixture'] = {
            'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(),
            'adb': 'platform-tools/adb', 'files': {'platform-tools/adb': {
                'size': len(self.data), 'sha256': hashlib.sha256(self.data).hexdigest()}}}

    def test_archive_corruption_creates_no_output(self):
        self.fixture()
        raw = bytearray(self.archive.read_bytes())
        raw[40] ^= 1
        self.archive.write_bytes(raw)
        with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
            prepare(self.archive, self.output, 'fixture', self.metadata)
        self.assertFalse(self.output.exists())

    def test_unsafe_unused_member_is_rejected(self):
        for name in ('platform-tools/../../escape', '/platform-tools/escape',
                     'platform-tools/adb:stream', 'platform-tools\\escape'):
            self.fixture((name, b'bad'))
            with self.assertRaisesRegex(ValueError, 'archive path'):
                prepare(self.archive, self.output, 'fixture', self.metadata)
            self.assertFalse(self.output.exists())

    def test_symlink_member_is_rejected(self):
        member = zipfile.ZipInfo('platform-tools/link')
        member.create_system = 3
        member.external_attr = 0o120777 << 16
        self.fixture((member, b'adb'))
        with self.assertRaisesRegex(ValueError, 'symlink'):
            prepare(self.archive, self.output, 'fixture', self.metadata)

    def test_member_pin_mismatch_creates_no_output(self):
        self.fixture()
        self.metadata['platforms']['fixture']['files']['platform-tools/adb']['sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'Member hash'):
            prepare(self.archive, self.output, 'fixture', self.metadata)
        self.assertFalse(self.output.exists())

    def test_receipt_is_last_and_existing_output_preserved(self):
        self.fixture()
        receipt = prepare(self.archive, self.output, 'fixture', self.metadata)
        self.assertEqual(receipt, json.loads((self.output / 'receipt.json').read_text()))
        self.assertEqual(self.data, (self.output / 'platform-tools/adb').read_bytes())
        self.assertFalse(receipt['complete_mtk_runtime'])
        with self.assertRaisesRegex(ValueError, 'new output'):
            prepare(self.archive, self.output, 'fixture', self.metadata)
        self.assertEqual(self.data, (self.output / 'platform-tools/adb').read_bytes())

    def test_cross_platform_smoke_refused_without_execution(self):
        self.fixture()
        with self.assertRaisesRegex(ValueError, 'different host'):
            smoke(self.output, 'fixture', self.metadata)

    def test_trickled_download_rechecks_total_deadline(self):
        response = MagicMock()
        response.__enter__.return_value = response
        response.read1.return_value = b'x'
        opener = MagicMock()
        opener.open.return_value = response
        with patch('host_dependencies.urllib.request.build_opener', return_value=opener), \
                patch('host_dependencies.time.monotonic', side_effect=[0, 1, 181]):
            with self.assertRaisesRegex(ValueError, 'deadline exceeded'):
                download({'url': 'https://dl.google.com/android/repository/fixture', 'size': 100},
                         self.root / 'download')
        response.read.assert_not_called()
        response.read1.assert_called_once_with(64 * 1024)


if __name__ == '__main__':
    unittest.main()
