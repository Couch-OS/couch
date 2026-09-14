import errno
import os
import plistlib
import struct
import sys
import threading
import time
from types import SimpleNamespace as NS
import unittest

if sys.platform == 'win32':
    raise unittest.SkipTest('The macOS callout transport needs termios; Windows never selects it')
import fcntl
import termios

from couch_install import InstallError
from mtk_session import Candidate
from mtk_tty import TtyEndpoint, TtyTransport, callout_device, location_topology, open_callout, registry_listing


class FakeUSBError(IOError):
    def __init__(self, strerror, error_code=None, errno=None):
        super().__init__(errno, strerror)
        self.backend_error_code = error_code


class FakeUSBTimeoutError(FakeUSBError):
    pass


USB = NS(core=NS(USBError=FakeUSBError, USBTimeoutError=FakeUSBTimeoutError))
CANDIDATE = Candidate(2, 7, (1,), 0x0e8d, 0x2000)


def device(*, address=7, location=0x02100000, vid=0x0e8d, pid=0x2000, callouts=("/dev/cu.usbmodem21001",),
           interface=1, cls="IOUSBHostDevice"):
    serial = [{"IOObjectClass": "IOSerialBSDClient", "IOCalloutDevice": path, "IODialinDevice": path.replace("cu.", "tty.")}
              for path in callouts]
    data = {"IOObjectClass": "IOUSBHostInterface", "bInterfaceNumber": interface, "bInterfaceClass": 10,
            "IORegistryEntryChildren": [{"IOObjectClass": "AppleUSBACMData", "IORegistryEntryChildren": serial}]}
    control = {"IOObjectClass": "IOUSBHostInterface", "bInterfaceNumber": 0, "bInterfaceClass": 2,
               "IORegistryEntryChildren": [{"IOObjectClass": "AppleUSBACMControl"}]}
    return {"IOObjectClass": cls, "idVendor": vid, "idProduct": pid, "locationID": location, "USB Address": address,
            "IORegistryEntryChildren": [{"IOObjectClass": "AppleUSBHostCompositeDevice"}, control, data]}


class RegistryTests(unittest.TestCase):
    def test_location_encodes_bus_and_port_chain(self):
        self.assertEqual(location_topology(0x02100000), (2, (1,)))
        self.assertEqual(location_topology(0x14230000), (0x14, (2, 3)))
        self.assertEqual(location_topology(0), (0, ()))
        with self.assertRaises(InstallError):
            location_topology(-1)

    def test_resolves_exact_device_and_data_interface(self):
        listing = [device(address=3, location=0x02200000), device(), device(vid=0x1234)]
        self.assertEqual(callout_device(listing, CANDIDATE, 1), "/dev/cu.usbmodem21001")
        # A free interface (no serial client) lets libusb claim directly.
        self.assertIsNone(callout_device([device(callouts=())], CANDIDATE, 1))

    def test_rejects_ambiguous_or_mismatched_registry_state(self):
        for listing, interface in (
            ([device(address=8)], 1),                       # different USB address
            ([device(location=0x03100000)], 1),             # different bus
            ([device(), device()], 1),                      # duplicate device
            ([device()], 2),                                # interface not registered
            ([device(callouts=("/dev/cu.a", "/dev/cu.b"))], 1),
            ([device(callouts=("/dev/tty.usbmodem1",))], 1),
            ([device(callouts=("/dev/cu.usb modem",))], 1),
            ([device(cls="IOUSBHostInterface")], 1),
        ):
            with self.subTest(listing=listing, interface=interface):
                with self.assertRaises(InstallError):
                    callout_device(listing, CANDIDATE, interface)

    def test_registry_listing_parses_plist_and_bounds_output(self):
        payload = plistlib.dumps([device()])
        self.assertEqual(registry_listing(run=lambda *a, **k: NS(returncode=0, stdout=payload)),
                         [device()])
        with self.assertRaises(InstallError):
            registry_listing(run=lambda *a, **k: NS(returncode=1, stdout=payload))
        with self.assertRaises(InstallError):
            registry_listing(run=lambda *a, **k: NS(returncode=0, stdout=plistlib.dumps({"a": 1})))

    def test_open_callout_returns_transport_or_none(self):
        opened = []
        transport = open_callout(CANDIDATE, 1, USB, listing=lambda: [device()],
                                 transport=lambda path, usb: opened.append((path, usb)) or "transport")
        self.assertEqual(transport, "transport")
        self.assertEqual(opened, [("/dev/cu.usbmodem21001", USB)])
        self.assertIsNone(open_callout(CANDIDATE, 1, USB, listing=lambda: [device(callouts=())],
                                       transport=lambda *a: self.fail("opened a missing port")))


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.master, slave = os.openpty()
        self.addCleanup(self._close_master)
        self.ioctls = []
        def opener(path, flags):
            self.assertEqual(path, "/dev/cu.fixture")
            self.assertTrue(flags & os.O_NONBLOCK and flags & os.O_NOCTTY)
            os.set_blocking(slave, False)
            return slave
        def ioctl(fd, request, arg=None):
            # The pacing query must hit the real pty so the fixture reflects a
            # port that is or is not draining; everything else is recorded.
            if request == termios.TIOCOUTQ:
                return fcntl.ioctl(fd, request, arg)
            self.ioctls.append((request, arg))
        self.transport = TtyTransport("/dev/cu.fixture", USB, opener=opener, ioctl=ioctl)
        self.addCleanup(self.transport.close)

    def _close_master(self):
        try:
            os.close(self.master)
        except OSError:
            pass

    def test_open_is_exclusive_and_raw(self):
        self.assertEqual(self.ioctls, [(termios.TIOCEXCL, None)])
        iflag, oflag, cflag, lflag, ispeed, ospeed, cc = termios.tcgetattr(self.transport.fd)
        self.assertFalse(lflag & (termios.ECHO | termios.ICANON | termios.ISIG))
        self.assertFalse(iflag & (termios.ICRNL | termios.IXON))
        self.assertFalse(oflag & termios.OPOST)
        self.assertEqual(cflag & termios.CSIZE, termios.CS8)
        self.assertEqual((ispeed, ospeed), (termios.B115200, termios.B115200))

    def test_read_returns_available_bytes_like_a_short_packet(self):
        os.write(self.master, b"\x5f")
        self.assertEqual(self.transport.read(64, 500), b"\x5f")
        os.write(self.master, b"READY")
        buffer = bytearray(64)
        self.assertEqual(self.transport.read(buffer, 500), 5)
        self.assertEqual(bytes(buffer[:5]), b"READY")
        self.assertEqual(self.transport.read(0), b"")

    def test_read_timeout_raises_usb_timeout_with_upstream_wording(self):
        started = time.monotonic()
        with self.assertRaises(FakeUSBTimeoutError) as raised:
            self.transport.read(1, 50)
        self.assertIn("timed out", str(raised.exception.strerror))
        self.assertEqual(raised.exception.errno, errno.ETIMEDOUT)
        self.assertLess(time.monotonic() - started, 2)

    def test_write_delivers_all_bytes_and_reports_length(self):
        self.assertEqual(self.transport.write(b"\xa0", 500), 1)
        self.assertEqual(os.read(self.master, 16), b"\xa0")
        self.assertEqual(self.transport.write(b"", 500), 0)
        payload = bytes(range(256)) * 8
        received = bytearray()
        def drain():
            deadline = time.monotonic() + 5
            while len(received) < len(payload) and time.monotonic() < deadline:
                try:
                    received.extend(os.read(self.master, 4096))
                except BlockingIOError:
                    time.sleep(0.001)
        os.set_blocking(self.master, False)
        reader = threading.Thread(target=drain)
        reader.start()
        self.assertEqual(self.transport.write(payload, 5000), len(payload))
        reader.join()
        self.assertEqual(bytes(received), payload)

    def test_large_write_outlives_the_timeout_while_the_port_keeps_draining(self):
        # The DA writer pushes a 1 MiB chunk with a one-second timeout; over the
        # preloader's full-speed link that takes longer than one second.
        payload = bytes(range(256)) * 1024
        received = bytearray()
        os.set_blocking(self.master, False)
        def drain():
            deadline = time.monotonic() + 10
            while len(received) < len(payload) and time.monotonic() < deadline:
                try:
                    received.extend(os.read(self.master, 8192))
                except BlockingIOError:
                    pass
                time.sleep(0.008)
        reader = threading.Thread(target=drain)
        reader.start()
        started = time.monotonic()
        self.assertEqual(self.transport.write(payload, 50), len(payload))
        elapsed = time.monotonic() - started
        reader.join(10)
        self.assertEqual(bytes(received), payload)
        self.assertGreater(elapsed, 0.05, "fixture did not exercise a slow drain")

    def test_write_still_fails_when_the_port_stops_draining(self):
        started = time.monotonic()
        with self.assertRaises(FakeUSBTimeoutError):
            self.transport.write(b"\x00" * (4 * 1024 * 1024), 50)
        self.assertLess(time.monotonic() - started, 5)

    def test_timeout_frame_names_the_stalled_direction_not_the_helper(self):
        import linecache
        for direction, call in (("read", lambda: self.transport.read(1, 20)),
                                ("write", lambda: self.transport.write(b"x" * (4 * 1024 * 1024), 20))):
            with self.subTest(direction=direction):
                trace = None
                try:
                    call()
                except FakeUSBTimeoutError as error:
                    # assertRaises strips the traceback; keep it to check the frame.
                    trace = error.__traceback__
                self.assertIsNotNone(trace, "the fixture did not time out")
                while trace.tb_next is not None:
                    trace = trace.tb_next
                self.assertEqual(trace.tb_frame.f_code.co_name, direction)
                source = linecache.getline(trace.tb_frame.f_code.co_filename, trace.tb_lineno)
                self.assertIn("raise self._timed_out()", source)

    def test_large_write_waits_for_the_port_between_pieces_only(self):
        from unittest.mock import patch
        drains = []
        original = TtyTransport._drain
        def counting(transport, deadline):
            drains.append(True)
            return original(transport, deadline)
        # A protocol field is smaller than one piece and must not wait at all.
        with patch.object(TtyTransport, "_drain", counting):
            self.assertEqual(self.transport.write(b"\xa0", 500), 1)
        self.assertEqual(drains, [])
        self.assertEqual(os.read(self.master, 16), b"\xa0")
        # A bulk chunk waits between pieces, never after the last one.
        payload = bytes(range(256)) * (TtyTransport.PACED_PIECE * 3 // 256)
        received = bytearray()
        os.set_blocking(self.master, False)
        def reader():
            deadline = time.monotonic() + 10
            while len(received) < len(payload) and time.monotonic() < deadline:
                try:
                    received.extend(os.read(self.master, 8192))
                except BlockingIOError:
                    time.sleep(0.001)
        thread = threading.Thread(target=reader)
        thread.start()
        with patch.object(TtyTransport, "_drain", counting):
            self.assertEqual(self.transport.write(payload, 5000), len(payload))
        thread.join(10)
        self.assertEqual(bytes(received), payload)
        self.assertEqual(len(drains), 2)

    def test_disconnected_port_reports_no_such_device(self):
        os.close(self.master)
        with self.assertRaises(FakeUSBError) as raised:
            self.transport.read(1, 200)
        self.assertEqual(raised.exception.errno, errno.ENODEV)

    def test_line_coding_and_control_lines_use_the_tty(self):
        self.transport.set_line_coding(921600, 0, 8, 1)
        with self.assertRaises(InstallError):
            self.transport.set_line_coding(921600, 1, 8, 1)
        self.transport.setcontrollinestate(rts=True)
        self.transport.setcontrollinestate(rts=False, dtr=True)
        self.assertEqual(self.ioctls[1:], [(termios.TIOCMBIS, struct.pack("i", termios.TIOCM_RTS)),
                                           (termios.TIOCMBIC, struct.pack("i", termios.TIOCM_RTS)),
                                           (termios.TIOCMBIS, struct.pack("i", termios.TIOCM_DTR))])

    def test_endpoints_keep_descriptor_attributes_and_direction(self):
        incoming = TtyEndpoint(self.transport, NS(bEndpointAddress=0x81, wMaxPacketSize=64))
        outgoing = TtyEndpoint(self.transport, NS(bEndpointAddress=0x02, wMaxPacketSize=64))
        self.assertEqual((incoming.wMaxPacketSize, outgoing.bEndpointAddress), (64, 2))
        os.write(self.master, b"\x5a")
        self.assertEqual(incoming.read(64, timeout=500), b"\x5a")
        self.assertEqual(outgoing.write(b"\xdb", timeout=500), 1)
        self.assertEqual(os.read(self.master, 1), b"\xdb")
        with self.assertRaises(InstallError):
            incoming.write(b"x")
        with self.assertRaises(InstallError):
            outgoing.read(1)

    def test_closed_transport_refuses_transfers(self):
        self.transport.close()
        self.transport.close()
        with self.assertRaises(InstallError):
            self.transport.read(1)
        with self.assertRaises(InstallError):
            self.transport.write(b"x")


if __name__ == "__main__":
    unittest.main()
