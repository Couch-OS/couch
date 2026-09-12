#!/usr/bin/env python3
"""Operator-driven private Android -> RAM -> Wi-Fi OS installation wizard."""
from contextlib import redirect_stdout, redirect_stderr
import datetime
import getpass
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import warnings

import bootstrap_linux_stage as bootstrap
import couch_install as core

STAGE = Path(__file__).with_name('linux_stage')
sys.path.insert(0, str(STAGE))
import wifi_benchmark as wifi
import wifi_install as install

PATHS = ('manifest', 'baseline', 'stock_manifest', 'recovery', 'stage_image', 'stage_metadata',
         'checkout', 'loader', 'preloader')
PINS = ('manifest_sha256', 'stock_manifest_sha256', 'recovery_sha256', 'stage_sha256',
        'stage_metadata_sha256', 'confirm_cid_sha256', 'loader_sha256', 'preloader_sha256')


def load_config(path):
    path = core.regular(path).resolve()
    value = core.read_json(path)
    core.require(set(value) == {'schema', 'paths', 'pins', 'bus', 'ports'} and value['schema'] == 1,
                 'Unsupported private Wi-Fi trial configuration')
    core.require(set(value['paths']) == set(PATHS) and set(value['pins']) == set(PINS),
                 'Incomplete private Wi-Fi package')
    args = {}
    for key, name in value['paths'].items():
        core.require(isinstance(name, str) and not Path(name).is_absolute() and '..' not in Path(name).parts,
                     'Package input must be a relative path')
        args[key] = path.parent / name
    for key, checksum in value['pins'].items():
        core.require(isinstance(checksum, str) and len(checksum) == 64
                     and all(c in '0123456789abcdef' for c in checksum), 'Invalid package checksum')
        args[key] = checksum
    core.require(type(value['bus']) is int and value['bus'] > 0
                 and isinstance(value['ports'], str)
                 and all(p.isdecimal() and int(p)>0 for p in value['ports'].split('.')), 'Invalid USB topology')
    args.update(bus=value['bus'], ports=value['ports'], timeout=600,
                allow_private_flash=True, allow_private_wifi_install=True, stock_boot_confirmed=True,
                boot_after_capture=True, check_only=False)
    return args


def wait_stage(args, terminal, timeout=120):
    import usb.core
    ports = tuple(int(p) for p in args.ports.split('.'))
    terminal.stage(1, 'Starting the installer on your remote.',
                   'The remote is restarting automatically. Keep USB connected.')
    deadline = time.monotonic() + timeout
    while True:
        matches = [d for d in usb.core.find(find_all=True, idVendor=0x0e8d, idProduct=0x201c)
                   if d.bus == args.bus and tuple(d.port_numbers or ()) == ports]
        if len(matches) == 1:
            return
        core.require(len(matches) <= 1, 'Multiple devices matched the selected USB port')
        if time.monotonic() >= deadline:
            terminal.stage(1, 'The installer USB connection has not appeared yet.',
                           'If the screen is off, hold side Power until it turns on, then release. You can keep waiting without repeating backups.')
            if terminal.ask('Press Enter to keep waiting, or type stop: ').lower() == 'stop':
                raise core.InstallError('Stopped waiting for installer USB. Original backups are preserved.')
            deadline = time.monotonic() + timeout
        time.sleep(1)



def network_password(terminal, security):
    with warnings.catch_warnings():
        warnings.simplefilter('error', getpass.GetPassWarning)
        return None if security == 'open' else terminal.secret(getpass.getpass)


def network_form(terminal):
    while True:
        ssid = terminal.ask('Wi-Fi network name (SSID)', strip=False)
        security = terminal.choose('Wi-Fi security', [
            {'value': 'wpa2', 'label': 'WPA2 Personal', 'detail': 'Password-protected home network.'},
            {'value': 'open', 'label': 'Open network', 'detail': 'No Wi-Fi password.'}])
        core.require(security in ('wpa2', 'open'), 'Choose WPA2 or open')
        password = network_password(terminal, security)
        try:
            network = wifi.credentials(ssid, password)
        except ValueError as error:
            terminal.stage(2, str(error), 'Correct the network details below. The USB startup does not need to be repeated.')
            del ssid, password
            continue
        del ssid, password
        return network

def run(terminal, config, retry_from=None, restore_from=None):
    from wifi_view import WifiView
    view = WifiView(terminal)
    if restore_from is not None:
        view.STEPS = ('Prepare', 'USB startup', 'Wi-Fi', 'Saved originals', 'Restore Android', 'Android boot')
        view.write_detail = 'Restoring Android and verifying storage.'
    view.start()
    try:
        return run_wizard(view, config, retry_from, restore_from)
    finally:
        view.close()


def run_wizard(terminal, config, retry_from=None, restore_from=None):
    core.require(not (retry_from and restore_from), 'Choose restart or Android restore, not both')
    restoring = restore_from is not None
    retry_from = restore_from if restoring else retry_from
    os_name = 'Android' if restoring else 'Couch'
    backup_os = 'Android'
    args_dict = load_config(config)
    terminal.stage(0, 'Checking saved originals for restart.' if retry_from else 'Start with Android running and USB connected.',
                   'You will enter Wi-Fi credentials here. Wi-Fi carries the backups and OS images.')
    for command in ('git', 'openssl', 'debugfs', 'e2fsck', 'resize2fs'):
        core.require(shutil.which(command) is not None, f'Required host tool is missing: {command}')
    core.require(os.geteuid() == 0, 'USB installation needs root; use the supplied launcher, which invokes sudo.')
    root = Path(os.environ.get('COUCH_INSTALL_STATE', str(Path.home() / '.local/state/couch-installer')))
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    run_dir = root / ('wifi-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + secrets.token_hex(4))
    run_dir.mkdir(mode=0o700)
    args = SimpleNamespace(**args_dict, backup_dir=run_dir/'bootstrap', lock_dir=root/'locks')
    terminal.line('Checking package checksums and available backup space...')
    core.require(core.digest(args.manifest) == args.manifest_sha256, 'Release manifest checksum mismatch')
    release, bundle = core.load_release(args.manifest)
    core.require(release.get('private_candidate') is True and release.get('installable') is False,
                 'Expected private trial release')
    bootstrap.validate(args)
    prior = None
    if retry_from is not None:
        import wifi_retry
        prior = wifi_retry.validate(retry_from, release, terminal.line)
        backup_os = prior['original_os']
        if restoring:
            wifi_retry.require_android_restore(prior)
    terminal.path = str(run_dir/'installer.log')
    if prior is not None:
        terminal.stage(0, 'Restore Android from your verified originals.' if restoring else f'Restart using your verified original {backup_os} backups.',
                       'Keep the remote connected. Enter Wi-Fi credentials again after USB startup.')
        action = 'RESTORE' if restoring else 'RESTART'
        if terminal.choose('Restore Android?' if restoring else 'Restart installation?', [
                {'value': action, 'label': 'Restore Android' if restoring else 'Restart installation',
                 'detail': 'Use your verified original backups; keep USB connected.'},
                {'value': '', 'label': 'Cancel', 'detail': 'Leave the remote as it is.'}]) != action:
            terminal.result = 'ok'; terminal.path = ''; terminal.action = 'The remote is unchanged.'
            terminal.line('Cancelled. No USB session started.'); return 0
        skip_userdata = False
    else:
        terminal.stage(0, 'Install Couch after saving and verifying your Android backups.',
                       '1. Enable USB debugging in Android.\n'
                       '2. Connect your remote to this computer via USB.\n'
                       '3. Approve "Allow USB debugging?" on the remote.')
        choice = terminal.choose('Choose your backup mode', [
            {'value': 'INSTALL', 'label': 'Full Android backup', 'detail': 'Recommended · save Android apps, data and recovery originals.'},
            {'value': 'YOLO', 'label': 'YOLO · skip Android data backup', 'detail': 'Keep recovery and calibration backups; Android data cannot be restored.'},
            {'value': 'REINSTALL', 'label': 'Reinstall existing Couch', 'detail': 'Replace Couch and reset its settings. Existing Android backups stay untouched.'},
            {'value': '', 'label': 'Cancel', 'detail': 'No device changes.'}])
        if choice == 'REINSTALL':
            backup_os = 'Couch'
            args.reinstall_couch = True
            terminal.stage(0, 'Reinstall Couch and reset its settings.',
                           'Connect the remote via USB. These backups contain current Couch, not Android.\n'
                           'Keep any earlier Android backups on their original computer.')
            choice = terminal.choose('Back up current Couch data?', [
                {'value': 'INSTALL', 'label': 'Full Couch backup', 'detail': 'Save the currently installed OS and its settings before replacement.'},
                {'value': 'YOLO', 'label': 'YOLO · skip Couch data backup', 'detail': 'Settings and data cannot be recovered from this run.'},
                {'value': '', 'label': 'Cancel', 'detail': 'No device changes.'}])
        if choice not in ('INSTALL', 'YOLO'):
            terminal.result = 'ok'
            terminal.action = 'The remote is unchanged.'
            terminal.path = ''
            terminal.line('Cancelled. No USB session started.')
            return 0
        skip_userdata = choice == 'YOLO'
        if skip_userdata:
            terminal.stage(0, f'YOLO skips the backup of {backup_os} apps and personal data.',
                           'Boot, recovery, logo and calibration are still backed up.\n'
                           f'{backup_os} apps and personal data cannot be restored from these backups.')
            if terminal.choose(f'Continue without a {backup_os} data backup?', [
                    {'value': '', 'label': 'Cancel', 'detail': 'Go back and keep the remote unchanged.'},
                    {'value': 'YOLO', 'label': 'Continue with YOLO', 'detail': f'{backup_os} apps and personal data will not be backed up.'}]) != 'YOLO':
                terminal.result = 'ok'
                terminal.path = ''; terminal.action = 'The remote is unchanged.'
                terminal.line('Cancelled. No USB session started.')
                return 0
    selected = (set(release['images']) | core.IDENTITY_PARTITIONS) - ({'userdata'} if skip_userdata else set())
    needed = sum(release['partitions'][n]['size'] for n in selected)
    needed += sum(release['partitions'][n]['size'] for n in release['images'])
    core.require(shutil.disk_usage(root).free >= needed + 256*1024*1024,
                 'Insufficient disk space for selected backups and private OS image')
    with (run_dir/'installer.log').open('x') as log:
        os.chmod(run_dir/'installer.log',0o600)
        terminal.log = log
        try:
            terminal.line('Backup mode: ' + (f'YOLO ({backup_os} userdata backup skipped)' if skip_userdata else 'full'))
            terminal.stage(1, 'Preparing USB startup.', 'Keep USB connected.')
            def start_android():
                from android_start import reboot_selected_android
                if not reboot_selected_android(args, terminal):
                    terminal.stage(1, 'Waiting for the remote on USB.',
                                   'Hold side Power until the screen goes off, then release. Keep USB connected.')
            with redirect_stdout(terminal.writer), redirect_stderr(terminal.writer):
                if prior is None:
                    bootstrap.run(args, before_wait=start_android)
                else:
                    terminal.stage(1, 'Waiting to restart the RAM installer.',
                                   'Hold side Power until the screen goes off, then release. Keep USB connected.')
                    wifi_retry.bootstrap_stage(args, prior)
            # Preserve a complete command for undoing only this temporary boot.
            rollback = [sys.executable, str(Path(bootstrap.__file__).resolve())]
            for name in PATHS[1:] + PINS[1:] + ('bus','ports','timeout','lock_dir'):
                rollback.extend(('--'+name.replace('_','-'),str(getattr(args,name))))
            rollback.extend(['--backup-dir',str(run_dir/'rollback'), '--allow-private-flash',
                '--allow-private-wifi-install','--stock-boot-confirmed','--boot-after-capture',
                '--restore-boot-from',str(args.backup_dir),'--restore-receipt-sha256',
                core.digest(args.backup_dir/'backup-receipt.json')])
            if prior is None:
                install.save(run_dir/'restore-original-boot.json',rollback)
            else:
                install.save(run_dir/'original-backups.json', {'source': str(prior['directory'])})
            wait_stage(args, terminal)
            usb_args = SimpleNamespace(bus=args.bus,ports=args.ports,vid=0x0e8d,pid=0x201c)
            with wifi.usb_probe(usb_args) as (out,incoming):
                terminal.stage(2, 'Checking Wi-Fi hardware...')
                wifi.wait_ready(out,incoming)
                terminal.stage(2, 'Wi-Fi is ready.', 'Enter the Wi-Fi network for this transfer.' if restoring else 'Enter the network you want Couch to use.')
                from wifi_scan import network_form as scanned_network_form
                network = scanned_network_form(terminal, out, incoming)
                plan,paths=install.make_plan(release,bundle,args.backup_dir,args.stage_sha256,network,
                                             run_dir/'private-image',terminal.line, skip_userdata_backup=skip_userdata,
                                             reused_backups=prior['journal']['backups'] if prior else None,
                                             restore_originals=prior['directory']/'originals' if restoring else None)
                install.bind_plan(out,incoming,plan)
                with tempfile.TemporaryDirectory(prefix='couch-tls-') as temporary:
                    pem,identity=wifi.ephemeral_identity(temporary)
                    terminal.line('Connecting the remote to Wi-Fi...')
                    address=wifi.provision(out,incoming,{**network,**identity})
                    del network
            # Release USB before the long TLS transaction and final reboot.
            # Otherwise normal device disconnect can masquerade as failed cleanup.
            terminal.stage(3, 'Wi-Fi connected. Saving and verifying ' +
                           (f'recovery backups (YOLO skips {backup_os} data).' if skip_userdata else f'{backup_os} backups.'),
                           'Keep the remote powered and connected.')
            progress = terminal.progress
            def reboot():
                terminal.stage(5, f'{os_name} partition writes verified.',
                               f'Recovery backups saved; {backup_os} data was not backed up.' if skip_userdata
                               else f'Your {backup_os} backups are saved on this computer.')
                return terminal.choose(f'Boot {os_name}?', [
                    {'value': 'boot', 'label': f'Boot {os_name}', 'detail': 'All partition writes and checks completed.'},
                    {'value': 'stay', 'label': 'Stay in the installer', 'detail': 'Keep the remote available for diagnosis.'}]) == 'boot'
            with install.connect(address,pem,identity) as stream:
                install.transaction(stream,plan,paths,args.backup_dir,run_dir/'originals',progress,reboot,
                                    reuse_from=prior['directory']/'originals' if prior else None)
            terminal.stage(5, f'Waiting for the {os_name} home or setup screen.',
                           'If the screen stays off, hold the side Power button until it turns on, then release it.')
            answer=terminal.choose(f'Has the {os_name} home/setup screen appeared?', [
                {'value': 'yes', 'label': 'Yes, the home/setup screen is visible', 'detail': 'Confirm the device booted successfully.'},
                {'value': 'no', 'label': 'No, it has not appeared', 'detail': 'Keep USB connected for diagnosis.'}])
            install.save(run_dir/'first-boot.json',{'operator_confirmed':answer=='yes','automatic_verification':False,'os':os_name})
            terminal.result = 'ok' if answer == 'yes' else 'error'
            terminal.action = f'Keep your {backup_os} backups and installation log.'
            terminal.line('First boot confirmed.' if answer=='yes' else 'First boot remains unverified. Keep the remote connected for diagnosis.')
            if restoring and answer == 'yes':
                terminal.line('Android restored. Run the normal curl command without restore/retry options for the fresh installer test.')
            return 0 if answer=='yes' else 1
        except (Exception, KeyboardInterrupt) as error:
            terminal.line('Installation stopped: ' + (str(error) if not isinstance(error,getpass.GetPassWarning) else 'Hidden password entry is unavailable'))
            terminal.action = 'Keep USB connected. No automatic retry or restore was attempted. Preserve: ' + str(run_dir)
            terminal.render()
            return 130 if isinstance(error,KeyboardInterrupt) else 1
        finally:
            terminal.log = None
