import io
import os
import re
import unittest
from unittest.mock import patch

from couch_tui import Terminal
from wifi_view import WifiView


class Tty(io.StringIO):
    def isatty(self):
        return True


class WifiViewTests(unittest.TestCase):
    def test_onboarding_and_prompt_fit_common_terminal_sizes(self):
        view = WifiView(Terminal(io.StringIO(), io.StringIO()))
        view.stage(0, 'Install Couch after saving and verifying your Android backups.',
                   '1. Enable USB debugging in Android.\n'
                   '2. Connect your remote to this computer via USB.\n'
                   '3. Approve "Allow USB debugging?" on the remote.')
        view.prompting = True
        for columns, rows in ((80, 24), (60, 20), (100, 30)):
            with self.subTest(columns=columns, rows=rows):
                lines = view.frame_lines(columns, rows)
                self.assertLessEqual(len(lines), rows - 3)
                self.assertTrue(all(len(re.sub(r'\x1b\[[0-9;]*m', '', line)) <= columns for line in lines))
                frame = '\n'.join(lines)
                self.assertIn('Enable USB debugging', frame)
                self.assertIn('Allow USB debugging?', frame)
                self.assertIn('YOUR TURN', frame)

    def test_bootstrap_connection_and_calibration_clear_power_instruction(self):
        view = WifiView(Terminal(io.StringIO(), io.StringIO()))
        view.stage(1, 'Waiting for the remote on USB.', 'Hold Power.')
        view.write('DALegacy - Connected to stage2\n')
        self.assertIn('USB connected', view.detail)
        self.assertNotIn('Hold Power', view.action)
        view.write('Backing up identity partition: nvdata\n')
        self.assertEqual(view.detail, 'Saving calibration: nvdata')
        view.write('Verifying independent readback: nvdata\n')
        self.assertEqual(view.detail, 'Verifying saved calibration: nvdata')

    def test_raw_diagnostics_are_logged_without_flooding_the_screen(self):
        output, log = io.StringIO(), io.StringIO()
        view = WifiView(Terminal(io.StringIO(), output))
        view.log = log
        view.stage(1, 'Starting USB')
        view.write('DA internal register dump\nBackup: boot 0/1024 bytes (0%)\n')
        view.write('Backup: boot 1024/1024 bytes (100%)\n')
        self.assertIn('DA internal register dump', log.getvalue())
        self.assertNotIn('DA internal', output.getvalue())
        self.assertEqual(output.getvalue().count('Backup: boot'), 1)
        self.assertNotIn('\033', output.getvalue())
        self.assertIn('100%', view.counter)

    def test_input_preserves_ssid_spaces_and_never_enters_log(self):
        view = WifiView(Terminal(io.StringIO(' network name \n'), io.StringIO()))
        view.log = io.StringIO()
        self.assertEqual(view.ask('SSID: ', strip=False), ' network name ')
        self.assertNotIn('network name', view.log.getvalue())
        def password(prompt):
            self.assertTrue(view.paused)
            return 'private-password'
        self.assertEqual(view.secret(password), 'private-password')
        self.assertNotIn('private-password', view.log.getvalue())
        self.assertFalse(view.paused)

    def test_animation_pauses_for_input_and_honors_no_color(self):
        with patch.dict(os.environ, {'TERM': 'xterm', 'NO_COLOR': '1'}):
            output = Tty()
            view = WifiView(Terminal(io.StringIO(), output))
            view.render()
            first = output.getvalue()
            self.assertNotIn('\033[36m', first)
            view.paused = True
            view.render()
            self.assertEqual(first, output.getvalue())
            view.paused = False
            view.frame = 1
            view.render()
            self.assertNotEqual(first, output.getvalue()[len(first):])

    def test_install_step_begins_only_when_writing_os_images(self):
        view = WifiView(Terminal(io.StringIO(), io.StringIO()))
        view.stage(3, 'Backing up Android')
        view.progress('Backup: userdata 0/100 bytes (0%)')
        self.assertEqual(view.step, 3)
        view.progress('Write: recovery 0/100 bytes (0%)')
        self.assertEqual(view.step, 4)
        view.line('Installation stopped: readback mismatch')
        self.assertEqual(view.result, 'error')
        self.assertIn('readback mismatch', view.detail)
        self.assertEqual(view.counter, '')

    def test_split_progress_is_parsed_and_invalid_counters_are_ignored(self):
        view = WifiView(Terminal(io.StringIO(), io.StringIO()))
        view.write('Write: boot 1/')
        view.write('10 bytes (10%)\n')
        self.assertIn('10%', view.counter)
        counter = view.counter
        view.write('Write: boot 11/10 bytes (110%)\n')
        self.assertEqual(view.counter, counter)


if __name__ == '__main__':
    unittest.main()
