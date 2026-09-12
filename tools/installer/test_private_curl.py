import hashlib
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

import package_private_trial


class PrivateCurlTests(unittest.TestCase):
    def extract(self, entries, corrupt=False):
        script=Path(__file__).with_name('private_curl.sh.in').read_text()
        code=script.split("<<'COUCH_PRIVATE_EXTRACT'\n",1)[1].split('\nCOUCH_PRIVATE_EXTRACT',1)[0]
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);archive=root/'package.tar.gz'
            with tarfile.open(archive,'w:gz') as bundle:
                for name,kind in entries:
                    entry=tarfile.TarInfo(name)
                    if kind=='link':
                        entry.type=tarfile.SYMTYPE;entry.linkname='/etc/passwd';bundle.addfile(entry)
                    else:
                        data=b'never execute this fixture\n';entry.size=len(data);bundle.addfile(entry,io.BytesIO(data))
            checksum='0'*64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
            result=subprocess.run([sys.executable,'-',str(archive),checksum,str(root/'out')],input=code,text=True,capture_output=True)
            return result,(root/'out').exists()

    def test_bad_pin_and_unsafe_paths_never_extract(self):
        for entries,corrupt in [([('couch-installer/tools/installer/couch_tui.py','file')],True),
                ([('../escape','file')],False),([('couch-installer/link','link')],False)]:
            result,exists=self.extract(entries,corrupt)
            self.assertNotEqual(result.returncode,0);self.assertFalse(exists)

    def test_verified_package_extracts_without_execution(self):
        result,exists=self.extract([('couch-installer/bin/couch-installer-tui','file')])
        self.assertEqual(result.returncode,0,result.stderr);self.assertTrue(exists)
        self.assertNotIn('never execute',result.stdout)

    def test_builder_pins_package_and_does_not_publish_or_accept_remote_http(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);payload=root/'payload';payload.mkdir()
            (payload/'tools/installer').mkdir(parents=True)
            for name in ('trial.json','requirements.txt','tools/installer/couch_tui.py','tools/installer/ratatui_backend.py'):
                (payload/name).write_text('fixture')
            (payload/'bin').mkdir()
            (payload/'bin/couch-installer-tui').write_bytes(b'\x7fELF\x02\x01'+b'\0'*12+b'\x3e\x00')
            with self.assertRaises(ValueError):package_private_trial.package(payload,root/'bad','http://example.com')
            digest=package_private_trial.package(payload,root/'out','http://127.0.0.1:8765')
            self.assertIn(digest,(root/'out/install.sh').read_text())
            self.assertEqual(hashlib.sha256((root/'out/couch-installer.tar.gz').read_bytes()).hexdigest(),digest)


if __name__=='__main__':unittest.main()
