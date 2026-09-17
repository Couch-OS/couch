import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


MODULE = Path(__file__).resolve().parent / 'source/corresponding_source.py'
SPEC = importlib.util.spec_from_file_location('installer_corresponding_source', MODULE)
source = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(source)


class InstallerCorrespondingSource(unittest.TestCase):
    def make_repo(self, root):
        repo = root / 'repo'
        repo.mkdir()

        def git(*args):
            return subprocess.check_output(
                ['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL)

        git('init')
        git('config', 'user.name', 'Fixture')
        git('config', 'user.email', 'fixture@example.invalid')
        files = {
            'COPYING': 'fixture license\n',
            'README.md': 'installer source fixture\n',
            '.gitignore': 'target\n',
            '.github/workflows/installer-binaries.yml': 'name: installer\n',
            '.github/workflows/installer-windows-launcher-acceptance.yml': 'name: acceptance\n',
            'tools/installer/worker.py': 'print("committed")\n',
            'daemon/Cargo.toml': '[package]\nname="outside-scope"\nversion="0.1.0"\n',
        }
        for index, manifest in enumerate(source.MANIFESTS):
            files[manifest] = (
                f'[package]\nname="installer-{index}"\nversion="0.1.0"\n')
            files[str(Path(manifest).parent / 'Cargo.lock')] = (
                'version = 3\n[[package]]\nname="fixture"\nversion="0.1.0"\n')
        for name, contents in files.items():
            path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents)
        git('add', '.')
        git('commit', '-m', 'fixture')
        return repo, git('rev-parse', 'HEAD').decode().strip()

    def make_collection(self, root):
        collection = root / 'collection'
        collection.mkdir()
        config = collection / 'cargo-config/vendor.toml'
        config.parent.mkdir()
        config.write_text('fixture')
        kinds = {
            'project': 'couch-project-source',
            'cargo': 'couch-cargo-sources',
            'cargo-notices': 'couch-cargo-notices',
        }
        for name, subdirectory in (
                ('project', 'couch'), ('cargo', 'cargo-vendor'),
                ('cargo-notices', 'cargo-notices')):
            item = collection / subdirectory / 'source.txt'
            item.parent.mkdir(parents=True)
            item.write_text(name)
            receipt = {
                'schema': 1, 'kind': kinds[name], 'scope': 'installer',
                'complete': True, 'files': {'source.txt': source.sha(item)},
            }
            if name == 'project':
                receipt.update(commit='a' * 40, source_date_epoch=100)
                for manifest in source.MANIFESTS:
                    manifest_path = collection / 'couch' / manifest
                    manifest_path.parent.mkdir(parents=True, exist_ok=True)
                    manifest_path.write_text(
                        '[package]\nname="fixture"\nversion="0.1.0"\n')
                    receipt['files'][manifest] = source.sha(manifest_path)
                    lock = str(Path(manifest).parent / 'Cargo.lock')
                    lock_path = collection / 'couch' / lock
                    lock_path.write_text('version = 3\n')
                    receipt['files'][lock] = source.sha(lock_path)
            elif name == 'cargo':
                receipt.update(
                    packages=[], manifests=list(source.MANIFESTS),
                    locks=[str(Path(item).parent / 'Cargo.lock')
                           for item in source.MANIFESTS],
                    config_sha256=source.sha(config))
            else:
                receipt.update(packages=[], errors=[])
            (collection / f'{name}.json').write_text(json.dumps(receipt))
        return collection

    def add_rust_component(self, collection):
        directory = collection / 'external/rust-stdlib'
        directory.mkdir(parents=True)
        files = {}
        for filename in (
                'rust-src.tar.xz', 'configuration.json', 'build.sh',
                'toolchain.json'):
            path = directory / filename
            path.write_text(filename)
            files[filename] = source.sha(path)
        receipt = {
            'schema': 1, 'kind': 'couch-external-source',
            'scope': 'installer', 'component': 'rust-stdlib',
            'complete': True, 'source_archive': 'rust-src.tar.xz',
            'configuration': 'configuration.json', 'build_recipe': 'build.sh',
            'toolchain_receipt': 'toolchain.json',
            'binary_sha256': 'b' * 64, 'rust_release': '1.90.0',
            'rust_commit': 'c' * 40, 'files': files,
        }
        (collection / 'rust-stdlib.json').write_text(json.dumps(receipt))

    def test_project_uses_only_exact_committed_installer_scope(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, commit = self.make_repo(root)
            (repo / 'tools/installer/worker.py').write_text('dirty bytes\n')
            with self.assertRaisesRegex(ValueError, 'full Git commit'):
                source.project(repo, 'HEAD', root / 'symbolic')
            output = root / 'output'
            receipt = source.project(repo, commit, output)
            self.assertEqual(receipt['scope'], 'installer')
            self.assertEqual(receipt['commit'], commit)
            self.assertEqual(
                (output / 'couch/tools/installer/worker.py').read_text(),
                'print("committed")\n')
            self.assertFalse((output / 'couch/daemon').exists())
            self.assertTrue(
                (output / 'couch/.github/workflows/installer-binaries.yml').is_file())

    def test_cargo_requires_each_workspace_direct_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, commit = self.make_repo(root)
            output = root / 'output'
            source.project(repo, commit, output)
            missing = output / 'couch/tools/installer/linux_stage/storage/Cargo.lock'
            missing.unlink()
            with self.assertRaisesRegex(ValueError, 'storage/Cargo.lock'):
                source.cargo_sources(output, offline=True)

    def test_external_component_is_rust_only_and_hash_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            component = root / 'component'
            component.mkdir()
            names = (
                'rust-src.tar.xz', 'configuration.json', 'build.sh',
                'toolchain.json')
            for name in names:
                (component / name).write_text(name)
            receipt = {
                'schema': 1, 'kind': 'couch-external-source',
                'component': 'rust-stdlib', 'source_archive': names[0],
                'configuration': names[1], 'build_recipe': names[2],
                'toolchain_receipt': names[3], 'binary_sha256': 'a' * 64,
                'rust_release': '1.90.0', 'rust_commit': 'b' * 40,
                'files': {name: source.sha(component / name) for name in names},
            }
            receipt_path = component / 'receipt.json'
            receipt_path.write_text(json.dumps(receipt))
            imported = source.external_rust_sources(
                component, receipt_path, root / 'output')
            self.assertEqual(imported['scope'], 'installer')
            receipt['component'] = 'kernel'
            receipt_path.write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ValueError, 'only a Rust'):
                source.external_rust_sources(
                    component, receipt_path, root / 'wrong-component')
            receipt['component'] = 'rust-stdlib'
            receipt_path.write_text(json.dumps(receipt))
            (component / names[0]).write_text('tampered')
            with self.assertRaisesRegex(ValueError, 'checksum differs'):
                source.external_rust_sources(
                    component, receipt_path, root / 'tampered')

    def test_archive_requires_rust_and_rejects_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            collection = self.make_collection(root)
            archive = root / 'installer-source.tar.gz'
            with self.assertRaisesRegex(ValueError, 'rust-stdlib'):
                source.assemble(collection, archive)
            self.add_rust_component(collection)
            result = source.assemble(collection, archive)
            self.assertEqual(result['scope'], 'installer')
            self.assertFalse(result['os_source_covered'])
            self.assertFalse(result['native_platform_toolchains_verified'])
            self.assertEqual(source.verify_archive(archive)['scope'], 'installer')

            corrupt = root / 'corrupt.tar.gz'
            with tarfile.open(archive, 'r:gz') as original, \
                    tarfile.open(corrupt, 'w:gz') as altered:
                for entry in original:
                    data = original.extractfile(entry).read()
                    if entry.name.endswith('couch/source.txt'):
                        data = b'changed'
                    entry.size = len(data)
                    altered.addfile(entry, io.BytesIO(data))
            with self.assertRaisesRegex(ValueError, 'differs from'):
                source.verify_archive(corrupt)


if __name__ == '__main__':
    unittest.main()
