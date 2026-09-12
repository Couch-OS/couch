import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('busybox_build', Path(__file__).parents[1] / 'build-busybox.py')
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class BusyBoxBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        elf = bytearray(84)
        elf[:6] = b'\x7fELF\x01\x01'
        struct.pack_into('<H', elf, 18, 40)
        struct.pack_into('<I', elf, 28, 52)
        struct.pack_into('<I', elf, 36, 0x5000400)
        struct.pack_into('<HH', elf, 42, 32, 1)
        struct.pack_into('<I', elf, 52, 1)
        (self.root / 'busybox-armv7l').write_bytes(elf)
        (self.root / 'busybox-source.tar.bz2').write_bytes(b'fixture-source')
        for name in ('compiler.txt', 'installed-packages.txt', 'toolchain-closure.json', 'smoke.txt'):
            (self.root / name).write_text('fixture')
        (self.root / 'applets.txt').write_bytes((build.RECIPE / 'required-applets.txt').read_bytes())
        self.sha = build.digest(self.root / 'busybox-source.tar.bz2')
        self.receipt = {'schema': 1, 'source_sha256': self.sha, 'builder_image': build.IMAGE,
                        'recipe_sha256': {p.name: build.digest(p) for p in build.RECIPE.iterdir() if p.is_file()},
                        'artifacts': {p.name: build.digest(p) for p in self.root.iterdir()}}

    def verify(self):
        (self.root / 'receipt.json').write_text(json.dumps(self.receipt))
        with patch.object(build, 'SOURCE_SHA', self.sha):
            return build.verify(self.root)

    def test_valid_receipt_and_modified_binary_or_recipe(self):
        self.verify()
        (self.root / 'busybox-armv7l').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'artifact changed'):
            self.verify()
        self.receipt['recipe_sha256'] = {}
        with self.assertRaisesRegex(ValueError, 'recipe changed'):
            self.verify()

    def test_incomplete_applet_inventory_cannot_be_attested(self):
        (self.root / 'applets.txt').write_text('sh\n')
        self.receipt['artifacts']['applets.txt'] = build.digest(self.root / 'applets.txt')
        with self.assertRaisesRegex(ValueError, 'applet missing'):
            self.verify()

    def test_dynamic_or_soft_float_binaries_are_rejected(self):
        path = self.root / 'busybox-armv7l'
        original = path.read_bytes()
        for header in (2, 3):
            data = bytearray(original)
            struct.pack_into('<I', data, 52, header)
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, 'must be static'):
                build.static_arm(path)
        data = bytearray(original)
        struct.pack_into('<I', data, 36, 0x5000200)
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'hard-float'):
            build.static_arm(path)


if __name__ == '__main__':
    unittest.main()
