import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import corresponding_source as source


class Sources(unittest.TestCase):
    def test_installer_project_exports_exact_scoped_git_objects(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); repo = root / 'repo'; repo.mkdir()
            def git(*args): return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL)
            git('init'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@example.invalid')
            files = {
                    'COPYING': 'license\n', 'README.md': 'build the installer\n', '.gitignore': 'target\n',
                    '.github/workflows/installer-binaries.yml': 'name: installer\n',
                    '.github/workflows/installer-windows-launcher-acceptance.yml': 'name: acceptance\n',
                    'tools/installer/worker.py': 'print("tracked")\n',
                    'model/Cargo.toml': '[package]\nname="excluded"\nversion="0.1.0"\n'}
            for index, manifest in enumerate(source.INSTALLER_MANIFESTS):
                files[manifest] = f'[package]\nname="installer-{index}"\nversion="0.1.0"\n'
            for name, data in files.items():
                path = repo / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_text(data)
            git('add', '.'); git('commit', '-m', 'fixture'); commit = git('rev-parse', 'HEAD').decode().strip()
            (repo / 'tools/installer/worker.py').write_text('dirty bytes must not escape\n')
            output = root / 'output'; result = source.project(repo, commit, output, 'installer')
            self.assertEqual(result['scope'], 'installer')
            self.assertEqual((output / 'couch/tools/installer/worker.py').read_text(), 'print("tracked")\n')
            self.assertTrue((output / 'couch/.github/workflows/installer-binaries.yml').is_file())
            self.assertFalse((output / 'couch/model').exists())
            self.assertEqual(set(result['files']), {
                'COPYING', 'README.md', '.gitignore',
                '.github/workflows/installer-binaries.yml',
                '.github/workflows/installer-windows-launcher-acceptance.yml',
                *source.INSTALLER_MANIFESTS, 'tools/installer/worker.py'})

    def test_installer_cargo_requires_and_records_all_four_locked_workspaces(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory); root = output / 'couch'
            (output / 'project.json').write_text(json.dumps({
                'schema': 1, 'kind': 'couch-project-source', 'scope': 'installer',
                'complete': True}))
            for name in source.INSTALLER_MANIFESTS[:-1]:
                parent = root / Path(name).parent; parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
                (parent / 'Cargo.lock').write_text('version = 3\n[[package]]\nname="fixture"\nversion="0.1.0"\n')
            with self.assertRaisesRegex(ValueError, 'storage/Cargo.toml'):
                source.cargo_sources(output, scope='installer')
            name = source.INSTALLER_MANIFESTS[-1]
            parent = root / Path(name).parent; parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
            with self.assertRaisesRegex(ValueError, 'storage/Cargo.lock'):
                source.cargo_sources(output, scope='installer')
            (parent / 'Cargo.lock').write_text('version = 3\n[[package]]\nname="fixture"\nversion="0.1.0"\n')
            vendor = output / 'cargo-vendor/fixture-1.0'; vendor.mkdir(parents=True)
            (vendor / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.0"\nlicense="MIT"\n')
            with patch.object(source.INSTALLER_SOURCE.subprocess, 'check_output', return_value=b'[source.crates-io]\n'), \
                    patch.object(source.INSTALLER_SOURCE.subprocess, 'run') as metadata:
                result = source.cargo_sources(output, scope='installer')
            self.assertEqual(result['scope'], 'installer')
            self.assertEqual(result['manifests'], list(source.INSTALLER_MANIFESTS))
            self.assertEqual(metadata.call_count, 4)

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
        self.assertFalse(source.source_allowed('local.env'))
        self.assertTrue(source.source_allowed('local.env.example'))
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

    def test_installer_external_scope_accepts_only_audited_rust_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); component = root / 'component'; component.mkdir()
            names = ('rust-src.tar.xz', 'configuration.json', 'build.sh', 'toolchain.json')
            for name in names:
                (component / name).write_text(name)
            receipt = {'schema': 1, 'kind': 'couch-external-source', 'component': 'rust-stdlib',
                       'source_archive': names[0], 'configuration': names[1],
                       'build_recipe': names[2], 'toolchain_receipt': names[3],
                       'binary_sha256': 'a' * 64, 'rust_release': '1.90.0',
                       'rust_commit': 'b' * 40,
                       'files': {name: source.sha(component / name) for name in names}}
            path = component / 'receipt.json'; path.write_text(json.dumps(receipt))
            result = source.external_sources(component, path, root / 'output', 'installer')
            self.assertEqual(result['scope'], 'installer')
            self.assertTrue((root / 'output/external/rust-stdlib/rust-src.tar.xz').is_file())
            receipt['component'] = 'kernel'; path.write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ValueError, 'only a Rust'):
                source.external_sources(component, path, root / 'other', 'installer')

    def test_bluez_component_carries_every_recipe_input_and_refuses_drift(self):
        import json
        import collect_external_sources as collect
        with tempfile.TemporaryDirectory() as directory:
            build = Path(directory) / 'build'
            contents = {'bluez-5.79.tar.xz': b'tarball', 'build.sh': b'#!/bin/sh', 'README.md': b'why',
                        '0001-fix.patch': b'patch', 'aports/APKBUILD': b'recipe', 'aports/local.patch': b'alpine patch'}
            for name, data in contents.items():
                path = build / 'source' / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(data)
            (build / 'couch-bluetoothd').write_bytes(b'\x7fELF binary')
            receipt = {'schema': 1, 'kind': 'couch-bluez-build', 'binary': 'couch-bluetoothd',
                       'binary_sha256': hashlib.sha256(b'\x7fELF binary').hexdigest(), 'bluez_version': '5.79',
                       'bluez_url': 'https://example.invalid/bluez-5.79.tar.xz', 'bluez_sha256': hashlib.sha256(b'tarball').hexdigest(),
                       'aports_commit': 'a' * 40, 'aports_recipe': 'main/bluez', 'aports_patches': ['local.patch'],
                       'couch_patches': ['0001-fix.patch'], 'container': 'alpine@sha256:' + 'b' * 64, 'platform': 'linux/arm/v7',
                       'pinned_packages': [], 'packages': [], 'needed': [], 'cflags': '-Os', 'ldflags': '',
                       'source_files': {name: hashlib.sha256(data).hexdigest() for name, data in contents.items()}}
            (build / 'build.json').write_text(json.dumps(receipt))
            component = Path(directory) / 'component'
            result = collect.bluez(build, component)
            self.assertEqual(result['source_archive'], 'bluez-5.79.tar.xz')
            imported = source.external_sources(component, component / 'receipt.json', Path(directory) / 'release')
            self.assertEqual(imported['binary_sha256'], receipt['binary_sha256'])
            self.assertTrue((Path(directory) / 'release/external/bluez/aports/local.patch').is_file())
            self.assertFalse((Path(directory) / 'release/external/bluez/couch-bluetoothd').exists())
            (build / 'source/0001-fix.patch').write_bytes(b'changed after the build')
            with self.assertRaisesRegex(ValueError, 'differs from its build receipt'):
                collect.bluez(build, Path(directory) / 'again')

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
            components = [('project', 'couch'), ('cargo', 'cargo-vendor'), ('cargo-notices', 'cargo-notices'), ('alpine', 'alpine'), ('kernel', 'external/kernel'), ('busybox', 'external/busybox'), ('rust-stdlib', 'external/rust-stdlib'), ('bluez', 'external/bluez')]
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

    def test_installer_archive_is_distinct_and_cannot_satisfy_full_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'collection'; root.mkdir()
            config = root / 'cargo-config/vendor.toml'; config.parent.mkdir(); config.write_text('fixture')
            for name, subdir in [('project', 'couch'), ('cargo', 'cargo-vendor'),
                                 ('cargo-notices', 'cargo-notices')]:
                path = root / subdir / 'source.txt'; path.parent.mkdir(parents=True); path.write_text(name)
                kinds = {'project': 'couch-project-source', 'cargo': 'couch-cargo-sources',
                         'cargo-notices': 'couch-cargo-notices'}
                value = {'schema': 1, 'kind': kinds[name], 'scope': 'installer', 'complete': True,
                         'files': {'source.txt': source.sha(path)}}
                if name == 'project':
                    value.update(commit='a' * 40, source_date_epoch=100)
                    for manifest in source.INSTALLER_MANIFESTS:
                        manifest_path = root / 'couch' / manifest
                        manifest_path.parent.mkdir(parents=True, exist_ok=True)
                        manifest_path.write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
                        value['files'][manifest] = source.sha(manifest_path)
                        lock = str(Path(manifest).parent / 'Cargo.lock')
                        lock_path = root / 'couch' / lock; lock_path.write_text('version = 3\n')
                        value['files'][lock] = source.sha(lock_path)
                if name == 'cargo':
                    value.update(packages=[], manifests=list(source.INSTALLER_MANIFESTS),
                                 locks=[str(Path(item).parent / 'Cargo.lock')
                                        for item in source.INSTALLER_MANIFESTS],
                                 config_sha256=source.sha(config))
                if name == 'cargo-notices': value.update(packages=[], errors=[])
                (root / (name + '.json')).write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, 'rust-stdlib'):
                source.assemble(root, Path(directory) / 'missing-rust.tar.gz', 'installer')
            rust_directory = root / 'external/rust-stdlib'; rust_directory.mkdir(parents=True)
            rust_files = {}
            for filename in ('rust-src.tar.xz', 'configuration.json', 'build.sh', 'toolchain.json'):
                path = rust_directory / filename; path.write_text(filename)
                rust_files[filename] = source.sha(path)
            (root / 'rust-stdlib.json').write_text(json.dumps({
                'schema': 1, 'kind': 'couch-external-source', 'scope': 'installer',
                'component': 'rust-stdlib', 'complete': True,
                'source_archive': 'rust-src.tar.xz', 'configuration': 'configuration.json',
                'build_recipe': 'build.sh', 'toolchain_receipt': 'toolchain.json',
                'binary_sha256': 'b' * 64, 'rust_release': '1.90.0',
                'rust_commit': 'c' * 40, 'files': rust_files}))
            archive = Path(directory) / 'installer-source.tar.gz'
            result = source.assemble(root, archive, 'installer')
            self.assertEqual(result['scope'], 'installer')
            verified = source.verify_archive(archive)
            self.assertEqual(verified['scope'], 'installer')
            with tarfile.open(archive) as packaged:
                manifest = json.load(packaged.extractfile('couch-installer-source/SOURCE-MANIFEST.json'))
                self.assertEqual(manifest['kind'], 'couch-installer-corresponding-source-archive')
                self.assertEqual(manifest['scope'], 'installer')
                self.assertFalse(manifest['os_source_covered'])
                self.assertTrue(manifest['rust_source_component_included'])
                self.assertFalse(manifest['native_platform_toolchains_verified'])
                self.assertNotIn('alpine.json', manifest['files'])
            with self.assertRaisesRegex(ValueError, 'scope differs'):
                source.assemble(root, Path(directory) / 'false-full.tar.gz', 'full')
            self.assertFalse((Path(directory) / 'false-full.tar.gz').exists())

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
