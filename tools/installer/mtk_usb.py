"""Experimental exact-device PyUSB binding; DA startup validated on one HA100.

Instantiating imports dependencies; enumerate/claim/start are explicit operations.
The read-only capture CLI uses this backend; physical installation stays disabled.
"""
from contextlib import contextmanager
import importlib
import errno
import importlib.abc
import importlib.util
import logging
from pathlib import Path
import signal
import struct
import sys
import tempfile
import threading
import time

from couch_install import InstallError, require
from mtk_session import Candidate, ReadPolicy, loader_bytes, select_candidate, source_pin


class _Deadline(BaseException):
    # Upstream has broad Exception handlers. A deadline must escape those loops.
    pass


@contextmanager
def bounded_operation(seconds):
    require(threading.current_thread() is threading.main_thread() and hasattr(signal, "setitimer"),
            "Physical MTK operations require the Linux/macOS main thread")
    require(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0), "Another operation owns the deadline timer")
    previous = signal.getsignal(signal.SIGALRM)
    def expired(signum, frame):
        raise _Deadline()
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds, 0.1)
    try:
        yield
    except _Deadline as error:
        raise InstallError("MTK operation deadline exceeded; session cannot reconnect") from error
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


class PinnedImports(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    """Compile verified package source directly, ignoring on-disk bytecode caches."""
    def __init__(self, checkout):
        self.checkout = Path(checkout).resolve()
        self.pinned = source_pin(self.checkout)
        require(not any(name == "mtkclient" or name.startswith("mtkclient.") for name in sys.modules),
                "Start the physical backend in a fresh process without imported mtkclient modules")
        self.loaded = []

    def find_spec(self, fullname, path=None, target=None):
        if fullname != "mtkclient" and not fullname.startswith("mtkclient."):
            return None
        base = self.checkout.joinpath(*fullname.split("."))
        filename = base / "__init__.py" if (base / "__init__.py").exists() else base.with_suffix(".py")
        require(filename in self.pinned, f"Module is not in the source pin: {fullname}")
        return importlib.util.spec_from_file_location(fullname, filename, loader=self,
                    submodule_search_locations=[str(base)] if filename.name == "__init__.py" else None)

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        import hashlib
        path = Path(module.__spec__.origin)
        data = path.read_bytes()
        require(hashlib.sha256(data).hexdigest() == self.pinned[path], "Source changed during import")
        self.loaded.append(module.__name__)
        exec(compile(data, str(path), "exec"), module.__dict__)

    def close(self):
        if self in sys.meta_path:
            sys.meta_path.remove(self)
        for name in reversed(self.loaded):
            sys.modules.pop(name, None)


def descriptor(device):
    return Candidate(device.bus, device.address, tuple(device.port_numbers or ()), device.idVendor, device.idProduct)


class PacketBufferedInput:
    """Read complete USB packets without losing surplus protocol bytes."""
    def __init__(self, endpoint):
        self.endpoint = endpoint
        self.pending = bytearray()
        require(type(endpoint.wMaxPacketSize) is int and 0 < endpoint.wMaxPacketSize <= 1024
                and endpoint.wMaxPacketSize & (endpoint.wMaxPacketSize - 1) == 0,
                "Unsupported USB bulk packet size")

    def __getattr__(self, name):
        return getattr(self.endpoint, name)

    def read(self, size_or_buffer, timeout=None):
        size = size_or_buffer if isinstance(size_or_buffer, int) else len(size_or_buffer)
        require(0 <= size <= 1024 * 1024, "USB protocol read exceeds transfer limit")
        timeout = min(1000, timeout) if timeout is not None and timeout > 0 else 1000
        empty_transfers = 0
        while len(self.pending) < size:
            remaining = size - len(self.pending)
            packet = self.endpoint.wMaxPacketSize
            request = ((remaining + packet - 1) // packet) * packet
            data = bytes(self.endpoint.read(request, timeout=timeout))
            require(len(data) <= request,
                    f"Oversized USB transfer: received={len(data)}, requested={request}, logical={size}")
            # A successful zero-length bulk transfer terminates a USB transfer,
            # not the DA's length-delimited byte stream. It supplies no bytes.
            # Bound even fast repeated ZLPs; the enclosing operation also has
            # an absolute deadline. Never discard nonempty protocol data.
            if not data:
                empty_transfers += 1
                require(empty_transfers <= 8,
                        f"Too many zero-length USB transfers: requested={request}, logical={size}, "
                        f"buffered={len(self.pending)}")
                continue
            self.pending.extend(data)
        data = bytes(self.pending[:size])
        del self.pending[:size]
        if isinstance(size_or_buffer, int):
            return data
        memoryview(size_or_buffer).cast("B")[:size] = data
        return size


def strict_handshake(cdc, *, sleep=time.sleep):
    cdc.set_line_coding(921600, 0, 8, 1)
    cdc.setcontrollinestate(rts=True)

    def write(byte):
        require(cdc.EP_OUT.write(bytes([byte]), timeout=500) == 1,
                f"Preloader handshake write failed at {byte:02x}")

    def check(byte, reply):
        require(len(reply) == 1 and reply[0] == (byte ^ 0xff),
                f"Preloader handshake echo mismatch at {byte:02x}: {bytes(reply).hex() or 'empty'}; "
                f"buffered={bytes(getattr(cdc.EP_IN, 'pending', b''))[:16].hex() or 'none'} "
                "(no unrecognized bytes discarded)")

    write(0xa0)
    reply = cdc.EP_IN.read(1, timeout=500)
    banners = 0
    while reply == b"R":
        banner = reply + cdc.EP_IN.read(4, timeout=500)
        require(banner == b"READY", f"Unrecognized preloader preamble: {banner.hex()}")
        banners += 1
        require(banners <= 8, "Too many READY banners before download handshake")
        if banners == 1:
            # MT6580 usb_listen consumes the trigger A0 before entering the
            # download handler, which then expects its own four-byte sync.
            # The listener polls every 20 ms. Keep the second A0 in a later
            # receive cycle; no mode command or reset is needed.
            sleep(0.03)
            write(0xa0)
        reply = cdc.EP_IN.read(1, timeout=500)
    check(0xa0, reply)
    for byte in (0x0a, 0x50, 0x05):
        write(byte)
        check(byte, cdc.EP_IN.read(1, timeout=500))


class ExactUsbBackend:
    def __init__(self, checkout, *, preloader=None, preloader_sha256=None, usb=None, bindings=None):
        self.checkout = checkout
        require((preloader is None) == (preloader_sha256 is None), "Provide both board preloader path and SHA-256")
        # Read-only board-data input. It is never passed to a partition writer
        # or used as the downloaded DA executable.
        self.preloader_data = loader_bytes(preloader, preloader_sha256) if preloader is not None else None
        if usb is None:
            import usb.core
            import usb.util
            usb = sys.modules["usb"]
        self.usb = usb
        self.bindings = bindings  # Unit tests supply constructors; production uses pinned imports.
        self.device = None
        self.interfaces = []
        self.detached = []
        self.mtk = None
        self.imports = None
        self.work = None
        self.started = False

    def enumerate(self):
        return [descriptor(dev) for dev in self.usb.core.find(find_all=True, idVendor=0x0e8d)]

    def claim(self, expected):
        with bounded_operation(10):
            self._claim(expected)

    def _claim(self, expected):
        require(self.device is None, "USB backend already claimed a device")
        devices = list(self.usb.core.find(find_all=True, idVendor=0x0e8d))
        select_candidate([descriptor(dev) for dev in devices], expected)
        dev = next(dev for dev in devices if descriptor(dev) == expected)
        self.device = dev  # Retain this exact descriptor; never rediscover on an error.
        dev.default_timeout = 1000
        config = dev.get_active_configuration()  # No reset or set_configuration fallback.
        self.configuration = config
        candidates = []
        for interface in config:
            if interface.bAlternateSetting != 0 or interface.bInterfaceClass != 0x0a:
                continue
            incoming = [ep for ep in interface if ep.bmAttributes & 3 == 2 and ep.bEndpointAddress & 0x80]
            outgoing = [ep for ep in interface if ep.bmAttributes & 3 == 2 and not ep.bEndpointAddress & 0x80]
            if len(incoming) == len(outgoing) == 1:
                candidates.append((interface, incoming[0], outgoing[0]))
        require(len(candidates) == 1, "Expected one CDC data interface with bulk IN/OUT")
        self.interface, ep_in, self.ep_out = candidates[0]
        self.ep_in = PacketBufferedInput(ep_in)
        for number in dict.fromkeys((0, self.interface.bInterfaceNumber)):
            try:
                active = dev.is_kernel_driver_active(number)
            except NotImplementedError:
                active = False
            if active:
                dev.detach_kernel_driver(number)
                self.detached.append(number)
            self.usb.util.claim_interface(dev, number)
            self.interfaces.append(number)

    def claimed_candidate(self):
        return descriptor(self.device) if self.device else None

    def _bindings(self):
        if self.bindings is not None:
            return self.bindings
        self.imports = PinnedImports(self.checkout)
        sys.meta_path.insert(0, self.imports)
        config = importlib.import_module("mtkclient.config.mtk_config").MtkConfig
        mtk = importlib.import_module("mtkclient.Library.mtk_class").Mtk
        return config, mtk

    def _forbidden(self, *args, **kwargs):
        raise InstallError("Reset, reconnect, security bypass and flash writes are disabled")

    def prepare(self, loader):
        require(self.device is None and self.mtk is None, "Prepare the backend before waiting for USB")
        self.prepared_loader = bytes(loader)
        with bounded_operation(30):
            Config, Mtk = self._bindings()
            self.work = tempfile.TemporaryDirectory(prefix="couch-mtk-session-")
            loader_path = Path(self.work.name) / "approved-loader.bin"
            loader_path.write_bytes(loader)
            loader_path.chmod(0o600)
            config = Config(loglevel=logging.CRITICAL)
            config.loader = str(loader_path)
            config.preloader = self.preloader_data
            config.stock = True
            config.skipwdt = True
            config.reconnect = False
            config.write_preloader_to_file = False
            config.hwparam_path = self.work.name
            config.vid, config.pid = 0x0e8d, 0x2000
            # No interface is claimed during construction. An explicit numeric
            # placeholder avoids this upstream revision's broken default_ids
            # iteration for -1; start_readonly binds the observed CDC interface.
            config.interface = 0
            self.mtk = Mtk(config=config, loglevel=logging.CRITICAL)
            mtk = self.mtk
            daconfig = mtk.daloader.daconfig
            if self.preloader_data is not None:
                require(daconfig.emi is not None and len(daconfig.emi) > 0,
                        "Approved board preloader has no supported EMI data")
            extract_emi = daconfig.extract_emi
            def approved_emi_only(preloader=None):
                require((preloader is None and self.preloader_data is None)
                        or (isinstance(preloader, (bytes, bytearray)) and preloader == self.preloader_data),
                        "Unapproved automatic board preloader selection is disabled")
                return extract_emi(preloader)
            daconfig.extract_emi = approved_emi_only
            set_da = mtk.daloader.set_da
            def set_legacy_da():
                result = set_da()
                require(type(mtk.daloader.da).__name__ == "DALegacy", "Only the reviewed legacy DA path is supported")
                # Legacy startup otherwise walks upstream Loader/Preloader and
                # guesses a board using a partial DRAM signature. An empty
                # private directory disables that scan. approved_emi_only also
                # rejects any unapproved filename passed to extraction.
                mtk.daloader.da.pathconfig.get_loader_path = lambda: self.work.name
                return result
            mtk.daloader.set_da = set_legacy_da

    def start_readonly(self, loader, policy):
        require(policy == ReadPolicy(), "Unsupported MTK startup policy")
        require(self.device is not None and not self.started, "Claim a fresh USB session before starting")
        require(self.mtk is not None and self.prepared_loader == loader, "Prepare the verified loader before USB capture")
        self.started = True
        with bounded_operation(30):
            mtk = self.mtk
            config = mtk.config
            config.vid, config.pid = self.device.idVendor, self.device.idProduct
            config.interface = self.interface.bInterfaceNumber
            cdc = mtk.port.cdc
            cdc.device, cdc.interface = self.device, self.interface.bInterfaceNumber
            cdc.EP_IN, cdc.EP_OUT = self.ep_in, self.ep_out
            cdc.vid, cdc.pid = self.device.idVendor, self.device.idProduct
            cdc.connected = True
            cdc.timeout = 1000
            # Disable every discovery/reset entry point before protocol work.
            cdc.connect = self._forbidden
            cdc.detectdevices = self._forbidden
            cdc.get_interface_count = lambda: self.configuration.bNumInterfaces
            cdc.close = self._forbidden
            mtk.port.close = self._forbidden
            mtk.port.detectusbdevices = self._forbidden
            mtk.bypass_security = self._forbidden
            mtk.crasher = self._forbidden
            mtk.preloader.reset_to_brom = self._forbidden
            mtk.daloader.writeflash = self._forbidden
            mtk.daloader.formatflash = self._forbidden
            mtk.daloader.shutdown = self._forbidden
            # One handshake only: bypass Port.handshake's rediscovery loop and
            # Preloader.init's thousand retries. Exceptions stop the transaction.
            def handshake(**kwargs):
                strict_handshake(cdc)
                return True
            mtk.port.handshake = handshake
            # Guard init_hwcode before upstream can take an unrelated chip's
            # initialization branches, including IoT register writes.
            init_hwcode = config.init_hwcode
            def checked_hwcode(code):
                require(code == 0x6580, "Expected MT6580 before DA startup")
                return init_hwcode(code)
            config.init_hwcode = checked_hwcode
            # The upstream helper reports an unsupported target-config command
            # as all security flags false. Require a real successful response.
            def target_config(display=False):
                preloader = mtk.preloader
                require(preloader.echo(preloader.Cmd.GET_TARGET_CONFIG.value), "Target security query unsupported")
                raw = preloader.rbyte(6)
                require(isinstance(raw, (bytes, bytearray)) and len(raw) == 6, "Short target security response")
                value, status = struct.unpack(">IH", raw)
                require(status <= 0xff, "Target security query failed")
                return {key: bool(value & mask) for key, mask in
                        (("sbc", 1), ("sla", 2), ("daa", 4), ("epp", 8), ("cert", 16),
                         ("memread", 32), ("memwrite", 64), ("cmdC8", 128))}
            mtk.preloader.get_target_config = target_config
            mtk.preloader.display = False
            require(mtk.preloader.init(display=False, directory=self.work.name), "Preloader initialization failed")
            require(config.hwcode == 0x6580 and not config.iot and not config.is_brom,
                    "Only an MT6580 preloader session is supported; BootROM requires separate validation")
            require(isinstance(config.target_config, dict)
                    and all(config.target_config.get(key) is False for key in ("sbc", "sla", "daa")),
                    "Protected target requires a separately validated loader/authentication path")
            mtk.daloader.patch = False
            # Direct unpatched DA upload; never DaHandler.configure_da.
            require(mtk.daloader.upload_da(preloader=self.preloader_data), "Approved DA upload failed")
            require(not mtk.daloader.patch, "Unexpected download-agent patching")
            readflash = mtk.daloader.readflash
            read_failed = False
            def bounded_read(**kwargs):
                nonlocal read_failed
                require(not read_failed, "MTK read session failed; start a new session")
                try:
                    with bounded_operation(10):
                        return readflash(**kwargs)
                except BaseException:
                    read_failed = True
                    raise
            mtk.daloader.readflash = bounded_read
            return mtk

    def boot_after_capture(self):
        """Request the MT6580 legacy DA watchdog reboot after verified work.

        FINISH/HOME_SCREEN acknowledges but powers this HA100 off. Use the
        MT6580-tested exchange in saleemrashid/mediatek_flash_tool commit
        dd4f3640be4a271ee72bd43005986307ad46050f, src/mtk_da.c:396 and
        flash_tool/main.c:239: zero delay, synchronous, no download bit, retain
        RTC. This changes no partition or persistent boot-mode marker.
        """
        require(self.started and self.mtk is not None, "No active DA session")
        require(not getattr(self, "boot_requested", False), "Boot request already attempted")
        self.boot_requested = True  # Never retry an ambiguous protocol result.
        with bounded_operation(10):
            for payload in (b"\xdb", struct.pack(">I", 0), b"\x00", b"\x00", b"\x00", b"\x01"):
                require(self.ep_out.write(payload, timeout=1000) == len(payload),
                        "Short DA watchdog reboot request")
            require(bytes(self.ep_in.read(1, timeout=1000)) == b"\x5a",
                    "DA watchdog reboot was not acknowledged")
            # Let firmware disconnect before cleanup. Do not attach cdc_acm to
            # the DA's malformed CDC descriptors or claim the returning gadget.
            original = self.claimed_candidate()
            while original in self.enumerate():
                time.sleep(0.05)

    def close(self, reset=False):
        require(reset is False, "USB reset is disabled")
        errors = []
        if self.device is not None:
            for number in reversed(self.interfaces):
                try:
                    self.usb.util.release_interface(self.device, number)
                except Exception as error:
                    errors.append((f"release interface {number}", error))
            for number in reversed(self.detached):
                try:
                    self.device.attach_kernel_driver(number)
                except Exception as error:
                    errors.append((f"reattach kernel driver on interface {number}", error))
            try:
                self.usb.util.dispose_resources(self.device)
            except Exception as error:
                errors.append(("dispose USB resources", error))
        self.interfaces, self.detached, self.device = [], [], None
        if self.mtk is not None:
            self.mtk.port.cdc.connected = False
            self.mtk = None
        if self.work is not None:
            self.work.cleanup()
            self.work = None
        if self.imports is not None:
            self.imports.close()
            self.imports = None
        errors = [(operation, error) for operation, error in errors if getattr(error, "errno", None) != errno.ENODEV]
        if errors:
            details = "; ".join(f"{operation}: {error}" for operation, error in errors)
            raise InstallError(f"USB cleanup failed: {details}; disconnect the cable before another session") from errors[0][1]
