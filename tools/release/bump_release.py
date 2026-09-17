#!/usr/bin/env python3
"""Update published installer commands after an installer release is available.

The legacy source path remains the website's published installer pointer. Runtime
promotions do not call this tool; installer build versions live in installer/VERSION.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SOURCE = 'tools/release/current-release.txt'
REPOSITORY_SOURCE = 'tools/release/installer-repository.txt'
REPOSITORIES = ('dangerouslaser/couch', 'dangerouslaser/couch-installer')
DOWNLOAD_BASE = re.compile(r'https://github\.com/dangerouslaser/(?:couch|couch-installer)/releases/download/')
# Files whose dated tags are all the published release: the install commands a
# reader copies. The separately published site reads this source file from its
# pinned Couch checkout during its own build, so it has no cross-repository
# literal to rewrite here.
GOVERNED = ('README.md', 'docs/installer.md')
# Dated tags here are ordering examples and test fixtures, not install
# instructions, so a bump has to leave them alone.
EXEMPT = (SOURCE, 'tools/installer/couch_tui.py', 'tools/installer/release_discovery.py',
          'tools/installer/test_release_discovery.py',
          # Version-rendering and boot-release tests: sample tags, not install
          # instructions, and one of them is whatever number a promotion takes.
          'daemon/couch-updates/src/lib.rs',
          # Historical decoder vocabulary used to validate first-upgrade rollback.
          'model/couch-model/src/storage.rs')
# v0.1.0-alpha.<date>.<n>. The leading v is optional because the site's JSON-LD
# softwareVersion omits it, and the bounds keep .dev builds, the <date>.<n>
# placeholders in the flow documents and older undated alphas out.
TAG = re.compile(r'(?<![\w.-])(v?)([0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]{8}\.[0-9]+)(?![\w.-])')
INSTALLER_TAG = re.compile(r'(?<![\w.-])installer-v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:[.-][A-Za-z0-9]+)*)?(?![\w.-])')


def valid(tag):
    return bool((tag.startswith('v') and TAG.fullmatch(tag)) or INSTALLER_TAG.fullmatch(tag))


def read(path):
    return path.read_text(encoding='utf-8')


def write(path, text):
    # Explicit newline: a bump on Windows must not rewrite every line ending.
    path.write_text(text, encoding='utf-8', newline='\n')


def source_tag(root=ROOT):
    tag = read(root / SOURCE).strip()
    if not valid(tag):
        raise ValueError(SOURCE + ' does not hold a release tag: ' + tag)
    return tag


def source_repository(root=ROOT):
    path = root / REPOSITORY_SOURCE
    repository = read(path).strip() if path.exists() else REPOSITORIES[0]
    if repository not in REPOSITORIES:
        raise ValueError('Unsupported published installer repository')
    return repository


def rewrite(text, tag):
    """Replace every dated tag, keeping each literal's own v prefix."""
    if tag.startswith('installer-'):
        # One combined pass prevents the legacy matcher from rewriting a dated
        # alpha inside a newly inserted independent installer tag.
        combined = re.compile(INSTALLER_TAG.pattern + '|' + TAG.pattern)
        return combined.sub(lambda match: tag, text)
    text = INSTALLER_TAG.sub(lambda match: tag, text)
    return TAG.sub(lambda match: match.group(1) + tag[1:], text)


def strays(root, tag):
    """Tracked files outside the governed set that carry the published tag."""
    try:
        # Independent versions also appear in schema examples and tooling help.
        # Only published download URLs outside the governed files are strays.
        needle = '/releases/download/' + tag + '/' if tag.startswith('installer-') else tag.removeprefix('v')
        found = subprocess.run(['git', '-C', str(root), 'grep', '-lI', '-F', '--', needle],
                               capture_output=True, text=True, check=False)
    except OSError:
        found = None
    # No git and no work tree mean no file list; the governed files are still
    # checked, which is the part a promotion actually depends on.
    if found is None or found.returncode > 1:
        return []
    return sorted(set(found.stdout.splitlines()) - set(GOVERNED) - set(EXEMPT))


def check(root=ROOT, scan=True):
    """Return one message per literal that disagrees with the source file."""
    tag = source_tag(root)
    expected_base = 'https://github.com/' + source_repository(root) + '/releases/download/'
    problems = []
    for name in GOVERNED:
        seen = 0
        for number, line in enumerate(read(root / name).splitlines(), 1):
            for base in DOWNLOAD_BASE.finditer(line):
                if base.group(0) != expected_base:
                    problems.append('%s:%d: installer download repository differs from %s' % (name, number, REPOSITORY_SOURCE))
            combined = re.compile(INSTALLER_TAG.pattern + '|' + TAG.pattern)
            for match in combined.finditer(line):
                seen += 1
                if match.group(0).removeprefix('v') != tag.removeprefix('v'):
                    problems.append('%s:%d: %s is not the published %s' % (name, number, match.group(0), tag))
        if not seen:
            problems.append('%s: no release tag left; %s governs its install commands' % (name, SOURCE))
    for name in strays(root, tag) if scan else []:
        problems.append('%s: carries %s but no bump rewrites it; add it to GOVERNED or EXEMPT' % (name, tag))
    return problems


def bump(tag, root=ROOT, repository=None):
    """Write the tag to the source file and rewrite the governed literals."""
    if not valid(tag):
        raise ValueError('Expected an installer tag (installer-v0.1.0) or legacy promotion tag, got: ' + tag)
    selected_repository = source_repository(root) if repository is None else repository
    if selected_repository not in REPOSITORIES:
        raise ValueError('Unsupported published installer repository')
    if selected_repository != REPOSITORIES[0] and not tag.startswith('installer-'):
        raise ValueError('Separate installer repository requires an installer-v tag')
    changed = []
    for name in (SOURCE,) + GOVERNED:
        path = root / name
        old = read(path)
        new = tag + '\n' if name == SOURCE else DOWNLOAD_BASE.sub(
            'https://github.com/' + selected_repository + '/releases/download/', rewrite(old, tag))
        if new != old:
            write(path, new)
            changed.append(name)
    if repository is not None:
        path = root / REPOSITORY_SOURCE
        new = selected_repository + '\n'
        if not path.exists() or read(path) != new:
            write(path, new)
            changed.append(REPOSITORY_SOURCE)
    return changed


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    # The example stays a placeholder: a real tag here would be a literal
    # this tool does not govern.
    parser.add_argument('tag', nargs='?', help='the published installer tag, for example installer-v0.1.0')
    parser.add_argument('--check', action='store_true',
                        help='report files that disagree with ' + SOURCE + ' instead of rewriting them')
    parser.add_argument('--repository', choices=REPOSITORIES,
                        help='published installer repository; retained separately from the release tag')
    args = parser.parse_args()
    if args.check:
        if args.tag or args.repository:
            parser.error('--check takes no tag or repository; it reads the published pointers')
        failures = check()
        for failure in failures:
            print(failure, file=sys.stderr)
        if failures:
            sys.exit(1)
        print('Release literals match ' + source_tag() + ' in: ' + ', '.join(GOVERNED))
    else:
        if not args.tag:
            parser.error('a tag is required unless --check is given')
        changed = bump(args.tag, repository=args.repository)
        for name in changed:
            print('updated ' + name)
        if not changed:
            print('already at ' + args.tag)
