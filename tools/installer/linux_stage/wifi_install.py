#!/usr/bin/env python3
"""Private USB-bound TLS installer client. No passwords in argv or journals."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import ssl
import struct
import subprocess
import tempfile
import zlib

import wifi_benchmark as wifi
from benchmark import read_exact, response

CHUNK = 1024 * 1024
ORDER = ('recovery', 'userdata', 'logo', 'odmdtbo', 'boot')
IDENTITY = {'proinfo', 'nvram', 'nvdata', 'protect1', 'protect2'}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path, progress=None, phase='Verify host backup'):
    with Path(path).open('rb') as file:
        if progress is None:
            return hashlib.file_digest(file, 'sha256').hexdigest()
        total = os.fstat(file.fileno()).st_size
        done, full = 0, hashlib.sha256()
        def report():
            progress(f'{phase}: {Path(path).stem} {done}/{total} bytes ({done * 100 // total if total else 0}%)')
        report()
        while raw := file.read(CHUNK):
            full.update(raw); done += len(raw); report()
        require(done == total, 'Host file size changed during verification')
        return full.hexdigest()


def receive_verification(stream, target, phase, total, progress):
    """Progress is informational; the final checked hash/phase still gates ACK."""
    previous = -1
    for _ in range((total + CHUNK - 1) // CHUNK + 3):
        event = receive_json(stream)
        if event.get('event') != 'verify_progress':
            require(previous in (-1, total), 'Verification progress ended before full readback')
            return event  # Older pinned stages have no progress frames.
        require(set(event) == {'event', 'phase', 'target', 'done', 'total'}
                and event['phase'] == phase and event['target'] == target
                and type(event['total']) is int and event['total'] == total
                and type(event['done']) is int and previous < event['done'] <= total
                and event['done'] >= 0, 'Invalid verification progress')
        require(previous != -1 or event['done'] == 0, 'Verification progress must start at zero')
        previous = event['done']
        label = 'Verify backup' if phase == 'backup' else 'Verify installed'
        progress(f'{label}: {target} {previous}/{total} bytes ({previous * 100 // total}%)')
    raise ValueError('Excessive verification progress')


def save(path, value):
    path = Path(path)
    pending = path.with_suffix('.pending')
    with pending.open('x') as file:
        json.dump(value, file, indent=2)
        file.flush()
        os.fsync(file.fileno())
    pending.chmod(0o600)
    os.replace(pending, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def json_bytes(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def send_json(stream, value):
    data = value if isinstance(value, bytes) else json_bytes(value)
    require(0 < len(data) <= CHUNK, 'JSON frame exceeds bound')
    stream.sendall(struct.pack('<I', len(data)) + data)


def receive_json(stream):
    endpoint = wifi.TlsEndpoint(stream)
    size, = struct.unpack('<I', read_exact(endpoint, 4))
    require(0 < size <= CHUNK, 'Invalid JSON frame size')
    value = json.loads(read_exact(endpoint, size))
    require(isinstance(value, dict), 'Expected JSON object')
    if value.get('event') == 'error':
        raise ValueError('Device stopped the installation. Preserve originals and journal; inspect USB diagnostics.')
    return value


def ack(stream, event, target='none', sha256=''):
    send_json(stream, {'ack': event, 'target': target, 'sha256': sha256})


def send_chunk(stream, raw):
    require(0 < len(raw) <= CHUNK, 'Invalid image chunk')
    data = zlib.compress(raw, 1)
    kind = 1
    if len(data) >= len(raw):
        data, kind = raw, 0
    stream.sendall(struct.pack('<III', len(raw), len(data), kind) + data)


def receive_chunk(stream, expected):
    endpoint = wifi.TlsEndpoint(stream)
    size, wire, kind = struct.unpack('<III', read_exact(endpoint, 12))
    require(size == expected and 0 < size <= CHUNK and 0 < wire <= CHUNK and kind in (0, 1),
            'Invalid backup chunk')
    data = read_exact(endpoint, wire)
    if kind == 0:
        require(wire == size, 'Raw backup chunk size mismatch')
        return data
    decoder = zlib.decompressobj()
    raw = decoder.decompress(data, size + 1)
    require(len(raw) == size and decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail,
            'Compressed backup exceeds bound or is truncated')
    return raw


def copy_private_image(source, destination):
    """Copy regular image bytes without GNU-only cp flags (also works on macOS)."""
    source, destination = Path(source), Path(destination)
    require(source.is_file() and not source.is_symlink(), 'Expected regular source image')
    with source.open('rb') as incoming, destination.open('xb') as outgoing:
        destination.chmod(0o600)
        while block := incoming.read(CHUNK):
            if not block.strip(b'\0'):
                outgoing.seek(len(block), 1)
            else:
                outgoing.write(block)
        outgoing.truncate(incoming.tell())
        outgoing.flush()
        os.fsync(outgoing.fileno())


def customize_userdata(source, destination, network):
    """Modify a private copy before hashing; debugfs never opens a block device."""
    require(Path(source).is_file() and not Path(source).is_symlink(), 'Expected regular OS image')
    require(not Path(destination).exists(), 'Private image already exists')
    require(shutil.which('debugfs') is not None, 'Install e2fsprogs (debugfs) before starting the trial')
    copy_private_image(source, destination)
    Path(destination).chmod(0o600)
    security = ('    key_mgmt=NONE\n' if network['psk_hex'] is None else
                '    key_mgmt=WPA-PSK\n    proto=RSN\n    psk=' + network['psk_hex'] + '\n')
    data = ('network={\n    ssid=' + network['ssid_hex'] + '\n    scan_ssid=1\n' + security + '}\n').encode()
    # Only generated paths appear in debugfs commands; never interpolate SSID/password.
    with tempfile.TemporaryDirectory(prefix='couch-network-') as temporary:
        root = Path(temporary)
        config = root / 'networks.conf'
        config.write_bytes(data)
        config.chmod(0o600)
        commands = root / 'commands'
        commands.write_text(f'write {json.dumps(config.as_posix(), ensure_ascii=False)} /opt/couch/networks.conf\nset_inode_field /opt/couch/networks.conf mode 0100600\n')
        result = subprocess.run(['debugfs', '-w', '-f', str(commands), str(destination)],
                                capture_output=True, text=True)
        require(result.returncode == 0, 'Private network configuration failed')
        check = subprocess.run(['debugfs', '-R', 'cat /opt/couch/networks.conf', str(destination)],
                               capture_output=True)
        require(check.returncode == 0 and check.stdout == data, 'Private network configuration readback mismatch')
    with Path(destination).open('rb') as file:
        os.fsync(file.fileno())


def compact_userdata(path, partition_size, progress=print):
    """Shrink a private regular ext4 file; only filesystem tools determine its end."""
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), 'Expected regular private ext4 image')
    for tool in ('e2fsck', 'resize2fs'):
        require(shutil.which(tool) is not None, f'Required host tool is missing: {tool}')
    progress('Compacting the OS filesystem; empty partition space will not be flashed...')
    for argv in (['e2fsck', '-fn'], ['resize2fs', '-M'], ['e2fsck', '-fn']):
        result = subprocess.run([*argv, str(path)], capture_output=True)
        require(result.returncode == 0, 'OS filesystem compaction/check failed')
    with path.open('rb') as file:
        file.seek(1024); sb = file.read(1024)
    require(len(sb) == 1024 and sb[56:58] == b'\x53\xef'
            and struct.unpack_from('<I', sb, 24)[0] == 2, 'Expected 4 KiB ext4 image')
    size = struct.unpack_from('<I', sb, 4)[0] * 4096
    require(0 < size <= path.stat().st_size <= partition_size, 'Compact filesystem size mismatch')
    # resize2fs can leave a regular file longer than the checked filesystem.
    # Remove only bytes beyond its superblock-declared end, never live blocks.
    if size < path.stat().st_size:
        with path.open('r+b') as file:
            file.truncate(size); file.flush(); os.fsync(file.fileno())
        result = subprocess.run(['e2fsck', '-fn', str(path)], capture_output=True)
        require(result.returncode == 0, 'Trimmed OS filesystem check failed')
    require(struct.unpack_from('<III', sb, 92) == (0x2c, 0x42, 0x73), 'Ext4 features changed')
    with path.open('rb') as file: os.fsync(file.fileno())
    progress(f'OS image: {size / 1024**2:.1f} MiB; partition: {partition_size / 1024**3:.2f} GiB. Remaining space is grown on device.')
    return size


def make_plan(release, bundle, bootstrap, stage_sha256, network, work, progress=print, *, skip_userdata_backup=False, reused_backups=None, restore_originals=None):
    work = Path(work)
    work.mkdir(mode=0o700)
    baseline = json.loads((Path(bootstrap) / 'baseline.json').read_text())
    receipt = json.loads((Path(bootstrap) / 'backup-receipt.json').read_text())
    journal_path = Path(bootstrap) / 'journal.json'
    require(receipt.get('complete') is True and receipt['journal_sha256'] == digest(journal_path),
            'Bootstrap receipt is incomplete or changed')
    bootstrap_journal = json.loads(journal_path.read_text())
    require(bootstrap_journal.get('complete') is True and bootstrap_journal['stage_sha256'] == stage_sha256,
            'Unexpected bootstrap image')
    original_os = bootstrap_journal.get('original_os', 'Android')
    require(original_os in ('Android', 'Couch'), 'Unknown original OS in bootstrap proof')
    require(restore_originals is None or original_os == 'Android',
            'Couch backups cannot be used as Android restoration images')
    require(release['partitions'] == baseline['partitions'], 'Release differs from observed device layout')
    require(receipt['originals']['boot'] == digest(Path(bootstrap) / 'boot.img'), 'Original boot backup changed')
    if restore_originals is None:
        progress('Preparing a private OS image with your selected Wi-Fi network...')
        customize_userdata(Path(bundle) / release['images']['userdata']['file'], work / 'userdata.img', network)
        compact_userdata(work / 'userdata.img', baseline['partitions']['userdata']['size'], progress)
    else:
        require(not skip_userdata_backup and reused_backups
                and set(reused_backups) == (set(release['images']) | IDENTITY), 'Restore requires full original backups')
        require(reused_backups['boot'] == receipt['originals']['boot'], 'Restore original boot differs')
        progress('Preparing verified Android originals for restoration; boot is restored last.')
    require(type(skip_userdata_backup) is bool, 'Invalid backup policy')
    plan = {'schema': 1, 'skip_userdata_backup': skip_userdata_backup, 'reused_backups': reused_backups or {},
            'nonce': os.urandom(32).hex(), 'manifest_sha256': hashlib.sha256(json_bytes(release)).hexdigest(),
            'cid': baseline['cid'], 'capacity': baseline['capacity'], 'partitions': baseline['partitions'],
            'identity_sha256': baseline['identity_sha256'], 'images': {},
            'original_boot_sha256': receipt['originals']['boot'], 'stage_sha256': stage_sha256}
    paths = {}
    for name in ORDER:
        if name not in release['images']:
            continue
        path = (Path(restore_originals) / (name + '.img') if restore_originals is not None else
                (work / 'userdata.img' if name == 'userdata' else Path(bundle) / release['images'][name]['file']))
        require(path.is_file() and not path.is_symlink(), 'Expected regular image source')
        progress('Hashing image chunks: ' + name)
        chunks, full = [], hashlib.sha256()
        with path.open('rb') as file:
            while raw := file.read(CHUNK):
                chunks.append(hashlib.sha256(raw).hexdigest())
                full.update(raw)
        checksum = full.hexdigest()
        require(0 < path.stat().st_size <= release['partitions'][name]['size']
                and (name == 'userdata' or path.stat().st_size == release['partitions'][name]['size']), 'OS image size changed')
        if restore_originals is not None:
            require(path.stat().st_size == baseline['partitions'][name]['size']
                    and checksum == reused_backups[name], 'Android original changed before restoration')
        else:
            require(name == 'userdata' or checksum == release['images'][name]['sha256'], 'OS image changed')
        plan['images'][name] = {'size': path.stat().st_size, 'sha256': checksum, 'chunks': chunks}
        paths[name] = path
    if restore_originals is not None:
        plan['manifest_sha256'] = hashlib.sha256(json_bytes({'purpose': 'restore-original-android',
            'images': plan['images'], 'partitions': plan['partitions']})).hexdigest()
        save(work / 'operation.json', {'operation': 'restore-original-android', 'source': str(Path(restore_originals).resolve())})
    save(work / 'plan.json', plan)
    return plan, paths


def bind_plan(out, incoming, plan):
    data = json_bytes({'plan_sha256': hashlib.sha256(json_bytes(plan)).hexdigest(), 'nonce': plan['nonce']})
    require(out.write(struct.pack('<4sIQ', b'CBP1', 6, len(data)), timeout=30000) == 16
            and out.write(data, timeout=30000) == len(data), 'Short install binding')
    response(incoming, 0)


def transaction(stream, plan, paths, bootstrap, destination, progress=print, reboot=lambda: False, *, reuse_from=None):
    destination = Path(destination)
    destination.mkdir(mode=0o700)
    skip_userdata = plan.get('skip_userdata_backup', False)
    require(type(skip_userdata) is bool, 'Invalid backup policy')
    selected_backups = (set(plan['images']) | IDENTITY) - ({'userdata'} if skip_userdata else set())
    reused = plan.get('reused_backups', {})
    require(isinstance(reused, dict) and bool(reused) == (reuse_from is not None), 'Reused backup source missing')
    if reused:
        require(not skip_userdata and set(reused) == selected_backups, 'Reused backup inventory mismatch')
    needed = sum(plan['partitions'][name]['size'] for name in selected_backups)
    require(shutil.disk_usage(destination).free >= needed + 16 * CHUNK, 'Insufficient space for full original backups')
    bootstrap_journal = Path(bootstrap) / 'journal.json'
    receipt = json.loads((Path(bootstrap) / 'backup-receipt.json').read_text())
    require(receipt.get('complete') is True and receipt['journal_sha256'] == digest(bootstrap_journal),
            'Bootstrap proof changed before transaction')
    original_os = json.loads(bootstrap_journal.read_text()).get('original_os', 'Android')
    require(original_os in ('Android', 'Couch'), 'Unknown original OS in bootstrap proof')
    record = {'schema': 1, 'original_os': original_os, 'complete': False, 'backup_complete': False, 'backups': {}, 'writes': {},
              'backup_policy': 'reused_full' if reused else ('skip_userdata' if skip_userdata else 'full'),
              'plan_sha256': hashlib.sha256(json_bytes(plan)).hexdigest(), 'nonce': plan['nonce'],
              'bootstrap_receipt_sha256': digest(Path(bootstrap) / 'backup-receipt.json')}
    journal = destination / 'journal.json'
    save(journal, record)
    stream.sendall(struct.pack('<4sIQ', b'CBP1', 10, 0))
    send_json(stream, plan)
    bound = receive_json(stream)
    require(bound == {'event': 'bound', **{key: plan[key] for key in
            ('nonce', 'cid', 'capacity', 'partitions', 'manifest_sha256')}}, 'RAM stage identity/binding mismatch')
    expected_names = [] if reused else sorted(selected_backups - {'boot'})
    expected_writes = [name for name in ORDER if name in plan['images']]
    try:
        event = receive_json(stream)
        require(event == {'event': 'original_boot', 'target': 'boot', 'sha256': plan['original_boot_sha256']},
                'Original boot receipt mismatch')
        source = Path(bootstrap) / 'boot.img'
        require(digest(source) == plan['original_boot_sha256'], 'Original boot backup corrupted')
        shutil.copyfile(source, destination / 'boot.img')
        with (destination / 'boot.img').open('rb') as file:
            os.fsync(file.fileno())
        record['backups']['boot'] = digest(destination / 'boot.img')
        save(journal, record)
        ack(stream, 'original_boot', 'boot', record['backups']['boot'])
        if reused:
            record['reused_from'] = str(Path(reuse_from).resolve())
            for name in sorted(selected_backups - {'boot'}):
                source = Path(reuse_from) / (name + '.img')
                require(source.is_file() and not source.is_symlink()
                        and source.stat().st_size == plan['partitions'][name]['size']
                        and digest(source, progress) == reused[name], 'Saved original changed before restart')
                target = destination / (name + '.img')
                copy_private_image(source, target)
                target.chmod(0o600)
                with target.open('rb') as file: os.fsync(file.fileno())
                require(digest(target, progress) == reused[name], 'Copied original backup differs')
                record['backups'][name] = reused[name]
                save(journal, record)
        for name in expected_names:
            size = plan['partitions'][name]['size']
            require(receive_json(stream) == {'event': 'backup', 'target': name, 'size': size}, 'Unexpected backup order')
            full, done = hashlib.sha256(), 0
            path = destination / f'{name}.img'
            with path.open('xb') as file:
                while done < size:
                    raw = receive_chunk(stream, min(CHUNK, size - done))
                    full.update(raw)
                    # Sparse host files still preserve every original byte.
                    if not raw.strip(b'\0'):
                        file.seek(len(raw), 1)
                    else:
                        file.write(raw)
                    done += len(raw)
                    progress(f'Backup: {name} {done}/{size} bytes ({done * 100 // size}%)')
                file.truncate(size)
                file.flush()
                os.fsync(file.fileno())
            path.chmod(0o600)
            require(receive_json(stream) == {'event': 'checking_backup', 'target': name}, 'Missing backup readback phase')
            progress('Checking independent storage readback: ' + name)
            checked = receive_verification(stream, name, 'backup', size, progress)
            checksum = full.hexdigest()
            require(checked == {'event': 'backup_verified', 'target': name, 'sha256': checksum}, 'Backup readback mismatch')
            require(digest(path, progress) == checksum, 'Host backup file readback mismatch')
            if name in IDENTITY:
                require(checksum == plan['identity_sha256'][name], 'Identity changed during backup')
            record['backups'][name] = checksum
            save(journal, record)
            ack(stream, 'backup_verified', name, checksum)
        require(receive_json(stream) == {'event': 'backups_complete'}, 'Backups did not complete')
        record['backup_complete'] = True
        save(journal, record)
        ack(stream, 'backups_complete')
        for name in expected_writes:
            image = plan['images'][name]
            require(receive_json(stream) == {'event': 'writing', 'target': name}, 'Unexpected write order')
            require(digest(paths[name], progress, 'Verify image') == image['sha256'], 'Host image changed before write')
            record['writes'][name] = 'writing'
            save(journal, record)
            ack(stream, 'writing', name)
            done = 0
            with Path(paths[name]).open('rb') as file:
                for checksum in image['chunks']:
                    raw = file.read(CHUNK)
                    require(hashlib.sha256(raw).hexdigest() == checksum, 'Image changed during transfer')
                    send_chunk(stream, raw)
                    done += len(raw)
                    progress(f'Write: {name} {done}/{image["size"]} bytes ({done * 100 // image["size"]}%)')
                require(done == image['size'] and not file.read(1), 'Image length changed')
            phases = ('synced', 'verified')
            if name == 'userdata' and image['size'] < plan['partitions'][name]['size']:
                phases += ('expanding', 'expanded')
            for phase in phases:
                event = (receive_verification(stream, name, 'write', image['size'], progress)
                         if phase == 'verified' else receive_json(stream))
                require(event == {'event': phase, 'target': name}, 'Storage verification phase mismatch')
                record['writes'][name] = phase
                save(journal, record)
                ack(stream, phase, name)
                progress({'synced': 'Storage flushed; independently verifying: ',
                          'verified': 'Readback verified: ', 'expanding': 'Growing verified filesystem to fill partition: ',
                          'expanded': 'Filesystem expanded and checked: '}[phase] + name)
        require(receive_json(stream) == {'event': 'complete', 'target': 'none'}, 'Missing transaction completion')
        record['partitions_verified'] = True
        save(journal, record)
        ack(stream, 'complete')
        require(receive_json(stream) == {'event': 'installed', 'verified': True}, 'Final identity checks failed')
        record.update(complete=True, identity_verified_after=True)
        save(journal, record)
        progress('OS images and identity verified. ' +
                 (f'YOLO: recovery backups saved; {original_os} app/data backup was skipped.' if skip_userdata
                  else f'Original {original_os} backups are saved.'))
        start = reboot()
        send_json(stream, {'action': 'reboot' if start else 'leave'})
        if start:
            require(receive_json(stream) == {'event': 'rebooting'}, 'Reboot acknowledgment missing')
        return record
    except BaseException as error:
        record['stopped'] = type(error).__name__
        save(journal, record)
        raise


def connect(address, pem, identity):
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_verify_locations(cadata=pem)
    raw = socket.create_connection((address, 8443), timeout=30)
    try:
        stream = context.wrap_socket(raw, server_hostname='couch-probe')
        # Whole-device SHA on ARM may take minutes; individual transfer waits
        # remain bounded, with explicit phase text during long local readback.
        stream.settimeout(900)
        stream.sendall(bytes.fromhex(identity['token_hex']))
        require(read_exact(wifi.TlsEndpoint(stream), 4) == b'OKAY', 'TLS authentication rejected')
        return stream
    except BaseException:
        raw.close()
        raise
