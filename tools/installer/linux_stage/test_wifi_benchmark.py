import io
import ssl
import socket
import threading
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import wifi_benchmark as wifi
from wifi_benchmark import credentials, ephemeral_identity, TlsEndpoint

class WifiTests(unittest.TestCase):
    def test_failed_hardware_never_requests_credentials(self):
        from contextlib import contextmanager
        @contextmanager
        def probe(args):
            yield None, None
        with patch.object(wifi, 'usb_probe', probe), \
                patch.object(wifi, 'wifi_status', return_value={'status': 'failed', 'error': 'loader-exit'}), \
                patch('builtins.input') as ssid, patch.object(wifi.getpass, 'getpass') as password, \
                patch.object(wifi, 'provision') as provision, patch('sys.stdout', io.StringIO()):
            with self.assertRaisesRegex(ValueError, 'loader exited'):
                wifi.main(['--bus', '1', '--ports', '1', '--vid', '0xe8d', '--pid', '0x201c'])
            ssid.assert_not_called()
            password.assert_not_called()
            provision.assert_not_called()

    def test_existing_provisioning_is_not_overwritten(self):
        with patch.object(wifi, 'wifi_status', return_value={'status': 'ready', 'provisioned': True}):
            with self.assertRaisesRegex(ValueError, 'already received credentials'):
                wifi.wait_ready(None, None)

    def test_failed_status_with_stale_ip_never_returns_connection_success(self):
        class Output:
            def write(self, data, **kwargs): return len(data)
        with patch.object(wifi, 'response'), patch.object(wifi, 'wifi_status',
                return_value={'status': 'failed', 'ip': '192.0.2.1', 'error': 'supplicant-exit'}):
            with self.assertRaisesRegex(ValueError, 'authentication process stopped'):
                wifi.provision(Output(), None, {})

    def test_untrusted_diagnostic_text_is_not_echoed(self):
        with self.assertRaises(ValueError) as error:
            wifi.check_failure({'status': 'failed', 'error': 'private-network-secret'})
        self.assertNotIn('private-network-secret', str(error.exception))

    def test_known_wpa2_psk_vector_and_bounds(self):
        self.assertEqual(credentials('IEEE','password')['psk_hex'],
            'f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e')
        self.assertIsNone(credentials('open',None)['psk_hex'])
        for ssid,password in [('',None),('x'*33,None),('ssid','short'),('ssid','bad\npassword')]:
            with self.assertRaises(ValueError): credentials(ssid,password)

    def test_ephemeral_certificate_and_key_are_usable_without_system_trust(self):
        with tempfile.TemporaryDirectory() as directory:
            pem, identity=ephemeral_identity(directory)
            self.assertEqual(len(bytes.fromhex(identity['token_hex'])),32)
            self.assertEqual(bytes.fromhex(identity['certificate_hex']),ssl.PEM_cert_to_DER_cert(pem))
            context=ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.minimum_version=context.maximum_version=ssl.TLSVersion.TLSv1_3
            context.load_verify_locations(cadata=pem)
            self.assertTrue(context.check_hostname)
            self.assertEqual(context.verify_mode,ssl.CERT_REQUIRED)
            self.assertEqual(len(context.get_ca_certs(binary_form=True)),0) # pinned non-CA leaf only
            server_context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            server_context.minimum_version=server_context.maximum_version=ssl.TLSVersion.TLSv1_3
            server_context.load_cert_chain(str(Path(directory)/'certificate.pem'),str(Path(directory)/'key.pem'))
            with socket.socket() as listener:
                listener.bind(('127.0.0.1',0));listener.listen(1)
                def serve():
                    raw,_=listener.accept()
                    with server_context.wrap_socket(raw,server_side=True) as stream:
                        stream.sendall(b'OKAY')
                worker=threading.Thread(target=serve);worker.start()
                with socket.create_connection(listener.getsockname(),timeout=2) as raw:
                    with context.wrap_socket(raw,server_hostname='couch-probe') as stream:
                        self.assertEqual(stream.recv(4),b'OKAY')
                worker.join(timeout=2)
                self.assertFalse(worker.is_alive())


    def test_tls_adapter_does_not_drop_partial_reads(self):
        class Socket:
            data=io.BytesIO(b'abc')
            def recv(self,n):return self.data.read(n)
            def sendall(self,data):self.sent=data
        sock=Socket(); adapter=TlsEndpoint(sock)
        self.assertEqual(adapter.write(b'hello'),5)
        self.assertEqual(adapter.read(2),b'ab')
        self.assertEqual(adapter.read(2),b'c')

if __name__=='__main__':unittest.main()
