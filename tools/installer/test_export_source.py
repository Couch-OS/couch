import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import subprocess


SPEC = importlib.util.spec_from_file_location(
    "export_source", Path(__file__).with_name("export_source.py")
)
export_source = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(export_source)


class SourceExportTests(unittest.TestCase):
    def source(self, root):
        installer = root / "tools/installer"
        (installer / "host").mkdir(parents=True)
        (installer / "host/Cargo.toml").write_text("[package]\nname = 'fixture'\n")
        (installer / "pins").mkdir()
        (installer / "pins/ha100.json").write_text('{"schema": 1}\n')
        (installer / "target").mkdir()
        (installer / "target/artifact").write_bytes(b"compiled")
        (installer / "__pycache__").mkdir()
        (installer / "__pycache__/worker.pyc").write_bytes(b"cache")
        (installer / "local.env").write_text("private=true\n")
        (installer / "owner.pem").write_text("private certificate\n")
        (root / "COPYING").write_text("fixture license\n")
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        subprocess.run(
            ["git", "-C", str(root), "add", "COPYING", "tools/installer/host/Cargo.toml", "tools/installer/pins/ha100.json"],
            check=True,
        )
        return installer

    def test_exports_installer_tree_and_records_exact_source_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            source = self.source(root)
            output = Path(temporary) / "export"
            manifest = export_source.export(root, output)
            self.assertTrue((output / "tools/installer/host/Cargo.toml").is_file())
            self.assertTrue((output / "tools/installer/pins/ha100.json").is_file())
            self.assertTrue((output / "COPYING").is_file())
            for name in ("target/artifact", "__pycache__/worker.pyc", "local.env", "owner.pem"):
                self.assertFalse((output / "tools/installer" / name).exists())
            recorded = {entry["path"] for entry in manifest["files"]}
            self.assertEqual(
                recorded,
                {
                    "COPYING",
                    "tools/installer/host/Cargo.toml",
                    "tools/installer/pins/ha100.json",
                },
            )
            receipt = json.loads((output / "SOURCE-EXPORT.json").read_text())
            self.assertTrue(receipt["source_only"])
            self.assertEqual(receipt["files"], manifest["files"])
            self.assertFalse((output / ".git").exists())
            self.assertFalse(receipt["development_untracked_source"])

    def test_opt_in_untracked_source_is_marked_development_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            installer = self.source(root)
            (installer / "new_worker.py").write_text("# development source\n")
            output = Path(temporary) / "export"
            manifest = export_source.export(root, output, include_new_source=True)
            self.assertTrue((output / "tools/installer/new_worker.py").is_file())
            self.assertFalse((output / "tools/installer/local.env").exists())
            self.assertTrue(manifest["development_untracked_source"])

    def test_repository_export_carries_only_installer_workflows_and_records_scaffold(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            self.source(root)
            for name in export_source.REPOSITORY_TEMPLATES:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('reviewed template: ' + name + '\n')
            for name in (*export_source.REPOSITORY_WORKFLOWS, '.github/workflows/runtime.yml'):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('name: fixture\n')
            subprocess.run(['git', '-C', str(root), 'add', 'tools/installer/repository', '.github'], check=True)
            output = Path(temporary) / 'repository'
            manifest = export_source.export(root, output, repository=True)
            self.assertTrue(manifest['repository_scaffold'])
            self.assertFalse((output / '.github/workflows/runtime.yml').exists())
            self.assertFalse((output / '.git').exists())
            for name in ('README.md', '.gitignore', '.gitattributes', *export_source.REPOSITORY_WORKFLOWS):
                record = next(item for item in manifest['files'] if item['path'] == name)
                self.assertEqual(record['sha256'], export_source.digest(output / name))
            subprocess.run(['git', 'init', '-q', str(output)], check=True)
            subprocess.run(['git', '-C', str(output), 'add', '.'], check=True)
            reexport = export_source.export(output, Path(temporary) / 'reexport', repository=True)
            self.assertEqual(reexport['files'], manifest['files'])

    def test_repository_export_refuses_unselected_templates_before_creating_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'
            self.source(root)
            output = Path(temporary) / 'repository'
            with self.assertRaisesRegex(ValueError, 'template is not selected'):
                export_source.export(root, output, repository=True)
            self.assertFalse(output.exists())

    def test_rejects_existing_or_in_tree_outputs_and_source_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            installer = self.source(root)
            with self.assertRaisesRegex(ValueError, "outside the source root"):
                export_source.export(root, root / "export")
            output = Path(temporary) / "output"
            output.mkdir()
            with self.assertRaisesRegex(ValueError, "new directory"):
                export_source.export(root, output)
            output.rmdir()
            (installer / "linked-source").symlink_to("host/Cargo.toml")
            subprocess.run(
                ["git", "-C", str(root), "add", "tools/installer/linked-source"], check=True
            )
            with self.assertRaisesRegex(ValueError, "refuses symlink"):
                export_source.export(root, output)


if __name__ == "__main__":
    unittest.main()
