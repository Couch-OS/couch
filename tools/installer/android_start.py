"""Reboot only the authorized Android device on the selected USB port."""
import hashlib
from pathlib import Path
import re
import shutil
import subprocess

from couch_install import require


def reboot_selected_android(args, terminal, *, sysfs=Path('/sys/bus/usb/devices'), run=subprocess.run):
    adb = shutil.which('adb')
    if not adb:
        return False
    port = f'{args.bus}-{args.ports}'
    try:
        serial = (sysfs / port / 'serial').read_text().strip()
    except OSError:
        return False
    if not serial or not re.fullmatch(r'[A-Za-z0-9._:-]+', serial):
        return False
    def command(*words):
        return run([adb, *words], capture_output=True, text=True, timeout=15)
    try:
        listing = command('devices')
        authorized = any(line.split() == [serial, 'device'] for line in listing.stdout.splitlines())
        if listing.returncode or not authorized:
            return False
        cid = command('-s', serial, 'shell', 'cat', '/sys/block/mmcblk0/device/cid')
        value = cid.stdout.strip()
        if cid.returncode or not re.fullmatch(r'[0-9a-fA-F]{32}', value):
            return False
    except (OSError, subprocess.TimeoutExpired):
        return False
    # The stock baseline pins Linux's canonical CID, not the DA wire encoding.
    require(hashlib.sha256(bytes.fromhex(value)).hexdigest() == args.confirm_cid_sha256,
            'Android on the selected USB port does not match the pinned remote; no reboot sent')
    terminal.stage(1, 'Restarting Android automatically.',
                   'USB startup is ready. Keep the remote connected; no button press is needed.')
    # Do not retry an ambiguous reboot result: it may already have disconnected.
    result = command('-s', serial, 'reboot')
    require(result.returncode == 0, 'ADB reboot was not acknowledged. Check the remote before retrying.')
    return True
