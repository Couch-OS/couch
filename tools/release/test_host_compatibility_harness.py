import copy
from contextlib import ExitStack
import hashlib
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch


HARNESS_PATH = Path(__file__).resolve().parents[1] / 'tests/denon-v1-host-compatibility.py'
SPEC = importlib.util.spec_from_file_location('denon_host_compatibility_harness', HARNESS_PATH)
harness = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(harness)


class HostCompatibilityBuildBindingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.args = SimpleNamespace(
            core_commit='a' * 40, confd=self.root / 'confd',
            build_receipt=self.root / 'build.json', out=self.root / 'results',
            apk=self.root / 'package.apk', key=self.root / 'key.pub',
            provenance=self.root / 'provenance.json')
        self.args.confd.write_bytes(b'non-executable binary fixture')
        self.build = dict(schema=1, kind='couch-arm-confd-build',
                          source_commit=self.args.core_commit,
                          target='armv7-unknown-linux-musleabihf', git_clean=True,
                          binary_sha256=hashlib.sha256(self.args.confd.read_bytes()).hexdigest())
        self.write_receipt(self.build)

    def write_receipt(self, value):
        self.args.build_receipt.write_text(json.dumps(value))

    def forbid_execution(self, stack):
        guards = []
        for owner, name in ((harness.subprocess, 'run'), (harness.subprocess, 'Popen'),
                            (harness.socket, 'socket'), (harness.urllib.request, 'urlopen')):
            guards.append(stack.enter_context(patch.object(
                owner, name, side_effect=RuntimeError('Unexpected process or network operation'))))
        return guards

    def assert_refused_before_execution(self):
        with ExitStack() as stack:
            guards = self.forbid_execution(stack)
            with self.assertRaises((AssertionError, ValueError, TypeError)):
                harness.run(self.args)
            for guard in guards:
                guard.assert_not_called()
        self.assertFalse(self.args.out.exists(), 'Refusal must not emit success evidence')

    def test_exact_clean_source_target_and_binary_binding_is_accepted_without_execution(self):
        with ExitStack() as stack:
            guards = self.forbid_execution(stack)
            self.assertIsNone(harness.validate_core_build(self.args))
            for guard in guards:
                guard.assert_not_called()
        self.assertFalse(self.args.out.exists())

    def test_other_source_commit_is_refused_before_any_harness_activity(self):
        self.build['source_commit'] = 'b' * 40
        self.write_receipt(self.build)
        self.assert_refused_before_execution()

    def test_changed_binary_cannot_reuse_a_clean_frozen_build_receipt(self):
        self.args.confd.write_bytes(b'different executable bytes')
        self.assert_refused_before_execution()

    def test_dirty_build_and_truthy_non_boolean_values_are_refused(self):
        for clean in (False, 0, 1, 'true', None):
            with self.subTest(git_clean=clean):
                self.write_receipt({**self.build, 'git_clean': clean})
                self.assert_refused_before_execution()

    def test_receipt_field_types_identity_and_exact_shape_are_required(self):
        changes = [('schema', True), ('schema', 1.0), ('schema', '1'),
                   ('kind', 'other-build'), ('target', 'x86_64-unknown-linux-gnu'),
                   ('source_commit', None), ('binary_sha256', 123),
                   ('binary_sha256', 'not-a-sha256'), ('binary_sha256', 'A' * 64)]
        for key, value in changes:
            with self.subTest(field=key, value=value):
                self.write_receipt({**self.build, key: value})
                self.assert_refused_before_execution()
        for key in self.build:
            with self.subTest(missing=key):
                incomplete = copy.deepcopy(self.build)
                del incomplete[key]
                self.write_receipt(incomplete)
                self.assert_refused_before_execution()
        for value in ({**self.build, 'extra': 'unreviewed'}, {}, [], None):
            with self.subTest(shape=value):
                self.write_receipt(value)
                self.assert_refused_before_execution()

    def test_frozen_commit_argument_requires_the_full_lowercase_commit_identity(self):
        for commit in ('main', 'a' * 39, 'A' * 40, None, 1):
            with self.subTest(commit=commit):
                self.args.core_commit = commit
                self.assert_refused_before_execution()

    def test_missing_symlinked_oversized_and_non_json_receipts_are_refused(self):
        self.args.build_receipt.unlink()
        self.assert_refused_before_execution()
        original = self.root / 'original.json'
        original.write_text(json.dumps(self.build))
        self.args.build_receipt.symlink_to(original)
        self.assert_refused_before_execution()
        self.args.build_receipt.unlink()
        for content in ('', ' ' * 65537, '{broken json'):
            with self.subTest(receipt_length=len(content)):
                self.args.build_receipt.write_text(content)
                self.assert_refused_before_execution()


if __name__ == '__main__':
    unittest.main()
