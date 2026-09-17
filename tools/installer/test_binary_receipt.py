import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from binary_receipt import TARGETS, file_receipt, native, read_installer_version, universal


class ReceiptTests(unittest.TestCase):
    def fixture(self, root):
        source = "a" * 40
        version = "v0.1.0"
        inputs, output = root / "inputs", root / "universal"
        output.mkdir()
        for component in ("host", "tui"):
            (output / f"couch-installer-{component}").write_bytes(b"combined fixture")
        for platform in ("macos-x64", "macos-arm64"):
            binaries = {}
            for component in ("host", "tui"):
                path = inputs / f"couch-installer-{component}-{platform}" / f"couch-installer-{component}"
                path.parent.mkdir(parents=True)
                path.write_bytes(f"{platform} {component}".encode())
                binaries[component] = file_receipt(path)
            receipt = {"schema": 1, "kind": "couch-installer-native-build", "source_commit": source,
                       "installer_version": version,
                       "platform": platform, "target": TARGETS[platform], "binaries": binaries,
                       "toolchain": {"rustc_verbose": "fixture compiler"}}
            path = inputs / f"couch-installer-build-{platform}" / "build.json"
            path.parent.mkdir()
            path.write_text(json.dumps(receipt))
        return source, version, inputs, output

    def test_universal_binds_exact_native_receipts_and_binary_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            source, version, inputs, output = self.fixture(Path(directory))
            receipt = universal(source, inputs, output, version)
            self.assertEqual(receipt["source_commit"], source)
            self.assertEqual(receipt["installer_version"], version)
            for platform, saved in receipt["inputs"].items():
                self.assertEqual(saved["receipt"], file_receipt(inputs / f"couch-installer-build-{platform}" / "build.json"))
                self.assertEqual(saved["build"]["toolchain"]["rustc_verbose"], "fixture compiler")
            self.assertEqual(receipt["binaries"]["host"], file_receipt(output / "couch-installer-host"))
            path = inputs / "couch-installer-host-macos-arm64" / "couch-installer-host"
            path.write_bytes(b"altered artifact")
            with self.assertRaisesRegex(ValueError, "differs"):
                universal(source, inputs, output, version)

    def test_universal_refuses_mixed_source_or_architecture_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            source, version, inputs, output = self.fixture(Path(directory))
            path = inputs / "couch-installer-build-macos-x64" / "build.json"
            original = json.loads(path.read_text())
            for key, value in [("source_commit", "b" * 40), ("target", TARGETS["macos-arm64"]),
                               ("platform", "macos-arm64")]:
                changed = dict(original, **{key: value})
                path.write_text(json.dumps(changed))
                with self.assertRaisesRegex(ValueError, "differs"):
                    universal(source, inputs, output, version)
            original["installer_version"] = "v0.1.1"
            path.write_text(json.dumps(original))
            with self.assertRaisesRegex(ValueError, "differs"):
                universal(source, inputs, output, version)

    def test_native_records_explicit_toolchain_and_sysroot_hashes_without_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compiler, cargo, libdir = root / "rustc", root / "cargo", root / "lib"
            compiler.write_bytes(b"compiler")
            cargo.write_bytes(b"cargo")
            libdir.mkdir()
            (libdir / "libstd-fixture.rlib").write_bytes(b"standard library")
            for component in ("host", "tui"):
                binary = root / component / "target" / TARGETS["linux-x64"] / "release" / f"couch-installer-{component}"
                binary.parent.mkdir(parents=True)
                binary.write_bytes(component.encode())
            commands = {
                ("rustup", "which", "rustc"): str(compiler),
                ("rustup", "which", "cargo"): str(cargo),
                ("rustc", "--print", "target-libdir", "--target", TARGETS["linux-x64"]): str(libdir),
                ("rustc", "-vV"): "rustc fixture\ncommit-hash: abc",
                ("cargo", "-vV"): "cargo fixture",
                ("rustup", "show", "active-toolchain"): "stable-fixture (default)",
            }
            with patch("binary_receipt.command", side_effect=lambda *args: commands[args]):
                receipt = native("linux-x64", "a" * 40, root, "v0.1.0")
            self.assertNotIn(str(root), json.dumps(receipt))
            self.assertEqual(receipt["installer_version"], "v0.1.0")
            self.assertEqual(receipt["toolchain"]["target_sysroot_files"]["libstd-fixture.rlib"],
                             file_receipt(libdir / "libstd-fixture.rlib"))
            self.assertEqual(receipt["toolchain"]["rustc_verbose"], "rustc fixture\ncommit-hash: abc")

    def test_installer_version_is_a_small_semver_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "VERSION").write_text("v1.2.3-alpha.4\n")
            self.assertEqual(read_installer_version(root), "v1.2.3-alpha.4")
            longest = "v1.2.3-" + "a" * 121
            (root / "VERSION").write_text(longest + "\n")
            self.assertEqual(read_installer_version(root), longest)
            for invalid in ("1.2.3", "v01.2.3", "v1.2", "v1.2.3+local"):
                (root / "VERSION").write_text(invalid)
                with self.assertRaisesRegex(ValueError, "invalid"):
                    read_installer_version(root)
            (root / "VERSION").write_text(longest + "a")
            with self.assertRaisesRegex(ValueError, "invalid"):
                read_installer_version(root)


if __name__ == "__main__":
    unittest.main()
