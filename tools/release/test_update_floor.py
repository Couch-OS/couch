"""The compatibility floor, against the bundles that were actually published."""
from pathlib import Path
import tempfile
import unittest

import runtime_inventory
import contextlib
import io

from update_floor import (allowed, bundle_names, FLOOR_RELEASE, inventory_files, main,
                          must_be_executable, problems, REQUIRED, tree_files,
                          tree_preview_features)

REPO = Path(__file__).resolve().parents[2]

# The file list of the published .148 runtime bundle, read from its
# couch-...-ha100-update.json asset (2026-09-15). This is the release the public
# installer's command line names. The tag is not spelled out: a release bump
# rewrites every dated tag in the files it governs, and a fixture recording what
# one particular release published must not follow it (tools/release/
# bump_release.py, which would otherwise call this a stray).
PUBLISHED_148 = (
    'build.json', 'confd.sh', 'couch-confd', 'couch-coreelec', 'couch-gui', 'couch-sonos',
    'couch-system', 'couch-wmt-properties.so', 'gui-start.sh', 'hardware-init.sh',
    'licenses/IRDB-CC0.txt', 'licenses/IRDB-MIT.txt', 'licenses/Inter-OFL.txt',
    'licenses/Lato-OFL.txt', 'licenses/Lucide-ISC.txt', 'portal.sh', 'setup-mode.sh',
    'stage2.sh', 'station.sh', 'system.sh', 'wifi-conf.sh', 'www/cgi-bin/enroll',
    'www/cgi-bin/save', 'www/cgi-bin/scan', 'www/cgi-bin/setpw', 'www/index.html')
# The same list for the published .164.dev bundle: .148 plus a BlueZ notice and
# the three Bluetooth binaries. Because .164.dev sorts above .148 and is a
# published prerelease, it is what a remote on the floor release is offered,
# and these three names are why it cannot install anything at all.
PUBLISHED_164_DEV = tuple(sorted(PUBLISHED_148 + (
    'couch-bluetoothd', 'couch-bt-bridge', 'couch-bt-hid', 'licenses/BlueZ-GPL-2.0.txt')))
BLUETOOTH_BINARIES = ('couch-bluetoothd', 'couch-bt-bridge', 'couch-bt-hid')


def listing(names):
    return [{'path': name, 'size': 4096,
             'mode': 0o755 if must_be_executable(name) else 0o644} for name in names]


class FloorTests(unittest.TestCase):
    def test_the_floor_required_list_is_the_publisher_required_list(self):
        """The publisher builds its bundle from staging.rs REQUIRED; a name
        added there has to be a name the floor already knows."""
        source = (REPO / 'daemon/couch-updates/src/staging.rs').read_text()
        block = source.split('const REQUIRED: &[&str] = &[', 1)[1].split('];', 1)[0]
        current = tuple(line.strip().strip(',').strip('"') for line in block.strip().splitlines())
        self.assertEqual(current, REQUIRED)

    def test_the_published_148_bundle_is_installable_at_the_floor(self):
        self.assertEqual(problems(listing(PUBLISHED_148)), [])

    def test_licence_texts_are_accepted_at_the_floor(self):
        """Pinned because it was reported the other way round. `licenses/*.txt`
        is in the floor's allowlist and always has been; the licence files are
        not why .148 could not be installed."""
        for name in PUBLISHED_148:
            if name.startswith('licenses/'):
                self.assertTrue(allowed(name), name)
        self.assertTrue(allowed('licenses/BlueZ-GPL-2.0.txt'))
        self.assertFalse(allowed('licenses/notice.html'))

    def test_the_published_dev_bundle_is_refused_for_its_bluetooth_binaries(self):
        found = problems(listing(PUBLISHED_164_DEV))
        self.assertEqual(found, [f'{name}: not on the {FLOOR_RELEASE} allowlist'
                                 for name in BLUETOOTH_BINARIES])

    def test_one_new_couch_binary_refuses_the_whole_bundle(self):
        for name in BLUETOOTH_BINARIES + ('couch-matter', 'couch-hue'):
            found = problems(listing(PUBLISHED_148 + (name,)))
            self.assertEqual(found, [f'{name}: not on the {FLOOR_RELEASE} allowlist'], name)

    def test_the_runtime_payload_inventory_publishes_only_floor_safe_names(self):
        """The regression guard: every name tools/release/runtime_inventory.py
        puts in the runtime payload and the publisher would therefore bundle.
        A binary added to RUNTIME or RUNTIME_ALPINE fails here, not in the
        field. It belongs in the boot ramdisk until the floor moves."""
        self.assertEqual(problems(inventory_files()), [],
                         f'ships in the runtime bundle but the {FLOOR_RELEASE} updater '
                         'refuses it; ship it in the boot ramdisk instead')
        # What a promotion publishes today: the .148 bundle plus the BlueZ
        # notice this branch added. Nothing else, and in particular no binary.
        self.assertEqual([f['path'] for f in inventory_files()],
                         sorted(PUBLISHED_148 + ('licenses/BlueZ-GPL-2.0.txt',)))

    def test_the_bluetooth_binaries_stay_out_of_the_runtime_payload(self):
        for name in BLUETOOTH_BINARIES:
            self.assertNotIn(name, runtime_inventory.RUNTIME, name)
            self.assertNotIn(name, runtime_inventory.RUNTIME_ALPINE, name)
            self.assertIn(name, runtime_inventory.BOOT_EXTRA, name)

    def test_names_outside_the_allowlist_are_refused(self):
        for name in ('../boot.img', '/etc/shadow', 'os-baseline.json', 'update-key.pub',
                     'runtime-boot.sh', 'config.json', 'www/../../etc/shadow', 'www/.env',
                     'bin/couch-bt-hid', 'couch-..', 'www/cgi-bin/sh', 'licenses/x.html',
                     'www/app.wasm', '', 'a' * 181):
            self.assertFalse(allowed(name), name)
        for name in ('fbcon', 'www/index.html', 'www/app.js', 'www/f.woff2',
                     'www/cgi-bin/save', 'licenses/Lato-OFL.txt') + REQUIRED:
            self.assertTrue(allowed(name), name)

    def test_modes_sizes_and_missing_required_files_are_refused(self):
        files = listing(PUBLISHED_148)
        self.assertEqual(problems([f for f in files if f['path'] != 'couch-gui']),
                         [f'couch-gui: required by the floor and missing'])
        bad = listing(PUBLISHED_148)
        bad[bad.index(next(f for f in bad if f['path'] == 'stage2.sh'))]['mode'] = 0o644
        self.assertIn('stage2.sh: must be mode 0755', problems(bad))
        self.assertTrue(any('exceeds' in reason for reason in
                            problems(listing(PUBLISHED_148) + [
                                {'path': 'fbcon', 'size': 65 * 1024 * 1024, 'mode': 0o755}])))
        self.assertIn('kind is \'boot\', not "runtime"',
                      problems(listing(PUBLISHED_148), kind='boot'))

    def test_a_clean_tree_is_checked_as_the_publisher_would_bundle_it(self):
        with tempfile.TemporaryDirectory() as scratch:
            tree = Path(scratch)
            for name in REQUIRED:
                (tree / name).write_bytes(b'x')
            for name in ('os-baseline.json', 'update-key.pub', 'runtime-boot.sh'):
                (tree / name).write_bytes(b'x')
            (tree / 'www/cgi-bin').mkdir(parents=True)
            (tree / 'www/index.html').write_bytes(b'x')
            (tree / 'www/cgi-bin/save').write_bytes(b'x')
            (tree / 'licenses').mkdir()
            (tree / 'licenses/Lato-OFL.txt').write_bytes(b'x')
            # Files the publisher never bundles do not reach the floor.
            self.assertNotIn('os-baseline.json', bundle_names(tree))
            self.assertNotIn('runtime-boot.sh', bundle_names(tree))
            self.assertEqual(problems(tree_files(tree)), [])
            # A Bluetooth binary left in the tree would be bundled, and refused.
            (tree / 'couch-bt-hid').write_bytes(b'x')
            self.assertIn('couch-bt-hid', bundle_names(tree))
            self.assertEqual(problems(tree_files(tree)),
                             [f'couch-bt-hid: not on the {FLOOR_RELEASE} allowlist'])

    def test_a_tree_with_a_protocol_3_preview_daemon_is_refused_unless_asked_for(self):
        def check(*arguments):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                status = main([*arguments])
            return status, output.getvalue()

        with tempfile.TemporaryDirectory() as scratch:
            tree = Path(scratch)
            for name in REQUIRED:
                (tree / name).write_bytes(b'x')
            self.assertEqual(tree_preview_features(tree), [])
            status, output = check('--tree', str(tree))
            self.assertEqual(status, 0)
            self.assertNotIn('PREVIEW', output)
            # An ordinary tree passes with the flag as well: it allows, it does not claim.
            self.assertEqual(check('--tree', str(tree), '--allow-preview')[0], 0)
            (tree / 'couch-confd').write_bytes(b'\x7fELF\x00\xffCOUCH-PREVIEW-BUILD protocol-3\n\x00')
            self.assertEqual(tree_preview_features(tree), ['protocol-3'])
            status, output = check('--tree', str(tree))
            self.assertEqual(status, 1)
            self.assertIn('OK: installable by', output)
            self.assertIn('PREVIEW BUILD', output)
            self.assertIn('REFUSED: pass --allow-preview', output)
            status, output = check('--tree', str(tree), '--allow-preview')
            self.assertEqual(status, 0)
            self.assertIn('PREVIEW BUILD', output)
            self.assertNotIn('REFUSED', output)
            # The flag forgives the preview and nothing else.
            (tree / 'couch-bt-hid').write_bytes(b'x')
            self.assertEqual(check('--tree', str(tree), '--allow-preview')[0], 1)
            (tree / 'couch-bt-hid').unlink()
            # No daemon: nothing to mark, and the floor reports it missing as before.
            (tree / 'couch-confd').unlink()
            self.assertEqual(tree_preview_features(tree), [])
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            main(['--inventory', '--allow-preview'])


if __name__ == '__main__':
    unittest.main()
