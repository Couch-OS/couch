#!/usr/bin/env python3
"""Provision temporary WPA2/open Wi-Fi over physical USB, then benchmark pinned TLS."""
import argparse
import getpass
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import secrets
import socket
import ssl
import struct
import subprocess
import tempfile
import time

from benchmark import benchmark, header, read_exact, response, usb_probe


def credentials(ssid, password):
    encoded = ssid if isinstance(ssid, bytes) else ssid.encode('utf-8')
    if not 1 <= len(encoded) <= 32:
        raise ValueError('SSID must be 1–32 bytes')
    if password is None:
        psk = None
    else:
        try:
            raw = password.encode('ascii')
        except UnicodeEncodeError as error:
            raise ValueError('WPA2 passphrase must contain ASCII characters') from error
        if not 8 <= len(raw) <= 63 or any(c < 32 or c > 126 for c in raw):
            raise ValueError('WPA2 passphrase must be 8–63 printable ASCII characters')
        psk = hashlib.pbkdf2_hmac('sha1', raw, encoded, 4096, 32).hex()
    return {'ssid_hex': encoded.hex(), 'psk_hex': psk}


def ephemeral_identity(directory):
    directory = Path(directory)
    cert, key, der = (directory / name for name in ('certificate.pem', 'key.pem', 'key.der'))
    env = os.environ.copy()
    def openssl(*args):
        subprocess.run(['openssl', *args], check=True, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, env=env)
    openssl('req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
            '-nodes', '-keyout', str(key), '-out', str(cert), '-days', '1',
            '-subj', '/CN=couch-probe', '-addext', 'subjectAltName=DNS:couch-probe',
            '-addext', 'basicConstraints=critical,CA:FALSE',
            '-addext', 'keyUsage=critical,digitalSignature',
            '-addext', 'extendedKeyUsage=serverAuth')
    openssl('pkcs8', '-topk8', '-nocrypt', '-in', str(key), '-outform', 'DER', '-out', str(der))
    pem = cert.read_text()
    return pem, {'certificate_hex': ssl.PEM_cert_to_DER_cert(pem).hex(),
                 'private_key_hex': der.read_bytes().hex(), 'token_hex': secrets.token_hex(32)}


class TlsEndpoint:
    def __init__(self, stream):
        self.stream = stream
    def read(self, count, **kwargs):
        return self.stream.recv(count)
    def write(self, data, **kwargs):
        self.stream.sendall(data)
        return len(data)


FAILURES = {
    'detect-node': 'Wi-Fi detection device is missing',
    'loader-exit': 'Vendor Wi-Fi loader exited unsuccessfully',
    'transport-node': 'Wi-Fi transport device is missing',
    'wifi-node': 'Wi-Fi power-control device is missing',
    'launcher-exit': 'Vendor Wi-Fi launcher stopped',
    'transport-timeout': 'Wi-Fi transport initialization timed out',
    'power-on': 'Wi-Fi chip power-on failed',
    'interface-timeout': 'Wi-Fi interface did not appear',
    'interface-up': 'Wi-Fi interface could not be enabled',
    'dhcp-exit': 'Address acquisition stopped; association or DHCP may have failed',
    'supplicant-exit': 'Wi-Fi authentication process stopped',
}


def wifi_status(out, incoming):
    if out.write(struct.pack('<4sIQ', b'CBP1', 5, 0), timeout=30000) != 16:
        raise ValueError('Short status request')
    magic, status, size = struct.unpack('<4sIQ', read_exact(incoming, 16))
    if magic != b'CBR1' or status != 0 or not 1 <= size <= 512:
        raise ValueError('Invalid Wi-Fi status frame')
    value = json.loads(read_exact(incoming, size))
    if not isinstance(value, dict) or value.get('status') not in (
            'waiting', 'initializing', 'ready', 'connecting', 'connected', 'failed'):
        raise ValueError('Invalid Wi-Fi status')
    return value


def check_failure(value):
    if value['status'] == 'failed':
        error = value.get('error')
        reason = FAILURES.get(error) if isinstance(error, str) else None
        raise ValueError('RAM-stage Wi-Fi failed: ' + (reason or
                         'This stage cannot identify the cause; a diagnostic stage boot is needed'))


def wait_ready(out, incoming):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        value = wifi_status(out, incoming)
        check_failure(value)
        if value.get('provisioned') or value['status'] in ('connecting', 'connected'):
            raise ValueError('This RAM stage already received credentials. Reboot it for a new session.')
        if value['status'] == 'ready':
            return
        time.sleep(1)
    raise ValueError('Wi-Fi hardware did not become ready; credentials were not requested or sent')


def provision(out, incoming, payload):
    data = json.dumps(payload, separators=(',', ':')).encode()
    if not 1 <= len(data) <= 16384:
        raise ValueError('Provisioning payload too large')
    frame = struct.pack('<4sIQ', b'CBP1', 4, len(data))
    if out.write(frame, timeout=30000) != 16 or out.write(data, timeout=30000) != len(data):
        raise ValueError('Short USB provisioning write')
    response(incoming, 0)
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        value = wifi_status(out, incoming)
        check_failure(value)
        if value['status'] == 'connected' and value.get('ip'):
            return str(ipaddress.IPv4Address(value['ip']))
        time.sleep(1)
    raise ValueError('Timed out waiting for RAM-stage Wi-Fi')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bus', type=int, required=True)
    parser.add_argument('--ports', required=True)
    parser.add_argument('--vid', type=lambda n: int(n, 0), required=True)
    parser.add_argument('--pid', type=lambda n: int(n, 0), required=True)
    parser.add_argument('--open-network', action='store_true')
    parser.add_argument('--mib', type=int, default=32, choices=range(1, 65))
    parser.add_argument('--hash-recovery', action='store_true')
    args = parser.parse_args(argv)
    print('Checking Wi-Fi hardware over USB before asking for credentials...', flush=True)
    with usb_probe(args) as (out, incoming):
        wait_ready(out, incoming)
        print('Credentials are sent over USB into temporary RAM only.', flush=True)
        network = credentials(input('Wi-Fi SSID: '), None if args.open_network else getpass.getpass('WPA2 password: '))
        with tempfile.TemporaryDirectory(prefix='couch-tls-') as temporary:
            pem, identity = ephemeral_identity(temporary)
            address = provision(out, incoming, {**network, **identity})
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
            context.load_verify_locations(cadata=pem)
            with socket.create_connection((address, 8443), timeout=30) as raw:
                with context.wrap_socket(raw, server_hostname='couch-probe') as stream:
                    endpoint = TlsEndpoint(stream)
                    endpoint.write(bytes.fromhex(identity['token_hex']))
                    if read_exact(endpoint, 4) != b'OKAY':
                        raise ValueError('TLS session authentication rejected')
                    for direction in (1, 2):
                        print(benchmark(endpoint, endpoint, args.mib * 1048576, direction), flush=True)
                    if args.hash_recovery:
                        endpoint.write(header(3, 1))
                        response(endpoint, 72)
                        value = read_exact(endpoint, 72)
                        print({'partition': 'recovery', 'sha256': value[:64].decode(),
                               'local_seconds': struct.unpack('<Q', value[64:])[0] / 1e9})


if __name__ == '__main__':
    main()
