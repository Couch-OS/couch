import io
import json
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from installer_pins import load as load_installer_pin

vendor = load_installer_pin('private_vendor')
from clean_stage import GENERATED, checksum, StageError
from prepare_private_rootfs import prepare
from prepare_rootfs import normalize

class PrivateVendorTests(unittest.TestCase):
    def bundle(self, root):
        base=root/'base';base.mkdir()
        contents={name:('fixture '+name).encode() for name in vendor.ALLOWED}
        for name in vendor.STATIC+vendor.FIRMWARE:
            path=base/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(contents[name])
        image=root/'image.img';image.write_bytes(b'\0'*1080+b'\x53\xef')
        hashes={role:vendor.file_sha(image) for role in ('system','vendor')}
        output=root/'bundle'
        with patch.object(vendor,'read_image',side_effect=lambda image,name:contents[name]), patch.object(vendor.subprocess,'run',return_value=SimpleNamespace(stderr='debugfs fixture')):
            manifest=vendor.extract(base,image,image,hashes,output)
        return output,manifest

    def test_only_missing_static_configuration_is_extracted_and_all_existing_files_verified(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);bundle,manifest=self.bundle(root)
            self.assertEqual({r['path'] for r in manifest['files'] if r['operation']=='extracted missing file'},set(vendor.MISSING))
            self.assertEqual(vendor.verify_bundle(bundle),manifest)
            self.assertFalse(manifest['redistribution_authorized'])
            self.assertEqual(bundle.stat().st_mode & 0o777,0o700)
            (bundle/'vendor/firmware/WMT_SOC.cfg').write_text('tampered')
            with self.assertRaisesRegex(ValueError,'hash mismatch'):vendor.verify_bundle(bundle)

    def test_existing_bundle_mismatch_and_wrong_image_hash_fail_before_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);base=root/'base';base.mkdir()
            for name in vendor.STATIC+vendor.FIRMWARE:
                path=base/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(b'changed')
            image=root/'image';image.write_bytes(b'\0'*1080+b'\x53\xef')
            digest=vendor.file_sha(image)
            with self.assertRaisesRegex(ValueError,'image hash mismatch'):
                vendor.extract(base,image,image,{'system':'0'*64,'vendor':digest},root/'bad')
            with patch.object(vendor,'read_image',return_value=b'original'), self.assertRaisesRegex(ValueError,'differs from original'):
                vendor.extract(base,image,image,{'system':digest,'vendor':digest},root/'bad')
            self.assertFalse((root/'bad').exists())

    def test_rejects_uninventoried_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);bundle,_=self.bundle(root)
            path=bundle/'authorized_keys';path.write_text('secret')
            with self.assertRaises(ValueError):vendor.verify_bundle(bundle)
            path.unlink();path.symlink_to('vendor/firmware/WMT_SOC.cfg')
            with self.assertRaises(ValueError):vendor.verify_bundle(bundle)

    def test_private_overlay_is_reproducible_but_default_public_validation_rejects_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);bundle,manifest=self.bundle(root)
            staging=root/'staging';staging.mkdir();data=io.BytesIO()
            with tarfile.open(fileobj=data,mode='w') as archive:
                for name,content in GENERATED.items():
                    member=tarfile.TarInfo(name);member.mode=0o644;member.size=len(content)
                    archive.addfile(member,io.BytesIO(content))
            raw,_=normalize(data.getvalue(),1234)
            (staging/'rootfs-staging.tar.gz').write_bytes(raw)
            (staging/'staging.json').write_text(json.dumps({'kind':'couch-packaged-staging','installable':False,'source_date_epoch':1234,'archive_sha256':checksum(raw)}))
            first=prepare(staging,bundle,root/'one');second=prepare(staging,bundle,root/'two')
            self.assertEqual(first,second)
            output=(root/'one/rootfs-staging.tar.gz').read_bytes()
            with self.assertRaises(StageError):normalize(output,1234)
            hashes={'opt/couch/'+r['path']:r['sha256'] for r in manifest['files']}
            self.assertEqual(normalize(output,1234,hashes)[0],output)
            hashes['opt/couch/vendor/firmware/WMT_SOC.cfg']='0'*64
            with self.assertRaisesRegex(StageError,'member mismatch'):normalize(output,1234,hashes)
            self.assertTrue(first['private_only']);self.assertFalse(first['installable'])


class DependencyClosureTests(unittest.TestCase):
    def test_reachable_missing_dependency_is_not_confused_with_unused_library(self):
        from audit_vendor_elf import closure
        records=[{'path':'vendor/bin/wmt_loader','needed':['libc.so']},
                 {'path':'vendor/bin/wmt_launcher','needed':['libc.so']},
                 {'path':'system/lib/libc.so','needed':['ld-android.so']},
                 {'path':'system/lib/libutils.so','needed':['libvndksupport.so']}]
        report=closure(records,['wmt_loader','wmt_launcher'],['ld-android.so'])
        self.assertEqual(report['missing_required'],[])
        self.assertEqual(report['missing_outside_wmt_closure'],['libvndksupport.so'])
        self.assertEqual(closure(records,['wmt_loader'],[])['missing_required'],['ld-android.so'])

if __name__=='__main__':unittest.main()
