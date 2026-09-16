import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

from official_runtime import checked_member, extract, reconstruct, sha, transfer_ranges

try:
    import brotli
except ImportError:
    brotli = None


class OfficialRuntimeTests(unittest.TestCase):
    def test_only_bounded_nonoverlapping_full_transfer_lists(self):
        valid = b'4\n2\n0\n0\nnew 2,0,1\nzero 2,1,2\nerase 2,2,3\n'
        self.assertEqual(transfer_ranges(valid, 3*4096), [(0,4096)])
        invalid = [valid.replace(b'new', b'move'), valid.replace(b'2,0,1', b'2,0,4'),
                   valid.replace(b'2,1,2', b'2,0,2'), valid.replace(b'2,0,1', b'3,0,1'),
                   valid.replace(b'2,0,1', b'2,-1,1'), valid.replace(b'4\n2\n0\n0', b'4\n2\n1\n0'),
                   valid.replace(b'4\n2\n', b'4\n3\n')]
        for data in invalid:
            with self.subTest(data=data), self.assertRaises(ValueError):
                transfer_ranges(data, 3*4096)
        with self.assertRaises(ValueError):
            transfer_ranges(valid, 3*1024**3)

    def archive(self, records):
        raw = io.BytesIO()
        with zipfile.ZipFile(raw, 'w') as archive:
            for name, data in records.items():
                archive.writestr(name, data)
        raw.seek(0)
        return zipfile.ZipFile(raw)

    def test_zip_member_length_hash_and_symlink_checks(self):
        with self.archive({'member': b'content'}) as archive:
            expected = {'size':7, 'sha256':sha(b'content')}
            result = bytearray()
            checked_member(archive,'member',expected,result.extend)
            self.assertEqual(result,b'content')
            for pin in ({**expected,'size':8},{**expected,'sha256':'0'*64}):
                with self.assertRaises(ValueError):
                    checked_member(archive,'member',pin,lambda data: None)
        raw=io.BytesIO()
        with zipfile.ZipFile(raw,'w') as archive:
            info=zipfile.ZipInfo('link');info.external_attr=0o120777<<16
            archive.writestr(info,b'target')
        raw.seek(0)
        with zipfile.ZipFile(raw) as archive, self.assertRaises(ValueError):
            checked_member(archive,'link',{'size':6,'sha256':sha(b'target')},lambda data:None)

    @unittest.skipUnless(brotli,'Real Brotli tests require python3-brotli')
    def test_brotli_bounds_truncation_and_exact_image_digest(self):
        transfer=b'4\n2\n0\n0\nnew 2,1,2\nzero 2,0,1\n'
        payload=b'A'*4096
        expected=b'\0'*4096+payload
        for compressed, correct in [(brotli.compress(payload),True),
                (brotli.compress(payload+b'extra'),False),
                (brotli.compress(payload[:-1]),False),
                (brotli.compress(payload)[:-1],False)]:
            records={'system.transfer.list':transfer,'system.new.dat.br':compressed}
            pin={'members':{n:{'size':len(b),'sha256':sha(b)} for n,b in records.items()},
                 'images':{'system':{'size':8192,'sha256':sha(expected)}}}
            with tempfile.TemporaryDirectory() as directory, self.archive(records) as archive:
                output=Path(directory)/'image'
                if correct:
                    reconstruct(archive,'system',pin,output)
                    self.assertEqual(output.read_bytes(),expected)
                else:
                    with self.assertRaises(ValueError):
                        reconstruct(archive,'system',pin,output)

    def test_archive_tampering_fails_before_output_and_existing_output_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);archive=root/'ota';archive.write_bytes(b'wrong')
            with self.assertRaises(ValueError):
                extract(archive,root/'out',{'size':5,'sha256':'0'*64})
            self.assertFalse((root/'out').exists())
            with self.assertRaises(ValueError):
                extract(archive,root,{})


if __name__ == '__main__':
    unittest.main()
