#!/usr/bin/env python3
"""Inventory clean Couch runtime inputs and emit a noninstallable staging spec.

Never reads a device, copies a vendor tree, or invents redistribution permission.
"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys

from kernel_provenance import boot_kernel, PIN
from verify_integration_set import DEFAULT as DEFAULT_INTEGRATION_SET
from verify_integration_set import receipt as integration_receipt
from verify_integration_set import verify_receipt as verify_integration_receipt

from clean_stage import REPO, StageError, archive_name, build, require, secret_path

sys.path.insert(0, str(REPO / 'tools/installer/image'))
from neutral_ramdisk import arm_static as neutral_arm_static
from neutral_ramdisk import cpio_files as neutral_cpio_files
from neutral_ramdisk import regular as neutral_regular
from neutral_ramdisk import sha


def _stage_error(function, *args):
    try:
        return function(*args)
    except ValueError as error:
        raise StageError(str(error)) from error


def regular(path):
    return _stage_error(neutral_regular, path)


def arm_static(data):
    return _stage_error(neutral_arm_static, data)


def cpio_files(data):
    return _stage_error(neutral_cpio_files, data)

# What the runtime bundle carries. Every name here is published in the signed
# update and must therefore be a name the OLDEST DEPLOYED UPDATER accepts:
# tools/release/update_floor.py holds that allowlist and the reason it cannot
# move. Adding a binary here strands every remote that has not updated past it.
RUNTIME = {
    'couch-coreelec': 'clients/target/armv7-unknown-linux-musleabihf/release/couch-coreelec',
    'couch-sonos': 'clients/target/armv7-unknown-linux-musleabihf/release/couch-sonos',
    'couch-gui': 'ui/target/armv7-unknown-linux-musleabihf/release/couch-gui',
    'couch-system': 'daemon/target/armv7-unknown-linux-musleabihf/release/couch-system',
    'couch-confd': 'daemon/target/armv7-unknown-linux-musleabihf/release/couch-confd',
    'fbcon': 'build/fbcon',
}
# Runtime executables linked against the Alpine root's shared libraries, which
# run inside the chroot. Empty: couch-bluetoothd was the only one and it now
# travels in the boot ramdisk with the rest of the Bluetooth stack.
RUNTIME_ALPINE = {}
# The Bluetooth stack, which does NOT go in the runtime bundle. These names are
# newer than the oldest deployed updater's allowlist, so a bundle carrying one
# is refused before download - by every remote, for every later release, until
# a full OS reinstall (docs/runtime-updates.md, "Compatibility floor"). They
# ride in the boot ramdisk's /extra instead, beside the kernel that gives them
# /dev/vhci and delivered only to images that take the boot payload;
# tools/release/prepare_boot_candidates.py puts them there and
# couch_system::bluetooth::base() copies them out of /extra into shared /tmp
# when the runtime has no copy. 'alpine' is bluetoothd patched to keep bonded
# devices' CCCs, linked against the Alpine root (third_party/bluez/build.sh).
BOOT_EXTRA = {
    'couch-bt-bridge': ('clients/target/armv7-unknown-linux-musleabihf/release/couch-bt-bridge', True),
    'couch-bt-hid': ('clients/target/armv7-unknown-linux-musleabihf/release/couch-bt-hid', True),
    'couch-bluetoothd': ('build/bluez/couch-bluetoothd', 'alpine'),
}
SCRIPTS = ('stage2.sh', 'runtime-boot.sh', 'hardware-init.sh', 'gui-start.sh', 'confd.sh', 'setup-mode.sh', 'portal.sh', 'system.sh',
           'wifi-conf.sh', 'station.sh')
CGI = ('save', 'setpw', 'scan', 'enroll')
LICENSES = {'Lato-OFL.txt': 'tools/release/licenses/Lato-OFL.txt',
            'Inter-OFL.txt': 'assets/inter/LICENSE.txt', 'Lucide-ISC.txt': 'assets/lucide/LICENSE',
            'IRDB-MIT.txt': 'daemon/couch-confd/assets/ir/LICENSE-Flipper-MIT.txt',
            'IRDB-CC0.txt': 'daemon/couch-confd/assets/ir/LICENSE-CC0.txt',
            'BlueZ-GPL-2.0.txt': 'tools/release/licenses/BlueZ-GPL-2.0.txt'}
VENDOR_REQUIRED = (
    'vendor/lib/modules/wmt_drv.ko', 'vendor/lib/modules/wmt_chrdev_wifi.ko',
    'vendor/lib/modules/wlan_drv.ko', 'vendor/bin/wmt_loader', 'vendor/bin/wmt_launcher',
    'vendor/firmware/ROMv2_lm_patch_1_0_hdr.bin', 'vendor/firmware/ROMv2_lm_patch_1_1_hdr.bin',
    'vendor/firmware/WIFI_RAM_CODE_6580', 'vendor/firmware/WMT_SOC.cfg',
    'vendor/etc/selinux/nonplat_property_contexts', 'system/etc/selinux/plat_property_contexts',
    'system/etc/ld.config.txt', 'system/bin/linker',
    *('system/lib/' + name for name in ('libc.so', 'libm.so', 'libdl.so', 'libc++.so',
       'liblog.so', 'libcutils.so', 'libbacktrace.so', 'libunwind.so', 'libutils.so',
       'libnetd_client.so', 'libbase.so', 'liblzma.so')),
)
BOOT_SOURCES = ('initramfs/init', 'initramfs/boot-health.sh', 'recovery/init',
                'tools/mkcpio.py', 'tools/bootimg.py', 'tools/build.sh', 'tools/build-recovery.sh',
                'kernel/pack.py', 'kernel/couch-ha100.config', 'kernel/release-pin.json',
                'kernel/source_policy.py', 'tools/release/kernel_provenance.py', 'src/fbcon.c', 'src/font.h', 'src/logo.h')


def arm_alpine(data):
    require(len(data) >= 52 and data[:6] == b'\x7fELF\x01\x01' and data[18:20] == b'\x28\x00',
            'Expected little-endian ARM32 ELF')
    require(b'/lib/ld-musl-armhf.so.1\x00' in data, 'Expected a musl ARM executable for the Alpine root')


def describe(root, relative):
    data = regular(root / relative)
    return {'source': relative, 'size': len(data), 'sha256': sha(data)}


def tree_inventory(root, directory):
    base = root / directory
    require(base.is_dir() and not base.is_symlink(), f'Missing source directory: {directory}')
    result = []
    for path in sorted(base.rglob('*')):
        require(not path.is_symlink(), 'Source inventory must not traverse symlinks')
        if path.is_file():
            result.append(describe(root, path.relative_to(root).as_posix()))
    return result


def embedded_web(root, daemon):
    files = tree_inventory(root, 'web/couch-web/dist')
    require(any(f['source'].endswith('/index.html') for f in files) and
            any(f['source'].endswith('.wasm') for f in files) and
            any(f['source'].endswith('.js') for f in files), 'Incomplete web bundle')
    missing = [f['source'] for f in files if regular(root / f['source']) not in daemon]
    require(not missing, 'Daemon does not embed the current complete web bundle')
    return files


def boot_inventory(root, relative, init_source):
    data = regular(root / relative)
    require(len(data) >= 2048 and data[:8] == b'ANDROID!', 'Invalid Android boot image')
    kernel_size, _, ramdisk_size, _, second_size, _, _, page, version = struct.unpack_from('<9I', data, 8)
    require(page == 2048 and version == 0 and second_size == 0, 'Unsupported boot image layout')
    begin = page + ((kernel_size + page - 1) // page) * page
    require(kernel_size > 0 and ramdisk_size > 0 and begin + ramdisk_size <= len(data), 'Truncated boot payload')
    kernel = data[page:page+kernel_size]
    require(len(kernel) >= 48 and kernel[36:40] == b'\x18\x28\x6f\x01', 'Missing ARM zImage magic')
    entries = cpio_files(gzip.decompress(data[begin:begin+ramdisk_size]))
    require('init' in entries and 'bin/busybox' in entries, 'Missing boot init/BusyBox')
    arm_static(entries['bin/busybox'])
    expected = {'init': init_source, 'bin/busybox': 'build/busybox-armv7l', 'extra/fbcon': 'build/fbcon'}
    if init_source == 'initramfs/init':
        expected['extra/boot-health.sh'] = 'initramfs/boot-health.sh'
    mismatches = [name for name, source in expected.items() if entries.get(name) != regular(root / source)]
    promoted = (boot_kernel(data, json.loads(PIN.read_text()))
                if init_source == 'initramfs/init' else None)
    return {'promoted_kernel': promoted, **describe(root, relative), 'mismatched_source_members': mismatches, 'kernel_sha256': sha(kernel), 'ramdisk_entries': len(entries),
            'init_matches_current_source': entries['init'] == regular(root / init_source),
            'busybox_matches_candidate': entries['bin/busybox'] == regular(root / 'build/busybox-armv7l'),
            'status': 'candidate only; boot/kernel/DTB provenance and hardware validation required'}


def audit(root=REPO, vendor=None, boot='build/couch-board-init-fixed.img', recovery='build/couch-recovery.img'):
    artifacts, blockers, embedded, sources = [], [], {}, []
    binaries = {}
    def add(source, destination, mode, executable=False):
        try:
            data = regular(root / source)
            if executable == 'alpine':
                arm_alpine(data)
            elif executable:
                arm_static(data)
            artifacts.append({'source': source, 'destination': destination, 'mode': mode, 'sha256': sha(data)})
            return data
        except ValueError as error:
            blockers.append(f'{destination}: {error}')
    for name, source in RUNTIME.items():
        binaries[name] = add(source, 'opt/couch/' + name, 0o755, True)
    for name, source in RUNTIME_ALPINE.items():
        binaries[name] = add(source, 'opt/couch/' + name, 0o755, 'alpine')
    add('build/couch-wmt-properties.so', 'opt/couch/couch-wmt-properties.so', 0o755)
    for name in SCRIPTS:
        add('stage2/' + name, 'opt/couch/' + name, 0o755)
    add('stage2/www/index.html', 'opt/couch/www/index.html', 0o644)
    for name in CGI:
        add('stage2/www/cgi-bin/' + name, 'opt/couch/www/cgi-bin/' + name, 0o755)
    for name, source in LICENSES.items():
        add(source, 'opt/couch/licenses/' + name, 0o644)
    try:
        embedded['web'] = embedded_web(root, binaries.get('couch-confd') or b'')
    except ValueError as error:
        blockers.append(str(error))
    try:
        embedded['gui_inputs'] = [item for directory in ('ui/couch-gui/fonts', 'ui/couch-gui/assets', 'ui/couch-gui/ui')
                                  for item in tree_inventory(root, directory)]
        embedded['gui_policy'] = describe(root, 'ui/couch-gui/build.rs')
        for name in ('Lato-Regular.ttf', 'Lato-Bold.ttf'):
            regular(root / 'ui/couch-gui/fonts' / name)
        require(regular(root / 'ui/couch-gui/assets/lucide.alpha') in (binaries.get('couch-gui') or b''),
                'GUI does not embed the current Lucide bitmap catalog')
    except ValueError as error:
        blockers.append(str(error))
    clean_runtime_ready = not blockers
    # The boot ramdisk's /extra, inventoried separately from the runtime tree
    # because none of it is published in the runtime bundle. Missing pieces are
    # release blockers, not clean-runtime blockers: the runtime is complete
    # without them, the Bluetooth-capable boot image is not.
    boot_extra = []
    for name, (source, kind) in sorted(BOOT_EXTRA.items()):
        try:
            data = regular(root / source)
            if kind == 'alpine':
                arm_alpine(data)
            else:
                arm_static(data)
            boot_extra.append({'source': source, 'destination': 'extra/' + name,
                               'mode': 0o755, 'sha256': sha(data)})
        except ValueError as error:
            blockers.append(f'extra/{name}: {error}')
    vendor = vendor or root / 'build/vendor-bundle'
    vendor_files, vendor_missing = [], []
    for name in VENDOR_REQUIRED:
        try:
            vendor_files.append(describe(vendor, name))
        except ValueError:
            vendor_missing.append(name)
    for directory in ('vendor/firmware', 'vendor/etc/selinux', 'system/etc/selinux'):
        if (vendor / directory).is_dir():
            for item in tree_inventory(vendor, directory):
                if item['source'] not in {f['source'] for f in vendor_files}:
                    vendor_files.append(item)
    if vendor_missing:
        blockers.append('Vendor bundle lacks required files (see vendor.missing)')
    blockers.extend(['Vendor extraction source hashes and redistribution evidence are not attested',
                     'Clean source-to-binary build provenance is not attested',
                     'Project/Rust dependency notices and GPL corresponding-source release review pending',
                     'Signed release manifest and physical boot/recovery validation pending'])
    boot_images = {}
    for role, path, init in [('boot', boot, 'initramfs/init'), ('recovery', recovery, 'recovery/init')]:
        try:
            boot_images[role] = boot_inventory(root, path, init)
            if boot_images[role]['mismatched_source_members']:
                blockers.append(f'{role} candidate has missing/outdated ramdisk source members')
        except (ValueError, OSError, EOFError) as error:
            blockers.append(f'{role} candidate: {error}')
    for name in (*BOOT_SOURCES, 'build/busybox-armv7l'):
        try:
            sources.append(describe(root, name))
        except ValueError as error:
            blockers.append(str(error))
    blockers.append('Boot/recovery kernel, DTB/header, BusyBox source provenance and complete partition sizing pending')
    return {'schema': 1, 'kind': 'couch-runtime-payload-inventory', 'installable': False,
            'clean_runtime_ready': clean_runtime_ready, 'payload_complete': False,
            'artifacts': artifacts, 'embedded': embedded, 'boot_images': boot_images,
            'boot_payload_binaries': boot_extra,
            'boot_sources': sources, 'vendor': {'files': sorted(vendor_files, key=lambda f: f['source']),
            'missing': vendor_missing, 'required_source_images': ['vendor.img', 'system.img'],
            'extraction_recipe': 'tools/extract-vendor.sh (development tool; not invoked)',
            'included_in_staging': False,
            'redistribution': 'unreviewed; file possession is not permission'}, 'blockers': blockers}


def cargo_inventory(root, workspace):
    """Record declared licenses, not an automatic license-compliance decision."""
    data = subprocess.check_output(['cargo', 'metadata', '--manifest-path',
        str(root / workspace / 'Cargo.toml'), '--format-version', '1', '--locked', '--offline',
        '--filter-platform', 'armv7-unknown-linux-musleabihf'], stderr=subprocess.DEVNULL)
    metadata = json.loads(data)
    packages = [{'name': item['name'], 'version': item['version'], 'license': item.get('license'),
                 'source': item.get('source') or 'workspace',
                 'license_file': Path(item['license_file']).name if item.get('license_file') else None}
                for item in metadata['packages']]
    return {'lockfile': describe(root, workspace + '/Cargo.lock'),
            'scope': 'Cargo target resolution, including build dependencies; not binary reachability',
            'packages': sorted(packages, key=lambda p: (p['name'], p['version']))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base_spec', type=Path, help='Existing clean Alpine staging spec')
    parser.add_argument('output', type=Path, help='New inventory/spec output directory')
    parser.add_argument('--vendor-dir', type=Path)
    parser.add_argument('--boot-image', default='build/couch-board-init-fixed.img')
    parser.add_argument('--recovery-image', default='build/couch-recovery.img')
    parser.add_argument('--integration-set', type=Path, default=DEFAULT_INTEGRATION_SET,
                        help='Pinned core/package compatibility set to record')
    parser.add_argument('--integration-receipt', type=Path,
                        help='Offline feed-byte verification receipt for a release candidate')
    args = parser.parse_args()
    require(not args.output.exists(), 'Output directory must be new')
    result = audit(vendor=args.vendor_dir, boot=args.boot_image, recovery=args.recovery_image)
    result['tested_integration_set'] = (verify_integration_receipt(
        args.integration_set, args.integration_receipt, require_artifacts=True)
        if args.integration_receipt else integration_receipt(args.integration_set))
    diff = subprocess.check_output(['git', '-C', str(REPO), 'diff', 'HEAD', '--',
        'ui', 'model', 'clients', 'daemon', 'web', 'src', 'stage2', 'initramfs', 'recovery'])
    result['tracked_payload_worktree_clean'] = not diff
    result['tracked_payload_diff_sha256'] = sha(diff)
    result['cargo'] = {}
    for workspace in ('ui', 'daemon', 'clients'):
        try:
            result['cargo'][workspace] = cargo_inventory(REPO, workspace)
        except (subprocess.CalledProcessError, OSError, ValueError):
            result['blockers'].append(f'{workspace}: offline locked dependency-license inventory unavailable')
    spec = json.loads(args.base_spec.read_text())
    spec['source_commit'] = subprocess.check_output(['git', '-C', str(REPO), 'rev-parse', 'HEAD'], text=True).strip()
    result['inventory_source_commit'] = spec['source_commit']
    result['source_commit_role'] = 'inventory checkout; not a source-to-binary build attestation'
    spec['artifacts'] = result['artifacts']
    # Verify the complete proposed clean runtime, including the pinned base,
    # before emitting a staging spec. Vendor/source blockers remain explicit.
    if result['clean_runtime_ready']:
        build(spec)
    args.output.mkdir()
    (args.output / 'payload-inventory.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    if result['clean_runtime_ready']:
        (args.output / 'staging-input.json').write_text(json.dumps(spec, indent=2, sort_keys=True) + '\n')
    print(f"Clean runtime ready: {result['clean_runtime_ready']}; release blockers: {len(result['blockers'])}")


if __name__ == '__main__':
    main()
