#!/usr/bin/env python3
"""Exercise the actual host renderer's offline PPM path, never /dev/fb0."""
import argparse
from pathlib import Path
import subprocess
import tempfile


def check(renderer):
    with tempfile.TemporaryDirectory(prefix='couch-display-test-') as temporary:
        root = Path(temporary)
        frames = []
        for index, event in enumerate((
            'v1 wait none 0 0 0 waiting none\n',
            'v1 download userdata 2097152 4194304 1048576 connected none\n',
            'v1 error recovery 4096 8192 0 failed verify\n',
        )):
            source, output = root / 'state', root / f'{index}.ppm'
            source.write_text(event)
            subprocess.run([str(renderer), '--ppm', str(source), str(output)], check=True)
            data = output.read_bytes()
            header = b'P6\n480 800\n255\n'
            assert data.startswith(header) and len(data) == len(header) + 480*800*3
            frames.append(data)
        assert len(set(frames)) == 3
        for bad in ('v1 wait secret 0 0 0 waiting none\n',
                    'v1 write boot 9 1 0 connected none\n',
                    'v1 wait none 0 0 0 waiting none\nsecret\n',
                    'v1 wait none 0 0 0 SSID-password none\n', 'x'*10000):
            source.write_text(bad)
            output = root / 'bad.ppm'
            result = subprocess.run([str(renderer), '--ppm', str(source), str(output)])
            assert result.returncode == 2 and not output.exists()
    print('Renderer: 3 visual states and 5 malformed/private-input cases passed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--renderer', type=Path, required=True)
    check(parser.parse_args().renderer.resolve())
