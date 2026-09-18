"""Seed an OS capability marker only after checking its reviewed build inputs."""
import io
import json
from pathlib import Path
import re
import tarfile

from clean_stage import archive_name, checksum, require

PIN = Path(__file__).with_name('ha100_os_baseline.json')
MARKER = 'opt/couch/os-baseline.json'
ARCHIVE = 'package_closure_archive'
ARCHIVE_FILE = re.compile(r'[A-Za-z0-9][A-Za-z0-9+_.-]*\.tar\.gz')


def archive_pin(pin=None):
    """The reviewed retained closure archive, or None while none is published.

    `package_closure_sha256` stays alongside it and is not redundant. The
    archive digest binds the exact bytes a build host received; the manifest
    digest is the reviewed inventory identity, which survives re-archiving and
    is what `seed` writes the OS capability marker against. An archive that is
    repacked keeps the second; an archive whose packages were edited and whose
    manifest was rewritten to match loses it.
    """
    pin = json.loads(PIN.read_text()) if pin is None else pin
    require(ARCHIVE in pin, 'OS baseline must state its retained closure archive')
    value = pin[ARCHIVE]
    if value is None:
        return None
    require(isinstance(value, dict) and set(value) == {'file', 'size', 'sha256'},
            'Retained closure archive pin has missing or unexpected fields')
    require(isinstance(value['file'], str) and ARCHIVE_FILE.fullmatch(value['file']),
            'Retained closure archive pin needs a plain archive filename')
    require(type(value['size']) is int and 0 < value['size'], 'Retained closure archive pin needs a positive size')
    require(isinstance(value['sha256'], str) and re.fullmatch(r'[0-9a-f]{64}', value['sha256']),
            'Retained closure archive pin needs a SHA-256 digest')
    return value


def check_archive(data, pin=None):
    """Bind retained archive bytes to the reviewed pin before they are trusted."""
    value = archive_pin(pin)
    require(value is not None, 'No reviewed retained closure archive is pinned for this OS baseline')
    require(len(data) == value['size'] and checksum(data) == value['sha256'],
            'Retained closure archive bytes differ from the reviewed pin')
    return value


def seed(data, closure_digest, *, pin=None):
    pin = json.loads(PIN.read_text()) if pin is None else pin
    require(closure_digest == pin['package_closure_sha256'],
            'OS baseline requires the reviewed FFmpeg package closure')
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        members = archive.getmembers()
        # Alpine's assembler emits ./ prefixes. Canonicalize before admission so
        # aliases cannot evade duplicate or preexisting-marker rejection.
        indexed = {archive_name(item.name): item for item in members}
        require(len(indexed) == len(members), 'Duplicate baseline input path')
        require(MARKER not in indexed, 'OS baseline marker must be generated')
        require({'opt/couch/runtime-boot.sh', 'usr/bin/ffmpeg'} <= indexed.keys(),
                'OS baseline requires stable bootstrap and FFmpeg files')
        boot = indexed['opt/couch/runtime-boot.sh']
        require(boot.isreg() and boot.mode & 0o111 and checksum(archive.extractfile(boot).read()) == pin['runtime_boot_sha256'],
                'OS baseline requires the reviewed stable runtime bootstrap')
        ffmpeg = indexed['usr/bin/ffmpeg']
        require(ffmpeg.isreg() and ffmpeg.mode & 0o111, 'OS baseline requires installed FFmpeg')
        header = archive.extractfile(ffmpeg).read(20)
        require(header[:6] == b'\x7fELF\x01\x01' and header[18:20] == b'\x28\x00',
                'OS baseline requires ARM FFmpeg')
        marker = json.dumps({key: pin[key] for key in ('schema', 'model', 'id')},
                            sort_keys=True, separators=(',', ':')).encode() + b'\n'
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w', format=tarfile.USTAR_FORMAT) as result:
            for item in members:
                result.addfile(item, archive.extractfile(item) if item.isreg() else None)
            item = tarfile.TarInfo(MARKER)
            item.mode, item.size = 0o644, len(marker)
            result.addfile(item, io.BytesIO(marker))
    return output.getvalue(), json.loads(marker)
