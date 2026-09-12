import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import subprocess
import unittest
from unittest.mock import patch

import prepare_wifi_ramdisk as wifi


class WifiRamdiskTests(unittest.TestCase):
    def test_loader_android_exit_requires_detected_transport(self):
        for detected in (False, True):
            with self.subTest(detected=detected), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'tmp').mkdir()
                (root / 'dev').mkdir()
                devices = root / 'devices'
                devices.write_text('154 mtk_wcn_detect\n153 mtk_wmt_WIFI_chrdev\n' +
                                   ('190 mtk_stp_wmt\n' if detected else ''))
                busybox = root / 'busybox'
                busybox.write_text('#!/bin/sh\ncase "$1" in mknod) exit 0;; *) exec "$@";; esac\n')
                busybox.chmod(0o700)
                loader = root / 'loader'
                loader.write_text('#!/bin/sh\nexit 255\n')
                loader.chmod(0o700)
                script = (wifi.REPO / 'tools/installer/wifi-stage/wifi-init').read_text()
                script = script.split('step transport\n')[0]
                script = script.replace('/tmp/', str(root / 'tmp') + '/')
                script = script.replace('/dev/', str(root / 'dev') + '/')
                script = script.replace('/proc/devices', str(devices))
                script = script.replace('/vendor/bin/wmt_loader', str(loader))
                script = script.replace('BB=/bin/busybox', 'BB=' + str(busybox))
                result = subprocess.run(['sh'], input=script, text=True, capture_output=True, timeout=5)
                self.assertEqual(result.returncode, 0 if detected else 1)
                log = (root / 'tmp/probe.log').read_text()
                self.assertIn('WiFi loader exit: 255', log)
                if not detected:
                    self.assertEqual((root / 'tmp/couch-wifi.error').read_text(), 'loader-exit\n')
                else:
                    self.assertIn('transport registered despite Android exit status', log)
                    self.assertFalse((root / 'tmp/couch-wifi.error').exists())

    def test_missing_radio_device_reports_failure_before_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tmp').mkdir()
            (root / 'dev').mkdir()
            busybox = root / 'busybox'
            busybox.write_text('#!/bin/sh\ncase "$1" in mknod|awk) exit 0;; *) exit 7;; esac\n')
            busybox.chmod(0o700)
            script = (wifi.REPO / 'tools/installer/wifi-stage/wifi-init').read_text()
            script = script.replace('/tmp/', str(root / 'tmp') + '/')
            script = script.replace('/dev/', str(root / 'dev') + '/')
            script = script.replace('BB=/bin/busybox', 'BB=' + str(busybox))
            result = subprocess.run(['sh'], input=script, text=True, capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 1)
            self.assertEqual((root / 'tmp/couch-wifi.status').read_text(), 'failed\n')
            self.assertEqual((root / 'tmp/couch-wifi.error').read_text(), 'detect-node\n')
            self.assertIn('WiFi failed: detect-node', (root / 'tmp/probe.log').read_text())
            self.assertFalse((root / 'tmp/couch-wpa_supplicant.conf').exists())

    def test_no_storage_nodes_except_readonly_recovery(self):
        raw = wifi.ramdisk({'init': b'init', 'bin/busybox': b'bb'})
        self.assertEqual(wifi.cpio_files(raw)['etc/firmware'], b'/vendor/firmware')
        offset, blocks = 0, []
        while True:
            fields = [int(raw[offset + 6 + i * 8:offset + 14 + i * 8], 16) for i in range(13)]
            size, length = fields[6], fields[11]
            name = raw[offset + 110:offset + 110 + length - 1].decode()
            if name == 'TRAILER!!!':
                break
            if fields[1] & 0o170000 == 0o060000:
                blocks.append((name, fields[1], fields[9], fields[10]))
            offset = (offset + 110 + length + 3) & ~3
            offset = (offset + size + 3) & ~3
        self.assertEqual(blocks, [('dev/mmcblk0p9', 0o060400, 179, 9)])

    def test_duplicate_elf_aliases_are_links_and_cellular_firmware_excluded(self):
        entries = wifi.cpio_files(wifi.ramdisk({'lib/loader.so': b'\x7fELFsame',
                                                'lib/libc.so': b'\x7fELFsame'}))
        self.assertIn(b'/lib/', entries['lib/loader.so'])
        self.assertNotIn('modem_1_wg_n.img', wifi.FIRMWARE)
        self.assertIn('WIFI_RAM_CODE_6580', wifi.FIRMWARE)

    def test_bootstrap_does_not_reuse_persistent_stage2_or_credentials(self):
        base = wifi.REPO / 'tools/installer/wifi-stage'
        scripts = '\n'.join((base / name).read_text() for name in ('init', 'wifi-init', 'dhcp'))
        for forbidden in ('mmcblk0p23', 'stage2.sh', 'props.tar', 'COUCH_WIFI_PSK=', 'mdev -s\n'):
            self.assertNotIn(forbidden, scripts)
        self.assertIn('/tmp/couch-wpa_supplicant.conf', scripts)
        self.assertIn('>/dev/null 2>&1', scripts)

    def test_vendor_manifest_cannot_self_authorize_changed_payload(self):
        pin = json.loads((wifi.REPO / 'tools/release/ha100_official_runtime.json').read_text())
        records = [dict(record) for record in pin['files']]
        records[0]['sha256'] = '0' * 64
        manifest = {'source_images': pin['images'], 'files': records}
        with patch.object(wifi, 'verify_bundle', return_value=manifest), \
                patch.object(wifi, 'audit') as audit:
            with self.assertRaisesRegex(ValueError, 'pinned official runtime'):
                wifi.vendor_files(Path('/unused'))
            audit.assert_not_called()

    def test_cached_package_hash_verified_before_parsing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'packages').mkdir()
            (root / 'packages/wpa_supplicant-1.apk').write_bytes(b'bad')
            (root / 'closure.json').write_text(json.dumps({
                'architecture': 'armv7', 'kind': 'couch-offline-package-closure',
                'files': {'packages/wpa_supplicant-1.apk': '0' * 64}}))
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                wifi.alpine_files(root)

    def test_library_symlink_cannot_escape_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'packages').mkdir()
            buffer = io.BytesIO()
            with tarfile.open(fileobj=buffer, mode='w:gz') as archive:
                member = tarfile.TarInfo('lib/libescape.so')
                member.type = tarfile.SYMTYPE
                member.linkname = '/etc/shadow'
                archive.addfile(member)
            data = buffer.getvalue()
            (root / 'packages/wpa_supplicant-1.apk').write_bytes(data)
            (root / 'closure.json').write_text(json.dumps({
                'architecture': 'armv7', 'kind': 'couch-offline-package-closure',
                'files': {'packages/wpa_supplicant-1.apk': hashlib.sha256(data).hexdigest()}}))
            with self.assertRaisesRegex(ValueError, 'Unsafe APK library link'):
                wifi.alpine_files(root)


if __name__ == '__main__':
    unittest.main()
