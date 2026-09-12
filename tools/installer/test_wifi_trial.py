import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from unittest.mock import Mock
from types import SimpleNamespace, ModuleType

import couch_tui
import wifi_trial


class WifiTrialTests(unittest.TestCase):
    def test_late_stage_can_continue_without_repeating_bootstrap(self):
        usb = ModuleType('usb'); usb.core = ModuleType('usb.core')
        device = SimpleNamespace(bus=1, port_numbers=(1,))
        usb.core.find = Mock(side_effect=[[], [device]])
        terminal = Mock(); terminal.ask.return_value = ''
        with patch.dict('sys.modules', {'usb': usb, 'usb.core': usb.core}), \
                patch.object(wifi_trial.time, 'monotonic', side_effect=[0, 121, 122]), \
                patch.object(wifi_trial.time, 'sleep'):
            wifi_trial.wait_stage(SimpleNamespace(bus=1, ports='1'), terminal)
        terminal.ask.assert_called_once()

    def config(self, root):
        value={'schema':1,'paths':{name:'inputs/'+name for name in wifi_trial.PATHS},
               'pins':{name:'a'*64 for name in wifi_trial.PINS},'bus':1,'ports':'1'}
        path=root/'trial.json';path.write_text(json.dumps(value));return path,value

    def test_package_cannot_supply_credentials_or_escaping_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);path,value=self.config(root)
            self.assertEqual(wifi_trial.load_config(path)['bus'],1)
            value['password']='secret';path.write_text(json.dumps(value))
            with self.assertRaises(couch_tui.core.InstallError):wifi_trial.load_config(path)
            del value['password'];value['paths']['loader']='../outside';path.write_text(json.dumps(value))
            with self.assertRaises(couch_tui.core.InstallError):wifi_trial.load_config(path)

    def test_cancellation_never_starts_usb_or_requests_password(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);path,_=self.config(root)
            terminal=couch_tui.Terminal(io.StringIO('\n'),io.StringIO())
            release={'private_candidate':True,'installable':False,'images':{'boot':{}},
                     'partitions':{name:{'size':4096} for name in {'boot'}|couch_tui.core.IDENTITY_PARTITIONS}}
            with patch.dict(wifi_trial.os.environ,{'COUCH_INSTALL_STATE':str(root/'state')}), \
                    patch.object(wifi_trial.os,'geteuid',return_value=0), \
                    patch.object(wifi_trial.shutil,'which',return_value='/fixture/tool'), \
                    patch.object(wifi_trial.core,'digest',return_value='a'*64), \
                    patch.object(wifi_trial.core,'load_release',return_value=(release,root)), \
                    patch.object(wifi_trial.bootstrap,'validate'),patch.object(wifi_trial.bootstrap,'run') as bootstrap, \
                    patch.object(wifi_trial.getpass,'getpass') as password:
                self.assertEqual(wifi_trial.run(terminal,path),0)
                bootstrap.assert_not_called();password.assert_not_called()

    def test_invalid_wifi_input_can_be_corrected_without_restarting_usb(self):
        terminal=Mock()
        terminal.ask.side_effect=['', 'my-network']
        terminal.choose.side_effect=['wpa2','wpa2']
        terminal.secret.side_effect=['short','valid-fixture-password']
        network=wifi_trial.network_form(terminal)
        self.assertEqual(network['ssid_hex'], 'my-network'.encode().hex())
        terminal.stage.assert_called_once()
        self.assertEqual(terminal.secret.call_count,2)

    def test_restore_requires_wifi_and_cannot_be_combined_with_restart(self):
        with patch('sys.stderr',io.StringIO()):
            self.assertEqual(couch_tui.main(['--wifi-restore-from','fixture']),2)
            self.assertEqual(couch_tui.main(['--wifi-trial','fixture','--wifi-restore-from','a','--wifi-retry-from','b']),2)

    def test_wifi_mode_cannot_be_mixed_with_other_install_modes(self):
        with patch('sys.stderr',io.StringIO()):
            self.assertEqual(couch_tui.main(['--wifi-trial','fixture','--simulation']),2)


if __name__=='__main__':unittest.main()
