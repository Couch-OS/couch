import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location('sercmd', Path(__file__).parents[1] / 'sercmd.py')
sercmd = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sercmd)


class SerialSettingsTests(unittest.TestCase):
    def test_exported_and_quoted_local_values_are_read(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / 'local.env'
            config.write_text("export SER_HOST='serial builder'\nSER_PORT='/dev/tty remote' # comment\n")
            with patch.dict('os.environ', {'COUCH_LOCAL_CONFIG': str(config)}, clear=True):
                self.assertEqual(sercmd.local_settings(), {
                    'SER_HOST': 'serial builder',
                    'SER_PORT': '/dev/tty remote',
                })

    def test_local_serial_port_setting_is_used_before_discovery(self):
        with patch.dict('os.environ', {}, clear=True):
            self.assertEqual(sercmd.port({'SER_PORT': '/dev/tty-configured'}), '/dev/tty-configured')

    def test_multiple_unquoted_values_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / 'local.env'
            config.write_text('SER_HOST=one two\n')
            with patch.dict('os.environ', {'COUCH_LOCAL_CONFIG': str(config)}, clear=True):
                with self.assertRaisesRegex(ValueError, 'one quoted'):
                    sercmd.local_settings()


if __name__ == '__main__':
    unittest.main()
