import gzip
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

import prepare_probe_ramdisk as probe
import test_kernel_provenance as fixtures
from test_runtime_inventory import elf


class ProbeRamdiskTests(unittest.TestCase):
    def test_inventory_contains_only_recovery_block_device_and_no_persistent_init(self):
        raw = probe.ramdisk(b'init', b'busybox', b'probe')
        offset = 0
        blocks = []
        names = []
        while raw[offset:offset+6] == b'070701':
            fields = [int(raw[offset+6+i*8:offset+14+i*8], 16) for i in range(13)]
            mode, size, major, minor, length = fields[1], fields[6], fields[9], fields[10], fields[11]
            name = raw[offset+110:offset+110+length-1].decode()
            names.append(name)
            if name == 'TRAILER!!!':
                break
            if mode & 0o170000 == 0o060000:
                blocks.append((name, mode, major, minor))
            offset = (offset+110+length+3) & ~3
            offset = (offset+size+3) & ~3
        self.assertEqual(blocks, [('dev/mmcblk0p9', 0o060400, 179, 9)])
        self.assertNotIn('dev/ttyGS0', names)  # Its major must be discovered at runtime.

    def test_pack_roundtrip_pinned_kernel_private_gates_and_missing_service(self):
        fixture = fixtures.KernelProvenanceTests()
        fixture.setUp()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = {name: root / name for name in ('template', 'manifest', 'pin', 'busybox', 'service')}
            paths['template'].write_bytes(fixture.boot)
            paths['manifest'].write_text(json.dumps(fixture.manifest))
            paths['pin'].write_text(json.dumps(fixture.pin))
            paths['busybox'].write_bytes(elf())
            with patch.object(probe, 'PIN', paths['pin']):
                with self.assertRaises(ValueError):
                    probe.prepare(paths['template'], paths['manifest'], paths['busybox'], paths['service'], root/'missing')
                self.assertFalse((root/'missing').exists())
                paths['service'].write_bytes(elf())
                result = probe.prepare(paths['template'], paths['manifest'], paths['busybox'], paths['service'], root/'out')
                self.assertFalse(result['installable'] or result['physical_boot_verified'] or result['functionfs_verified'])
                image = (root/'out/probe.img').read_bytes()
                self.assertEqual(len(image), 16777216)
                page = struct.unpack_from('<I', image, 36)[0]
                size = struct.unpack_from('<I', image, 8)[0]
                rd_size = struct.unpack_from('<I', image, 16)[0]
                start = page + ((size+page-1)//page)*page
                entries = probe.cpio_files(gzip.decompress(image[start:start+rd_size]))
                self.assertEqual(entries['init'], (probe.REPO/'tools/installer/probe/init').read_bytes())
                self.assertNotIn('extra/boot-health.sh', entries)
                with self.assertRaises(ValueError):
                    probe.prepare(paths['template'], paths['manifest'], paths['busybox'], paths['service'], root/'out')

    def test_dynamic_or_invalid_service_rejected_without_output(self):
        with self.assertRaises(ValueError):
            probe.arm_static(b'placeholder, not an executable')


if __name__ == '__main__':
    unittest.main()
