#!/usr/bin/env python3
"""Forward a kernel recipe to an explicitly configured remote builder."""
import os
from pathlib import PurePosixPath
import re
import shlex
import subprocess
import sys


def setting(env, name):
    value = env.get(name, '')
    if not value:
        raise ValueError(f'{name} must be set in local.env or the environment')
    return value


def remote_settings(env):
    host = setting(env, 'KERNEL_REMOTE_HOST')
    root = setting(env, 'KERNEL_REMOTE_RECIPE_ROOT')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.@_-]*', host):
        raise ValueError('KERNEL_REMOTE_HOST contains unsupported characters')
    # rsync passes this argument through the remote shell.  Keep the recipe root
    # deliberately conservative even though the SSH mkdir invocation is quoted.
    if not re.fullmatch(r'/[A-Za-z0-9._/-]*', root):
        raise ValueError('KERNEL_REMOTE_RECIPE_ROOT contains unsupported characters')
    path = PurePosixPath(root)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('KERNEL_REMOTE_RECIPE_ROOT must be an absolute clean POSIX path')
    return host, str(path)


def remote_command(env, root, profile):
    forwarded = [f'{key}={env[key]}' for key in ('KTREE', 'KOUT', 'KIMAGE', 'JOBS', 'KBUILD_BUILD_HOST') if env.get(key)]
    # Explicitly bypass the remote checkout's local.env mode.  That file may
    # describe a different builder and must not make this invocation recurse.
    return shlex.join(['env', *forwarded, 'sh', f'{root}/kernel/build.sh', '--local', profile])


def run(profile, env=os.environ):
    if profile not in ('normal', 'diagnostic'):
        raise ValueError('Unknown kernel profile')
    host, root = remote_settings(env)
    recipe = f'{root}/kernel'
    subprocess.run(['ssh', host, shlex.join(['mkdir', '-p', recipe])], check=True)
    subprocess.run(['rsync', '-a', 'kernel/', f'{host}:{recipe}/'], check=True)
    return subprocess.call(['ssh', host, remote_command(env, root, profile)])


if __name__ == '__main__':
    try:
        raise SystemExit(run(sys.argv[1] if len(sys.argv) == 2 else 'normal'))
    except ValueError as error:
        raise SystemExit(str(error))
