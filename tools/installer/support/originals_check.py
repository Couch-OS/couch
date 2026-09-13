#!/usr/bin/env python3
"""Explain why a Couch installer session's saved Android originals were refused.

Usage: python3 couch-originals-check.py ~/.couch-installer/install-XXXXXXXXXXXXXXXX

Reads only the session's journal and the first bytes of the saved boot and
overlay originals. Prints which installer release ran and which structural
check the boot/overlay pair fails, if any. Nothing is sent anywhere and nothing
is written.
"""
import gzip, json, struct, sys, zlib
from pathlib import Path

session = Path(sys.argv[1]).expanduser()
events = sorted(session.glob('event-*.json'))
release = None
for path in events:
    try:
        record = json.loads(path.read_text())
    except Exception:
        continue
    text = json.dumps(record)
    if release is None and '"release"' in text:
        release = record.get('release') or record.get('evidence', {}).get('release')
print(f'session: {session}')
print(f'journal events: {len(events)}; installer release recorded: {release}')

def field(data, offset):
    return struct.unpack_from('<I', data, offset)[0]

boot = session / 'bootstrap-boot.img'
if boot.is_file():
    data = boot.read_bytes()
    print(f'boot original: {len(data)} bytes, magic {data[:8]!r}')
    if data[:8] == b'ANDROID!':
        kernel, ramdisk, page = field(data, 8), field(data, 16), field(data, 36)
        print(f'  header v0 fields: kernel {kernel} bytes, ramdisk {ramdisk} bytes, page {page}, os_version 0x{field(data, 44):08x}')
        start = (page + kernel + page - 1) // page * page
        head = data[start:start + 8]
        kinds = {b'\x1f\x8b': 'gzip', b'\x04"M\x18': 'lz4 frame', b'\x02!L\x18': 'lz4 legacy',
                 b'\xfd7zXZ\x00': 'xz', b']\x00\x00': 'lzma', b'BZh': 'bzip2', b'(\xb5/\xfd': 'zstd'}
        kind = next((name for magic, name in kinds.items() if head.startswith(magic)), 'unknown')
        print(f'  ramdisk at 0x{start:x}, first bytes {head.hex()} -> {kind}')
        if kind == 'gzip':
            try:
                archive = zlib.decompress(data[start:start + ramdisk], 16 + zlib.MAX_WBITS)
                names, off = [], 0
                while archive[off:off + 6] in (b'070701', b'070702'):
                    size = int(archive[off + 54:off + 62], 16); nsize = int(archive[off + 94:off + 102], 16)
                    name = archive[off + 110:off + 110 + nsize - 1].decode(errors='replace')
                    if name == 'TRAILER!!!':
                        break
                    names.append(name)
                    off = (off + 110 + nsize + 3) & ~3
                    off = (off + size + 3) & ~3
                roots = [n for n in names if '/' not in n]
                print(f'  ramdisk is newc cpio with {len(names)} entries; init.rc at root: {"init.rc" in names}')
                print(f'  root entries: {", ".join(sorted(roots)[:20])}')
            except Exception as error:
                print(f'  ramdisk gzip/cpio walk failed: {error}')
else:
    print('boot original: missing (the installer stopped before saving it)')

overlay = session / 'bootstrap-odmdtbo.img'
if overlay.is_file():
    data = overlay.read_bytes()
    magic = struct.unpack_from('>I', data, 0)[0]
    print(f'overlay original: {len(data)} bytes, magic 0x{magic:08x} {data[8:12]!r} (MediaTek dtbo expects 0x88168858 "dtbo")')
    fdt = struct.unpack_from('>I', data, 0x400)[0]
    print(f'  at 0x400: 0x{fdt:08x} (device tree expects 0xd00dfeed)')
    if fdt == 0xd00dfeed:
        total = struct.unpack_from('>I', data, 0x404)[0]
        print(f'  device tree {total} bytes; names mediatek, compatibles: {b"mediatek," in data[0x400:0x400 + total]}')
else:
    print('overlay original: missing')
