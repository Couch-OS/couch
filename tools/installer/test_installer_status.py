import io
import unittest

from installer_status import Display, safe_line


class StatusTests(unittest.TestCase):
    def test_raw_identifiers_and_untrusted_diagnostics_never_shown(self):
        for line in ('CID: private', 'Error secret=abc', '\x1b[2JWrite: boot 0/12 bytes (0%)',
                     'Backing up identity partition: secret', 'Write: boot 20/10 bytes (200%)'):
            self.assertIsNone(safe_line(line))
        self.assertEqual(safe_line('RAM bootstrap stopped: secret=abc'),
                         'Installer stopped or reported an error. Operator review required.')

    def test_initial_replayed_counter_has_unknown_rate_then_measures_new_bytes(self):
        output = io.StringIO()
        now = [10.0]
        display = Display(output, lambda: now[0])
        display.show('Backup: boot 1048576/4194304 bytes (25%)')
        self.assertIn('rate unknown', output.getvalue())
        self.assertIn('ETA --:--:--', output.getvalue())
        output.seek(0); output.truncate()
        now[0] = 11.0
        display.show('Backup: boot 2097152/4194304 bytes (50%)')
        self.assertIn('1.00 MiB/s', output.getvalue())
        display.show('Hash readback: boot 0/4194304 bytes (0%)')
        self.assertIn('rate unknown', output.getvalue())

    def test_identity_and_success_messages_are_whitelisted(self):
        self.assertEqual(safe_line('Backing up identity partition: nvdata'),
                         'Backing up identity partition: nvdata')
        self.assertIsNone(safe_line('nvdata=raw private bytes'))


if __name__ == '__main__':
    unittest.main()
