"""Exercise shared build-cache invalidation without device images or hardware."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'build-fbcon.sh'


@unittest.skipUnless(sys.platform.startswith(('linux', 'darwin')), 'NDK build hosts')
class FbconBuildTests(unittest.TestCase):
    def test_source_dependencies_rebuild_and_unchanged_input_reuses_binary(self):
        with tempfile.TemporaryDirectory(prefix='couch-fbcon-') as temporary:
            root = Path(temporary)
            (root / 'tools').mkdir()
            (root / 'src').mkdir()
            shutil.copy2(SCRIPT, root / 'tools/build-fbcon.sh')
            host = 'darwin-x86_64' if sys.platform == 'darwin' else 'linux-x86_64'
            binary = root / 'ndk/toolchains/llvm/prebuilt' / host / 'bin'
            binary.mkdir(parents=True)
            compiler = binary / 'armv7a-linux-androideabi21-clang'
            compiler.write_text('#!/bin/sh\necho build >> count\n[ ! -f fail ] || exit 1\nwhile [ "$1" != -o ]; do shift; done\nshift\nprintf new > "$1"\n')
            compiler.chmod(0o755)
            strip = binary / 'llvm-strip'
            strip.write_text('#!/bin/sh\nexit 0\n')
            strip.chmod(0o755)
            files = [root / 'src' / name for name in ('fbcon.c', 'logo.h', 'font.h')]
            for path in files:
                path.write_text('fixture')
                os.utime(path, (1, 1))
            env = {**os.environ, 'NDK': str(root / 'ndk')}
            def run():
                return subprocess.run(['sh', str(root / 'tools/build-fbcon.sh')],
                                      env=env, capture_output=True, timeout=5)
            self.assertEqual(run().returncode, 0)
            self.assertEqual(run().returncode, 0)
            self.assertEqual((root / 'count').read_text().splitlines(), ['build'])
            for index, path in enumerate(files, 2):
                future = time.time() + 10
                os.utime(path, (future, future))
                self.assertEqual(run().returncode, 0)
                self.assertEqual(len((root / 'count').read_text().splitlines()), index)
                os.utime(path, (1, 1))
            # A compiler failure cannot replace a good binary or permit packaging.
            os.utime(files[1], (time.time() + 10,) * 2)
            (root / 'fail').touch()
            before = (root / 'build/fbcon').read_bytes()
            self.assertNotEqual(run().returncode, 0)
            self.assertEqual((root / 'build/fbcon').read_bytes(), before)
            self.assertEqual(list((root / 'build').glob('*.pending.*')), [])


if __name__ == '__main__':
    unittest.main()
