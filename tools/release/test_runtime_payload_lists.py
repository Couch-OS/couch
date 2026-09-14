"""Compare the four hand-maintained descriptions of /opt/couch.

The runtime payload is described in four places in three languages and nothing
compared them: runtime_inventory stages the files, clean_stage decides which
destinations a staging spec may name, daemon/couch-updates/src/staging.rs lists
what an installable update must contain, and tools/build-release.sh builds the
ARM binaries. Drift between them is silent until a device rejects an update or
a payload ships without a binary. These tests are that comparison; they do not
replace the lists.
"""
from pathlib import Path
import re
import tempfile
import unittest

from clean_stage import artifact_destination, LICENSE_FILES, RECOVERY_CGI, REPO
from runtime_inventory import audit, CGI, LICENSES, RUNTIME, SCRIPTS

CARGO_OUTPUT = 'target/armv7-unknown-linux-musleabihf/release/'
STAGING = REPO / 'daemon/couch-updates/src/staging.rs'
BUILD = REPO / 'tools/build-release.sh'
# clean_stage writes the build attestation into the payload; it is not an
# input, so runtime_inventory never inventories it.
GENERATED = frozenset(('build.json',))


def staged():
    """Every destination audit() places, taken from audit() itself.

    An empty root has no inputs, so each add() records its destination as a
    blocker: reading them back keeps this test honest if audit changes.
    """
    with tempfile.TemporaryDirectory() as directory:
        blockers = audit(Path(directory), vendor=Path(directory))['blockers']
    found = [match[1] for match in (re.fullmatch(r'(opt/couch/\S+): .*', line) for line in blockers) if match]
    assert found, 'audit() reported no missing runtime destinations'
    return found


def rust_required():
    text = STAGING.read_text()
    const = re.search(r'const REQUIRED: &\[&str\] = &\[(.*?)\];', text, re.S)
    assert const, 'staging.rs no longer declares REQUIRED as a &[&str]'
    return frozenset(re.findall(r'"([^"]+)"', const[1]))


class RuntimePayloadListTests(unittest.TestCase):
    def test_every_staged_file_is_a_destination_clean_stage_accepts(self):
        for destination in staged():
            self.assertEqual(artifact_destination(destination), destination)

    def test_the_updater_requires_only_files_the_payload_contains(self):
        names = {destination.rsplit('/', 1)[1] for destination in staged()} | GENERATED
        self.assertEqual(rust_required() - names, set(),
                         'staging.rs REQUIRED names a file no payload contains: every update would be refused')

    def test_required_scripts_and_binaries_come_from_the_python_lists(self):
        required = rust_required()
        self.assertEqual({name for name in required if name.endswith('.sh')} - set(SCRIPTS), set())
        binaries = {name for name in required if not name.endswith(('.sh', '.so', '.json'))}
        self.assertEqual(binaries - set(RUNTIME), set())

    def test_build_release_builds_exactly_the_cargo_binaries_the_payload_stages(self):
        # fbcon and couch-wmt-properties.so have their own build scripts; this
        # is only the ARM cargo output both files name by path.
        cargo = {name for name, source in RUNTIME.items() if CARGO_OUTPUT in source}
        built = set(re.findall(r'/target/\$TARGET/release/([A-Za-z0-9._-]+)', BUILD.read_text()))
        self.assertEqual(cargo, built)

    def test_the_duplicated_licence_and_cgi_lists_agree(self):
        self.assertEqual(set(LICENSES), set(LICENSE_FILES))
        self.assertEqual(set(CGI), set(RECOVERY_CGI))


if __name__ == '__main__':
    unittest.main()
