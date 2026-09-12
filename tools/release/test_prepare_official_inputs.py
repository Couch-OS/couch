import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import prepare_official_inputs as inputs


class OwnerInputTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.ota = self.root/'official.zip'
        self.members = {name: name.encode() for name in inputs.BOOTSTRAP_MEMBERS}
        with zipfile.ZipFile(self.ota, 'w') as bundle:
            for name, data in self.members.items():
                bundle.writestr(name, data)
            bundle.writestr('lk.img', b'must not extract')
            bundle.writestr('../escape', b'must not extract')
        self.pin = self.root/'pin.json'
        self.pin.write_text(json.dumps({'size': self.ota.stat().st_size,
            'sha256': hashlib.sha256(self.ota.read_bytes()).hexdigest(), 'version': 'fixture'}))
        self.approved = {name: (len(data), hashlib.sha256(data).hexdigest()) for name, data in self.members.items()}

    def prepare(self, destination):
        with patch.object(inputs.official_runtime, 'PIN', self.pin), patch.object(inputs, 'BOOTSTRAP_MEMBERS', self.approved):
            return inputs.prepare(self.ota, destination, runtime=False)

    def test_only_approved_members_are_extracted_and_not_labeled_originals(self):
        destination = self.root/'output'
        result = self.prepare(destination)
        self.assertEqual({p.name for p in (destination/'bootstrap').iterdir()}, set(self.members))
        self.assertFalse(result['original_device_backup'])
        self.assertFalse(result['installable'])
        self.assertFalse(result['redistribution_authorized'])
        self.assertFalse((self.root/'escape').exists())
        self.assertEqual(destination.stat().st_mode & 0o077, 0)

    def test_wrong_archive_or_member_hash_and_existing_destination_fail_closed(self):
        destination = self.root/'output'
        self.approved['boot.img'] = (len(self.members['boot.img']), '0'*64)
        with self.assertRaises(ValueError):
            self.prepare(destination)
        self.assertFalse(destination.exists())
        self.ota.write_bytes(self.ota.read_bytes()+b'x')
        with self.assertRaises(ValueError):
            self.prepare(destination)
        destination.mkdir()
        with self.assertRaises(ValueError):
            self.prepare(destination)

    def test_vendor_extraction_failure_does_not_publish_partial_inputs(self):
        with patch.object(inputs.official_runtime, 'PIN', self.pin), \
             patch.object(inputs, 'BOOTSTRAP_MEMBERS', self.approved), \
             patch.object(inputs.official_runtime, 'extract', side_effect=ValueError('bad vendor')):
            with self.assertRaises(ValueError):
                inputs.prepare(self.ota, self.root/'output')
        self.assertFalse((self.root/'output').exists())
