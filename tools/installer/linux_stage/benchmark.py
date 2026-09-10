#!/usr/bin/env python3
"""Explicit private USB RAM benchmark. No device-storage writes or reset commands."""
import argparse
from contextlib import contextmanager
import struct
import time

MAX = 64 * 1024 * 1024
CHUNK = 65536


def header(op, length):
    if (op == 0 and length == 0) or (op in (1, 2) and 0 < length <= MAX) or (op == 3 and length == 1):
        return struct.pack('<4sIQ', b'CBP1', op, length)
    raise ValueError('Unsupported command or bounded length')


def read_exact(endpoint, count):
    result = bytearray()
    while len(result) < count:
        block = bytes(endpoint.read(min(CHUNK, count - len(result)), timeout=30000))
        if not block:
            raise ValueError('Short/empty response')
        result.extend(block)
    return bytes(result)


def response(endpoint, expected):
    magic, status, size = struct.unpack('<4sIQ', read_exact(endpoint, 16))
    if magic != b'CBR1' or status != 0 or size != expected:
        raise ValueError('Unexpected response frame')


def benchmark(out, incoming, length, direction):
    start = time.monotonic()
    if out.write(header(direction, length), timeout=30000) != 16:
        raise ValueError('Short command write')
    if direction == 1:
        response(incoming, length)
    remaining = length
    pattern = bytes([0xa5]) * CHUNK
    while remaining:
        count = min(remaining, CHUNK)
        if direction == 1:
            if read_exact(incoming, count) != pattern[:count]:
                raise ValueError('RAM pattern mismatch')
        elif out.write(pattern[:count], timeout=30000) != count:
            raise ValueError('Short RAM write')
        remaining -= count
    if direction == 2:
        response(incoming, 0)
    elapsed = time.monotonic() - start
    return {'direction': 'device-to-host' if direction == 1 else 'host-to-device',
            'bytes': length, 'seconds': elapsed, 'MiB_per_second': length / 1048576 / elapsed}


@contextmanager
def usb_probe(args):
    import usb.core
    import usb.util
    ports = tuple(int(n) for n in args.ports.split('.'))
    devices = [dev for dev in usb.core.find(find_all=True, idVendor=args.vid, idProduct=args.pid)
               if dev.bus == args.bus and tuple(dev.port_numbers or ()) == ports]
    if len(devices) != 1:
        raise ValueError('Exactly one probe must match VID/PID and physical bus/port')
    dev = devices[0]
    claimed = None
    try:
        cfg = dev.get_active_configuration()  # No reset or set_configuration fallback.
        interfaces = [item for item in cfg if item.bInterfaceClass == 0xff
                      and item.bInterfaceSubClass == 0 and item.bInterfaceProtocol == 0
                      and item.bNumEndpoints == 2]
        if len(interfaces) != 1:
            raise ValueError('Expected exactly one vendor benchmark interface')
        interface = interfaces[0]
        eps = list(interface)
        if any(ep.bmAttributes & 3 != 2 for ep in eps):
            raise ValueError('Probe endpoints must be bulk')
        outgoing = [ep for ep in eps if ep.bEndpointAddress & 0x80 == 0]
        incoming = [ep for ep in eps if ep.bEndpointAddress & 0x80]
        if len(outgoing) != 1 or len(incoming) != 1:
            raise ValueError('Expected one IN and one OUT endpoint')
        # No kernel driver detachment: refuse rather than disrupting another service.
        if dev.is_kernel_driver_active(interface.bInterfaceNumber):
            raise ValueError('Probe interface already has a kernel driver')
        usb.util.claim_interface(dev, interface.bInterfaceNumber)
        claimed = interface.bInterfaceNumber
        out, incoming = outgoing[0], incoming[0]
        if out.write(header(0, 0), timeout=30000) != 16:
            raise ValueError('Short ping')
        response(incoming, 4)
        if read_exact(incoming, 4) != b'CBP1':
            raise ValueError('Not the read-only probe protocol')
        yield out, incoming
    finally:
        if claimed is not None:
            usb.util.release_interface(dev, claimed)
        usb.util.dispose_resources(dev)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bus', type=int, required=True)
    parser.add_argument('--ports', required=True)
    parser.add_argument('--vid', type=lambda n: int(n, 0), required=True)
    parser.add_argument('--pid', type=lambda n: int(n, 0), required=True)
    parser.add_argument('--mib', type=int, default=32, choices=range(1, 65))
    parser.add_argument('--hash-recovery', action='store_true')
    args = parser.parse_args()
    with usb_probe(args) as (out, incoming):
        for direction in (1, 2):
            print(benchmark(out, incoming, args.mib * 1048576, direction), flush=True)
        if args.hash_recovery:
            if out.write(header(3, 1), timeout=30000) != 16:
                raise ValueError('Short hash request')
            response(incoming, 72)
            result = read_exact(incoming, 72)
            print({'partition': 'recovery', 'sha256': result[:64].decode('ascii'),
                   'local_seconds': struct.unpack('<Q', result[64:])[0] / 1e9})


if __name__ == '__main__':
    main()
