"""macOS transport for a preloader whose CDC interfaces Apple's ACM driver owns.

macOS binds its built-in CDC ACM driver to the MediaTek preloader and exposes
the data interface as /dev/cu.usbmodem*. libusb can read every descriptor of
that device, but it can only take the interface away from the kernel driver by
capturing the device, which needs root or an Apple entitlement. This module
keeps the installer unprivileged: the exact libusb candidate is resolved to
its own callout device through the I/O Registry, and the download protocol
runs over that byte stream with the endpoint-like read/write surface the
reviewed mtkclient revision expects.

Nothing here rediscovers devices. A candidate is resolved once, by USB bus,
address, vendor and product, to the callout device that hangs off the exact
CDC data interface number the descriptor walk selected.

Status after hardware runs on 2026-09-12/13: this transport reads every
partition correctly, but a download-agent write of more than a few kilobytes
was never reliably acknowledged through it, and the driver's behaviour on the
wire cannot be observed without root. The installer therefore runs its
MediaTek worker as root on macOS and uses libusb, the path validated on Linux.
This module remains for unprivileged read-only attachment.
"""
import errno
import os
import plistlib
import re
import select
import struct
import subprocess
import time
try:
    import fcntl
    import termios
except ImportError:  # Windows never selects the callout path; keep the backend importable.
    fcntl = termios = None

from couch_install import require

IOREG = ('/usr/sbin/ioreg', '-a', '-l', '-r', '-c', 'IOUSBHostDevice')
CALLOUT = re.compile(r'^/dev/cu\.[A-Za-z0-9._-]{1,64}$')


def location_topology(location_id):
    """Apple encodes the bus in the top byte and the port chain in nibbles."""
    require(type(location_id) is int and 0 <= location_id <= 0xffffffff, 'Invalid USB location')
    ports = []
    for shift in range(20, -1, -4):
        nibble = (location_id >> shift) & 0xf
        if nibble == 0:
            break
        ports.append(nibble)
    return location_id >> 24, tuple(ports)


def _children(node):
    children = node.get('IORegistryEntryChildren', [])
    require(isinstance(children, list) and all(isinstance(c, dict) for c in children), 'Invalid registry tree')
    return children


def _callouts(node):
    found = []
    device = node.get('IOCalloutDevice')
    if device is not None:
        found.append(device)
    for child in _children(node):
        found.extend(_callouts(child))
    return found


def callout_device(registry, candidate, interface_number):
    """Resolve one libusb candidate and CDC data interface to its callout node.

    Returns None when no serial client is attached to that interface, which
    means the kernel left the interface free and libusb may claim it directly.
    """
    require(isinstance(registry, list) and all(isinstance(n, dict) for n in registry), 'Invalid registry listing')
    devices = []
    for node in registry:
        if node.get('IOObjectClass') != 'IOUSBHostDevice':
            continue
        location = node.get('locationID')
        if type(location) is not int:
            continue
        bus, _ = location_topology(location)
        if (node.get('idVendor'), node.get('idProduct'), bus, node.get('USB Address')) == \
                (candidate.vid, candidate.pid, candidate.bus, candidate.address):
            devices.append(node)
    require(len(devices) == 1, 'Selected USB device is not registered exactly once on macOS')
    interfaces = [child for child in _children(devices[0])
                  if child.get('IOObjectClass') == 'IOUSBHostInterface'
                  and child.get('bInterfaceNumber') == interface_number]
    require(len(interfaces) == 1, 'Selected CDC data interface is not registered exactly once on macOS')
    callouts = _callouts(interfaces[0])
    if not callouts:
        return None
    require(len(callouts) == 1 and isinstance(callouts[0], str) and CALLOUT.match(callouts[0]),
            'macOS exposed an unexpected serial port for the selected interface')
    return callouts[0]


def registry_listing(run=subprocess.run):
    result = run(IOREG, capture_output=True, timeout=10)
    require(result.returncode == 0 and 0 < len(result.stdout) <= 64 * 1024 * 1024, 'I/O Registry query failed')
    listing = plistlib.loads(result.stdout)
    require(isinstance(listing, list), 'Unexpected I/O Registry listing')
    return listing


class TtyTransport:
    """Raw, exclusive byte stream over a callout device with USB-style timeouts."""
    BAUD = getattr(termios, 'B115200', None)  # Line coding never reaches the bulk data path.
    DEFAULT_TIMEOUT = 1000
    PACED_PIECE = 4096  # Bytes queued before waiting for the port to transmit them.

    def __init__(self, path, usb, *, opener=os.open, ioctl=None):
        require(termios is not None and fcntl is not None, 'Serial transport requires a POSIX host')
        require(isinstance(path, str) and CALLOUT.match(path), 'Invalid callout device')
        self.usb = usb
        self.path = path
        self.ioctl = fcntl.ioctl if ioctl is None else ioctl
        self.fd = opener(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        try:
            self.ioctl(self.fd, termios.TIOCEXCL)
            self._raw()
        except BaseException:
            os.close(self.fd)
            self.fd = None
            raise

    def _raw(self):
        attrs = termios.tcgetattr(self.fd)
        iflag, oflag, cflag, lflag, _, _, cc = attrs
        iflag &= ~(termios.IGNBRK | termios.BRKINT | termios.PARMRK | termios.ISTRIP | termios.INLCR
                   | termios.IGNCR | termios.ICRNL | termios.IXON | termios.IXOFF | termios.IXANY)
        oflag &= ~termios.OPOST
        lflag &= ~(termios.ECHO | termios.ECHONL | termios.ICANON | termios.ISIG | termios.IEXTEN)
        cflag &= ~(termios.CSIZE | termios.PARENB | termios.CSTOPB | termios.CRTSCTS)
        cflag |= termios.CS8 | termios.CREAD | termios.CLOCAL
        cc[termios.VMIN], cc[termios.VTIME] = 0, 0
        termios.tcsetattr(self.fd, termios.TCSANOW, [iflag, oflag, cflag, lflag, self.BAUD, self.BAUD, cc])

    def _timeout(self, timeout):
        return (timeout if timeout is not None and timeout > 0 else self.DEFAULT_TIMEOUT) / 1000.0

    # These build the fault; the caller raises it. Raising inside a shared helper
    # would make every timeout report this one line, hiding whether a read or a
    # write stalled, which is exactly what the reviewed diagnostic reports.
    def _timed_out(self):
        return self.usb.core.USBTimeoutError('Operation timed out', -7, errno.ETIMEDOUT)

    def _gone(self):
        return self.usb.core.USBError('No such device (it may have been disconnected)', -4, errno.ENODEV)

    def read(self, size_or_buffer, timeout=None):
        """Return the bytes already available, like one USB transfer ending on a short packet."""
        require(self.fd is not None, 'Serial transport closed')
        size = size_or_buffer if isinstance(size_or_buffer, int) else len(size_or_buffer)
        require(0 <= size <= 1024 * 1024, 'Serial protocol read exceeds transfer limit')
        if size == 0:
            return b'' if isinstance(size_or_buffer, int) else 0
        deadline = time.monotonic() + self._timeout(timeout)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise self._timed_out()
            readable, _, _ = select.select([self.fd], [], [], remaining)
            if not readable:
                raise self._timed_out()
            try:
                data = os.read(self.fd, size)
            except BlockingIOError:
                continue
            except OSError as error:
                if error.errno in (errno.ENXIO, errno.ENODEV, errno.EIO):
                    raise self._gone() from error
                raise
            if not data:
                raise self._gone()
            if isinstance(size_or_buffer, int):
                return data
            memoryview(size_or_buffer).cast('B')[:len(data)] = data
            return len(data)

    def _drain(self, deadline):
        """Wait until the port has actually put the queued bytes on the wire.

        A completed transmission means the device's endpoint accepted the bytes,
        so this is the only device-level backpressure a callout device offers.
        Poll the queue instead of blocking in tcdrain, so a stalled port raises
        the same timeout as any other stall rather than hanging past the
        caller's operation bound.
        """
        while True:
            try:
                queued = struct.unpack('i', self.ioctl(self.fd, termios.TIOCOUTQ, struct.pack('i', 0)))[0]
            except (OSError, TypeError, struct.error):
                # No queue query on this port: deliver unpaced rather than block
                # in tcdrain, which cannot be bounded by the caller's deadline.
                return
            if queued <= 0:
                return
            if time.monotonic() >= deadline:
                raise self._timed_out()
            time.sleep(0.001)

    def write(self, data, timeout=None):
        """Deliver every byte, pacing the device rather than filling the port.

        libusb hands one bulk transfer to the kernel and the device is paced by
        that transfer completing. A callout device accepts far more than the
        device can consume: the ACM driver keeps streaming while the download
        agent falls behind, the agent never sees a complete chunk, so it never
        acknowledges and never commits. A failed write leaves the partition
        byte-identical, which is exactly what the saved originals showed.

        So hand over a bounded piece at a time and wait for it to reach the wire
        before queueing more. The timeout is an inactivity bound: a stalled or
        vanished port still fails within it, while steady progress keeps the
        transfer alive, and the enclosing bounded_operation stays the absolute
        cap. Writes smaller than one piece, which is every protocol field and
        acknowledgement, behave exactly as before.
        """
        require(self.fd is not None, 'Serial transport closed')
        data = memoryview(bytes(data))
        limit = self._timeout(timeout)
        deadline = time.monotonic() + limit
        sent = 0
        while sent < len(data):
            piece = min(len(data), sent + self.PACED_PIECE)
            while sent < piece:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise self._timed_out()
                _, writable, _ = select.select([], [self.fd], [], remaining)
                if not writable:
                    raise self._timed_out()
                try:
                    count = os.write(self.fd, data[sent:piece])
                except BlockingIOError:
                    continue
                except OSError as error:
                    if error.errno in (errno.ENXIO, errno.ENODEV, errno.EIO):
                        raise self._gone() from error
                    raise
                if count:
                    sent += count
                    deadline = time.monotonic() + limit
            if sent < len(data):
                # Only between pieces: the point is to not queue more than the
                # port has transmitted. After the last piece there is no more to
                # queue, and small protocol writes never reach this at all.
                self._drain(deadline)
                deadline = time.monotonic() + limit
        return sent

    def set_line_coding(self, baudrate=None, parity=0, databits=8, stopbits=1, isFtdi=False):
        # Upstream requests 921600/8N1 out of serial habit; the preloader's USB
        # CDC function ignores line coding. Reapply the raw 8N1 settings so the
        # ACM driver's SET_LINE_CODING matches what libusb sessions send.
        require(parity == 0 and databits == 8 and stopbits == 1 and not isFtdi, 'Unsupported line coding')
        self._raw()

    def setcontrollinestate(self, rts=None, dtr=None, is_ftdi=False):
        require(not is_ftdi, 'Unsupported control line request')
        for flag, value in ((termios.TIOCM_RTS, rts), (termios.TIOCM_DTR, dtr)):
            if value is None:
                continue
            self.ioctl(self.fd, termios.TIOCMBIS if value else termios.TIOCMBIC, struct.pack('i', flag))

    def close(self):
        if self.fd is not None:
            fd, self.fd = self.fd, None
            os.close(fd)


class TtyEndpoint:
    """Descriptor attributes from the real endpoint; transfers over the callout device."""
    # This transport deliberately advertises no smaller protocol chunk. Announcing
    # 64 KiB in the command header behaved exactly like the reviewed 1 MiB, and
    # upstream hardcodes 1 MiB, so the agent may ignore the announced size and
    # still wait for a full chunk. Keep the protocol byte-identical to the path
    # validated on Linux and pace delivery in the transport instead.

    def __init__(self, transport, endpoint):
        self.transport = transport
        self.endpoint = endpoint
        self.incoming = bool(endpoint.bEndpointAddress & 0x80)

    def __getattr__(self, name):
        return getattr(self.endpoint, name)

    def read(self, size_or_buffer, timeout=None):
        require(self.incoming, 'Read requested on the OUT endpoint')
        return self.transport.read(size_or_buffer, timeout)

    def write(self, data, timeout=None):
        require(not self.incoming, 'Write requested on the IN endpoint')
        return self.transport.write(data, timeout)


def open_callout(candidate, interface_number, usb, *, listing=registry_listing, transport=TtyTransport):
    """Return a TtyTransport for the exact candidate, or None when libusb may claim directly."""
    path = callout_device(listing(), candidate, interface_number)
    if path is None:
        return None
    return transport(path, usb)
