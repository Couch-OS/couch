"""Bounded shared progress schema for host status and device rendering; no raw logs."""
from dataclasses import dataclass
import math
import re

PHASES = {'wait', 'connect', 'download', 'verify', 'write', 'complete', 'error', 'backup'}
TARGETS = {'none', 'boot', 'recovery', 'userdata', 'logo', 'odmdtbo', 'ram',
           'proinfo', 'nvram', 'nvdata', 'protect1', 'protect2'}
WIFI = {'waiting', 'ready', 'connecting', 'connected', 'failed'}
ERRORS = {'none', 'network', 'verify', 'storage', 'protocol'}
MAX_BYTES = 128 * 1024**3


@dataclass(frozen=True)
class Event:
    phase: str = 'wait'
    target: str = 'none'
    done: int = 0
    total: int = 0
    rate: int = 0
    wifi: str = 'waiting'
    error: str = 'none'

    def encode(self):
        if self.phase not in PHASES or self.target not in TARGETS or self.wifi not in WIFI or self.error not in ERRORS:
            raise ValueError('Unknown display status code')
        if any(type(v) is not int or not 0 <= v <= MAX_BYTES for v in (self.done, self.total, self.rate)) or self.done > self.total:
            raise ValueError('Invalid display counters')
        return f'v1 {self.phase} {self.target} {self.done} {self.total} {self.rate} {self.wifi} {self.error}\n'


def from_host_counter(line, *, elapsed=0, previous=0, wifi='waiting'):
    """Match the same measured counter format consumed by TUI PhaseProgress.

    Caller resets elapsed/previous at phase changes. Replayed initial counters
    default to rate unknown; never infer rates from historic byte totals.
    """
    match = re.fullmatch(r'(Backup|Hash readback|Write|Verify image): ([a-z0-9_]+) (\d+)/(\d+) bytes \((\d+)%\)', line)
    if not match:
        raise ValueError('Not a recognized host progress counter')
    phase, target, done, total, percent = match.groups()
    done, total, percent = int(done), int(total), int(percent)
    if total <= 0 or percent != done * 100 // total:
        raise ValueError('Inconsistent host progress counter')
    if not math.isfinite(elapsed) or elapsed < 0 or type(previous) is not int or not 0 <= previous <= done:
        raise ValueError('Invalid progress sampling interval')
    rate = int((done-previous)/elapsed) if elapsed > 0 else 0
    event = Event({'Backup': 'backup', 'Hash readback': 'verify', 'Write': 'write', 'Verify image': 'verify'}[phase],
                  target, done, total, rate, wifi)
    event.encode()
    return event
