#!/usr/bin/env python3
"""Use an explicitly configured USB serial host (SER_HOST=local for a local cable)."""
import os, sys, termios, time, select, glob, fcntl, re, shlex, subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def local_settings():
    values = {}
    path = Path(os.environ.get('COUCH_LOCAL_CONFIG', ROOT / 'local.env'))
    if not path.is_file():
        return values
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        match = re.fullmatch(r'(?:export[ \t]+)?([A-Za-z_][A-Za-z0-9_]*)[ \t]*=(.*)', line)
        if not match:
            continue
        key, raw_value = match.groups()
        if key not in {'SER_HOST', 'SER_PORT', 'SER_SETTLE', 'SER_TIMEOUT'}:
            continue
        try:
            parsed = shlex.split(raw_value, comments=True, posix=True)
        except ValueError as error:
            raise ValueError(f'invalid {key} in {path}: {error}') from error
        if len(parsed) != 1:
            raise ValueError(f'{key} in {path} must be one quoted or unquoted value')
        values[key] = parsed[0]
    return values

def port(settings=None):
    explicit = os.environ.get("SER_PORT") or (settings or {}).get("SER_PORT")
    if explicit:
        return explicit
    p = sorted(glob.glob("/dev/serial/by-id/usb-Android_Android_*-if00"))
    if not p:
        p = sorted(glob.glob("/dev/ttyACM*") + glob.glob("/dev/cu.usbmodem*"))
    if len(p) != 1:
        sys.exit(f"expected one serial remote, found {len(p)}; set SER_PORT explicitly")
    return p[0]

# Commands that sleep go quiet mid-run, so the idle "settle" window has to be
# longer than the longest sleep or we cut the reply off. SER_SETTLE raises it.
def run(cmd, settle=None, timeout=None):
    settings = local_settings()
    settle = float(settle if settle is not None else os.environ.get("SER_SETTLE", settings.get("SER_SETTLE", 1.5)))
    timeout = float(timeout if timeout is not None else os.environ.get("SER_TIMEOUT", settings.get("SER_TIMEOUT", 6.0)))
    host = os.environ.get("SER_HOST", settings.get("SER_HOST", "local"))
    if host != "local":
        env = ["SER_HOST=local", f"SER_SETTLE={settle}", f"SER_TIMEOUT={timeout}"]
        serial_port = os.environ.get("SER_PORT", settings.get("SER_PORT"))
        if serial_port:
            env.append(f"SER_PORT={serial_port}")
        remote = shlex.join(["env", *env, "python3", "-", cmd])
        result = subprocess.run(
            ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, remote],
            input=Path(__file__).read_text(), text=True, capture_output=True, check=True,
        )
        return result.stdout
    fd = os.open(port(settings), os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        # Lock the actual device inode, including when callers use a by-id alias.
        # Another sercmd reader must not consume this command's response.
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        a = termios.tcgetattr(fd)
        a[0] = a[1] = a[3] = 0                       # iflag oflag lflag: raw
        a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
        a[4] = a[5] = termios.B115200
        a[6][termios.VMIN] = 0
        a[6][termios.VTIME] = 0
        termios.tcsetattr(fd, termios.TCSANOW, a)
        termios.tcflush(fd, termios.TCIOFLUSH)

        os.write(fd, (cmd + "\n").encode())
        out, deadline, last = b"", time.time() + timeout, time.time()
        while time.time() < deadline:
            r, _, _ = select.select([fd], [], [], 0.2)
            if r:
                try:
                    chunk = os.read(fd, 4096)
                except BlockingIOError:
                    continue
                if chunk:
                    out += chunk
                    last = time.time()
            elif out and time.time() - last > settle:
                break
        return out.decode("utf-8", "replace")
    finally:
        os.close(fd)

if __name__ == "__main__":
    print(run(" ".join(sys.argv[1:]) if len(sys.argv) > 1 else "uname -a"), end="")
