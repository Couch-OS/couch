import errno
from types import SimpleNamespace as NS
import unittest

from couch_install import InstallError
from mtk_session import Candidate
from mtk_com import (LIBUSB_SERVICES, PIECE, SerialTransport, default_callout, libusb_bound, matching_ports,
                     open_serial_port, port_chain, recorded_services)


class FakeUSBError(IOError):
    def __init__(self, message, code, err):
        super().__init__(err, message)
        self.backend_error_code = code


class FakeUSBTimeoutError(FakeUSBError):
    pass


USB = NS(core=NS(USBError=FakeUSBError, USBTimeoutError=FakeUSBTimeoutError))


class SerialException(IOError):
    pass


class SerialTimeoutException(SerialException):
    pass


class FakePort:
    """A pyserial-like COM port with a scripted receive queue and a transmit log."""
    def __init__(self, name, **settings):
        self.name = name
        self.settings = settings
        self.incoming = bytearray()
        self.written = []
        self.timeout = settings.get('timeout')
        self.write_timeout = settings.get('write_timeout')
        self.baudrate = settings.get('baudrate')
        self.rts = self.dtr = None
        self.is_open = True
        self.fail_write_after = None
        self.gone = False

    @property
    def in_waiting(self):
        if self.gone:
            raise SerialException('ClearCommError failed')
        return len(self.incoming)

    def read(self, size=1):
        if self.gone:
            raise SerialException('device reports readiness to read but returned no data')
        data = bytes(self.incoming[:size])
        del self.incoming[:size]
        return data

    def write(self, data):
        if self.gone:
            raise SerialException('WriteFile failed')
        if self.fail_write_after is not None and len(self.written) >= self.fail_write_after:
            raise SerialTimeoutException('Write timeout')
        self.written.append(bytes(data))
        return len(data)

    def close(self):
        self.is_open = False


class FakeSerialModule:
    SerialException = SerialException
    SerialTimeoutException = SerialTimeoutException

    def __init__(self, fail_opens=0):
        self.ports = []
        self.fail_opens = fail_opens

    def Serial(self, name, **settings):
        if self.fail_opens:
            self.fail_opens -= 1
            raise SerialException('could not open port: access denied')
        port = FakePort(name, **settings)
        self.ports.append(port)
        return port


def port(device, vid=0x0e8d, pid=0x0003, location='1-3.2'):
    return NS(device=device, vid=vid, pid=pid, location=location)


class ResolutionTests(unittest.TestCase):
    def test_location_strings_yield_the_hub_port_chain(self):
        self.assertEqual(port_chain('1-3.2'), (3, 2))
        self.assertEqual(port_chain('1-7'), (7,))
        self.assertEqual(port_chain('1-3.2:x.1'), (3, 2))
        self.assertIsNone(port_chain(None))
        self.assertIsNone(port_chain(''))
        self.assertIsNone(port_chain('PCIROOT(0)#PCI(1400)'))

    def test_matching_prefers_the_exact_chain_and_falls_back_to_unlocated_ports(self):
        ports = [port('COM3', location='1-3.2'), port('COM4', location='1-5'), port('COM5', location=None),
                 port('COM6', pid=0x201c, location='1-3.2')]
        self.assertEqual([p.device for p in matching_ports(ports, 0x0e8d, 0x0003, (3, 2))], ['COM3'])
        self.assertEqual([p.device for p in matching_ports(ports, 0x0e8d, 0x0003, (9,))], ['COM5'])
        self.assertEqual([p.device for p in matching_ports(ports, 0x0e8d, 0x201c, (3, 2))], ['COM6'])
        with self.assertRaises(InstallError):
            matching_ports(ports, 0x0e8d, 0x0003, ())

    def test_libusb_binding_needs_every_recorded_instance(self):
        self.assertFalse(libusb_bound([]))
        self.assertTrue(libusb_bound(['WinUSB']))
        self.assertTrue(libusb_bound(['winusb', 'libusbK']))
        self.assertFalse(libusb_bound(['WinUSB', 'usbser']))
        self.assertFalse(libusb_bound(['usbser']))
        self.assertFalse(libusb_bound([None]))
        self.assertEqual(LIBUSB_SERVICES, {'winusb', 'libusbk', 'libusb0'})

    def test_recorded_services_read_only_the_device_key(self):
        class Registry:
            HKEY_LOCAL_MACHINE = object()
            KEY_READ = 1
            KEY_WOW64_64KEY = 256
            REG_SZ = 1
            opened = []
            def OpenKey(self, root, path, reserved, access):
                Registry.opened.append(path)
                if path.endswith(r'VID_0E8D&PID_0003'):
                    return _Key(['5&1&0&3', '5&2&0&4', 'unbound'])
                if path == '5&1&0&3':
                    return _Key([], {'Service': ('usbser', 1)})
                if path == '5&2&0&4':
                    return _Key([], {'Service': ('WinUSB', 1)})
                if path == 'unbound':
                    return _Key([], {})
                raise OSError(errno.ENOENT, 'missing')
            def EnumKey(self, key, index):
                if index >= len(key.names):
                    raise OSError(errno.ENOENT, 'no more')
                return key.names[index]
            def QueryValueEx(self, key, name):
                if name not in key.values:
                    raise OSError(errno.ENOENT, 'missing')
                return key.values[name]
        class _Key:
            def __init__(self, names, values=None):
                self.names, self.values = names, values or {}
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False
        self.assertEqual(recorded_services(0x0e8d, 0x0003, registry=Registry()), ['usbser', 'WinUSB', None])
        self.assertEqual(recorded_services(0x0e8d, 0x0004, registry=Registry()), [])
        self.assertTrue(Registry.opened[0].startswith(r'SYSTEM\CurrentControlSet\Enum\USB\VID_0E8D&PID_0003'))

    def test_open_returns_none_when_winusb_is_bound_without_waiting(self):
        candidate = Candidate(1, 2, (3, 2), 0x0e8d, 0x0003)
        calls = []
        result = open_serial_port(candidate, 1, USB, comports=lambda: calls.append(1) or [],
                                  services=lambda vid, pid: ['WinUSB'], now=lambda: 0.0,
                                  sleep=lambda s: self.fail('slept'))
        self.assertIsNone(result)
        self.assertEqual(calls, [])

    def test_open_waits_for_the_port_then_returns_a_transport_for_it(self):
        candidate = Candidate(1, 2, (3, 2), 0x0e8d, 0x0003)
        clock = [0.0]
        listings = [[], [], [port('COM7')]]
        opened = []
        def transport(name, usb, **kwargs):
            opened.append((name, usb))
            return 'transport'
        result = open_serial_port(candidate, 1, USB, comports=lambda: listings.pop(0),
                                  services=lambda vid, pid: ['usbser'], transport=transport,
                                  now=lambda: clock[0], sleep=lambda s: clock.__setitem__(0, clock[0] + s))
        self.assertEqual(result, 'transport')
        self.assertEqual(opened, [('COM7', USB)])
        self.assertGreater(clock[0], 0)

    def test_open_fails_when_no_port_appears_or_two_do(self):
        candidate = Candidate(1, 2, (3, 2), 0x0e8d, 0x0003)
        clock = [0.0]
        with self.assertRaises(InstallError):
            open_serial_port(candidate, 1, USB, comports=lambda: [], services=lambda vid, pid: [],
                             now=lambda: clock[0], sleep=lambda s: clock.__setitem__(0, clock[0] + s), wait=1.0)
        with self.assertRaises(InstallError):
            open_serial_port(candidate, 1, USB, comports=lambda: [port('COM1'), port('COM2')],
                             services=lambda vid, pid: [], now=lambda: 0.0, sleep=lambda s: None)
        with self.assertRaises(InstallError):
            open_serial_port(candidate, 1, USB, comports=lambda: [port('LPT1')],
                             services=lambda vid, pid: [], now=lambda: 0.0, sleep=lambda s: None)

    def test_default_callout_follows_the_platform(self):
        from mtk_tty import open_callout
        self.assertIs(default_callout('win32'), open_serial_port)
        self.assertIs(default_callout('darwin'), open_callout)
        self.assertIs(default_callout('linux'), open_callout)


class TransportTests(unittest.TestCase):
    def transport(self, **kwargs):
        module = FakeSerialModule(**kwargs)
        clock = [0.0]
        transport = SerialTransport('COM9', USB, serial_module=module, now=lambda: clock[0],
                                    sleep=lambda s: clock.__setitem__(0, clock[0] + s))
        return transport, module.ports[-1]

    def test_open_retries_briefly_while_windows_starts_the_port(self):
        transport, port = self.transport(fail_opens=3)
        self.assertEqual(port.name, 'COM9')
        self.assertEqual((port.settings['baudrate'], port.settings['rtscts'], port.settings['dsrdtr']),
                         (115200, False, False))
        clock = [0.0]
        with self.assertRaises(FakeUSBError) as caught:
            SerialTransport('COM9', USB, serial_module=FakeSerialModule(fail_opens=100), now=lambda: clock[0],
                            sleep=lambda s: clock.__setitem__(0, clock[0] + s))
        self.assertEqual(caught.exception.errno, errno.ENODEV)
        with self.assertRaises(InstallError):
            SerialTransport('/dev/ttyUSB0', USB, serial_module=FakeSerialModule())

    def test_read_returns_what_arrived_without_waiting_for_the_rest(self):
        transport, port = self.transport()
        port.incoming.extend(b'READY')
        self.assertEqual(transport.read(64, timeout=500), b'READY')
        self.assertEqual(port.timeout, 0.5)
        port.incoming.extend(b'\x5f')
        self.assertEqual(transport.read(1, timeout=500), b'\x5f')
        buffer = bytearray(4)
        port.incoming.extend(b'abcdef')
        self.assertEqual(transport.read(buffer, timeout=None), 4)
        self.assertEqual(bytes(buffer), b'abcd')
        self.assertEqual(port.timeout, 1.0)
        self.assertEqual(transport.read(0), b'')

    def test_read_timeout_and_disconnect_raise_usb_errors(self):
        transport, port = self.transport()
        with self.assertRaises(FakeUSBTimeoutError) as timeout:
            transport.read(1, timeout=200)
        self.assertEqual((timeout.exception.backend_error_code, timeout.exception.errno), (-7, errno.ETIMEDOUT))
        port.gone = True
        with self.assertRaises(FakeUSBError) as gone:
            transport.read(1)
        self.assertEqual((gone.exception.backend_error_code, gone.exception.errno), (-4, errno.ENODEV))
        with self.assertRaises(InstallError):
            transport.read(1024 * 1024 + 1)

    def test_write_delivers_in_bounded_pieces_and_reports_the_length(self):
        transport, port = self.transport()
        self.assertEqual(transport.write(b'\xa0', timeout=500), 1)
        self.assertEqual(port.write_timeout, 0.5)
        data = bytes(range(256)) * (PIECE // 128)  # two pieces exactly
        self.assertEqual(transport.write(data), len(data))
        self.assertEqual([len(w) for w in port.written], [1, PIECE, PIECE])
        self.assertEqual(b''.join(port.written[1:]), data)
        self.assertEqual(transport.write(b''), 0)

    def test_write_timeout_and_disconnect_raise_usb_errors(self):
        transport, port = self.transport()
        port.fail_write_after = 1
        with self.assertRaises(FakeUSBTimeoutError):
            transport.write(b'x' * (PIECE + 1))
        port.gone = True
        with self.assertRaises(FakeUSBError):
            transport.write(b'x')

    def test_line_coding_and_control_lines_go_through_the_port(self):
        transport, port = self.transport()
        transport.set_line_coding(921600, 0, 8, 1)
        self.assertEqual(port.baudrate, 921600)
        transport.setcontrollinestate(rts=True)
        self.assertEqual((port.rts, port.dtr), (True, None))
        transport.setcontrollinestate(dtr=False)
        self.assertEqual((port.rts, port.dtr), (True, False))
        with self.assertRaises(InstallError):
            transport.set_line_coding(115200, 1, 8, 1)
        with self.assertRaises(InstallError):
            transport.setcontrollinestate(rts=True, is_ftdi=True)

    def test_closed_transport_refuses_transfers(self):
        transport, port = self.transport()
        transport.close()
        self.assertFalse(port.is_open)
        transport.close()
        for call in (lambda: transport.read(1), lambda: transport.write(b'x'),
                     lambda: transport.set_line_coding(), lambda: transport.setcontrollinestate(rts=True)):
            with self.assertRaises(InstallError):
                call()


if __name__ == '__main__':
    unittest.main()
