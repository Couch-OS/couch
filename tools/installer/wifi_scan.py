"""Remote-only USB Wi-Fi discovery and credential picker; never log SSIDs."""
import json
import struct
import unicodedata

import wifi_trial as trial

SHORT_SECURITY = {'wpa2': 'WPA2', 'open': 'Open', 'enterprise': 'Enterprise',
                  'wpa3': 'WPA3-only', 'wep': 'WEP', 'unsupported': 'Unsupported'}

SECURITY = {'wpa2': 'WPA2 Personal', 'open': 'Open', 'enterprise': 'Enterprise · unsupported',
            'wpa3': 'WPA3-only · unsupported', 'wep': 'WEP · unsupported',
            'unsupported': 'Unsupported security'}


def scan(out, incoming):
    # Old stages terminate USB on unknown opcodes. Negotiate before sending op7.
    status = trial.wifi.wifi_status(out, incoming)
    if status.get('scan') is not True:
        return {'status': 'unsupported', 'networks': [], 'truncated': False}
    if out.write(struct.pack('<4sIQ', b'CBP1', 7, 0), timeout=30000) != 16:
        raise ValueError('Wi-Fi scan USB request was incomplete; reconnect the installer stage')
    magic, code, size = struct.unpack('<4sIQ', trial.wifi.read_exact(incoming, 16))
    if magic != b'CBR1' or code != 0 or not 1 <= size <= 16384:
        raise ValueError('Invalid Wi-Fi scan frame; installer USB session stopped')
    # Never turn transport/JSON errors into manual fallback: framing may be lost.
    try:
        value = json.loads(trial.wifi.read_exact(incoming, size))
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise ValueError('Invalid Wi-Fi scan encoding') from None
    if (not isinstance(value, dict) or set(value) != {'status', 'networks', 'truncated'}
            or value['status'] not in ('ok', 'unavailable')
            or type(value['truncated']) is not bool
            or not isinstance(value['networks'], list) or len(value['networks']) > 64):
        raise ValueError('Invalid Wi-Fi scan response')
    for network in value['networks']:
        if (not isinstance(network, dict) or set(network) != {'ssid_hex', 'security', 'dbm'}
                or not isinstance(network['security'], str) or network['security'] not in SECURITY
                or type(network['dbm']) is not int
                or not -127 <= network['dbm'] <= 0 or not isinstance(network['ssid_hex'], str)
                or not 2 <= len(network['ssid_hex']) <= 64 or len(network['ssid_hex']) % 2
                or any(c not in '0123456789abcdef' for c in network['ssid_hex'])):
            raise ValueError('Invalid scanned network')
    if value['status'] != 'ok' and value['networks']:
        raise ValueError('Unexpected scan failure payload')
    return value


def display_ssid(raw):
    # Surrogateescape preserves arbitrary SSID octets; controls/bidi cannot affect TUI.
    result = []
    for char in raw.decode('utf-8', 'surrogateescape'):
        code = ord(char)
        if 0xdc80 <= code <= 0xdcff:
            result.append(f'\\x{code - 0xdc00:02x}')
        elif unicodedata.category(char).startswith('C') or char in '\\':
            result.extend(f'\\x{byte:02x}' for byte in char.encode('utf-8'))
        else:
            result.append(char)
    return ''.join(result)


def network_form(terminal, out, incoming):
    while True:
        terminal.stage(2, 'Scanning for Wi-Fi networks visible to your remote…',
                       'Keep the remote near your router. You can also enter a hidden network manually.')
        result = scan(out, incoming)
        networks = result['networks']
        if result['status'] == 'unsupported':
            detail = 'This installer stage does not support scanning. Enter your network manually.'
        elif result['status'] != 'ok':
            detail = 'The remote could not complete its scan. Try again or enter your network manually.'
        elif not networks:
            detail = 'No named networks were found. Move closer, scan again, or enter a hidden network.'
        elif result['truncated']:
            detail = 'Showing a limited list. Scan again or enter a network that is not listed.'
        else:
            detail = 'Choose a network, then enter its password. Only your remote was scanned.'
        terminal.stage(2, 'Choose Wi-Fi for your remote.', detail)
        options = [{'value': str(i),
                    'label': f"{display_ssid(bytes.fromhex(n['ssid_hex']))} · {SHORT_SECURITY[n['security']]} · {n['dbm']} dBm",
                    'detail': SECURITY[n['security']] + ' · Signal measured by your remote.'} for i, n in enumerate(networks)]
        options.append({'value': 'manual', 'label': 'Enter network manually / hidden network',
                        'detail': 'Type the exact SSID and select WPA2 Personal or open.'})
        if result['status'] != 'unsupported':
            options.append({'value': 'rescan', 'label': 'Scan again', 'detail': 'Refresh networks visible to the remote.'})
        selected = terminal.choose('Available Wi-Fi networks', options)
        if selected == 'manual':
            return trial.network_form(terminal)
        if selected == 'rescan':
            continue
        # Choice values are IDs, never display labels or raw SSIDs.
        if selected not in {str(i) for i in range(len(networks))}:
            raise ValueError('Invalid scanned network selection')
        network = networks[int(selected)]
        if network['security'] not in ('open', 'wpa2'):
            terminal.stage(2, 'This network requires unsupported Wi-Fi security.',
                           'Choose WPA2 Personal or an open network; the installer cannot join enterprise, WEP or WPA3-only networks.')
            terminal.ask('Press Enter to choose another network')
            continue
        while True:
            password = trial.network_password(terminal, network['security'])
            try:
                return trial.wifi.credentials(bytes.fromhex(network['ssid_hex']), password)
            except ValueError as error:
                terminal.stage(2, str(error), 'Enter the password again. The selected SSID is unchanged.')
            finally:
                del password
