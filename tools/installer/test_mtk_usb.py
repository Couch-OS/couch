import hashlib
import errno
import array
import importlib
from pathlib import Path
import signal
import sys
import tempfile
import time
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch

from couch_install import InstallError
from mtk_session import ReadPolicy, read_session
from mtk_usb import ExactUsbBackend, PinnedImports, bounded_operation, descriptor, strict_handshake, PacketBufferedInput
from test_mtk_readonly import fake_session


class Interface(list):
    bAlternateSetting = 0
    bInterfaceClass = 10
    bInterfaceNumber = 1


class Configuration(list):
    bNumInterfaces = 2


class DALegacy:
    def __init__(self):
        self.pathconfig = NS(get_loader_path=lambda: "/unapproved/generic/loaders")


class UsbBackendTests(unittest.TestCase):
    def setUp(self):
        self.events = []
        self.handshake_bytes = []
        def write(data, **kwargs):
            self.handshake_bytes.extend(data)
            return len(data)
        def read(length, **kwargs):
            return bytes([self.handshake_bytes[-1] ^ 0xff if self.handshake_result else 0])
        interface = Interface([NS(bmAttributes=2, bEndpointAddress=0x81, wMaxPacketSize=64, read=read),
                               NS(bmAttributes=2, bEndpointAddress=2, write=write)])
        self.dev = NS(bus=1, address=2, port_numbers=(3,), idVendor=0x0e8d, idProduct=0x2000,
                      get_active_configuration=lambda: Configuration([interface]),
                      is_kernel_driver_active=lambda number: True,
                      detach_kernel_driver=lambda number: self.events.append(("detach", number)),
                      attach_kernel_driver=lambda number: self.events.append(("attach", number)))
        self.usb = NS(core=NS(find=lambda **kwargs: [self.dev]),
                      util=NS(claim_interface=lambda dev, number: self.events.append(("claim", number)),
                              release_interface=lambda dev, number: self.events.append(("release", number)),
                              dispose_resources=lambda dev: self.events.append("dispose")))
        self.security = 0
        self.hwcode = 0x6580
        self.upload_result = True
        self.handshake_result = True
        self.backend = ExactUsbBackend("unused-test-checkout", usb=self.usb, bindings=(self.config, self.mtk))
        self.addCleanup(self.backend.close)

    def config(self, **kwargs):
        config = NS(iot=False, is_brom=False)
        config.init_hwcode = lambda code: setattr(config, "hwcode", code)
        return config

    def mtk(self, config, **kwargs):
        mtk, _, _ = fake_session()
        mtk.config = config
        mtk.port = NS(cdc=NS(set_line_coding=lambda *args: None, setcontrollinestate=lambda **kwargs: None))
        def initialize(**kwargs):
            mtk.port.handshake()
            config.init_hwcode(self.hwcode)
            config.target_config = mtk.preloader.get_target_config()
            return True
        mtk.preloader = NS(init=initialize,
                           echo=lambda value: True,
                           Cmd=NS(GET_TARGET_CONFIG=NS(value=b"x")),
                           rbyte=lambda length: self.security.to_bytes(4, "big") + b"\0\0")
        mtk.daloader.daconfig.emi = b"fixture EMI" if config.preloader else None
        mtk.daloader.daconfig.extract_emi = lambda preloader=None: None
        mtk.daloader.set_da = lambda: setattr(mtk.daloader, "da", DALegacy())
        def upload(**kwargs):
            mtk.daloader.daconfig.extract_emi(kwargs.get("preloader"))
            mtk.daloader.set_da()
            self.events.append("upload")
            self.assertTrue(config.stock and config.skipwdt)
            self.assertFalse(config.reconnect)
            self.assertEqual(Path(config.loader).read_bytes(), b"loader")
            return self.upload_result
        mtk.daloader.upload_da = upload
        return mtk

    def start(self):
        self.backend.prepare(b"loader")
        self.backend.claim(descriptor(self.dev))
        return self.backend.start_readonly(b"loader", ReadPolicy())

    def test_claims_exact_descriptor_and_restores_interfaces(self):
        self.backend.claim(descriptor(self.dev))
        self.assertIs(self.backend.device, self.dev)
        self.assertEqual(self.dev.default_timeout, 1000)
        self.backend.close()
        self.assertEqual(self.events, [("detach", 0), ("claim", 0), ("detach", 1), ("claim", 1),
                                       ("release", 1), ("release", 0), ("attach", 1), ("attach", 0), "dispose"])

    def test_prepare_uses_explicit_placeholder_without_claiming_usb(self):
        self.backend.prepare(b"loader")
        self.assertEqual(self.backend.mtk.config.interface, 0)
        self.assertIsNone(self.backend.device)
        self.assertEqual(self.events, [])
        self.backend.claim(descriptor(self.dev))
        mtk = self.backend.start_readonly(b"loader", ReadPolicy())
        self.assertEqual(mtk.config.interface, 1)

    def test_partial_claim_failure_is_cleaned_up(self):
        def claim(dev, number):
            if number == 1:
                raise OSError("claim failed")
        self.usb.util.claim_interface = claim
        with self.assertRaises(OSError):
            self.backend.claim(descriptor(self.dev))
        self.backend.close()
        self.assertIn(("release", 0), self.events)
        self.assertIn(("attach", 1), self.events)
        self.assertIn("dispose", self.events)

    def test_disconnected_device_cleanup_is_not_a_second_failure(self):
        self.backend.claim(descriptor(self.dev))
        def gone(*args):
            raise OSError(errno.ENODEV, "Device gone")
        self.usb.util.release_interface = gone
        self.dev.attach_kernel_driver = gone
        self.usb.util.dispose_resources = gone
        self.backend.close()
        self.assertIsNone(self.backend.device)

    def test_cleanup_error_identifies_operation_without_swallowing_enoent(self):
        self.backend.claim(descriptor(self.dev))
        def missing(number):
            raise OSError(errno.ENOENT, "Entity not found")
        self.dev.attach_kernel_driver = missing
        with self.assertRaisesRegex(InstallError, "reattach kernel driver on interface 1.*interface 0"):
            self.backend.close()
        self.assertIn("dispose", self.events)
        self.assertIsNone(self.backend.device)

    def test_boot_request_checks_exact_legacy_ack_and_never_resets(self):
        self.start()
        writes = []
        replies = iter([b"\x5a"])
        self.backend.ep_out = NS(write=lambda data, **kw: writes.append(data) or len(data))
        self.backend.ep_in = NS(read=lambda *a, **kw: next(replies))
        self.backend.enumerate = lambda: []
        self.backend.boot_after_capture()
        self.assertEqual(writes, [b"\xdb", b"\x00\x00\x00\x00", b"\x00", b"\x00", b"\x00", b"\x01"])
        with self.assertRaisesRegex(InstallError, "already attempted"):
            self.backend.boot_after_capture()

    def test_boot_request_rejects_short_write_and_final_nack(self):
        self.start()
        writes = []
        for short in (True, False):
            with self.subTest(short=short):
                self.backend.boot_requested = False
                replies = iter([b"\xa5"])
                self.backend.ep_out = NS(write=lambda data, **kw: writes.append(data) or
                                         (0 if short else len(data)))
                self.backend.ep_in = NS(read=lambda *a, **kw: next(replies))
                with self.assertRaisesRegex(InstallError, "Short DA watchdog|not acknowledged"):
                    self.backend.boot_after_capture()

    def test_boot_request_never_continues_after_bad_ack(self):
        self.start()
        writes = []
        self.backend.ep_out = NS(write=lambda data, **kw: writes.append(data) or len(data))
        self.backend.ep_in = NS(read=lambda *a, **kw: b"\xa5")
        with self.assertRaisesRegex(InstallError, "not acknowledged"):
            self.backend.boot_after_capture()
        self.assertEqual(writes, [b"\xdb", b"\x00\x00\x00\x00", b"\x00", b"\x00", b"\x00", b"\x01"])
        with self.assertRaisesRegex(InstallError, "already attempted"):
            self.backend.boot_after_capture()

    def test_direct_da_start_disables_reconnect_and_write_entries(self):
        mtk = self.start()
        self.assertIn("upload", self.events)
        self.assertEqual(self.handshake_bytes, [0xa0, 0x0a, 0x50, 0x05])
        for function in (mtk.port.cdc.connect, mtk.port.close, mtk.bypass_security,
                         mtk.daloader.writeflash, mtk.daloader.formatflash):
            with self.assertRaises(InstallError):
                function()
        work = self.backend.work.name
        self.backend.close()
        self.assertFalse(Path(work).exists())

    def test_session_gate_runs_concrete_backend_with_mock_usb_end_to_end(self):
        with tempfile.TemporaryDirectory() as root:
            loader = Path(root) / "loader.bin"
            loader.write_bytes(b"loader")
            checksum = hashlib.sha256(b"loader").hexdigest()
            with patch("mtk_session.source_pin", return_value={}):
                with read_session(root, loader, checksum, Path(root) / "locks", descriptor(self.dev),
                                  lambda checkout: self.backend) as reader:
                    report = reader.backup_identity(Path(root) / "originals", reader.description["storage_id"])
                    self.assertTrue(report["complete"])
            self.assertIn("dispose", self.events)

    def test_wrong_chip_and_protected_targets_never_upload(self):
        for code, security in ((0x1234, 0), (0x6580, 1)):
            with self.subTest(code=code, security=security):
                self.hwcode, self.security = code, security
                with self.assertRaises(InstallError):
                    self.start()
                self.assertNotIn("upload", self.events)
                self.backend.close()
                self.backend.started = False

    def test_failed_handshake_does_not_retry_or_upload(self):
        self.handshake_result = False
        with self.assertRaisesRegex(InstallError, "handshake echo mismatch"):
            self.start()
        self.assertNotIn("upload", self.events)

    def test_generic_board_fallback_is_disabled(self):
        mtk = self.start()
        self.assertEqual(mtk.daloader.da.pathconfig.get_loader_path(), self.backend.work.name)
        self.assertFalse((Path(self.backend.work.name) / "Preloader").exists())
        with self.assertRaisesRegex(InstallError, "automatic board preloader"):
            mtk.daloader.daconfig.extract_emi("/unapproved/generic/preloader.bin")

    def test_pinned_board_data_is_used_only_for_emi(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "original-preloader.img"
            data = b"synthetic original board data"
            path.write_bytes(data)
            backend = ExactUsbBackend("unused", preloader=path,
                                      preloader_sha256=hashlib.sha256(data).hexdigest(),
                                      usb=self.usb, bindings=(self.config, self.mtk))
            try:
                backend.prepare(b"loader")
                backend.claim(descriptor(self.dev))
                mtk = backend.start_readonly(b"loader", ReadPolicy())
                self.assertEqual(mtk.config.preloader, data)
                self.assertEqual(Path(mtk.config.loader).read_bytes(), b"loader")
                with self.assertRaisesRegex(InstallError, "automatic board preloader"):
                    mtk.daloader.daconfig.extract_emi(b"different board")
            finally:
                backend.close()

    def test_missing_security_response_is_not_treated_as_unprotected(self):
        original = self.mtk
        def mtk(config, **kwargs):
            result = original(config, **kwargs)
            result.preloader.echo = lambda value: False
            return result
        self.backend.bindings = (self.config, mtk)
        with self.assertRaisesRegex(InstallError, "query unsupported"):
            self.start()
        self.assertNotIn("upload", self.events)

    def test_deadline_escapes_and_restores_timer(self):
        previous = signal.getsignal(signal.SIGALRM)
        with self.assertRaisesRegex(InstallError, "deadline exceeded"):
            with bounded_operation(0.01):
                time.sleep(1)
        self.assertEqual(signal.getitimer(signal.ITIMER_REAL), (0.0, 0.0))
        self.assertEqual(signal.getsignal(signal.SIGALRM), previous)

    def test_packet_reads_keep_surplus_for_next_protocol_request(self):
        reads = []
        def read(size, timeout):
            reads.append((size, timeout))
            return b"abcdef"
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=read))
        self.assertEqual(incoming.read(1), b"a")
        self.assertEqual(incoming.read(3), b"bcd")
        target = array.array("B", [0, 0])
        self.assertEqual(incoming.read(target, timeout=0), 2)
        self.assertEqual(target.tobytes(), b"ef")
        self.assertEqual(reads, [(64, 1000)])

    def test_zero_length_transfer_boundaries_preserve_protocol_bytes(self):
        replies = iter([b"", b"a" * 64, b"", b"bc"])
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=lambda size, timeout: next(replies)))
        target = array.array("B", [0] * 65)
        self.assertEqual(incoming.read(target), 65)
        self.assertEqual(target.tobytes(), b"a" * 64 + b"b")
        self.assertEqual(incoming.read(1), b"c")

    def test_repeated_zero_length_transfers_stop(self):
        calls = []
        def read(size, timeout):
            calls.append(size)
            return b""
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=read))
        with self.assertRaisesRegex(InstallError, "Too many zero-length USB transfers.*logical=1"):
            incoming.read(1)
        self.assertEqual(len(calls), 9)

    def test_oversized_transfer_is_distinct_and_rejected(self):
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=lambda size, timeout: b"x" * 65))
        with self.assertRaisesRegex(InstallError, "Oversized USB transfer: received=65, requested=64"):
            incoming.read(1)

    def test_full_packet_handshake_still_sends_exactly_four_commands(self):
        writes, reads = [], []
        def read(size, timeout):
            reads.append(size)
            return b"\x5f\xf5\xaf\xfa"
        cdc = NS(EP_IN=PacketBufferedInput(NS(wMaxPacketSize=64, read=read)),
                 EP_OUT=NS(write=lambda data, timeout: writes.append(data) or len(data)),
                 set_line_coding=lambda *args: None, setcontrollinestate=lambda **kwargs: None)
        strict_handshake(cdc)
        self.assertEqual(writes, [b"\xa0", b"\x0a", b"\x50", b"\x05"])
        self.assertEqual(reads, [64])

    def test_malformed_ready_is_rejected_without_restarting(self):
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=lambda size, timeout: b"REXDY\x5f"))
        writes = []
        cdc = NS(EP_IN=incoming, EP_OUT=NS(write=lambda data, timeout: writes.append(data) or len(data)),
                 set_line_coding=lambda *args: None, setcontrollinestate=lambda **kwargs: None)
        with self.assertRaisesRegex(InstallError, "Unrecognized preloader preamble"):
            strict_handshake(cdc)
        self.assertEqual(writes, [b"\xa0"])
        self.assertEqual(incoming.pending, b"\x5f")

    def test_known_ready_preamble_adds_one_trigger_but_not_mode_commands(self):
        writes, pauses = [], []
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64,
                    read=lambda size, timeout: b"READYREADY\x5f\xf5\xaf\xfa"))
        cdc = NS(EP_IN=incoming, EP_OUT=NS(write=lambda data, timeout: writes.append(data) or len(data)),
                 set_line_coding=lambda *args: None, setcontrollinestate=lambda **kwargs: None)
        strict_handshake(cdc, sleep=pauses.append)
        self.assertEqual(writes, [b"\xa0", b"\xa0", b"\x0a", b"\x50", b"\x05"])
        self.assertEqual(pauses, [0.03])
        self.assertEqual(incoming.pending, b"")

    def test_ready_loop_is_bounded(self):
        writes = []
        incoming = PacketBufferedInput(NS(wMaxPacketSize=64, read=lambda size, timeout: b"READY"))
        cdc = NS(EP_IN=incoming, EP_OUT=NS(write=lambda data, timeout: writes.append(data) or len(data)),
                 set_line_coding=lambda *args: None, setcontrollinestate=lambda **kwargs: None)
        with self.assertRaisesRegex(InstallError, "Too many READY"):
            strict_handshake(cdc, sleep=lambda _: None)
        self.assertEqual(writes, [b"\xa0", b"\xa0"])

    def test_pinned_importer_compiles_source_without_using_python_cache(self):
        with tempfile.TemporaryDirectory() as root:
            package = Path(root) / "mtkclient"
            package.mkdir()
            source = package / "__init__.py"
            source.write_text("VALUE = 'verified source'\n")
            (package / "__init__.pyc").write_bytes(b"not valid bytecode")
            pinned = {source.resolve(): hashlib.sha256(source.read_bytes()).hexdigest()}
            with patch("mtk_usb.source_pin", return_value=pinned):
                finder = PinnedImports(root)
                sys.meta_path.insert(0, finder)
                try:
                    self.assertEqual(importlib.import_module("mtkclient").VALUE, "verified source")
                finally:
                    finder.close()
            self.assertNotIn("mtkclient", sys.modules)


if __name__ == "__main__":
    unittest.main()
