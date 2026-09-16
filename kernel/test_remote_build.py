import importlib.util
from pathlib import Path
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location('remote_build', Path(__file__).with_name('remote-build.py'))
remote_build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(remote_build)


class RemoteBuildTests(unittest.TestCase):
    def test_explicit_remote_settings_reject_shell_metacharacters(self):
        with self.assertRaisesRegex(ValueError, 'unsupported'):
            remote_build.remote_settings({'KERNEL_REMOTE_HOST': 'builder;id',
                                          'KERNEL_REMOTE_RECIPE_ROOT': '/srv/recipe'})
        with self.assertRaisesRegex(ValueError, 'unsupported'):
            remote_build.remote_settings({'KERNEL_REMOTE_HOST': 'builder.example',
                                          'KERNEL_REMOTE_RECIPE_ROOT': '../recipe'})
        with self.assertRaisesRegex(ValueError, 'unsupported'):
            remote_build.remote_settings({'KERNEL_REMOTE_HOST': 'builder.example',
                                          'KERNEL_REMOTE_RECIPE_ROOT': '/srv/recipe;id'})

    def test_forwarded_values_are_shell_quoted(self):
        command = remote_build.remote_command({
            'KTREE': '/srv/kernel tree',
            'KOUT': '/srv/output;not-a-command',
            'KBUILD_BUILD_HOST': 'historic builder',
        }, '/srv/couch recipe', 'normal')
        self.assertIn("'KTREE=/srv/kernel tree'", command)
        self.assertIn("'KOUT=/srv/output;not-a-command'", command)
        self.assertIn("'KBUILD_BUILD_HOST=historic builder'", command)
        self.assertTrue(command.endswith("'/srv/couch recipe/kernel/build.sh' --local normal"))

    def test_rsync_uses_conservative_destination_without_unsupported_flag(self):
        env = {'KERNEL_REMOTE_HOST': 'builder', 'KERNEL_REMOTE_RECIPE_ROOT': '/srv/recipe'}
        with patch.object(remote_build.subprocess, 'run') as run, \
                patch.object(remote_build.subprocess, 'call', return_value=0):
            remote_build.run('normal', env)
        self.assertEqual(run.call_args_list[1].args[0],
                         ['rsync', '-a', 'kernel/', 'builder:/srv/recipe/kernel/'])

    def test_unexported_local_config_reaches_remote_helper(self):
        root = Path(__file__).parents[1]
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            config = temporary / 'local.env'
            config.write_text('\n'.join((
                'KERNEL_BUILD_MODE=remote',
                'KERNEL_REMOTE_HOST=builder.example',
                'KERNEL_REMOTE_RECIPE_ROOT=/srv/recipe',
                'KTREE=/srv/kernel',
                'KBUILD_BUILD_HOST=historical-builder',
            )) + '\n')
            fake_python = temporary / 'python3'
            fake_python.write_text('#!/bin/sh\nprintf "%s|%s|%s|%s|%s\\n" "$KERNEL_REMOTE_HOST" "$KERNEL_REMOTE_RECIPE_ROOT" "$KTREE" "$KBUILD_BUILD_HOST" "$*"\n')
            fake_python.chmod(0o755)
            env = os.environ | {
                'COUCH_LOCAL_CONFIG': str(config),
                'PATH': str(temporary) + os.pathsep + os.environ['PATH'],
            }
            result = subprocess.run(['sh', str(root / 'kernel/build.sh'), '--remote', 'normal'],
                                    cwd=root, env=env, text=True, capture_output=True, check=True)
        self.assertEqual(result.stdout.strip(),
                         'builder.example|/srv/recipe|/srv/kernel|historical-builder|kernel/remote-build.py normal')


if __name__ == '__main__':
    unittest.main()
