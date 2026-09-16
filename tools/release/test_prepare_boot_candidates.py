import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import prepare_boot_candidates as candidate
import test_kernel_provenance as fixtures
from test_runtime_inventory import alpine_elf, elf


class BootCandidateTests(unittest.TestCase):
    def test_unverified_busybox_receipt_blocks_new_ramdisks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / 'build/busybox-source/receipt.json'
            receipt.parent.mkdir(parents=True)
            receipt.write_text('{malformed')
            with self.assertRaisesRegex(ValueError, 'verified BusyBox 1.37'):
                candidate.verified_busybox(root)

    def test_clean_full_size_images_keep_distinct_kernels_and_noninstallable_state(self):
        fixture = fixtures.KernelProvenanceTests()
        fixture.setUp()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, content in {'initramfs/init': b'normal init', 'recovery/init': b'recovery init',
                'initramfs/boot-health.sh': b'health', 'build/busybox-armv7l': elf(),
                'build/fbcon': elf(),
                'clients/target/armv7-unknown-linux-musleabihf/release/couch-bt-bridge': elf(),
                'clients/target/armv7-unknown-linux-musleabihf/release/couch-bt-hid': elf(),
                'build/bluez/couch-bluetoothd': alpine_elf(),
                'tools/mkcpio.py': (candidate.REPO / 'tools/mkcpio.py').read_bytes()}.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            normal, recovery, manifest, pin = [root / name for name in ('normal', 'recovery.img', 'manifest.json', 'pin.json')]
            normal.write_bytes(fixture.boot)
            other = bytearray(fixture.boot)
            other[2100] ^= 1
            recovery.write_bytes(other)
            manifest.write_text(json.dumps(fixture.manifest))
            pin.write_text(json.dumps(fixture.pin))
            with patch.object(candidate, 'PIN', pin), \
                    patch.object(candidate, 'verified_busybox', return_value=root / 'build/busybox-armv7l'):
                result = candidate.prepare(normal, recovery, manifest, root / 'out', root)
                self.assertIs(result['installable'], False)
                for role in ('boot', 'recovery'):
                    self.assertEqual((root / 'out' / (role + '.img')).stat().st_size, 16777216)
                    names = result['images'][role]['ramdisk_payload_files']
                    self.assertIn('init', names)
                    self.assertNotIn('extra/props.tar.gz', names)
                self.assertIn('extra/boot-health.sh', result['images']['boot']['ramdisk_payload_files'])
                # The whole Bluetooth stack rides here, because none of it can
                # be published in a runtime bundle (update_floor.py).
                for name in candidate.BOOT_EXTRA:
                    self.assertIn('extra/' + name, result['images']['boot']['ramdisk_payload_files'])
                    self.assertNotIn('extra/' + name, result['images']['recovery']['ramdisk_payload_files'])
                self.assertNotIn('extra/boot-health.sh', result['images']['recovery']['ramdisk_payload_files'])
                with self.assertRaises(ValueError):
                    candidate.prepare(normal, normal, manifest, root / 'bad', root)
                with self.assertRaises(ValueError):
                    candidate.prepare(normal, recovery, manifest, root / 'out', root)


if __name__ == '__main__':
    unittest.main()
