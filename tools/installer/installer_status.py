#!/usr/bin/env python3
"""Read-only view of an existing installer log. Never starts or controls installation."""
import argparse
import io
import os
from pathlib import Path
import select
import stat
import sys
import termios
import time
import tty

from couch_tui import LOGO, PhaseProgress

PARTITIONS = {'boot', 'recovery', 'userdata', 'odmdtbo', 'logo',
              'proinfo', 'nvram', 'nvdata', 'protect1', 'protect2'}
SUCCESS = 'Boot stage readback and retained stock recovery/identity verified. Original boot and recovery are backed up; userdata untouched.'


def safe_line(line):
    """Return only reviewed counters/status; never pass arbitrary child output."""
    line = line.strip()
    match = PhaseProgress.PATTERN.fullmatch(line)
    if match:
        _, name, done, total = match.groups()
        if name in PARTITIONS and 0 <= int(done) <= int(total) and 0 < int(total) <= 128 * 1024**3:
            return line
        return None
    for prefix in ('Backing up identity partition: ', 'Verifying independent readback: ',
                   'Verifying runtime baseline: '):
        if line.startswith(prefix) and line[len(prefix):] in PARTITIONS:
            return line
    if line == 'Waiting for selected preloader. USB is used only to bootstrap the RAM stage.':
        return 'Waiting for the remote on USB'
    if line == SUCCESS:
        return 'Bootstrap verified. Original boot/recovery saved; userdata untouched.'
    if line == 'DA exit acknowledged; hold Power if off. RAM-stage startup remains unverified.':
        return 'USB bootstrap finished. Waiting for RAM-stage startup validation.'
    if line.startswith(('RAM bootstrap stopped:', 'Traceback (most recent call last):', 'Interrupted; retain originals')):
        return 'Installer stopped or reported an error. Operator review required.'
    return None


class Display:
    def __init__(self, output, clock=time.monotonic):
        self.output = output
        self.buffer = io.StringIO()
        self.progress = PhaseProgress(self.buffer, clock)

    def show(self, line):
        counter = PhaseProgress.PATTERN.fullmatch(line)
        initial = bool(counter) and (self.progress.key !=
                  (counter[1], counter[2], int(counter[4])) or int(counter[3]) < self.progress.previous)
        self.buffer.seek(0); self.buffer.truncate()
        self.progress.line(line)
        rendered = self.buffer.getvalue()
        if initial:
            rendered = rendered.replace('0.00 MiB/s', 'rate unknown')
        self.output.write(rendered)
        self.output.flush()


def follow(path, output=sys.stdout):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    original = None
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError('Expected regular log file')
        with os.fdopen(fd, 'r', encoding='utf-8', errors='replace') as source:
            fd = None
            # Seed exactly one latest safe line; replay cannot manufacture speed.
            size = os.fstat(source.fileno()).st_size
            source.seek(max(0, size - 1024 * 1024))
            if size > 1024 * 1024:
                source.readline()
            history = source.read()
            lines = history.split('\n')
            pending = lines.pop()
            latest = next((safe for line in reversed(lines) if (safe := safe_line(line))), None)
            output.write('\033[2J\033[H' + LOGO + '\n  INSTALLER STATUS — READ ONLY\n\n'
                         '  Watching the existing process; this view cannot flash or cancel it.\n'
                         '  Rates measure new log updates only. q / Ctrl-C closes this viewer.\n\n')
            display = Display(output)
            display.show(latest or 'Waiting for a recognized progress update')
            if sys.stdin.isatty():
                original = termios.tcgetattr(sys.stdin.fileno())
                tty.setcbreak(sys.stdin.fileno())
            while True:
                if original is not None and select.select([sys.stdin], [], [], 0.2)[0]:
                    if os.read(sys.stdin.fileno(), 1) in (b'q', b'Q'):
                        break
                elif original is None:
                    time.sleep(0.2)
                if os.fstat(source.fileno()).st_size < source.tell():
                    display.show('Log was truncated; reopen viewer to follow the new session')
                    break
                chunk = source.read()
                pending += chunk
                if len(pending) > 128 * 1024:
                    pending = ''  # Discard unbounded/non-line diagnostics; never display them.
                while '\n' in pending:
                    line, pending = pending.split('\n', 1)
                    safe = safe_line(line)
                    if safe is not None:
                        display.show(safe)
    except KeyboardInterrupt:
        pass
    finally:
        if original is not None:
            termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)
        if fd is not None:
            os.close(fd)
        output.write('\nViewer closed. Installer process was not signalled.\n')
        output.flush()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', type=Path, required=True)
    args = parser.parse_args()
    try:
        follow(args.log)
    except (OSError, ValueError):
        parser.exit(1, 'Could not read the selected log. Installer process was not changed.\n')
