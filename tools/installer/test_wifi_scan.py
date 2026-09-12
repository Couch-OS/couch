import getpass
import hashlib
import io
import json
import struct
import unittest
import warnings
from unittest.mock import patch

import wifi_scan
import wifi_trial


def frame(value):
    body = json.dumps(value).encode()
    return struct.pack('<4sIQ', b'CBR1', 0, len(body)) + body


class Endpoint:
    def __init__(self, data=b''):
        self.data = io.BytesIO(data)
        self.sent = []
    def read(self, n, **_):
        return self.data.read(n)
    def write(self, data, **_):
        self.sent.append(data)
        return len(data)


class Terminal:
    def __init__(self, choices=(), text=(), password='password'):
        self.choices, self.text = iter(choices), iter(text)
        self.password = password
        self.prompts, self.stages = [], []
        self.secrets = 0
    def choose(self, title, options):
        self.prompts.append((title, options))
        return next(self.choices)
    def ask(self, title, **_):
        return next(self.text, '')
    def secret(self, _):
        self.secrets += 1
        return self.password
    def stage(self, *values):
        self.stages.append(values)


def status(scan=True):
    return {'status': 'ready', 'scan': scan}


def result(networks=(), state='ok'):
    return {'status': state, 'networks': list(networks), 'truncated': False}


def network(raw=b'network', security='wpa2', dbm=-40):
    return {'ssid_hex': raw.hex(), 'security': security, 'dbm': dbm}


class ScanTests(unittest.TestCase):
    def test_old_stage_never_receives_unknown_opcode(self):
        endpoint = Endpoint(frame({'status': 'ready'}))
        value = wifi_scan.scan(endpoint, endpoint)
        self.assertEqual(value['status'], 'unsupported')
        self.assertEqual([struct.unpack('<4sIQ', b)[1] for b in endpoint.sent], [5])

    def test_structured_failure_is_consumed_before_next_status(self):
        endpoint = Endpoint(frame(status()) + frame(result(state='unavailable')) + frame(status()))
        self.assertEqual(wifi_scan.scan(endpoint, endpoint)['status'], 'unavailable')
        self.assertEqual(wifi_trial.wifi.wifi_status(endpoint, endpoint)['status'], 'ready')
        self.assertEqual(endpoint.data.read(), b'')

    def test_transport_errors_never_offer_manual_fallback(self):
        for response in [b'', b'CBR1', struct.pack('<4sIQ', b'CBR1', 0, 16385), frame({'oops': True})]:
            endpoint = Endpoint(frame(status()) + response)
            terminal = Terminal(['manual'], ['hidden'])
            with self.assertRaises((ValueError, struct.error)):
                wifi_scan.network_form(terminal, endpoint, endpoint)
            self.assertEqual(terminal.prompts, [])

    def test_raw_bytes_derive_psk_without_lossy_display_roundtrip(self):
        raw = b' a\xff\x00\x1b[31m\\ '
        endpoint = Endpoint(frame(status()) + frame(result([network(raw)])))
        terminal = Terminal(['0'])
        value = wifi_scan.network_form(terminal, endpoint, endpoint)
        self.assertEqual(value['ssid_hex'], raw.hex())
        self.assertEqual(value['psk_hex'], hashlib.pbkdf2_hmac('sha1', b'password', raw, 4096, 32).hex())
        label = terminal.prompts[0][1][0]['label']
        self.assertTrue(label.startswith(' a'))
        self.assertIn('\\xff', label)
        self.assertNotIn('\x1b', label)
        self.assertNotIn('\x00', label)
        self.assertTrue(all(raw.hex() not in str(v) for v in terminal.stages))

    def test_manual_hidden_and_failed_scan_fallback_keep_exact_ssid(self):
        for response in [result(), result(state='unavailable')]:
            endpoint = Endpoint(frame(status()) + frame(response))
            terminal = Terminal(['manual', 'open'], [' hidden '])
            value = wifi_scan.network_form(terminal, endpoint, endpoint)
            self.assertEqual(value, {'ssid_hex': b' hidden '.hex(), 'psk_hex': None})
            self.assertEqual(terminal.secrets, 0)

    def test_rescan_and_security_variants_are_distinct(self):
        variants = [network(security='wpa2'), network(security='open')]
        endpoint = Endpoint(frame(status()) + frame(result()) + frame(status()) + frame(result(variants)))
        terminal = Terminal(['rescan', '1'])
        self.assertIsNone(wifi_scan.network_form(terminal, endpoint, endpoint)['psk_hex'])
        self.assertNotEqual(terminal.prompts[1][1][0]['label'], terminal.prompts[1][1][1]['label'])
        self.assertEqual(terminal.secrets, 0)

    def test_echoed_password_fallback_is_rejected_for_scanned_network(self):
        endpoint = Endpoint(frame(status()) + frame(result([network()])))
        terminal = Terminal(['0'])
        def secret(_):
            warnings.warn('fixture', getpass.GetPassWarning)
            return 'password'
        terminal.secret = secret
        with self.assertRaises(getpass.GetPassWarning):
            wifi_scan.network_form(terminal, endpoint, endpoint)

    def test_control_and_bidi_display_is_inert_and_not_used_as_identity(self):
        self.assertEqual(wifi_scan.display_ssid(b'  test  '), '  test  ')
        self.assertNotIn('\u202e', wifi_scan.display_ssid('x\u202ey'.encode()))
        self.assertNotIn('\n', wifi_scan.display_ssid(b'line\nfeed'))

    def test_backend_keeps_scan_labels_in_prompt_channel_only(self):
        from ratatui_backend import BackendView
        raw = b'fixture-private-ssid'
        endpoint = Endpoint(frame(status()) + frame(result([network(raw)])))
        answers = '\n'.join(json.dumps(v) for v in [{'id': 1, 'value': '0'}, {'id': 2, 'value': 'fixture-password'}]) + '\n'
        output = io.StringIO()
        view = BackendView(io.StringIO(answers), output)
        view.log = io.StringIO()
        selected = wifi_scan.network_form(view, endpoint, endpoint)
        self.assertEqual(selected['ssid_hex'], raw.hex())
        events = [json.loads(line) for line in output.getvalue().splitlines()]
        non_prompts = json.dumps([v for v in events if v['event'] != 'prompt'])
        self.assertIn(raw.decode(), output.getvalue())
        for private in [raw.decode(), raw.hex(), 'fixture-password', selected['psk_hex']]:
            self.assertNotIn(private, non_prompts)
            self.assertNotIn(private, view.log.getvalue())
        self.assertNotIn('fixture-password', output.getvalue())

    def test_unsupported_network_never_requests_password(self):
        endpoint = Endpoint(frame(status()) + frame(result([network(security='enterprise')])) + frame(status()) + frame(result([network(security='open')])))
        terminal = Terminal(['0', '0'])
        self.assertIsNone(wifi_scan.network_form(terminal, endpoint, endpoint)['psk_hex'])
        self.assertEqual(terminal.secrets, 0)
        self.assertTrue(any('unsupported' in str(v) for v in terminal.stages))
