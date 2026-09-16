import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import build_fresh_core as core
from clean_stage import StageError


class FreshCoreBuildTests(unittest.TestCase):
    def test_old_receipt_refused_before_build(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            output.write_text('original receipt')
            with patch.object(core.subprocess, 'run') as run:
                with self.assertRaisesRegex(StageError, 'must be new'):
                    core.build(output)
                run.assert_not_called()
            self.assertEqual(output.read_text(), 'original receipt')

    def test_dirty_source_refused(self):
        with patch.object(core.subprocess, 'check_output', side_effect=['a' * 40, ' M stage2/runtime-boot.sh\n']):
            with self.assertRaisesRegex(StageError, 'clean frozen'):
                core.frozen_source(Path('/fixture'))

    def test_changed_source_after_build_has_no_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            with patch.object(core, 'frozen_source', side_effect=['a' * 40, 'b' * 40]), \
                    patch.object(core.subprocess, 'run') as run:
                with self.assertRaisesRegex(StageError, 'Source changed'):
                    core.build(output)
                self.assertEqual(run.call_count, 2)
            self.assertFalse(output.exists())

    def test_receipt_is_emitted_only_after_both_recipes_and_exact_binary_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            calls = []
            def run(command, **kwargs):
                self.assertFalse(output.exists())
                calls.append(Path(command[0]).name)
            with patch.object(core, 'frozen_source', return_value='a' * 40), \
                    patch.object(core.subprocess, 'run', side_effect=run), \
                    patch.object(core, 'regular', return_value=b'fixture-key-and-core'), \
                    patch.object(core, 'arm_static') as arm:
                result = core.build(output)
            self.assertEqual(calls, ['build-wmt-properties.sh', 'build-release.sh'])
            self.assertEqual(arm.call_count, 5)
            self.assertEqual(json.loads(output.read_text()), result)
            self.assertEqual(result['source_commit'], 'a' * 40)
            self.assertFalse(result['installable'])


if __name__ == '__main__':
    unittest.main()
