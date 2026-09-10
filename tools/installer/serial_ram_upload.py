#!/usr/bin/env python3
"""Upload one bounded file to recovery tmpfs, without executing it or flashing.

An interrupted transfer is ambiguous: do not send shell commands or retry until
an operator has independently restarted the recovery serial shell.
"""
import argparse
import fcntl
import hashlib
import os
from pathlib import Path
import re
import secrets
import select
import stat
import termios
import time
import tty

MAX_SIZE = 64 * 1024 * 1024
CHUNK = 64 * 1024


class UploadError(RuntimeError):
    pass


def write_all(fd, data, timeout=15):
    """One deadline per chunk; short writes advance, never resend accepted bytes."""
    end = time.monotonic() + timeout
    view = memoryview(data)
    while view:
        remaining = end - time.monotonic()
        if remaining <= 0 or not select.select([], [fd], [], remaining)[1]:
            raise UploadError('serial write timed out; receiver state is ambiguous')
        try:
            count = os.write(fd, view)
        except BlockingIOError:
            continue
        if not isinstance(count, int) or isinstance(count, bool) or count <= 0:
            raise UploadError('invalid serial write result')
        view = view[count:]


def wait_line(fd, expected, timeout):
    end = time.monotonic() + timeout
    pending = bytearray()
    total = 0
    while time.monotonic() < end:
        if not select.select([fd], [], [], max(0, end - time.monotonic()))[0]:
            break
        try:
            data = os.read(fd, 4096)
        except BlockingIOError:
            continue
        if not data:
            raise UploadError('serial disconnected')
        total += len(data)
        if total > 128 * 1024:
            raise UploadError('excessive serial response')
        pending.extend(data)
        while b'\n' in pending:
            line, _, rest = pending.partition(b'\n')
            pending = bytearray(rest)
            if line.rstrip(b'\r') == expected:
                return
            if line.startswith(b'COUCH_UPLOAD_DONE_'):
                raise UploadError('remote checksum or length mismatch')
    raise UploadError('serial handshake timed out; receiver state is ambiguous')


def receiver_command(nonce, size):
    if not re.fullmatch('[0-9a-f]{32}', nonce) or not 0 < size <= MAX_SIZE:
        raise ValueError('invalid transfer parameters')
    path = '/tmp/couch-upload-' + nonce
    # All interpolations are generated hex/integers; no user path becomes shell.
    # noclobber prevents pre-existing file/symlink replacement. Only tmpfs is valid.
    command = (
        "stty raw -echo; ( umask 077; set -C; "
        "grep -q ' /tmp tmpfs ' /proc/mounts && "
        f"exec 3>{path} || exit 1; "
        f"printf '\\nCOUCH_UPLOAD_READY_{nonce}\\n'; "
        f"head -c {size} >&3 || {{ while :; do sleep 3600; done; }}; exec 3>&-; "
        f"n=$(wc -c < {path}); h=$(sha256sum {path}); "
        f"printf '\\nCOUCH_UPLOAD_DONE_{nonce} %s %s\\n' \"$n\" \"${{h%% *}}\" )\n"
    )
    return path, command.encode()


def upload(port, source, timeout=120):
    source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    serial_fd = None
    try:
        before = os.fstat(source_fd)
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_SIZE:
            raise UploadError('source must be a regular file of 1 byte to 64 MiB')
        digest = hashlib.sha256()
        while chunk := os.read(source_fd, CHUNK):
            digest.update(chunk)
        os.lseek(source_fd, 0, os.SEEK_SET)
        nonce = secrets.token_hex(16)
        path, command = receiver_command(nonce, before.st_size)
        serial_fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        if not os.isatty(serial_fd):
            raise UploadError('explicit port must be a terminal device')
        fcntl.flock(serial_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        tty.setraw(serial_fd, termios.TCSANOW)
        # Do not flush: another operation's pending response must not be discarded.
        write_all(serial_fd, command)
        wait_line(serial_fd, f'COUCH_UPLOAD_READY_{nonce}'.encode(), timeout)
        remaining = before.st_size
        while remaining:
            chunk = os.read(source_fd, min(CHUNK, remaining))
            if not chunk:
                raise UploadError('source truncated during transfer')
            write_all(serial_fd, chunk)
            remaining -= len(chunk)
        after = os.fstat(source_fd)
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise UploadError('source changed during transfer')
        wait_line(serial_fd, (
            f'COUCH_UPLOAD_DONE_{nonce} {before.st_size} {digest.hexdigest()}'
        ).encode(), timeout)
        return {'path': path, 'bytes': before.st_size, 'sha256': digest.hexdigest()}
    finally:
        if serial_fd is not None:
            os.close(serial_fd)
        os.close(source_fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', required=True, help='Explicit local serial device; no discovery')
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--timeout', type=float, default=120)
    args = parser.parse_args()
    if not 0 < args.timeout <= 600:
        parser.error('timeout must be greater than 0 and at most 600 seconds')
    try:
        result = upload(args.port, args.source, args.timeout)
    except (OSError, UploadError) as error:
        parser.exit(1, f'{error}\nNo retry sent. Reset receiver before retrying an interrupted payload.\n')
    print(f"Verified RAM upload: {result['path']} ({result['bytes']} bytes, SHA256 {result['sha256']})")


if __name__ == '__main__':
    main()
