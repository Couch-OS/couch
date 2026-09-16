#!/usr/bin/env python3
"""Rewrite the published release tag everywhere from one source file."""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SOURCE = 'tools/release/current-release.txt'
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
          'daemon/couch-updates/src/lib.rs')
# v0.1.0-alpha.<date>.<n>. The leading v is optional because the site's JSON-LD
# softwareVersion omits it, and the bounds keep .dev builds, the <date>.<n>
# placeholders in the flow documents and older undated alphas out.
TAG = re.compile(r'(?<![\w.-])(v?)([0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]{8}\.[0-9]+)(?![\w.-])')


def valid(tag):
    return bool(tag.startswith('v') and TAG.fullmatch(tag))


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


def rewrite(text, tag):
    """Replace every dated tag, keeping each literal's own v prefix."""
    return TAG.sub(lambda match: match.group(1) + tag[1:], text)


def strays(root, tag):
    """Tracked files outside the governed set that carry the published tag."""
    try:
        found = subprocess.run(['git', '-C', str(root), 'grep', '-lI', '-F', '--', tag[1:]],
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
    problems = []
    for name in GOVERNED:
        seen = 0
        for number, line in enumerate(read(root / name).splitlines(), 1):
            for match in TAG.finditer(line):
                seen += 1
                if match.group(2) != tag[1:]:
                    problems.append('%s:%d: %s is not the published %s' % (name, number, match.group(0), tag))
        if not seen:
            problems.append('%s: no release tag left; %s governs its install commands' % (name, SOURCE))
    for name in strays(root, tag) if scan else []:
        problems.append('%s: carries %s but no bump rewrites it; add it to GOVERNED or EXEMPT' % (name, tag))
    return problems


def bump(tag, root=ROOT):
    """Write the tag to the source file and rewrite the governed literals."""
    if not valid(tag):
        raise ValueError('Expected a promotion tag like v0.1.0-alpha.<date>.<n>, got: ' + tag)
    changed = []
    for name in (SOURCE,) + GOVERNED:
        path = root / name
        old = read(path)
        new = tag + '\n' if name == SOURCE else rewrite(old, tag)
        if new != old:
            write(path, new)
            changed.append(name)
    return changed


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    # The example stays a placeholder: a real tag here would be a literal
    # this tool does not govern.
    parser.add_argument('tag', nargs='?', help='the promotion tag, for example v0.1.0-alpha.<date>.<n>')
    parser.add_argument('--check', action='store_true',
                        help='report files that disagree with ' + SOURCE + ' instead of rewriting them')
    args = parser.parse_args()
    if args.check:
        if args.tag:
            parser.error('--check takes no tag; it reads ' + SOURCE)
        failures = check()
        for failure in failures:
            print(failure, file=sys.stderr)
        if failures:
            sys.exit(1)
        print('Release literals match ' + source_tag() + ' in: ' + ', '.join(GOVERNED))
    else:
        if not args.tag:
            parser.error('a tag is required unless --check is given')
        changed = bump(args.tag)
        for name in changed:
            print('updated ' + name)
        if not changed:
            print('already at ' + args.tag)
