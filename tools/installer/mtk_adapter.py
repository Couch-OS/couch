#!/usr/bin/env python3
"""Private native-host MTK worker. Fixed RPC operations, no discovery or writes at startup.

Rust owns admission, durable originals, process locks/deadlines and sequencing.
This worker retains the reviewed MTK handshake/read/write implementation only.
Use an independently verified Python/runtime/source bundle. Never run it against
hardware outside the native host's admitted and supervised session.
"""
import copy
import hashlib
import json
import os
from contextlib import contextmanager
from pathlib import Path
import struct
import sys

# -I deliberately omits the script directory. This file and its sibling modules
# are themselves part of the installer's verified source inventory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from couch_install import IDENTITY_PARTITIONS, MODEL, fingerprint, regular, require
from mtk_readonly import ConnectedMtkReader, REVIEWED_REVISION
from mtk_session import Candidate, ReadPolicy, loader_bytes, source_pin
from mtk_usb import ExactUsbBackend, supervised_operations
from mtk_writer import ConnectedMtkWriter
from stage_usb import StageUsb
from couch_serial import CouchSerial, Unavailable

MAX = 1024 * 1024
READABLE = IDENTITY_PARTITIONS | {'boot', 'recovery', 'odmdtbo', 'logo'}


class Wire:
    def __init__(self, incoming, outgoing):
        self.incoming, self.outgoing = incoming, outgoing

    def exact(self, size):
        result = bytearray()
        while len(result) < size:
            block = self.incoming.read(size-len(result))
            require(block, 'Native host disconnected')
            result.extend(block)
        return bytes(result)

    def receive(self):
        size, = struct.unpack('<I', self.exact(4))
        require(0 < size <= MAX, 'RPC frame exceeds bound')
        value = json.loads(self.exact(size))
        require(isinstance(value, dict), 'Expected RPC object')
        return value

    def send(self, value):
        data = json.dumps(value, sort_keys=True, separators=(',', ':')).encode()
        require(0 < len(data) <= MAX, 'RPC response exceeds bound')
        self.outgoing.write(struct.pack('<I', len(data)))
        self.outgoing.write(data)
        self.outgoing.flush()

    def chunk(self, data):
        require(0 < len(data) <= MAX, 'RPC data exceeds bound')
        self.send({'event': 'chunk', 'size': len(data)})
        self.outgoing.write(struct.pack('<III', len(data), len(data), 0))
        self.outgoing.write(data)
        self.outgoing.flush()

    @contextmanager
    def deadline(self, seconds):
        require(isinstance(seconds, (int, float)) and 0 < seconds <= 900, 'Invalid operation deadline')
        self.send({'event': 'deadline', 'seconds': seconds})
        require(self.receive() == {'ack': 'deadline'}, 'Native supervisor did not arm deadline')
        try:
            yield
        finally:
            self.send({'event': 'deadline_end'})
            require(self.receive() == {'ack': 'deadline_end'}, 'Native supervisor did not close deadline')


class Adapter:
    def __init__(self, wire, *, backend=ExactUsbBackend, reader=ConnectedMtkReader, writer=ConnectedMtkWriter):
        self.wire = wire
        self.backend_factory, self.reader_factory, self.writer_factory = backend, reader, writer
        self.backend = self.reader = self.writer = None
        self.loader = self.stage = self.binding = None
        self.written = False
        self.verified = False
        self.boot_requested = False
        self.stage_usb = None
        self.selected = None
        self.failed = False
        self.authorization_attempted = False
        self.capture_context = None
        self.capture_reads = {}
        self.capture_verified = {}

    def prepare(self, command):
        require(self.backend is None, 'Adapter already prepared')
        require(set(command) == {'op', 'checkout', 'loader', 'loader_sha256', 'preloader', 'preloader_sha256', 'libusb'}, 'Unexpected preparation fields')
        source_pin(command['checkout'])
        self.loader = loader_bytes(command['loader'], command['loader_sha256'])
        self.backend = self.backend_factory(Path(command['checkout']), preloader=command['preloader'],
            preloader_sha256=command['preloader_sha256'], libusb_path=command['libusb'])
        self.backend.prepare(self.loader)
        self.wire.send({'event': 'prepared'})

    def start(self, command):
        require(self.backend is not None and self.reader is None, 'Expected prepared fresh adapter')
        require(set(command) == {'op', 'candidate'}, 'Unexpected startup fields')
        raw = command['candidate']
        require(isinstance(raw, dict) and set(raw) == {'bus', 'address', 'ports', 'vid', 'pid'}, 'Invalid USB selection')
        require(all(type(raw[n]) is int and raw[n] > 0 for n in ('bus', 'address', 'vid', 'pid'))
                and isinstance(raw['ports'], list) and raw['ports']
                and all(type(n) is int and 0 < n <= 255 for n in raw['ports']), 'Invalid USB topology')
        selected = Candidate(raw['bus'], raw['address'], tuple(raw['ports']), raw['vid'], raw['pid'])
        self.selected = selected
        self.backend.claim(selected)
        require(self.backend.claimed_candidate() == selected, 'Claimed USB selection differs')
        mtk = self.backend.start_readonly(self.loader, ReadPolicy())
        self.reader = self.reader_factory(mtk, REVIEWED_REVISION)
        self.capture_context = (self.backend, self.reader, self.backend.mtk,
            self.backend.device, self.selected, fingerprint(self.reader.description), self.reader.runtime_cid)
        self.wire.send({'event': 'connected', 'device': self.reader.description, 'cid': self.reader.runtime_cid})

    def invalidate_capture(self):
        self.capture_context = None
        self.capture_reads.clear()
        self.capture_verified.clear()

    def require_capture_connection(self):
        require(self.capture_context is not None and not self.failed, 'No live capture evidence')
        backend, reader, mtk, device, selected, description, cid = self.capture_context
        require(self.backend is backend and self.reader is reader
                and backend.mtk is mtk and backend.device is device
                and self.selected == selected and backend.claimed_candidate() == selected
                and fingerprint(reader.description) == description and reader.runtime_cid == cid,
                'Verified capture connection changed')

    def hash_partition(self, name):
        total = self.reader.description['partitions'][name]['size']
        full, done = hashlib.sha256(), 0
        for data in self.reader.chunks(name):
            full.update(data)
            done += len(data)
            self.wire.send({'event': 'progress', 'phase': 'Verify original',
                'target': name, 'done': done, 'total': total})
        require(done == total, 'Incomplete original readback')
        return full.hexdigest()

    def authorize_boot(self, command):
        require(not self.failed and not self.authorization_attempted, 'Boot authorization already consumed')
        self.authorization_attempted = True
        require(self.reader is not None and self.writer is None and not self.written, 'Expected fresh read-only session')
        self.require_capture_connection()
        require(set(command) == {'op', 'cid_sha256', 'partitions', 'identity_sha256', 'original_sha256', 'stage', 'stage_sha256'}, 'Unexpected boot admission fields')
        desc = self.reader.description
        require(desc['runtime_cid_sha256'] == command['cid_sha256'] and desc['partitions'] == command['partitions'], 'Observed device differs from admitted baseline')
        hashes = command['identity_sha256']
        originals = command['original_sha256']
        require(isinstance(hashes, dict) and set(hashes) == IDENTITY_PARTITIONS
                and isinstance(originals, dict) and set(originals) == {'boot', 'recovery', 'odmdtbo', 'logo'}, 'Incomplete original identity evidence')
        # No writes or reconnects are admitted between capture and authorization.
        # Each proof comes from a complete streamed backup plus an independent
        # device hash on this exact connection, never from the host's hash map.
        require(set(self.capture_verified) == READABLE, 'Missing independently verified capture')
        require(self.capture_verified == {**hashes, **originals}, 'Submitted originals differ from verified capture')
        stage = regular(command['stage']).resolve()
        require(stage.stat().st_size == desc['partitions']['boot']['size'] == 16*1024*1024, 'RAM stage must fill one boot partition')
        with stage.open('rb') as file:
            require(file.read(8) == b'ANDROID!', 'Invalid RAM boot image')
        binding = {**desc, 'model': MODEL, 'model_verified': True, 'identity_sha256': fingerprint(hashes)}
        release = {'schema': 1, 'model': MODEL, 'partitions': desc['partitions'],
                   'images': {'boot': {'file': stage.name, 'sha256': command['stage_sha256']}}}
        self.writer = self.writer_factory(self.backend.mtk, REVIEWED_REVISION, release=release,
            bundle=stage.parent, binding=binding, progress=lambda phase, target, done, total:
                self.wire.send({'event': 'progress', 'phase': phase, 'target': target, 'done': done, 'total': total}))
        self.reader = self.writer
        self.stage = stage
        self.binding = copy.deepcopy(command)
        self.invalidate_capture()
        self.wire.send({'event': 'boot_authorized'})

    def dispatch(self, command):
        require(not self.failed, 'Adapter stopped after failure')
        try:
            return self._dispatch(command)
        except BaseException:
            self.failed = True
            self.invalidate_capture()
            raise

    def _dispatch(self, command):
        op = command.get('op')
        if op == 'prepare':
            return self.prepare(command)
        if op == 'android_bind':
            require(set(command) == {'op', 'serial'} and self.backend is not None
                    and self.reader is None and isinstance(command['serial'], str)
                    and 1 <= len(command['serial']) <= 128, 'Invalid Android selection')
            matches, unreadable = [], 0
            with self.wire.deadline(15):
                for device in self.backend.usb.core.find(find_all=True, idVendor=0x0e8d,
                                                         backend=self.backend.usb_backend):
                    # A descriptor read can fail while another process holds the
                    # device (Windows refuses it under a running ADB server and
                    # PyUSB then reports a missing langid). Never let an unrelated
                    # MediaTek device abort the bind; report the count instead.
                    try:
                        serial = device.serial_number
                    except Exception:
                        unreadable += 1
                        continue
                    if serial == command['serial']:
                        matches.append(device)
            require(len(matches) == 1 and matches[0].bus and matches[0].port_numbers,
                    'Cannot bind authorized Android serial to one physical USB port'
                    + (f'; {unreadable} MediaTek USB device(s) had unreadable descriptors: stop other ADB servers '
                       'and tools holding the remote, then retry' if unreadable else ''))
            self.wire.send({'event': 'android_bound', 'bus': matches[0].bus,
                'ports': list(matches[0].port_numbers),
                'serial_sha256': hashlib.sha256(command['serial'].encode()).hexdigest()})
        elif op == 'couch_reboot':
            require(set(command) == {'op', 'candidate', 'cid'} and self.backend is not None
                    and self.reader is None and not getattr(self, 'couch_reboot_consumed', False),
                    'Invalid Couch reboot state')
            raw = command['candidate']
            require(isinstance(raw, dict) and set(raw) == {'bus', 'address', 'ports', 'vid', 'pid'}
                    and raw['vid'] == 0x0e8d and raw['pid'] == 0x201c
                    and type(raw['bus']) is int and raw['bus'] > 0
                    and isinstance(raw['ports'], list) and raw['ports']
                    and all(type(n) is int and 0 < n <= 255 for n in raw['ports']), 'Invalid Couch port')
            self.couch_reboot_consumed = True
            serial = None
            try:
                with self.wire.deadline(15):
                    serial = CouchSerial(self.backend.usb, self.backend.usb_backend,
                        Candidate(raw['bus'], raw['address'], tuple(raw['ports']), raw['vid'], raw['pid']))
                    serial.restart(command['cid'])
                result = 'requested'
            except Unavailable:
                result = 'unavailable'
            finally:
                if serial is not None:
                    serial.close()
            self.wire.send({'event': 'couch_reboot', 'result': result})
        elif op == 'enumerate':
            require(self.backend is not None and self.reader is None and set(command) == {'op'}, 'Unexpected enumeration')
            with self.wire.deadline(10):
                found = self.backend.enumerate()
            self.wire.send({'event': 'candidates', 'devices': [dict(bus=d.bus, address=d.address, ports=list(d.ports), vid=d.vid, pid=d.pid) for d in found]})
        elif op == 'start':
            self.start(command)
        elif op in ('read', 'hash'):
            require(self.reader is not None and set(command) == {'op', 'target'} and command['target'] in READABLE, 'Unsupported read target')
            name = command['target']
            self.require_capture_connection()
            if op == 'hash':
                checksum = self.hash_partition(name)
                self.require_capture_connection()
                if name in self.capture_reads:
                    require(checksum == self.capture_reads[name], 'Independent original readback differs')
                self.wire.send({'event': 'hash', 'target': name, 'sha256': checksum})
                if name in self.capture_reads:
                    self.capture_verified[name] = checksum
            else:
                self.capture_reads.pop(name, None)
                self.capture_verified.pop(name, None)
                total = self.reader.description['partitions'][name]['size']
                self.wire.send({'event': 'partition', 'target': name, 'size': total})
                full, done = hashlib.sha256(), 0
                for data in self.reader.chunks(name):
                    done += len(data)
                    require(0 < len(data) <= MAX and done <= total, 'Invalid original capture length')
                    full.update(data)
                    self.wire.chunk(data)
                require(done == total, 'Incomplete original capture')
                self.require_capture_connection()
                checksum = full.hexdigest()
                self.wire.send({'event': 'read_complete', 'target': name, 'sha256': checksum})
                self.capture_reads[name] = checksum
        elif op == 'authorize_boot':
            self.authorize_boot(command)
        elif op == 'write_boot':
            require(set(command) == {'op'} and self.writer is not None and not self.written, 'Boot write not admitted or already consumed')
            self.written = True  # Even an ambiguous first packet consumes this attempt.
            self.writer.write('boot', self.stage)
            require(self.writer.hash('boot') == self.binding['stage_sha256'], 'RAM boot readback differs')
            for name, checksum in {**self.binding['identity_sha256'], **{k:v for k,v in self.binding['original_sha256'].items() if k != 'boot'}}.items():
                require(self.writer.hash(name) == checksum, 'Retained originals changed after bootstrap')
            self.verified = True
            self.wire.send({'event': 'boot_verified'})
        elif op == 'boot':
            require(set(command) == {'op'} and self.verified and self.writer is not None, 'No verified boot operation')
            self.backend.boot_after_capture()
            self.boot_requested = True
            self.wire.send({'event': 'boot_requested'})
        elif op == 'stage_present':
            require(set(command) == {'op'} and self.boot_requested and self.backend is not None,
                    'Stage discovery requires verified bootstrap')
            with self.wire.deadline(10):
                found = [d for d in self.backend.usb.core.find(find_all=True, idVendor=0x0e8d,
                    idProduct=0x201c, backend=self.backend.usb_backend)
                    if d.bus == self.selected.bus and tuple(d.port_numbers or ()) == self.selected.ports]
            require(len(found) <= 1, 'Ambiguous stage device')
            self.wire.send({'event': 'stage_present', 'present': bool(found)})
        elif op == 'stage_open':
            require(set(command) == {'op'} and self.boot_requested and self.stage_usb is None,
                    'Stage USB not admitted or already opened')
            if self.writer is not None:
                self.writer.close()
                self.writer = None
            self.backend.close(reset=False)
            with self.wire.deadline(40):
                self.stage_usb = StageUsb(self.backend.usb, self.backend.usb_backend, self.selected)
            self.wire.send({'event': 'stage_open'})
        elif op in ('stage_status', 'stage_scan', 'stage_bind', 'stage_provision'):
            require(set(command) == {'op', 'payload'} and self.stage_usb is not None,
                    'Unexpected stage USB command')
            with self.wire.deadline(60):
                result = self.stage_usb.dispatch(op, command['payload'])
            self.wire.send({'event': op, 'result': result})
        elif op == 'stage_close':
            require(set(command) == {'op'} and self.stage_usb is not None, 'No stage USB session')
            self.stage_usb.close()
            self.stage_usb = None
            self.wire.send({'event': 'stage_closed'})
        elif op == 'close':
            require(set(command) == {'op'}, 'Unexpected close fields')
            self.close()
            self.wire.send({'event': 'closed'})
            return False
        else:
            require(False, 'Unsupported native adapter operation')
        return True

    def close(self):
        self.failed = True
        self.invalidate_capture()
        if self.stage_usb is not None:
            self.stage_usb.close()
            self.stage_usb = None
        if self.writer is not None:
            self.writer.close()
            self.writer = None
        if self.backend is not None:
            self.backend.close(reset=False)
            self.backend = None


def serve(wire, factory=Adapter):
    adapter = factory(wire)
    try:
        with supervised_operations(wire.deadline):
            while adapter.dispatch(wire.receive()) is not False:
                pass
    finally:
        adapter.close()


CATEGORIES = {'USBError', 'USBTimeoutError', 'InstallError', 'OSError',
              'PermissionError', 'TimeoutError', 'ValueError', 'TypeError',
              'AttributeError', 'RuntimeError'}
REVIEWED_SOURCES = {'mtk_adapter.py', 'mtk_usb.py', 'mtk_tty.py', 'mtk_com.py', 'stage_usb.py', 'mtk_readonly.py', 'mtk_writer.py'}


def _observed(error):
    """One exception as an allowlisted category, bounded codes and reviewed frame."""
    category = type(error).__name__
    if category not in CATEGORIES:
        category = 'WorkerError'
    result = {'category': category}
    for name in ('errno', 'backend_error_code'):
        value = getattr(error, name, None)
        if type(value) is int and -65536 <= value <= 65536:
            result[name] = value
    # A shared transport call sits under several protocol steps, so the innermost
    # frame alone cannot say which step stalled. Record the reviewed frames along
    # the way, outermost first, still as filenames and line numbers only.
    frames = []
    trace = error.__traceback__
    while trace is not None:
        filename = Path(trace.tb_frame.f_code.co_filename).name
        if filename in REVIEWED_SOURCES:
            result['source'] = filename
            result['line'] = trace.tb_lineno
            frames.append({'source': filename, 'line': trace.tb_lineno})
        trace = trace.tb_next
    if len(frames) > 1:
        result['frames'] = frames[-8:]
    return result


def failure_diagnostic(error):
    """Expose only numeric codes and reviewed source locations, never error text.

    The writer reports any non-InstallError USB fault as one ambiguous write, so
    the wrapper's own line is identical whatever actually failed and the
    originating errno and frame are lost. Report the deepest chained cause as
    well, under exactly the same allowlist: category, bounded numeric codes and
    a reviewed filename and line, never message text.
    """
    result = _observed(error)
    seen = {id(error)}
    cause = error.__cause__ or error.__context__
    while cause is not None and id(cause) not in seen and len(seen) <= 4:
        seen.add(id(cause))
        result['cause'] = _observed(cause)
        cause = cause.__cause__ or cause.__context__
    return result


def serve_stdio(factory=Adapter):
    # Keep the RPC pipe on its own descriptor. The pinned MTK library detaches
    # and re-wraps stdout/stderr independently while importing utils.py; aliasing
    # both streams to one TextIOWrapper makes its second detach fail.
    incoming = sys.stdin.buffer
    with os.fdopen(os.dup(sys.stdout.fileno()), 'wb') as outgoing:
        with open(os.devnull, 'wb') as quiet:
            os.dup2(quiet.fileno(), sys.stdout.fileno())
            os.dup2(quiet.fileno(), sys.stderr.fileno())
        wire = Wire(incoming, outgoing)
        try:
            serve(wire, factory)
            return 0
        except BaseException as error:
            try:
                wire.send({'event': 'error', 'preserve_originals': True,
                           'diagnostic': failure_diagnostic(error)})
            except BaseException:
                pass
            return 1


if __name__ == '__main__':
    require(sys.argv[1:] == ['--events-stdio'], 'Use the native host event channel')
    raise SystemExit(serve_stdio())
