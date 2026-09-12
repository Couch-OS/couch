"""JSON-pipe adapter for the Ratatui host UI; existing policy owns all device I/O."""
import argparse
from contextlib import redirect_stdout, redirect_stderr
import json
import os
from pathlib import Path
import sys
from types import SimpleNamespace

from wifi_view import WifiView
import wifi_trial


class BackendView(WifiView):
    def __init__(self, incoming, outgoing):
        super().__init__(SimpleNamespace(reader=incoming, writer=outgoing))
        self.incoming, self.outgoing = incoming, outgoing
        self.tty = False
        self.prompt_id = 0

    def emit(self, event):
        self.outgoing.write(json.dumps(event, ensure_ascii=True, allow_nan=False) + '\n')
        self.outgoing.flush()

    def render(self):
        if not hasattr(self, 'outgoing'):
            return
        self.emit({'event': 'state', 'step': self.step, 'steps': list(self.STEPS),
                   'detail': self.clean(self.detail), 'action': self.clean(self.action),
                   'result': self.result, 'log_path': self.path,
                   'measurement': list(self.measurement) if self.counter and self.measurement else None})

    def prompt(self, title, kind, options=None, strip=True):
        self.prompt_id += 1
        self.render()
        self.emit({'event': 'prompt', 'id': self.prompt_id, 'title': title,
                   'kind': kind, 'options': options or []})
        raw = self.incoming.readline(16385)
        if not raw or len(raw) > 16384:
            raise EOFError('Installer interface disconnected')
        response = json.loads(raw)
        if response == {'id': self.prompt_id, 'cancel': True}:
            raise KeyboardInterrupt
        if (set(response) != {'id', 'value'} or response['id'] != self.prompt_id
                or not isinstance(response['value'], str)):
            raise ValueError('Unexpected installer interface response')
        value = response['value']
        if kind == 'choice' and value not in {o['value'] for o in options}:
            raise ValueError('Invalid installer selection')
        # Input values, particularly credentials, never go through line()/log().
        return value.strip() if strip else value

    def ask(self, prompt, strip=True):
        return self.prompt(prompt, 'text', strip=strip)

    def choose(self, prompt, options):
        return self.prompt(prompt, 'choice', options=options)

    def secret(self, _read):
        return self.prompt('Wi-Fi password', 'password', strip=False)

    def close(self):
        self.render()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--config', type=Path, required=True)
    protocol = parser.add_mutually_exclusive_group(required=True)
    protocol.add_argument('--events-fd', type=int)
    protocol.add_argument('--events-stdio', action='store_true')
    parser.add_argument('--wifi-retry-from', type=Path)
    parser.add_argument('--wifi-restore-from', type=Path)
    args = parser.parse_args()
    # Retain private pipes before redirecting native stdout/stderr. Windows
    # cannot inherit a Unix socket descriptor; its anonymous pipes provide the
    # same separation between protocol traffic and native tool output.
    if args.events_stdio:
        incoming_fd, outgoing_fd = os.dup(0), os.dup(1)
        with open(os.devnull, 'w') as null:
            os.dup2(null.fileno(), 1); os.dup2(null.fileno(), 2)
        if os.name == 'nt':
            import signal
            def interrupted(_signal, _frame):
                raise KeyboardInterrupt
            signal.signal(signal.SIGBREAK, interrupted)
    else:
        incoming_fd, outgoing_fd = os.dup(args.events_fd), os.dup(args.events_fd)
    with os.fdopen(outgoing_fd, 'w', buffering=1) as events, \
            os.fdopen(incoming_fd, 'r') as incoming:
        view = BackendView(incoming, events)
        if args.wifi_restore_from:
            view.STEPS = ('Prepare', 'USB startup', 'Wi-Fi', 'Saved originals', 'Restore Android', 'Android boot')
            view.write_detail = 'Restoring Android and verifying storage.'
        code = 1
        with redirect_stdout(view), redirect_stderr(view):
            try:
                code = wifi_trial.run_wizard(view, args.config, args.wifi_retry_from, args.wifi_restore_from)
            except KeyboardInterrupt:
                view.result = 'error'; view.line('Stopped by you. Keep the saved originals and log.')
                code = 130
            except Exception:
                # Never render exception arguments from a credential or transport library.
                view.result = 'error'; view.line('Installer stopped. Check the saved log and package prerequisites.')
        view.emit({'event': 'finished', 'code': code})
        return code


if __name__ == '__main__':
    raise SystemExit(main())
