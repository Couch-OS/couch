"""Seed an OS capability marker only after checking its reviewed build inputs."""
import io
import json
from pathlib import Path
import tarfile

from clean_stage import checksum, require

PIN = Path(__file__).with_name('ha100_os_baseline.json')
MARKER = 'opt/couch/os-baseline.json'


def seed(data, closure_digest, *, pin=None):
    pin = json.loads(PIN.read_text()) if pin is None else pin
    require(closure_digest == pin['package_closure_sha256'],
            'OS baseline requires the reviewed FFmpeg package closure')
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        members = archive.getmembers()
        require(len({item.name for item in members}) == len(members), 'Duplicate baseline input path')
        require(MARKER not in {item.name for item in members}, 'OS baseline marker must be generated')
        require({'opt/couch/runtime-boot.sh', 'usr/bin/ffmpeg'} <= {item.name for item in members},
                'OS baseline requires stable bootstrap and FFmpeg files')
        boot = archive.getmember('opt/couch/runtime-boot.sh')
        require(boot.isreg() and boot.mode & 0o111 and checksum(archive.extractfile(boot).read()) == pin['runtime_boot_sha256'],
                'OS baseline requires the reviewed stable runtime bootstrap')
        ffmpeg = archive.getmember('usr/bin/ffmpeg')
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
