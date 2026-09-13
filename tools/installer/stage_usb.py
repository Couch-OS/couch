"""Fixed RAM-stage USB operations for the supervised native worker.

No flash opcodes are supported. Native Rust owns credentials, plan policy, TLS,
backup verification and installation. This bridge only transports bounded USB
setup frames on the physical port retained from the successful boot operation.
"""
import json
import struct
import sys
from couch_install import require
from mtk_com import open_serial_port
from mtk_session import Candidate

# Windows starts usbser for a stage it has never seen only after installing the
# driver; the native host allows this open forty seconds.
STAGE_PORT_WAIT = 30.0


def open_stage_port(selected, usb):
    """The stage's CDC ACM function as a COM port, found by the retained physical port."""
    stage = Candidate(selected.bus, 0, selected.ports, 0x0e8d, 0x201c)
    return open_serial_port(stage, 0, usb, services=lambda vid, pid: [], wait=STAGE_PORT_WAIT)


class StageUsb:
    def __init__(self, usb, backend, selected, *, platform=sys.platform, serial_port=open_stage_port):
        self.usb, self.device, self.interface, self.serial = usb, None, None, None
        devices = [d for d in usb.core.find(find_all=True, idVendor=0x0e8d,
                   idProduct=0x201c, backend=backend)
                   if d.bus == selected.bus and tuple(d.port_numbers or ()) == selected.ports]
        require(len(devices) == 1, 'Expected installer stage on the selected physical USB port')
        self.device = devices[0]
        try:
            interfaces = [i for i in self.device.get_active_configuration()
                if i.bInterfaceClass == 0xff and i.bInterfaceSubClass == 0
                and i.bInterfaceProtocol == 0 and i.bNumEndpoints == 2]
            require(len(interfaces) == 1, 'Expected one installer vendor interface')
            interface = interfaces[0]
            endpoints = list(interface)
            require(all(e.bmAttributes & 3 == 2 for e in endpoints), 'Expected bulk endpoints')
            incoming = [e for e in endpoints if e.bEndpointAddress & 0x80]
            outgoing = [e for e in endpoints if not e.bEndpointAddress & 0x80]
            require(len(incoming) == len(outgoing) == 1, 'Expected one IN and OUT endpoint')
            if platform == 'win32':
                # Windows has no driver for the vendor interface and libusb
                # cannot open one without it, but it binds usbser to the ACM
                # function the stage carries beside it, and the stage answers
                # the same framed protocol on that serial port.
                self.serial = serial_port(selected, usb)
                self.incoming = self.outgoing = self.serial
            else:
                try:
                    require(not self.device.is_kernel_driver_active(interface.bInterfaceNumber),
                            'Installer interface is owned by a kernel driver')
                except NotImplementedError:
                    pass
                usb.util.claim_interface(self.device, interface.bInterfaceNumber)
                self.interface = interface.bInterfaceNumber
                self.incoming, self.outgoing = incoming[0], outgoing[0]
            require(self.request(0, b'', maximum=4) == b'CBP1', 'Invalid installer protocol')
        except BaseException:
            self.close()
            raise

    def read(self, size):
        data = bytearray()
        while len(data) < size:
            part = bytes(self.incoming.read(size-len(data), timeout=30000))
            require(part and len(part) <= size-len(data), 'Invalid installer USB read')
            data.extend(part)
        return bytes(data)

    def request(self, opcode, data, *, maximum):
        require(opcode in (0, 4, 5, 6, 7) and len(data) <= 16384, 'Unsupported USB setup request')
        header = struct.pack('<4sIQ', b'CBP1', opcode, len(data))
        require(self.outgoing.write(header, timeout=30000) == len(header), 'Short USB setup header')
        if data:
            require(self.outgoing.write(data, timeout=30000) == len(data), 'Short USB setup body')
        magic, status, size = struct.unpack('<4sIQ', self.read(16))
        require(magic == b'CBR1' and status == 0 and size <= maximum, 'Invalid USB setup response')
        return self.read(size)

    def dispatch(self, operation, payload):
        if operation in ('stage_status', 'stage_scan'):
            require(payload is None, 'Unexpected USB query payload')
            opcode, maximum = (5, 512) if operation == 'stage_status' else (7, 16384)
            result = json.loads(self.request(opcode, b'', maximum=maximum))
            require(isinstance(result, dict), 'Expected USB response object')
            return result
        require(operation in ('stage_bind', 'stage_provision') and isinstance(payload, dict),
                'Invalid USB setup operation')
        if operation == 'stage_bind':
            require(set(payload) == {'plan_sha256', 'nonce'}, 'Invalid USB binding fields')
        else:
            require(set(payload) == {'ssid_hex', 'psk_hex', 'certificate_hex', 'private_key_hex', 'token_hex'},
                    'Invalid USB provisioning fields')
        data = json.dumps(payload, sort_keys=True, separators=(',', ':')).encode()
        require(self.request(6 if operation == 'stage_bind' else 4, data, maximum=0) == b'',
                'Unexpected USB setup acknowledgement')
        return {'accepted': True}

    def close(self):
        if self.serial is not None:
            serial, self.serial = self.serial, None
            serial.close()
        if self.device is not None:
            try:
                if self.interface is not None:
                    self.usb.util.release_interface(self.device, self.interface)
            finally:
                self.usb.util.dispose_resources(self.device)
                self.device = None
                self.interface = None
