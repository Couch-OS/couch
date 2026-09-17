import gzip
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parent


def standalone_builder(root):
    installer = root / 'tools/installer'
    image = installer / 'image'; image.mkdir(parents=True)
    (image / 'neutral_ramdisk.py').write_bytes((SOURCE / 'image/neutral_ramdisk.py').read_bytes())
    pins = installer / 'pins'; pins.mkdir()
    (pins / 'ha100_official_runtime.json').write_text(json.dumps({'sha256': 'a' * 64, 'files': []}))
    stage = installer / 'wifi-stage'; stage.mkdir()
    for name in ('init', 'wifi-init', 'dhcp'):
        (stage / name).write_text('#!/bin/sh\n')
    spec = importlib.util.spec_from_file_location('isolated_neutral_ramdisk', image / 'neutral_ramdisk.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class NeutralRamdiskTests(unittest.TestCase):
    def test_installer_only_builder_writes_reproducible_neutral_payload_without_release_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            builder = standalone_builder(root)
            busybox = root / 'busybox'; busybox.write_bytes(b'fixture busybox')
            service = root / 'probe'; service.write_bytes(b'COUCH_PRIVATE_WIFI_INSTALLER_V1')
            builder.alpine_files = lambda cache, filesystem=False: ({}, 'filesystem' if filesystem else 'runtime')
            builder.arm_static = lambda data: None
            first = builder.prepare(busybox, service, root / 'runtime-cache', root / 'filesystem-cache',
                                    None, None, root / 'first')
            second = builder.prepare(busybox, service, root / 'runtime-cache', root / 'filesystem-cache',
                                     None, None, root / 'second')
            self.assertEqual(first, second)
            self.assertEqual((root / 'first/installer.cpio.gz').read_bytes(),
                             (root / 'second/installer.cpio.gz').read_bytes())
            raw = gzip.decompress((root / 'first/installer.cpio.gz').read_bytes())
            self.assertEqual(builder.cpio_files(raw)['etc/couch-installer-mode'], b'private-install\n')
            self.assertEqual(json.loads((root / 'first/ramdisk.json').read_text()), first)
            self.assertEqual(first['owner_vendor_source_sha256'], 'a' * 64)


if __name__ == '__main__':
    unittest.main()
