import hashlib
import json
from pathlib import Path
import socket
import shutil
import struct
import tempfile
import threading
import unittest
from unittest.mock import patch

import wifi_install as install


class WifiInstallTests(unittest.TestCase):
    def test_selected_raw_ssid_and_directed_scan_persist_in_private_userdata(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, destination = root/'source', root/'private'
            source.write_bytes(b'filesystem fixture')
            captured = []
            def command(argv, **kwargs):
                if '-f' in argv:
                    config = Path(argv[argv.index('-f') + 1]).parent/'networks.conf'
                    captured.append(config.read_bytes())
                    self.assertEqual(config.stat().st_mode & 0o777, 0o600)
                    return SimpleNamespace(returncode=0)
                return SimpleNamespace(returncode=0, stdout=captured[-1])
            with patch.object(install.shutil, 'which', return_value='/fixture/debugfs'), \
                    patch.object(install, 'copy_private_image', side_effect=shutil.copyfile), \
                    patch.object(install.subprocess, 'run', side_effect=command):
                install.customize_userdata(source, destination, {'ssid_hex': b' hidden\xff '.hex(), 'psk_hex': None})
            self.assertIn(b'ssid=2068696464656eff20\n', captured[0])
            self.assertIn(b'scan_ssid=1\n', captured[0])
            self.assertEqual(source.read_bytes(), b'filesystem fixture')

    def test_compaction_trims_only_checked_filesystem_tail_and_rejects_short_file(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/'os.img'
            image=bytearray(12288)
            struct.pack_into('<I',image,1024+4,2)
            struct.pack_into('<I',image,1024+24,2)
            image[1024+56:1024+58]=b'\x53\xef'
            struct.pack_into('<III',image,1024+92,0x2c,0x42,0x73)
            path.write_bytes(image)
            with patch.object(install.shutil,'which',return_value='/fixture/tool'), patch.object(install.subprocess,'run',return_value=SimpleNamespace(returncode=0)) as run:
                self.assertEqual(install.compact_userdata(path,16384,lambda _:None),8192)
                self.assertEqual(path.read_bytes(),image[:8192])
                self.assertEqual(run.call_count,4)
                path.write_bytes(image[:4096])
                with self.assertRaises(ValueError): install.compact_userdata(path,16384,lambda _:None)
                self.assertEqual(path.stat().st_size,4096)

    def test_compression_is_bounded_and_truncated_streams_fail(self):
        class Stream:
            def __init__(self): self.data=bytearray()
            def sendall(self,data): self.data.extend(data)
            def recv(self,n):
                result=bytes(self.data[:n]);del self.data[:n];return result
        stream=Stream()
        install.send_chunk(stream,b'\0'*install.CHUNK)
        self.assertLess(len(stream.data),install.CHUNK//10)
        self.assertEqual(install.receive_chunk(stream,install.CHUNK),b'\0'*install.CHUNK)
        install.send_chunk(stream,b'x'*1024)
        with self.assertRaises(ValueError): install.receive_chunk(stream,1023)

    def exercise(self, corrupt_backup=False, wrong_phase=False, yolo=False, compact=False, progress_frames=False, reuse=False):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);bootstrap=root/'bootstrap';bootstrap.mkdir()
            old_boot=b'B'*4096
            (bootstrap/'boot.img').write_bytes(old_boot)
            (bootstrap/'journal.json').write_text(json.dumps({'complete':True,'original_os':'Android'}))
            (bootstrap/'backup-receipt.json').write_text(json.dumps({'complete':True,'journal_sha256':install.digest(bootstrap/'journal.json')}))
            names={'boot','recovery','userdata'}|install.IDENTITY
            images={name:bytes([i+1])*4096 for i,name in enumerate(install.ORDER) if name in names}
            paths={name:root/(name+'.img') for name in images}
            for name,data in images.items(): paths[name].write_bytes(data)
            plan={'schema':1,'nonce':'a'*64,'cid':'b'*32,'capacity':1048576,'manifest_sha256':'c'*64,
                'partitions':{name:{'offset':(i+1)*4096,'size':4096} for i,name in enumerate(sorted(names))},
                'images':{name:{'size':4096,'sha256':hashlib.sha256(data).hexdigest(),
                               'chunks':[hashlib.sha256(data).hexdigest()]} for name,data in images.items()},
                'original_boot_sha256':hashlib.sha256(old_boot).hexdigest(),
                'identity_sha256':{name:hashlib.sha256(b'O'*4096).hexdigest() for name in install.IDENTITY}}
            plan['skip_userdata_backup'] = yolo
            if compact: plan['partitions']['userdata']['size'] = 8192
            saved=root/'saved';saved.mkdir()
            if reuse:
                plan['reused_backups']={n:hashlib.sha256(old_boot if n=='boot' else b'O'*plan['partitions'][n]['size']).hexdigest() for n in names}
                for n in names: (saved/(n+'.img')).write_bytes(old_boot if n=='boot' else b'O'*plan['partitions'][n]['size'])
            client,server=socket.socketpair();client.settimeout(3);server.settimeout(3)
            self.addCleanup(client.close);self.addCleanup(server.close)
            errors=[];writes=[]
            def peer():
                try:
                    self.assertEqual(install.wifi.read_exact(install.wifi.TlsEndpoint(server),16),struct.pack('<4sIQ',b'CBP1',10,0))
                    self.assertEqual(install.receive_json(server),plan)
                    install.send_json(server,{'event':'bound',**{k:plan[k] for k in ('nonce','cid','capacity','partitions','manifest_sha256')}})
                    install.send_json(server,{'event':'original_boot','target':'boot','sha256':plan['original_boot_sha256']})
                    self.assertEqual(install.receive_json(server),{'ack':'original_boot','target':'boot','sha256':plan['original_boot_sha256']})
                    for name in ([] if reuse else sorted(names-{'boot'}-({'userdata'} if yolo else set()))):
                        install.send_json(server,{'event':'backup','target':name,'size':4096})
                        install.send_chunk(server,b'O'*4096)
                        install.send_json(server,{'event':'checking_backup','target':name})
                        if progress_frames:
                            for done in (0,4096): install.send_json(server,{'event':'verify_progress','phase':'backup','target':name,'done':done,'total':4096})
                        checksum='0'*64 if corrupt_backup else hashlib.sha256(b'O'*4096).hexdigest()
                        install.send_json(server,{'event':'backup_verified','target':name,'sha256':checksum})
                        if corrupt_backup: return
                        self.assertEqual(install.receive_json(server),{'ack':'backup_verified','target':name,'sha256':checksum})
                    install.send_json(server,{'event':'backups_complete'})
                    self.assertEqual(install.receive_json(server),{'ack':'backups_complete','target':'none','sha256':''})
                    for name in install.ORDER:
                        if name not in images: continue
                        install.send_json(server,{'event':'writing','target':'boot' if wrong_phase else name})
                        if wrong_phase: return
                        self.assertEqual(install.receive_json(server),{'ack':'writing','target':name,'sha256':''})
                        writes.append(name)
                        self.assertEqual(install.receive_chunk(server,4096),images[name])
                        phases = ('synced','verified') + (('expanding','expanded') if compact and name == 'userdata' else ())
                        for phase in phases:
                            if phase == 'verified' and progress_frames:
                                for done in (0,4096): install.send_json(server,{'event':'verify_progress','phase':'write','target':name,'done':done,'total':4096})
                            install.send_json(server,{'event':phase,'target':name})
                            self.assertEqual(install.receive_json(server),{'ack':phase,'target':name,'sha256':''})
                    install.send_json(server,{'event':'complete','target':'none'})
                    install.receive_json(server)
                    install.send_json(server,{'event':'installed','verified':True})
                    self.assertEqual(install.receive_json(server),{'action':'leave'})
                except Exception as error: errors.append(error)
                finally: server.close()
            thread=threading.Thread(target=peer);thread.start()
            try:
                if corrupt_backup or wrong_phase:
                    with self.assertRaises(ValueError): install.transaction(client,plan,paths,bootstrap,root/'originals',lambda _:None)
                else:
                    with patch.object(install.subprocess,'run',side_effect=lambda argv,**kw: shutil.copyfile(argv[-2],argv[-1])):
                        result=install.transaction(client,plan,paths,bootstrap,root/'originals',lambda _:None,reuse_from=saved if reuse else None)
                    self.assertTrue(result['complete'])
                    self.assertEqual(result['backup_policy'], 'reused_full' if reuse else ('skip_userdata' if yolo else 'full'))
                    self.assertEqual((root/'originals/userdata.img').exists(),not yolo)
                    self.assertEqual(result['writes']['userdata'],'expanded' if compact else 'verified')
                    self.assertEqual(writes,['recovery','userdata','boot'])
                record=json.loads((root/'originals/journal.json').read_text())
                if corrupt_backup or wrong_phase:
                    self.assertFalse(record['complete']);self.assertEqual(writes,[])
            finally:
                client.close();thread.join(4)
            self.assertFalse(thread.is_alive());self.assertEqual(errors,[])

    def test_full_host_transaction_preserves_backups_and_writes_boot_last(self): self.exercise()
    def test_bad_backup_stops_before_any_write(self): self.exercise(corrupt_backup=True)
    def test_wrong_write_order_never_acknowledged(self): self.exercise(wrong_phase=True)

    def test_yolo_compact_transaction_keeps_verification_and_expansion(self):
        self.exercise(yolo=True,compact=True,progress_frames=True)

    def test_restart_reuses_originals_without_backing_up_partial_install(self):
        self.exercise(reuse=True,compact=True,progress_frames=True)

    def test_progress_does_not_allow_bad_backup_digest(self):
        self.exercise(corrupt_backup=True,progress_frames=True)

    def test_verification_rejects_malformed_or_incomplete_progress(self):
        first={'event':'verify_progress','phase':'write','target':'userdata','done':0,'total':4096}
        final={'event':'verified','target':'userdata'}
        for events in ([first,final], [first,first], [{**first,'target':'boot'}],
                       [{**first,'total':8192}], [{**first,'done':True}],
                       [{**first,'done':4096}], [first,{**first,'done':4097}]):
            with self.subTest(events=events), patch.object(install,'receive_json',side_effect=events):
                with self.assertRaises(ValueError):
                    install.receive_verification(None,'userdata','write',4096,lambda _:None)
        with patch.object(install,'receive_json',side_effect=[first,{**first,'done':4096},final]):
            self.assertEqual(install.receive_verification(None,'userdata','write',4096,lambda _:None),final)

    def test_host_hash_reports_measured_progress(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'userdata.img'; raw=b'x'*(install.CHUNK+4096);path.write_bytes(raw)
            events=[]
            self.assertEqual(install.digest(path,events.append),hashlib.sha256(raw).hexdigest())
            self.assertEqual(len(events),3)
            self.assertIn('0/'+str(len(raw)),events[0])
            self.assertIn(str(len(raw))+'/'+str(len(raw)),events[-1])

    def test_private_image_copy_preserves_zero_tail_and_refuses_existing_targets(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source.img';target=root/'target.img'
            data=b'header'+b'\0'*(2*install.CHUNK)
            source.write_bytes(data)
            install.copy_private_image(source,target)
            self.assertEqual(target.read_bytes(),data)
            with self.assertRaises(FileExistsError): install.copy_private_image(source,target)
            self.assertEqual(source.read_bytes(),data)

    def test_private_network_is_verified_without_password_in_process_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source.img';source.write_bytes(b'fixture')
            network={'ssid_hex':'636f756368','psk_hex':'ab'*32}
            seen=[]
            def run(argv,**kwargs):
                seen.append(argv)
                if argv[0]=='cp': Path(argv[-1]).write_bytes(source.read_bytes())
                class Result: returncode=0;stdout=b'network={\n    ssid=636f756368\n    scan_ssid=1\n    key_mgmt=WPA-PSK\n    proto=RSN\n    psk='+b'ab'*32+b'\n}\n'
                return Result()
            with patch.object(install.shutil,'which',return_value='/usr/sbin/debugfs'),patch.object(install.subprocess,'run',run):
                install.customize_userdata(source,root/'private.img',network)
            self.assertNotIn(network['psk_hex'],str(seen))
            self.assertEqual(source.read_bytes(),b'fixture')

if __name__=='__main__':unittest.main()
