"""Windows transport for a USB device that Windows bound to its serial-port driver.

libusb on Windows can only open a device whose driver is WinUSB (or libusbK or
libusb0), and Windows never picks those by itself. What Windows does pick, on
its own and on every Windows 10 and 11 machine, is its built-in `usbser` driver
for any CDC ACM function. Both USB identities this installer talks to are CDC
ACM functions: the MediaTek preloader in download mode is one, and the RAM
installer stage carries one next to its vendor interface. So on Windows the
worker opens the COM port Windows created, the way SP Flash Tool has always
delivered download agents on Windows, and needs no driver work at all. libusb
keeps enumerating devices and serving descriptors; it is used for the transfer
only when someone has already bound WinUSB to the device.

Nothing here rediscovers devices. A libusb candidate is resolved once, by vendor,
product and physical port chain, to the one present COM port that belongs to
it. The transport then offers the endpoint-like read/write surface the reviewed
mtkclient revision expects, with the same USB-style errors the libusb path
raises, so every protocol step above it stays byte-identical to the path
validated on Linux.
"""
import errno
import re
import sys
import time

from couch_install import InstallError, require

PORT = re.compile(r'^COM\d{1,3}$')
# pyserial renders SetupAPI's location paths as "<root>-<port>.<port>..." with an
# optional ":x.<interface>" suffix on a composite function.
LOCATION = re.compile(r'^\d+-(\d+(?:\.\d+)*)(?::|$)')
# Drivers libusb itself can open; every other recorded binding means "serial or
# nothing", and only Windows' own usbser (or a vendor INF over it) creates a port.
LIBUSB_SERVICES = frozenset(('winusb', 'libusbk', 'libusb0'))
ENUM_USB = r'SYSTEM\CurrentControlSet\Enum\USB'
# How long a port may take to exist after libusb already enumerates the device:
# Windows starts usbser within a few hundred milliseconds when the device has
# been seen before, and the enclosing claim is bounded at ten seconds.
PORT_WAIT = 6.0
OPEN_RETRY = 2.0
DEFAULT_TIMEOUT = 1000
PIECE = 65536


def port_chain(location):
    """Physical hub port chain from a pyserial location string, or None."""
    if not isinstance(location, str):
        return None
    match = LOCATION.match(location)
    if match is None:
        return None
    return tuple(int(part) for part in match.group(1).split('.'))


def matching_ports(ports, vid, pid, chain):
    """Present serial ports belonging to vid/pid on the candidate's port chain.

    A port whose location Windows did not report is accepted only when no port
    reports the expected chain, and the caller still requires exactly one.
    """
    require(isinstance(chain, tuple) and chain, 'Invalid USB port chain')
    same = [p for p in ports if getattr(p, 'vid', None) == vid and getattr(p, 'pid', None) == pid
            and isinstance(getattr(p, 'device', None), str)]
    exact = [p for p in same if port_chain(getattr(p, 'location', None)) == chain]
    if exact:
        return exact
    return [p for p in same if port_chain(getattr(p, 'location', None)) is None]


def recorded_services(vid, pid, *, registry=None):
    """Driver service Windows recorded for each instance of vid/pid, None when unbound.

    Reads only `HKLM\\SYSTEM\\CurrentControlSet\\Enum\\USB\\VID_xxxx&PID_xxxx`,
    the same record the native host's pre-flight shows. An empty list means the
    device has never been enumerated on this machine.
    """
    if registry is None:
        import winreg  # Windows only; the caller never reaches this elsewhere.
        registry = winreg
    services = []
    try:
        root = registry.OpenKey(registry.HKEY_LOCAL_MACHINE, f'{ENUM_USB}\\VID_{vid:04X}&PID_{pid:04X}',
                                0, registry.KEY_READ | registry.KEY_WOW64_64KEY)
    except OSError:
        return services
    with root:
        index = 0
        while True:
            try:
                name = registry.EnumKey(root, index)
            except OSError:
                break
            index += 1
            require(index <= 256, 'Too many recorded USB instances')
            try:
                with registry.OpenKey(root, name, 0, registry.KEY_READ | registry.KEY_WOW64_64KEY) as instance:
                    value, kind = registry.QueryValueEx(instance, 'Service')
                    services.append(value if kind == registry.REG_SZ and isinstance(value, str) else None)
            except OSError:
                services.append(None)
    return services


def libusb_bound(services):
    """True when every recorded instance is bound to a driver libusb can open."""
    return bool(services) and all(isinstance(s, str) and s.lower() in LIBUSB_SERVICES for s in services)


def comports_windows():
    from serial.tools import list_ports
    return list(list_ports.comports())


def open_serial_port(candidate, interface_number, usb, *, comports=comports_windows, services=recorded_services,
                     transport=None, wait=PORT_WAIT, now=time.monotonic, sleep=time.sleep):
    """Return a SerialTransport for the exact candidate, or None when libusb should claim it.

    Same shape as mtk_tty.open_callout so ExactUsbBackend treats both alike.
    The interface number is not needed to find the port: Windows names the
    port after the device instance, and the candidate's port chain plus the
    vendor/product pair identify it.
    """
    del interface_number
    if transport is None:
        transport = SerialTransport
    if libusb_bound(services(candidate.vid, candidate.pid)):
        return None
    deadline = now() + wait
    while True:
        found = matching_ports(comports(), candidate.vid, candidate.pid, candidate.ports)
        require(len(found) <= 1, 'More than one serial port belongs to the selected USB device')
        if found:
            name = found[0].device
            require(PORT.match(name), 'Unexpected serial port name for the selected USB device')
            return transport(name, usb, now=now, sleep=sleep)
        if now() >= deadline:
            raise InstallError('Windows created no serial port for the selected USB device in time; '
                               'the download window may have closed before the driver started')
        sleep(0.05)


class SerialTransport:
    """USB-transfer semantics over a Windows COM port through pyserial.

    Reads return what has arrived, like one bulk transfer ending on a short
    packet, so the packet buffer above never waits a full timeout for bytes it
    already has. Writes go out in bounded pieces; usbser completes each write
    only when the device has taken the bytes, so the timeout is a real
    inactivity bound rather than a guess about driver buffering.
    """

    def __init__(self, name, usb, *, serial_module=None, now=time.monotonic, sleep=time.sleep):
        require(isinstance(name, str) and PORT.match(name), 'Invalid serial port name')
        if serial_module is None:
            import serial
            serial_module = serial
        self.serial = serial_module
        self.usb = usb
        self.name = name
        self.port = None
        self._read_timeout = None
        self._write_timeout = None
        deadline = now() + OPEN_RETRY
        while True:
            try:
                self.port = serial_module.Serial(name, baudrate=115200, bytesize=8, parity='N', stopbits=1,
                                                 timeout=1.0, write_timeout=1.0, xonxoff=False, rtscts=False,
                                                 dsrdtr=False)
                break
            except (serial_module.SerialException, OSError) as error:
                # Windows publishes the port name a moment before the device is
                # started; opening it then fails with access denied.
                if now() >= deadline:
                    raise self._gone() from error
                sleep(0.1)

    def _timeout(self, timeout):
        return (timeout if timeout is not None and timeout > 0 else DEFAULT_TIMEOUT) / 1000.0

    # These build the fault; the caller raises it, so the reviewed frame names
    # the read or the write that stalled rather than this helper.
    def _timed_out(self):
        return self.usb.core.USBTimeoutError('Operation timed out', -7, errno.ETIMEDOUT)

    def _gone(self):
        return self.usb.core.USBError('No such device (it may have been disconnected)', -4, errno.ENODEV)

    def read(self, size_or_buffer, timeout=None):
        """Return the bytes already available, waiting only for the first one."""
        require(self.port is not None, 'Serial transport closed')
        size = size_or_buffer if isinstance(size_or_buffer, int) else len(size_or_buffer)
        require(0 <= size <= 1024 * 1024, 'Serial protocol read exceeds transfer limit')
        if size == 0:
            return b'' if isinstance(size_or_buffer, int) else 0
        limit = self._timeout(timeout)
        try:
            if self._read_timeout != limit:
                self.port.timeout = limit
                self._read_timeout = limit
            data = self.port.read(1)
            if data and size > 1:
                waiting = self.port.in_waiting
                if waiting > 0:
                    data += self.port.read(min(size - 1, waiting))
        except (self.serial.SerialException, OSError) as error:
            raise self._gone() from error
        if not data:
            raise self._timed_out()
        require(len(data) <= size, 'Serial port returned more than requested')
        if isinstance(size_or_buffer, int):
            return data
        memoryview(size_or_buffer).cast('B')[:len(data)] = data
        return len(data)

    def write(self, data, timeout=None):
        """Deliver every byte in bounded pieces; the timeout bounds each piece."""
        require(self.port is not None, 'Serial transport closed')
        data = memoryview(bytes(data))
        limit = self._timeout(timeout)
        sent = 0
        while sent < len(data):
            piece = data[sent:sent + PIECE]
            try:
                if self._write_timeout != limit:
                    self.port.write_timeout = limit
                    self._write_timeout = limit
                count = self.port.write(piece)
            except self.serial.SerialTimeoutException as error:
                raise self._timed_out() from error
            except (self.serial.SerialException, OSError) as error:
                raise self._gone() from error
            require(count == len(piece), 'Short serial write')
            sent += count
        return sent

    def set_line_coding(self, baudrate=None, parity=0, databits=8, stopbits=1, isFtdi=False):
        # The preloader's CDC function ignores line coding; usbser forwards the
        # request exactly as libusb sessions send it through ctrl_transfer.
        require(parity == 0 and databits == 8 and stopbits == 1 and not isFtdi, 'Unsupported line coding')
        require(self.port is not None, 'Serial transport closed')
        if baudrate:
            require(type(baudrate) is int and 1200 <= baudrate <= 4000000, 'Unsupported baud rate')
            try:
                self.port.baudrate = baudrate
            except (self.serial.SerialException, OSError, ValueError) as error:
                raise self._gone() from error

    def setcontrollinestate(self, rts=None, dtr=None, is_ftdi=False):
        require(not is_ftdi, 'Unsupported control line request')
        require(self.port is not None, 'Serial transport closed')
        try:
            if rts is not None:
                self.port.rts = bool(rts)
            if dtr is not None:
                self.port.dtr = bool(dtr)
        except (self.serial.SerialException, OSError) as error:
            raise self._gone() from error

    def close(self):
        if self.port is not None:
            port, self.port = self.port, None
            port.close()


def default_callout(platform=sys.platform):
    """The per-platform resolver ExactUsbBackend consults before claiming through libusb."""
    if platform == 'win32':
        return open_serial_port
    from mtk_tty import open_callout
    return open_callout
