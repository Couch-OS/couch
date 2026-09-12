import hashlib
from pathlib import Path
import tempfile
from types import SimpleNamespace as NS
import unittest
from unittest.mock import Mock, patch

import android_start
import couch_install


class AndroidStartTests(unittest.TestCase):
    def test_only_authorized_device_on_selected_port_with_matching_cid_reboots(self):
        for state, matches in [('device', True), ('unauthorized', True), ('device', False)]:
            with self.subTest(state=state, matches=matches), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); (root/'1-1').mkdir(); (root/'1-1/serial').write_text('selected')
                runtime = bytes.fromhex('15010051415853376d6a218a04140142')
                args = NS(bus=1, ports='1', confirm_cid_sha256=hashlib.sha256(runtime).hexdigest() if matches else '0'*64)
                calls = []
                def run(argv, **kwargs):
                    calls.append(argv)
                    text = ('other device\nselected '+state+'\n') if argv[-1]=='devices' else runtime.hex()
                    return NS(returncode=0, stdout=text)
                with patch.object(android_start.shutil, 'which', return_value='/bin/adb'):
                    if not matches:
                        with self.assertRaises(couch_install.InstallError):
                            android_start.reboot_selected_android(args, Mock(), sysfs=root, run=run)
                    else:
                        self.assertEqual(android_start.reboot_selected_android(args, Mock(), sysfs=root, run=run), state=='device')
                reboots = [call for call in calls if call[-1]=='reboot']
                self.assertEqual(reboots, [['/bin/adb','-s','selected','reboot']] if state=='device' and matches else [])

    def test_missing_adb_uses_manual_fallback(self):
        with patch.object(android_start.shutil, 'which', return_value=None):
            self.assertFalse(android_start.reboot_selected_android(NS(), Mock()))
