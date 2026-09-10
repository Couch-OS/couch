#!/usr/bin/env python3
"""Terminal front end for Couch's existing installer policy (Linux first).

No hardware-write implementation lives here. Simulation must be explicitly
selected; an unavailable physical adapter never falls back to simulation.
"""
import argparse
from contextlib import redirect_stdout
from dataclasses import dataclass
from pathlib import Path
import sys
import signal
import re
import time
import subprocess
from types import SimpleNamespace

import couch_install as core

LOGO = r"""
                         _
   ___ ___  _   _  ___ | |__
  / __/ _ \| | | |/ __|| '_ \
 | (_| (_) | |_| | (__ | | | | _
  \___\___/ \__,_|\___||_| |_|(_)
"""


@dataclass(frozen=True)
class Plan:
    target: str
    release: str
    backups: tuple[str, ...]
    writes: tuple[str, ...]
    backup_dir: Path
    simulation: bool
    operation: str = "install"
    resume: bool = False
    boot_after: bool = False


class CoreAdapter:
    """Small boundary to the reviewed policy engine; never duplicates its checks."""
    def __init__(self, args):
        self.args = args
        self.prepared = None

    def plan(self):
        if not self.args.simulation:
            raise core.InstallError("Physical installation is not approved yet. USB observation is available; no remote will be flashed.")
        if not all((self.args.manifest, self.args.device_dir, self.args.identity, self.args.backup_dir)):
            raise core.InstallError("Simulation requires --manifest, --device-dir, --identity and --backup-dir.")
        release, bundle = core.load_release(self.args.manifest)
        device = core.FileDevice(self.args.device_dir)
        core.preflight(release, device)
        identity = core.identity_record(self.args.identity)
        self.prepared = (release, bundle, device, identity)
        return Plan(device.description['storage_id'], str(self.args.manifest),
                    tuple(sorted(core.IDENTITY_PARTITIONS | release['images'].keys())),
                    tuple(name for name in core.WRITE_ORDER if name in release['images']),
                    self.args.backup_dir, True, resume=self.args.resume)

    def apply(self, plan, confirmation, progress):
        if not plan.simulation or not self.args.simulation or self.prepared is None:
            raise core.InstallError("No reviewed simulation plan is available; physical writes remain disabled.")
        if confirmation != plan.target:
            raise core.InstallError("Target confirmation does not match")
        release, bundle, device, identity = self.prepared
        with redirect_stdout(progress):
            core.install_simulated(release, bundle, device, plan.backup_dir,
                                   identity, confirmation, self.args.resume)

    def observe(self, progress):
        with redirect_stdout(progress):
            core.watch_usb(self.args.usb_timeout)


class PhaseProgress:
    """Render only measured per-partition counters; never imply total-run progress."""
    PATTERN = re.compile(r"^(Backup|Hash readback|Write|Verify image): ([a-zA-Z0-9_]+) (\d+)/(\d+) bytes \(\d+%\)$")

    def __init__(self, output, clock=time.monotonic):
        self.output, self.clock = output, clock
        self.key = None
        self.active = False
        self.started = self.initial = self.previous = 0

    @staticmethod
    def duration(seconds):
        seconds = max(0, int(seconds))
        return f"{seconds // 3600:02}:{seconds // 60 % 60:02}:{seconds % 60:02}"

    def finish(self):
        if self.active:
            self.output.write('\n')
            self.output.flush()
            self.active = False

    def line(self, line):
        # Child logs must not inject terminal control sequences.
        clean = ''.join(c for c in line.rstrip('\r\n') if ord(c) >= 32 and ord(c) != 127)
        match = self.PATTERN.fullmatch(clean)
        if match:
            phase, name, done, total = match.groups()
            done, total = int(done), int(total)
            if total <= 0 or done > total:
                match = None
        if not match:
            # Preserve every error in full; other diagnostics share the status
            # line, avoiding a terminal full of repetitive USB/library logs.
            if any(word in clean.lower() for word in ('error', 'failed', 'failure', 'stopped', 'exception', 'traceback', 'mismatch', 'warning')):
                self.finish()
                self.output.write(clean + '\n')
            else:
                self.output.write('\r\033[2K' + clean[:160])
                self.active = True
            self.output.flush()
            return
        now = self.clock()
        key = (phase, name, total)
        if key != self.key or done < self.previous:
            self.finish()
            self.key, self.started, self.initial = key, now, done
        self.previous = done
        elapsed = max(0, now - self.started)
        rate = (done - self.initial) / elapsed if elapsed > 0 else 0
        eta = self.duration((total - done) / rate) if rate > 0 else '--:--:--'
        fraction = done / total
        bar = '#' * int(fraction * 12) + '-' * (12 - int(fraction * 12))
        self.output.write(f"\r\033[2K{phase}: {name} [{bar}] {fraction:5.1%} "
                          f"{done}/{total} bytes | {rate / 1048576:.2f} MiB/s | "
                          f"elapsed {self.duration(elapsed)} ETA {eta}")
        self.active = True
        self.output.flush()
        if done == total:
            self.finish()


class PrivateAdapter:
    """Explicit developer adapter; the public/default bootstrap never selects it."""
    private_trial = True
    PATHS = ('manifest', 'baseline', 'checkout', 'loader', 'preloader', 'backup_dir', 'lock_dir')
    REQUIRED = ('manifest', 'baseline', 'checkout', 'loader', 'preloader', 'backup_dir',
                'loader_sha256', 'preloader_sha256', 'confirm_cid_sha256', 'ports', 'bus')
    FLAGS = ('restore', 'resume', 'boot_after_install')

    def __init__(self, path, *, validate=None, launch=subprocess.Popen):
        path = core.regular(path)
        core.require(path.stat().st_mode & 0o077 == 0, "Private trial configuration must have permissions 0600 or stricter")
        value = core.read_json(path)
        allowed = set(self.REQUIRED) | set(self.PATHS) | set(self.FLAGS) | {'timeout'}
        core.require(isinstance(value, dict) and set(value) <= allowed and set(self.REQUIRED) <= set(value),
                     "Private trial configuration has missing or unknown fields")
        for key, item in value.items():
            core.require(not isinstance(item, str) or not any(ord(c)<32 or ord(c)==127 for c in item),
                         f"Control characters are not allowed in {key}")
        for key in self.PATHS:
            if key not in value:
                continue
            core.require(isinstance(value[key], str) and Path(value[key]).is_absolute(), f"{key} must be an absolute path")
            value[key] = Path(value[key])
        for key in self.FLAGS:
            core.require(type(value.get(key, False)) is bool, f"{key} must be true or false")
            value.setdefault(key, False)
        core.require(type(value['bus']) is int and value['bus'] > 0, "bus must be a positive integer")
        core.require(type(value.get('timeout',120)) in (int,float), "timeout must be numeric")
        for key in ('loader_sha256','preloader_sha256','confirm_cid_sha256','ports'):
            core.require(isinstance(value[key], str), f"{key} must be text")
        value.setdefault('timeout',120)
        value.setdefault('lock_dir',Path.home()/'.local/state/couch-installer/locks')
        value.update(allow_private_flash=True, check_only=False)
        self.args = SimpleNamespace(**value)
        self.validate, self.launch = validate, launch
        self.prepared = None

    def toggle_restore(self):
        self.args.restore = not self.args.restore
        self.prepared = None

    def toggle_resume(self):
        self.args.resume = not self.args.resume
        self.prepared = None

    def plan(self):
        import private_install
        # validate_inputs is metadata/file validation only. Never construct a
        # session, enumerate USB, or call private_install.run during planning.
        release, _, _, _ = (self.validate or private_install.validate_inputs)(self.args)
        order = ('userdata','logo','odmdtbo','boot','recovery') if self.args.restore else core.WRITE_ORDER
        self.prepared = Plan(self.args.confirm_cid_sha256, str(self.args.manifest),
            tuple(sorted(core.IDENTITY_PARTITIONS | release['images'].keys())),
            tuple(name for name in order if name in release['images']), self.args.backup_dir,
            False, 'restore' if self.args.restore else 'install', self.args.resume, self.args.boot_after_install)
        return self.prepared

    def command(self):
        argv = [sys.executable, str(Path(__file__).with_name('private_install.py')), '--allow-private-flash']
        for key in self.PATHS + ('loader_sha256','preloader_sha256','confirm_cid_sha256','ports','bus','timeout'):
            argv.extend(('--'+key.replace('_','-'), str(getattr(self.args,key))))
        argv.extend('--'+key.replace('_','-') for key in self.FLAGS if getattr(self.args,key))
        return argv

    @staticmethod
    def stop_process(process):
        if process.poll() is not None:
            return
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    def apply(self, plan, confirmation, progress):
        core.require(self.prepared is not None and plan == self.prepared and confirmation == plan.target,
                     "Review the private plan and enter its exact target CID hash before continuing")
        self.prepared = None  # One confirmation starts at most one process.
        process = self.launch(self.command(), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True, bufsize=1, start_new_session=True)
        display = PhaseProgress(progress)
        try:
            for line in process.stdout:
                display.line(line)
            code = process.wait()
            core.require(code == 0, f"Private {plan.operation} stopped (exit {code}). Keep the originals and journal; review the error above before retrying.")
        except BaseException:
            self.stop_process(process)
            raise
        finally:
            display.finish()
            process.stdout.close()

    def observe(self, progress):
        with redirect_stdout(progress):
            core.watch_usb(15)


class Terminal:
    def __init__(self, reader, writer):
        self.reader, self.writer = reader, writer

    def line(self, text=""):
        self.writer.write(text + '\n')
        self.writer.flush()

    def ask(self, prompt):
        self.writer.write(prompt)
        self.writer.flush()
        answer = self.reader.readline()
        if not answer:
            raise EOFError
        return answer.strip()

    def browse_releases(self):
        from release_discovery import discover, inspect_manifest
        channel = self.ask("Release channel [stable/alpha, default stable]: ") or 'stable'
        version = self.ask("Exact version (for example v0.1.0-alpha.1), or Enter to browse: ") or None
        choices = discover(channel, version)
        if not choices:
            self.line("No published releases with verified manifest metadata match this selection.")
            return
        for index, item in enumerate(choices, 1):
            self.line(f"  {index}  {item.tag} · {item.channel}")
        answer = self.ask("Choose a release number to inspect its metadata, or Enter to cancel: ")
        if not answer:
            return
        if not answer.isdecimal() or not 1 <= int(answer) <= len(choices):
            raise core.InstallError("Choose a listed release number")
        selected = choices[int(answer)-1]
        metadata = inspect_manifest(selected)
        self.release_selection = selected
        self.line(f"Pinned selection: {selected.tag} · manifest SHA-256 {selected.sha256}")
        self.line(f"Manifest lists {len(metadata['files'])} files; publisher installable flag: {metadata['installable']}.")
        self.line("Only release metadata was downloaded. No installer or OS payload was downloaded or executed.")
        self.line("GitHub checksums identify these bytes; they are not an independently verified publisher signature.")
        self.line("Planned installation uses WiFi for image transfer and USB for bootstrap/recovery. Public flashing remains disabled.")

    def run(self, adapter, simulation=False):
        self.line(LOGO)
        self.line("Couch installer · Linux")
        private = getattr(adapter, "private_trial", False)
        self.line("PRIVATE DEVELOPER TRIAL — not a public installer" if private else "SIMULATION — regular files only" if simulation else "Physical installation is not approved yet.")
        while True:
            self.line()
            self.line("  1  Review " + ("restore" if adapter.args.restore else "installation") + " trial plan" if private else "  1  Review installation plan" if simulation else "  1  Check installation availability")
            self.line("  2  Observe USB connection (descriptors only)")
            if private:
                self.line("  3  Switch install / restore")
                self.line("  4  Resume existing journal: " + ("YES" if adapter.args.resume else "NO"))
            if not private and not simulation:
                self.line("  r  Browse GitHub releases (metadata only)")
            self.line("  q  Quit")
            try:
                choice = self.ask("Choose [1/2/3/4/q]: " if private else "Choose [1/2/q]: " if simulation else "Choose [1/2/r/q]: ").lower()
                if private and choice in ("3","4"):
                    (adapter.toggle_restore if choice == "3" else adapter.toggle_resume)()
                    continue
                if choice in ('q', 'quit'):
                    return 0
                if choice == 'r' and not private and not simulation:
                    self.browse_releases()
                    continue
                if choice == '2':
                    self.line("Connect the remote by USB and reboot it. Ctrl-C stops observation.")
                    adapter.observe(self.writer)
                    continue
                if choice != '1':
                    self.line("Choose a listed option.")
                    continue
                self.line("Verifying release files and planning inputs; this may take a moment. No USB session is opened.")
                plan = adapter.plan()
                self.line("\nReview this plan")
                self.line("Mode: " + ("SIMULATION — no physical device" if plan.simulation else "PHYSICAL DEVICE"))
                self.line("Operation: " + plan.operation + (" · resume existing journal" if plan.resume else " · start new transaction"))
                self.line("Target CID SHA-256: " + plan.target if private else "Target: " + plan.target)
                self.line("Release: " + plan.release)
                self.line(("Verify retained originals: " if plan.operation == "restore" else "Back up: ") + ', '.join(plan.backups))
                self.line("Write and verify: " + ' -> '.join(plan.writes))
                self.line("Originals and journal: " + str(plan.backup_dir))
                self.line("Other partitions are preserved. " + ("Request boot only after verified readback; side Power may still be needed." if plan.boot_after else "No automatic reboot."))
                typed = self.ask("Type the exact target ID to continue, or Enter to cancel: ")
                if typed != plan.target:
                    self.line("Cancelled. Nothing was written.")
                    continue
                self.line("\nRunning the reviewed plan. Progress below comes from the policy engine.")
                adapter.apply(plan, typed, self.writer)
                self.line("Simulation completed. Keep the originals and journal." if plan.simulation else f"Private {plan.operation} readback completed. Normal startup remains unverified; keep the originals and journal.")
            except KeyboardInterrupt:
                self.line("\nInterrupted. Retain any originals and journal; do not delete recovery information.")
                return 130
            except EOFError:
                self.line("\nInput closed; exiting without further actions.")
                return 0
            except (core.InstallError, OSError, ValueError, KeyError, TypeError) as error:
                self.line("Stopped: " + str(error))


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument('--private-trial', type=Path, help='Developer-only private trial configuration (0600); public installation stays disabled')
    result.add_argument('--simulation', action='store_true', help='Explicitly use regular-file simulated device')
    result.add_argument('--manifest', type=Path)
    result.add_argument('--device-dir', type=Path)
    result.add_argument('--identity', type=Path)
    result.add_argument('--backup-dir', type=Path)
    result.add_argument('--resume', action='store_true')
    result.add_argument('--usb-timeout', type=float, default=15)
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    if args.private_trial and (args.simulation or args.manifest or args.device_dir or args.identity or args.backup_dir or args.resume):
        print("Use the private configuration file for all private-trial options; do not mix simulation options.", file=sys.stderr)
        return 2
    if not sys.platform.startswith('linux'):
        print("The installer terminal is currently supported on Linux only.", file=sys.stderr)
        return 2
    reader = sys.stdin
    opened = None
    try:
        if not reader.isatty():
            opened = open('/dev/tty', 'r', encoding='utf-8')
            reader = opened
        if not sys.stdout.isatty():
            raise OSError("Interactive output needs a terminal")
        try:
            adapter = PrivateAdapter(args.private_trial) if args.private_trial else CoreAdapter(args)
        except OSError as error:
            raise core.InstallError(f'Cannot read private trial configuration: {error}') from error
        return Terminal(reader, sys.stdout).run(adapter, args.simulation)
    except (core.InstallError, ValueError, KeyError, TypeError) as error:
        print("Private trial configuration rejected: " + str(error), file=sys.stderr)
        return 2
    except OSError:
        print("An interactive terminal is required. Run from a terminal; for automation use couch_install.py plan/simulate.", file=sys.stderr)
        return 2
    finally:
        if opened is not None:
            opened.close()


if __name__ == '__main__':
    sys.exit(main())
