import hashlib
import os
from pathlib import Path
import pty
import re
import tempfile
import threading
import unittest
from unittest.mock import patch

import serial_ram_upload as upload


class SerialUploadTests(unittest.TestCase):
    def exercise(self, corrupt=False):
        master, slave = pty.openpty()
        errors = []
        data = bytes(range(256)) * 1024

        def receiver():
            try:
                command = b''
                while not command.endswith(b'\n'):
                    command += os.read(master, 4096)
                nonce = re.search(rb'READY_([0-9a-f]{32})', command)[1]
                # Echo contains token but must not trigger a substring match.
                os.write(master, command)
                ready = b'\nCOUCH_UPLOAD_READY_' + nonce + b'\r\n'
                for part in (ready[:7], ready[7:20], ready[20:]):
                    os.write(master, part)
                received = bytearray()
                while len(received) < len(data):
                    received.extend(os.read(master, min(997, len(data) - len(received))))
                self.assertEqual(bytes(received), data)
                digest = '0' * 64 if corrupt else hashlib.sha256(data).hexdigest()
                os.write(master, b'\nCOUCH_UPLOAD_DONE_' + nonce +
                         f' {len(data)} {digest}\n'.encode())
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=receiver, daemon=True)
        try:
            with tempfile.TemporaryDirectory() as directory:
                source = Path(directory) / 'image'
                source.write_bytes(data)
                thread.start()
                if corrupt:
                    with self.assertRaisesRegex(upload.UploadError, 'checksum'):
                        upload.upload(os.ttyname(slave), source, 2)
                else:
                    result = upload.upload(os.ttyname(slave), source, 2)
                    self.assertEqual(result['sha256'], hashlib.sha256(data).hexdigest())
                    self.assertRegex(result['path'], r'^/tmp/couch-upload-[0-9a-f]{32}$')
                thread.join(3)
                self.assertFalse(thread.is_alive())
                self.assertEqual(errors, [])
        finally:
            os.close(master)
            os.close(slave)

    def test_fragmented_pty_handshake_echo_and_binary_payload(self):
        self.exercise()

    def test_pty_checksum_failure(self):
        self.exercise(corrupt=True)

    def test_partial_writes_never_duplicate_bytes(self):
        accepted = bytearray()
        def short_write(fd, view):
            count = min(3, len(view))
            accepted.extend(view[:count])
            return count
        with patch.object(upload.select, 'select', return_value=([], [7], [])), \
                patch.object(upload.os, 'write', side_effect=short_write):
            upload.write_all(7, b'0123456789')
        self.assertEqual(accepted, b'0123456789')

    def test_zero_write_fails_without_retry(self):
        with patch.object(upload.select, 'select', return_value=([], [7], [])), \
                patch.object(upload.os, 'write', return_value=0) as write:
            with self.assertRaises(upload.UploadError):
                upload.write_all(7, b'data')
            self.assertEqual(write.call_count, 1)

    def test_only_generated_ram_destination(self):
        with self.assertRaises(ValueError):
            upload.receiver_command('/dev/mmcblk0', 16)
        with self.assertRaises(ValueError):
            upload.receiver_command('a' * 32, upload.MAX_SIZE + 1)
        path, command = upload.receiver_command('a' * 32, 16)
        self.assertIn(b"grep -q ' /tmp tmpfs '", command)
        self.assertIn(b'set -C', command)
        self.assertNotIn(b'mmcblk', command)

    def test_source_symlink_rejected_before_serial_open(self):
        with tempfile.TemporaryDirectory() as directory:
            original = Path(directory) / 'original'
            original.write_bytes(b'bytes')
            link = Path(directory) / 'link'
            link.symlink_to(original)
            with self.assertRaises(OSError):
                upload.upload('/does-not-exist', link)


if __name__ == '__main__':
    unittest.main()
