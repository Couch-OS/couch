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
from test_fresh_os import binding_fixture


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
            binding = binding_fixture()
            binding['rootfs_archive_sha256'] = checksum(data)
            (root / 'fresh-core.json').write_text(json.dumps(binding))
            for kind in ('couch-packaged-staging', 'couch-private-vendor-staging'):
                (root / 'staging.json').write_text(json.dumps({
                    'kind': kind, 'installable': False, 'archive_sha256': checksum(data),
                    'source_commit': binding['source_commit']}))
                with patch('prepare_public_userdata.build_ext4') as builder:
                    with self.assertRaises(StageError):
                        prepare(root, root / 'tools', {}, root / 'out', root / 'fresh-core.json')
                    builder.assert_not_called()

    def test_fresh_binding_is_checked_before_builder_and_retained_in_image_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode='w'):
                pass
            data = raw.getvalue()
            (root / 'rootfs-staging.tar.gz').write_bytes(data)
            binding = binding_fixture()
            binding['rootfs_archive_sha256'] = checksum(data)
            (root / 'staging.json').write_text(json.dumps(dict(kind='couch-packaged-staging',
                installable=False, source_commit=binding['source_commit'], archive_sha256=checksum(data))))
            receipt = root / 'fresh-core.json'
            receipt.write_text(json.dumps(binding))
            output = root / 'output'
            with patch('prepare_public_userdata.build_ext4') as builder:
                with self.assertRaisesRegex(StageError, 'requires --fresh-core'):
                    prepare(root, root / 'tools', {}, output)
                bad = {**binding, 'rootfs_archive_sha256': '0' * 64}
                receipt.write_text(json.dumps(bad))
                with self.assertRaisesRegex(StageError, 'rootfs binding differs'):
                    prepare(root, root / 'tools', {}, output, receipt)
                builder.assert_not_called()
                receipt.write_text(json.dumps(binding))
                output.mkdir()
                builder.return_value = {'schema': 1, 'image': {'size': 123}, 'rootfs_archive_sha256': checksum(data)}
                result = prepare(root, root / 'tools', {}, output, receipt)
            self.assertEqual(result['fresh_core'], binding)
            self.assertEqual(json.loads((output / 'image.json').read_text())['fresh_core'], binding)

    def test_ram_subset_is_exact_pinned_inventory_and_excludes_unneeded_modem(self):
        root = PINS
        names = json.loads((root / 'ha100_ram_runtime.json').read_text())
        pins = json.loads((root / 'ha100_official_runtime.json').read_text())
        self.assertEqual(len(names), 19)
        self.assertEqual(len(set(names)), 19)
        self.assertTrue(set(names) <= {entry['path'] for entry in pins['files']})
        self.assertIn('vendor/bin/wmt_launcher', names)
        self.assertNotIn('vendor/firmware/modem_1_wg_n.img', names)
