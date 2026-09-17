import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from clean_stage import StageError, checksum
from installer_pins import PINS
from prepare_public_userdata import prepare


class PublicPayloadTests(unittest.TestCase):
    def test_owner_path_and_private_staging_stop_before_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode='w') as archive:
                item = tarfile.TarInfo('opt/couch/vendor/bin/wmt_loader')
                item.size = 3
                archive.addfile(item, io.BytesIO(b'bad'))
            data = raw.getvalue()
            (root / 'rootfs-staging.tar.gz').write_bytes(data)
            for kind in ('couch-packaged-staging', 'couch-private-vendor-staging'):
                (root / 'staging.json').write_text(json.dumps({
                    'kind': kind, 'installable': False, 'archive_sha256': checksum(data)}))
                with patch('prepare_public_userdata.build_ext4') as builder:
                    with self.assertRaises(StageError):
                        prepare(root, root / 'tools', {}, root / 'out')
                    builder.assert_not_called()

    def test_ram_subset_is_exact_pinned_inventory_and_excludes_unneeded_modem(self):
        root = PINS
        names = json.loads((root / 'ha100_ram_runtime.json').read_text())
        pins = json.loads((root / 'ha100_official_runtime.json').read_text())
        self.assertEqual(len(names), 19)
        self.assertEqual(len(set(names)), 19)
        self.assertTrue(set(names) <= {entry['path'] for entry in pins['files']})
        self.assertIn('vendor/bin/wmt_launcher', names)
        self.assertNotIn('vendor/firmware/modem_1_wg_n.img', names)
