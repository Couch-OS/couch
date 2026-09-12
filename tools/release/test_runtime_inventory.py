import io
from pathlib import Path
import struct
import tempfile
import unittest
from clean_stage import StageError, artifact_destination
from runtime_inventory import arm_static, cpio_files, embedded_web, VENDOR_REQUIRED, RUNTIME, audit


def elf(kind=1, machine=40):
    data = bytearray(84)
    data[:6] = b'\x7fELF\x01\x01'
    struct.pack_into('<H', data, 18, machine)
    struct.pack_into('<I', data, 28, 52)
    struct.pack_into('<HH', data, 42, 32, 1)
    struct.pack_into('<I', data, 52, kind)
    return bytes(data)


def cpio(items):
    output = bytearray()
    for name, content in [*items, ('TRAILER!!!', b'')]:
        encoded = name.encode() + b'\0'
        values = (1, 0o100644, 0, 0, 1, 0, len(content), 0, 0, 0, 0, len(encoded), 0)
        output += b'070701' + ''.join(f'{x:08x}' for x in values).encode() + encoded
        output += b'\0' * (-len(output) % 4)
        output += content
        output += b'\0' * (-len(output) % 4)
    return bytes(output)


class RuntimeInventoryTests(unittest.TestCase):
    def test_coreelec_inventory_requires_static_binary_and_records_its_hash(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = 'opt/couch/couch-coreelec'
            missing = audit(root)
            self.assertTrue(any(item.startswith(destination + ':') for item in missing['blockers']))
            source = root / RUNTIME['couch-coreelec']
            source.parent.mkdir(parents=True)
            source.write_bytes(elf())
            included = audit(root)
            artifact = next(item for item in included['artifacts'] if item['destination'] == destination)
            self.assertEqual(artifact['sha256'], hashlib.sha256(elf()).hexdigest())
            self.assertEqual(artifact['mode'], 0o755)
            self.assertEqual(artifact_destination(destination), destination)
            source.write_bytes(elf(3))
            rejected = audit(root)
            self.assertFalse(any(item['destination'] == destination for item in rejected['artifacts']))
            self.assertTrue(any(item.startswith(destination + ':') for item in rejected['blockers']))

    def test_sonos_inventory_requires_static_binary_and_records_its_hash(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = 'opt/couch/couch-sonos'
            missing = audit(root)
            self.assertTrue(any(item.startswith(destination + ':') for item in missing['blockers']))
            source = root / RUNTIME['couch-sonos']
            source.parent.mkdir(parents=True)
            source.write_bytes(elf())
            included = audit(root)
            artifact = next(item for item in included['artifacts'] if item['destination'] == destination)
            self.assertEqual(artifact['sha256'], hashlib.sha256(elf()).hexdigest())
            self.assertEqual(artifact['mode'], 0o755)
            self.assertEqual(artifact_destination(destination), destination)
            source.write_bytes(elf(3))
            rejected = audit(root)
            self.assertFalse(any(item['destination'] == destination for item in rejected['artifacts']))
            self.assertTrue(any(item.startswith(destination + ':') for item in rejected['blockers']))

    def test_runtime_executables_must_be_static_arm32(self):
        arm_static(elf())
        for data in (elf(2), elf(3), elf(machine=62), elf()[:70], b'not ELF'):
            with self.assertRaises(StageError):
                arm_static(data)

    def test_complete_web_bundle_must_be_in_daemon(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); dist = root / 'web/couch-web/dist'; dist.mkdir(parents=True)
            for name, data in [('index.html', b'<html>fixture</html>'), ('web.js', b'fixture JS'), ('web.wasm', b'wasm fixture')]:
                (dist / name).write_bytes(data)
            self.assertEqual(len(embedded_web(root, b'<html>fixture</html>fixture JSwasm fixture')), 3)
            with self.assertRaisesRegex(StageError, 'does not embed'):
                embedded_web(root, b'<html>fixture</html>fixture JS')
            (dist / 'web.wasm').unlink()
            with self.assertRaisesRegex(StageError, 'Incomplete web'):
                embedded_web(root, b'<html>fixture</html>fixture JS')

    def test_ramdisk_audit_rejects_private_state_and_bad_bounds(self):
        self.assertEqual(cpio_files(cpio([('init', b'clean')]))['init'], b'clean')
        for name in ('extra/props.tar.gz', 'root/.ssh/authorized_keys', '../escape', 'opt/couch/config.json'):
            with self.subTest(name=name), self.assertRaises(StageError):
                cpio_files(cpio([(name, b'private')]))
        for data in (cpio([('init', b'clean')])[:-110], b'bad cpio',
                     cpio([('init', b'clean')]) + cpio([('extra/props.tar.gz', b'private')])):
            with self.assertRaises(StageError):
                cpio_files(data)

    def test_vendor_contexts_are_required_but_not_permitted_as_clean_artifacts(self):
        self.assertIn('system/etc/selinux/plat_property_contexts', VENDOR_REQUIRED)
        self.assertIn('vendor/etc/selinux/nonplat_property_contexts', VENDOR_REQUIRED)
        with self.assertRaises(StageError):
            artifact_destination('opt/couch/vendor/firmware/ROMv2_lm_patch_1_1_hdr.bin')
        self.assertEqual(artifact_destination('opt/couch/licenses/Lato-OFL.txt'),
                         'opt/couch/licenses/Lato-OFL.txt')
        with self.assertRaises(StageError):
            artifact_destination('opt/couch/licenses/private.txt')


if __name__ == '__main__':
    unittest.main()
