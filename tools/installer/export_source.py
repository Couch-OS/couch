#!/usr/bin/env python3
"""Export the complete, source-only native installer closure.

The output keeps the repository-relative ``tools/installer`` layout so each
locked Rust workspace can build without the surrounding Couch checkout.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess


EXCLUDED_DIRECTORIES = frozenset({".git", "target", "__pycache__"})
EXCLUDED_SUFFIXES = frozenset({".pyc", ".pyo", ".swp", ".tmp"})
PRIVATE_BASENAMES = frozenset(
    {
        ".env",
        "local.env",
        "local.json",
        "credentials",
        "credentials.json",
        "id_ed25519",
        "id_rsa",
        "known_hosts",
    }
)
PRIVATE_SUFFIXES = frozenset({".key", ".pem", ".p12", ".pfx", ".kdbx"})
REPOSITORY_TEMPLATES = {
    'tools/installer/repository/README.md': 'README.md',
    'tools/installer/repository/gitignore': '.gitignore',
    'tools/installer/repository/gitattributes': '.gitattributes',
}
REPOSITORY_WORKFLOWS = (
    '.github/workflows/installer-binaries.yml',
    '.github/workflows/installer-windows-launcher-acceptance.yml',
)


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def excluded(relative):
    if any(part in EXCLUDED_DIRECTORIES for part in relative.parts):
        return True
    name = relative.name.lower()
    return (
        name in PRIVATE_BASENAMES
        or relative.suffix.lower() in EXCLUDED_SUFFIXES
        or relative.suffix.lower() in PRIVATE_SUFFIXES
        or name == ".ds_store"
    )


def git_source_files(source_root, source, include_new_source):
    """Return the reviewed installer tree, with opt-in development additions."""
    commands = [["git", "-C", str(source_root), "ls-files", "-z", "--", "tools/installer"]]
    if include_new_source:
        commands.append(
            [
                "git",
                "-C",
                str(source_root),
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                "tools/installer",
            ]
        )
    names = set()
    for command in commands:
        result = subprocess.run(command, check=True, capture_output=True)
        names.update(name for name in result.stdout.decode().split("\0") if name)
    result = []
    for name in sorted(names):
        path = source_root / name
        try:
            relative = path.relative_to(source)
        except ValueError as error:
            raise ValueError("Git reported a path outside tools/installer") from error
        if excluded(relative):
            continue
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise ValueError(f"source export refuses symlink: {relative}")
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError(f"source export refuses non-regular file: {relative}")
        result.append(path)
    return result


def root_source_files(source_root):
    """Return the repository license texts required with exported source."""
    result = subprocess.run(
        ["git", "-C", str(source_root), "ls-files", "-z", "--", "COPYING", "LICENSE"],
        check=True,
        capture_output=True,
    )
    files = []
    for name in sorted(name for name in result.stdout.decode().split("\0") if name):
        path = source_root / name
        mode = path.lstat().st_mode
        if not stat.S_ISREG(mode):
            raise ValueError(f"source export refuses non-regular license text: {name}")
        files.append(path)
    return files


def export(source_root, output, *, include_new_source=False, repository=False):
    source_root, output = Path(source_root).resolve(), Path(output).resolve()
    source = source_root / "tools" / "installer"
    if not source.is_dir() or source.is_symlink():
        raise ValueError("source root must contain a regular tools/installer directory")
    if output.exists() or output.is_symlink():
        raise ValueError("source export output must be a new directory")
    if output.is_relative_to(source_root):
        raise ValueError("source export output must be outside the source root")

    files = git_source_files(source_root, source, include_new_source)
    scaffold = []
    if repository:
        selected = {path.relative_to(source_root).as_posix() for path in files}
        for name, target in REPOSITORY_TEMPLATES.items():
            if name not in selected:
                raise ValueError('Repository template is not selected source: ' + name)
            scaffold.append((source_root / name, target))
        tracked = subprocess.check_output(
            ['git', '-C', str(source_root), 'ls-files', '-z', '--', *REPOSITORY_WORKFLOWS]
        ).decode().split('\0')
        for name in REPOSITORY_WORKFLOWS:
            path = source_root / name
            if name not in tracked or not stat.S_ISREG(path.lstat().st_mode):
                raise ValueError('Repository workflow must be tracked regular source: ' + name)
            scaffold.append((path, name))
    destination = output / "tools" / "installer"
    destination.mkdir(parents=True)
    inventory = []
    for path in files:
        relative = path.relative_to(source)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        target.chmod(path.stat().st_mode & 0o777)
        inventory.append(
            {
                "path": (Path("tools") / "installer" / relative).as_posix(),
                "size": target.stat().st_size,
                "sha256": digest(target),
            }
        )
    for path in root_source_files(source_root):
        target = output / path.name
        shutil.copyfile(path, target)
        target.chmod(path.stat().st_mode & 0o777)
        inventory.append(
            {
                "path": path.name,
                "size": target.stat().st_size,
                "sha256": digest(target),
            }
        )
    for path, name in scaffold:
        target = output / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        inventory.append({'path': name, 'size': target.stat().st_size, 'sha256': digest(target)})
    manifest = {
        "schema": 1,
        "kind": "couch-installer-source-export",
        "source_only": True,
        "development_untracked_source": include_new_source,
        "repository_scaffold": repository,
        "files": inventory,
    }
    (output / "SOURCE-EXPORT.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_root", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument('--repository', action='store_true',
                        help='include standalone README, ignore and line-ending rules, and installer CI workflows')
    parser.add_argument(
        "--include-new-source",
        action="store_true",
        help="include untracked, non-ignored installer source for local development only",
    )
    args = parser.parse_args()
    manifest = export(args.source_root, args.output, include_new_source=args.include_new_source,
                      repository=args.repository)
    print(f"Exported {len(manifest['files'])} installer source files.")


if __name__ == "__main__":
    main()
