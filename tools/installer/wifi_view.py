"""Compact installer view; raw diagnostics stay in the private session log."""
import os
import shutil
import threading
import time
import textwrap

from couch_tui import PhaseProgress


class WifiView:
    STEPS = ('Prepare', 'USB startup', 'Wi-Fi', 'Android backup', 'Install Couch', 'First boot')
    COLORS = {'active': '\033[38;5;87m', 'ok': '\033[38;5;120m',
              'error': '\033[38;5;203m', 'violet': '\033[38;5;141m',
              'muted': '\033[38;5;245m', 'text': '\033[38;5;253m',
              'rail': '\033[38;5;239m'}
    LOGO = (
        r'                       __     ',
        r'  _________  __  _____/ /_    ',
        r' / ___/ __ \/ / / / ___/ __ \ ',
        r'/ /__/ /_/ / /_/ / /__/ / / / ',
        r'\___/\____/\__,_/\___/_/ /_/. ',
    )

    def __init__(self, terminal, clock=time.monotonic):
        self.terminal, self.clock = terminal, clock
        self.reader = terminal.reader
        self.writer = self
        self.tty = bool(getattr(terminal.writer, 'isatty', lambda: False)()) and os.environ.get('TERM') != 'dumb'
        self.color = self.tty and 'NO_COLOR' not in os.environ
        self.log = None
        self.step = 0
        self.detail = 'Checking the installer package...'
        self.action = ''
        self.counter = ''
        self.result = 'active'
        self.path = ''
        self.pending = ''
        self.key = None
        self.previous = 0
        self.last_plain = None
        self.paused = False
        self.prompting = False
        self.lock = threading.RLock()
        self.stop = threading.Event()
        self.worker = None
        self.frame = 0
        self.started = self.clock()
        self.run_started = self.started
        self.measurement = None
        self.last_frame = None

    @staticmethod
    def clean(value):
        return ''.join(c for c in value if c == '\n' or (ord(c) >= 32 and ord(c) != 127))

    def start(self):
        self.render()
        if self.tty:
            self.worker = threading.Thread(target=self.animate, daemon=True)
            self.worker.start()

    def animate(self):
        while not self.stop.wait(0.15):
            with self.lock:
                self.frame += 1
                if not self.paused:
                    self.render()

    def paint(self, text, kind):
        return self.COLORS[kind] + text + '\033[0m' if self.color else text

    def frame_lines(self, columns, rows):
        """Build a bounded frame; prompt space is reserved below the panel."""
        width = max(20, min(86, columns - 4))
        inner = width - 4
        wide = width >= 66 and rows >= 24
        spinner = ('›' if self.prompting else '◜◠◝◞◡◟'[self.frame % 6]) if self.result == 'active' else ('✓' if self.result == 'ok' else '!')
        elapsed = PhaseProgress.duration(self.clock() - self.run_started)
        lines = []
        if wide:
            for i, row in enumerate(self.LOGO):
                lines.append(self.paint(row, 'active' if i < 3 else 'violet'))
        else:
            lines.append(self.paint('couch.', 'active') + self.paint('  /  INSTALLER', 'violet'))
        lines.append('')
        names = ('PREPARE', 'USB', 'WI-FI', 'BACKUP', 'INSTALL', 'BOOT')
        if width >= 60:
            route = []
            for index, name in enumerate(names):
                marker = '✓' if index < self.step else ('●' if index == self.step else '○')
                kind = 'ok' if index < self.step else ('active' if index == self.step else 'muted')
                route.append(self.paint(marker + ' ' + name, kind))
            lines.append('  '.join(route))
        else:
            lines.append(self.paint(f'STEP {self.step + 1} / 6  ·  {self.STEPS[self.step]}', 'active'))
        lines.append('')
        title = f' {spinner}  {self.STEPS[self.step].upper()} '
        right = f' {elapsed} '
        lines.append(self.paint('╭' + '─' + title + '─' * max(0, width-len(title)-len(right)-3) + right + '╮', self.result))

        def body(text='', kind='text'):
            text = self.clean(text)[:inner]
            return self.paint('│ ', 'rail') + self.paint(text.ljust(inner), kind) + self.paint(' │', 'rail')

        for text in textwrap.wrap(self.clean(self.detail), inner)[:3] or ['']:
            lines.append(body(text, 'error' if self.result == 'error' else 'text'))
        lines.append(body())
        if self.measurement and self.counter:
            done, total, rate = self.measurement
            size = max(8, inner - 7)
            count = int(done / total * size)
            bar = '━' * count + '─' * (size-count)
            text = f'{bar}  {done / total:4.0%}'
            lines.append(body(text, 'active'))
            amount = f'{done / 1048576:.1f} / {total / 1048576:.1f} MiB'
            speed = f'{rate / 1048576:.1f} MiB/s' if rate > 0 else 'measuring speed'
            lines.append(body(amount + '   ·   ' + speed, 'muted'))
        elif self.prompting:
            lines.append(body('YOUR TURN  ·  Enter your choice below.', 'violet'))
        elif self.result == 'active':
            size = min(24, inner - 14)
            position = self.frame % max(1, size * 2 - 2)
            position = min(position, size * 2 - 2 - position)
            scan = '─' * position + '◆' + '─' * (size-position-1)
            lines.append(body(scan + '  WORKING', 'violet'))
        else:
            lines.append(body('✓  COMPLETE' if self.result == 'ok' else '!  NEEDS ATTENTION', self.result))
        lines.append(body())
        lines.append(body('NEXT', 'violet'))
        action_lines = []
        for paragraph in self.clean(self.action or 'Follow the prompt below.').splitlines():
            action_lines.extend(textwrap.wrap(paragraph, inner))
        for text in action_lines[:4]:
            lines.append(body(text))
        lines.append(self.paint('╰' + '─' * (width-2) + '╯', 'rail'))
        # Errors retain the full log location in the final output, never truncate it.
        if self.path and self.result == 'error':
            lines += [''] + textwrap.wrap('Log: ' + self.path, width)
        # At small sizes, drop decoration first so the current action stays visible.
        if len(lines) > rows - 3 and wide:
            lines = [self.paint('couch.  /  INSTALLER', 'active')] + lines[len(self.LOGO):]
        if len(lines) > rows - 3 and self.result != 'error':
            lines = [line for line in lines if line][:max(1, rows-3)]
        return ['  ' + line for line in lines]

    def render(self):
        with self.lock:
            if self.paused:
                return
            out = self.terminal.writer
            if not self.tty:
                current = (self.step, self.detail, self.action, self.result)
                if current != self.last_plain:
                    out.write(f'[{self.step + 1}/{len(self.STEPS)}] {self.detail}\n')
                    if self.action:
                        out.write(self.action + '\n')
                    self.last_plain = current
                out.flush()
                return
            try:
                # COLUMNS/LINES can be stale after ssh, sudo or a resize.
                size = os.get_terminal_size(out.fileno())
            except (OSError, ValueError, AttributeError):
                size = shutil.get_terminal_size((80, 24))
            lines = self.frame_lines(size.columns, size.lines)
            # Paint one complete frame. Keep the cursor visible for line input.
            frame = '\n'.join(lines) + '\n\n'
            if frame != self.last_frame:
                out.write('\033[H' + '\n'.join('\033[2K' + line for line in lines) + '\n\n\033[J')
                out.flush()
                self.last_frame = frame

    def stage(self, index, detail, action=''):
        with self.lock:
            if self.log:
                self.log.write(f'Step {index + 1}: {detail}\n{action}\n')
                self.log.flush()
            self.step, self.detail, self.action = index, detail, action
            self.counter = ''
            self.render()

    def line(self, text=''):
        with self.lock:
            if self.log:
                self.log.write(text + '\n'); self.log.flush()
            self.detail = self.clean(text)
            self.counter = ''
            if text.startswith('Installation stopped:'):
                self.result = 'error'
                self.action = 'Keep USB connected. Save the log before retrying.'
            self.render()

    def write(self, value):
        """Stream for child diagnostics: log everything, display reviewed progress."""
        with self.lock:
            if self.log:
                self.log.write(value); self.log.flush()
            self.pending += value
            while '\n' in self.pending:
                line, self.pending = self.pending.split('\n', 1)
                self.consume(line)
            if len(self.pending) > 131072:
                self.pending = ''
        return len(value)

    def flush(self):
        if self.log:
            self.log.flush()

    def consume(self, line):
        line = line.strip()
        # These bootstrap operations precede the byte-counter callbacks. Do
        # not leave the power-button instruction on screen once USB connects.
        statuses = {
            'DALegacy - Successfully uploaded stage 2': 'USB connected. Checking device storage.',
            'DALegacy - Connected to stage2': 'USB connected. Checking device storage.',
        }
        detail = statuses.get(line)
        for prefix, label in (
                ('Backing up identity partition: ', 'Saving calibration: '),
                ('Verifying independent readback: ', 'Verifying saved calibration: '),
                ('Verifying runtime baseline: ', 'Checking calibration: ')):
            if line.startswith(prefix):
                name = line[len(prefix):]
                if name in {'nvdata', 'nvram', 'proinfo', 'protect1', 'protect2'}:
                    detail = label + name
        if self.step == 1 and detail:
            self.detail = detail
            self.action = 'Keep USB connected. The screen can stay off during this step.'
            self.counter = ''
            self.render()
            return
        match = PhaseProgress.PATTERN.fullmatch(line.strip())
        if not match:
            return
        phase, name, done, total = match.groups()
        done, total = int(done), int(total)
        if not 0 <= done <= total or total == 0:
            return
        if self.step == 1:
            self.action = 'Keep USB connected. The screen can stay off during this step.'
        key = phase, name, total
        if key != self.key or done < self.previous:
            self.key, self.started, self.initial = key, self.clock(), done
        self.previous = done
        elapsed = self.clock() - self.started
        rate = (done - self.initial) / elapsed if elapsed > 0 else 0
        self.detail = f'{phase}: {name}'
        fraction = done / total
        bar = '#' * int(fraction * 16) + '-' * (16 - int(fraction * 16))
        self.measurement = (done, total, rate)
        self.counter = f'[{bar}] {fraction:.0%}  {done / 1048576:.1f}/{total / 1048576:.1f} MiB'
        if rate > 0:
            self.counter += f'  {rate / 1048576:.1f} MiB/s'
        # Animation refreshes interactive counters; plain output emits each operation once.
        if not self.tty:
            self.render()

    def progress(self, line):
        with self.lock:
            if line.startswith('Write:') and self.step < 4:
                self.stage(4, getattr(self, 'write_detail', 'Writing Couch and verifying storage.'), 'Keep the remote powered and connected.')
            if PhaseProgress.PATTERN.fullmatch(line):
                self.write(line + '\n')
            else:
                self.line(line)

    def ask(self, prompt, strip=True):
        with self.lock:
            self.prompting = True
            self.last_frame = None
            self.render()
            self.paused = True
            self.terminal.writer.write(self.paint('  › ', 'active') + self.clean(prompt))
            self.terminal.writer.flush()
        try:
            answer = self.reader.readline()
            if not answer:
                raise EOFError
            return answer.strip() if strip else answer.rstrip('\r\n')
        finally:
            with self.lock:
                self.paused = False
                self.prompting = False

    def choose(self, prompt, options):
        # Text fallback for existing simulation/tests; the packaged UI uses menus.
        return self.ask(prompt + '\n' + ' / '.join(o['value'] or 'cancel' for o in options) + ': ')

    def secret(self, read):
        with self.lock:
            self.prompting = True
            self.last_frame = None
            self.render()
            self.paused = True
        try:
            return read('Wi-Fi password (hidden): ')
        finally:
            with self.lock:
                self.paused = False
                self.prompting = False

    def close(self):
        self.stop.set()
        if self.worker:
            self.worker.join(timeout=1)
        self.render()
