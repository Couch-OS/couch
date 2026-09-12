import gzip
import hashlib
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import corresponding_source as source


class Sources(unittest.TestCase):
    def test_source_license_headers_allow_case_and_whitespace_without_accepting_spdx_only(self):
        self.assertTrue(source.complete_mit_grant(b'Permission is hereby granted, free of charge\nThe Software is provided "as is"'))
        self.assertTrue(source.complete_mit_grant(b'PERMISSION IS HEREBY GRANTED, FREE OF CHARGE\nTHE SOFTWARE IS PROVIDED'))
        self.assertFalse(source.complete_mit_grant(b'SPDX-License-Identifier: MIT'))

    def test_notice_collection_uses_published_commit_and_keeps_vendor_unchanged(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); output = root / 'out'; cache = root / 'cache'
            repo = cache / 'fixture--crate.git'; repo.mkdir(parents=True)
            def git(*args): return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL)
            git('init'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@example.invalid')
            (repo / 'LICENSES').mkdir(); (repo / 'LICENSES/MIT.txt').write_text('published notice')
            git('add', '.'); git('commit', '-m', 'published'); commit = git('rev-parse', 'HEAD').decode().strip()
            (repo / 'LICENSES/MIT.txt').write_text('later unrelated notice')
            vendor = output / 'cargo-vendor/fixture-1.0'; vendor.mkdir(parents=True)
            (vendor / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.0"\nrepository="https://github.com/fixture/crate"\nlicense="MIT"\n')
            (vendor / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': commit}}))
            (output / 'cargo.json').write_text(json.dumps({'packages': [{'directory': 'fixture-1.0', 'notice_files': []}]}))
            before = source.tree_hashes(output / 'cargo-vendor')
            result = source.cargo_notices(output, cache, offline=True)
            self.assertTrue(result['complete'])
            self.assertEqual((output / 'cargo-notices/fixture-1.0/LICENSES/MIT.txt').read_text(), 'published notice')
            self.assertEqual(before, source.tree_hashes(output / 'cargo-vendor'))
            (vendor / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': '0' * 40}}))
            with self.assertRaisesRegex(ValueError, 'remain incomplete'):
                source.cargo_notices(output, cache, offline=True)
            self.assertFalse(json.loads((output / 'cargo-notices.json').read_text())['complete'])

    def test_reviewed_supplements_pin_identity_and_never_change_vendor_attribution(self):
        import json
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); vendor = root / 'vendor/example-1.0'; vendor.mkdir(parents=True)
            (vendor / 'Cargo.toml').write_text('[package]\nname="example"\nversion="1.0"\nlicense="MIT"\n')
            (vendor / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': 'a' * 40}}))
            text = root / 'MIT.txt'; text.write_text('Permission is hereby granted, free of charge. THE SOFTWARE IS PROVIDED AS IS.')
            review = {'cargo_toml_sha256': source.sha(vendor / 'Cargo.toml'), 'published_git_commit': 'a' * 40, 'declared_license': 'MIT', 'reason': 'Fixture explicit review', 'files': ['MIT.txt']}
            manifest = {'schema': 1, 'kind': 'couch-reviewed-license-supplements', 'packages': {'example-1.0': review}, 'files': {'MIT.txt': {'sha256': source.sha(text), 'url': 'https://example.invalid/pinned/MIT.txt'}}}
            path = root / 'review.json'; path.write_text(json.dumps(manifest))
            before = source.tree_hashes(vendor)
            result = source.license_supplement(root / 'out', vendor, path)
            self.assertFalse(result['upstream_notice_recovered'])
            self.assertNotIn('copyright_holder', result)
            self.assertEqual(before, source.tree_hashes(vendor))
            text.write_text('different license')
            with self.assertRaisesRegex(ValueError, 'checksum differs'):
                source.license_supplement(root / 'out', vendor, path)
            (vendor / 'Cargo.toml').write_text('[package]\nlicense="GPL-3.0-only"\n')
            with self.assertRaisesRegex(ValueError, 'package identity'):
                source.license_supplement(root / 'out', vendor, path)

    def test_temporary_collisions_preserve_existing_files_and_reports(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / 'source.tar.gz'; collision = root / 'source.tar.gz.download'
            collision.write_bytes(b'belongs to another run')
            with patch.object(source, 'urlopen') as network:
                with self.assertRaises(FileExistsError):
                    source.download('https://example.invalid/source', target, '0' * 128)
                network.assert_not_called()
            self.assertEqual(collision.read_bytes(), b'belongs to another run')
            report = root / 'report.json'; report.write_bytes(b'previous report')
            pending = root / 'report.json.report-new'; pending.write_bytes(b'other report run')
            with self.assertRaises(FileExistsError): source.report(report, {'new': True})
            self.assertEqual(report.read_bytes(), b'previous report')
            self.assertEqual(pending.read_bytes(), b'other report run')
            pending.unlink(); protected = root / 'protected'; protected.write_bytes(b'protected')
            pending.symlink_to(protected)
            with self.assertRaises(FileExistsError): source.report(report, {'new': True})
            self.assertEqual(protected.read_bytes(), b'protected')
            pending.unlink(); report.unlink(); report.symlink_to(protected)
            with self.assertRaisesRegex(ValueError, 'Symlink report'): source.report(report, {'new': True})
            self.assertEqual(protected.read_bytes(), b'protected')

    def test_paths_and_binary_inputs_fail_closed(self):
        for name in ('/root/key', '../key', 'a/../key', 'a\\key', 'a//key'):
            with self.assertRaises(ValueError): source.checked_path(name)
        for name in ('tools/private.img', 'assets/id_rsa', 'ui/foo.so'):
            with self.assertRaises(ValueError): source.source_allowed(name)
        self.assertFalse(source.source_allowed('tools/build/private-key.txt'))
        self.assertFalse(source.source_allowed('scratchpad/session.md'))
        self.assertFalse(source.source_allowed('spike/slint-fb/target/program'))
        self.assertTrue(source.source_allowed('clients/example/src/lib.rs'))

    def test_recipe_checksum_parser_never_executes_shell(self):
        digest = 'a' * 128
        self.assertEqual(source.checksums(f'source="$(touch /tmp/never-execute)"\nsha512sums="\n{digest} code.tar.gz\n"'), {'code.tar.gz': digest})
        for recipe in ('sha512sums="SKIP code.tar.gz"', f'{digest} ../escape'):
            with self.assertRaises(ValueError): source.checksums(recipe)
        with self.assertRaises(ValueError): source.checksums(f'{digest} code\n{"b" * 128} code')

    def test_multimember_apk_metadata_without_payload_extraction(self):
        def member(name, data):
            out = io.BytesIO()
            with tarfile.open(fileobj=out, mode='w') as archive:
                entry = tarfile.TarInfo(name); entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
            return gzip.compress(out.getvalue())
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory) / 'test.apk'
            p.write_bytes(member('.SIGN.RSA.fixture', b'fixture') + member('.PKGINFO', b'pkgname = test\npkgver = 1-r0\norigin = test\n') + member('etc/private', b'never extract'))
            self.assertEqual(source.apk_info(p)['pkgname'], 'test')
            self.assertFalse((Path(directory) / 'etc').exists())

    def test_recipe_symlinks_are_resolved_without_creating_links(self):
        def archive(target):
            out = io.BytesIO()
            with tarfile.open(fileobj=out, mode='w') as tar:
                entry = tarfile.TarInfo('install'); entry.size = 7
                tar.addfile(entry, io.BytesIO(b'fixture'))
                link = tarfile.TarInfo('upgrade'); link.type = tarfile.SYMTYPE; link.linkname = target
                tar.addfile(link)
            return out.getvalue()
        original = archive('install')
        files = source.unpack_recipe(original)
        self.assertEqual(files['upgrade'], b'fixture')
        self.assertEqual(files['.couch-original-recipe.tar'], original)
        for target in ('../outside', '/outside', 'upgrade', 'missing'):
            with self.assertRaises(ValueError): source.unpack_recipe(archive(target))

    def test_assembly_requires_all_source_bytes_not_just_inventories(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'Missing source component'):
                source.assemble(Path(directory), Path(directory) / 'source.tar.gz')
            receipt = Path(directory) / 'receipt.json'
            receipt.write_text('{"schema":1,"kind":"couch-external-source","component":"busybox","files":{},"complete":true}')
            with self.assertRaisesRegex(ValueError, 'source, configuration'):
                source.external_sources(Path(directory), receipt, Path(directory) / 'output')

    def test_cached_upstream_hash_is_checked_even_offline(self):
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory) / 'source.tar.gz'; p.write_bytes(b'fixture')
            digest = hashlib.sha512(b'fixture').hexdigest()
            source.download('https://example.invalid/source.tar.gz', p, digest, True)
            with self.assertRaises(ValueError): source.download('https://example.invalid/source.tar.gz', p, '0' * 128, True)
            with self.assertRaises(ValueError): source.download('http://example.invalid/source.tar.gz', p.with_name('missing'), digest)
            with self.assertRaises(ValueError): source.download('https://example.invalid/source.tar.gz', p.with_name('missing'), digest, True)

    def test_archive_is_deterministic_and_rejects_postcollection_mutation(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'collection'; root.mkdir()
            components = [('project', 'couch'), ('cargo', 'cargo-vendor'), ('cargo-notices', 'cargo-notices'), ('alpine', 'alpine'), ('kernel', 'external/kernel'), ('busybox', 'external/busybox'), ('rust-stdlib', 'external/rust-stdlib')]
            config = root / 'cargo-config/vendor.toml'; config.parent.mkdir(); config.write_text('fixture')
            for name, subdir in components:
                path = root / subdir / 'source.txt'; path.parent.mkdir(parents=True); path.write_text(name)
                value = {'complete': True, 'files': {'source.txt': source.sha(path)}}
                if name == 'project': value.update(commit='a' * 40, source_date_epoch=100)
                if name == 'cargo': value.update(packages=[], config_sha256=source.sha(config))
                if name in ('alpine', 'cargo-notices'): value.update(packages=[])
                (root / (name + '.json')).write_text(json.dumps(value))
            (root / 'private-unlisted.key').write_text('must not publish')
            first = Path(directory) / 'one.tar.gz'; second = Path(directory) / 'two.tar.gz'
            collision = first.with_name(first.name + '.partial'); collision.write_bytes(b'other archive run')
            with self.assertRaises(FileExistsError): source.assemble(root, first)
            self.assertEqual(collision.read_bytes(), b'other archive run')
            self.assertFalse(first.exists()); collision.unlink()
            self.assertTrue(source.assemble(root, first)['complete'])
            source.assemble(root, second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(source.verify_archive(first)["project_commit"], "a" * 40)
            with tarfile.open(first) as archive:
                self.assertNotIn('couch-source/private-unlisted.key', archive.getnames())
                self.assertIn('couch-source/NOTICES.md', archive.getnames())
            corrupt = Path(directory) / 'corrupt.tar.gz'
            with tarfile.open(first) as original, tarfile.open(corrupt, 'w:gz') as altered:
                for entry in original:
                    data = original.extractfile(entry).read()
                    if entry.name.endswith('couch/source.txt'): data = b'changed'
                    entry.size = len(data); altered.addfile(entry, io.BytesIO(data))
            with self.assertRaisesRegex(ValueError, 'differs from'):
                source.verify_archive(corrupt)
            (root / 'couch/source.txt').write_text('changed after receipt')
            with self.assertRaisesRegex(ValueError, 'changed after collection'):
                source.assemble(root, Path(directory) / 'bad.tar.gz')
            self.assertFalse((Path(directory) / 'bad.tar.gz').exists())

    def test_project_exports_exact_git_objects_not_dirty_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); repo = root / 'repo'; repo.mkdir()
            def git(*args): return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL)
            git('init'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@example.invalid')
            (repo / 'src').mkdir(); (repo / 'src/main.c').write_text('original\n'); (repo / 'COPYING').write_text('fixture license')
            (repo / 'scratchpad').mkdir(); (repo / 'scratchpad/private').write_text('excluded')
            git('add', '.'); git('commit', '-m', 'fixture'); commit = git('rev-parse', 'HEAD').decode().strip()
            (repo / 'src/main.c').write_text('dirty secret must not copy')
            output = root / 'output'; result = source.project(repo, commit, output)
            self.assertEqual((output / 'couch/src/main.c').read_text(), 'original\n')
            self.assertFalse((output / 'couch/scratchpad').exists()); self.assertEqual(result['commit'], commit)
            (output / 'couch/src/main.c').write_text('tampered')
            with self.assertRaises(ValueError): source.project(repo, commit, output)


if __name__ == '__main__': unittest.main()
