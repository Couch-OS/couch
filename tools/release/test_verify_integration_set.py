import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

import verify_integration_set as verify


class TestedIntegrationSetTests(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads(verify.DEFAULT.read_text())

    def write_manifest(self, root):
        path = root / "set.json"
        path.write_text(json.dumps(self.manifest))
        return path

    def test_committed_manifest_matches_current_contract_and_limits_claims(self):
        receipt = verify.receipt()
        self.assertEqual(receipt["integration_versions"], {"denon": "0.1.1"})
        self.assertFalse(receipt["artifact_bytes_verified"])
        self.assertTrue(all(value is False for value in receipt["rollout"].values()))

    def test_core_and_feed_repositories_may_name_either_couch_owner_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.manifest["core"]["repository"] = "https://github.com/Couch-OS/couch.git"
            self.manifest["feed"]["repository"] = "https://github.com/Couch-OS/couch-integrations.git"
            verify.validate_manifest(self.write_manifest(root))
            for section, url in (("core", "https://github.com/someone/couch.git"),
                                 ("feed", "https://github.com/couch-os/couch-integrations.git")):
                with self.subTest(section=section):
                    original = self.manifest[section]["repository"]
                    self.manifest[section]["repository"] = url
                    with self.assertRaisesRegex(ValueError, "identity"):
                        verify.validate_manifest(self.write_manifest(root))
                    self.manifest[section]["repository"] = original

    def test_rejects_automatic_or_bundled_rollout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for field in self.manifest["rollout"]:
                with self.subTest(field=field):
                    self.manifest["rollout"][field] = True
                    with self.assertRaisesRegex(ValueError, "explicit and unbundled"):
                        verify.receipt(self.write_manifest(root))
                    self.manifest["rollout"][field] = False

    def test_feed_bytes_and_provenance_are_bound_into_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            key = root / "key"
            shutil.copyfile(verify.REPO / "daemon/couch-integrations/src/official.rsa.pub", key)
            item = self.manifest["integrations"][0]
            package = root / "package.apk"
            package.write_bytes(b"package fixture")
            item["artifact"].update(size=package.stat().st_size,
                                    sha256=hashlib.sha256(package.read_bytes()).hexdigest())
            published = {key: value for key, value in item["provenance"].items()
                         if key not in ("file", "size", "sha256")}
            published.update(schema=2, id="denon", version="0.1.1")
            provenance = root / "provenance.json"
            provenance.write_text(json.dumps(published))
            item["provenance"].update(size=provenance.stat().st_size,
                                      sha256=hashlib.sha256(provenance.read_bytes()).hexdigest())
            index = root / "APKINDEX.tar.gz"
            payload = ("P:couch-integration-denon\nV:0.1.1-r0\nA:armv7\n"
                       f"S:{package.stat().st_size}\n\n").encode()
            with tarfile.open(index, "w:gz") as archive:
                info = tarfile.TarInfo("APKINDEX"); info.size = len(payload)
                import io
                archive.addfile(info, io.BytesIO(payload))
            self.manifest["feed"]["index"].update(size=index.stat().st_size,
                sha256=hashlib.sha256(index.read_bytes()).hexdigest())
            manifest = self.write_manifest(root)
            with mock.patch.object(verify, "require_clean_contract"):
                receipt = verify.receipt(manifest, key, index, {"denon": package}, {"denon": provenance})
            self.assertTrue(receipt["artifact_bytes_verified"])

    def test_rejects_partial_artifact_inputs(self):
        with mock.patch.object(verify, "require_clean_contract"):
            with self.assertRaisesRegex(ValueError, "Supply key, index"):
                verify.receipt(key_path=verify.REPO / "daemon/couch-integrations/src/official.rsa.pub")

    def test_rejects_changed_contract_after_tested_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tested = verify.git("rev-parse", "HEAD").stdout.strip()
            changed = self.manifest["core"]["contract_paths"][0]
            self.manifest["core"]["tested_commit"] = tested
            self.manifest["core"]["contract_paths"] = [changed]
            real_git = verify.git

            def changed_contract(*args, **kwargs):
                if args[:3] == ("diff", "--name-only", tested):
                    return subprocess.CompletedProcess(args, 0, changed + "\n", "")
                return real_git(*args, **kwargs)

            with mock.patch.object(verify, "git", side_effect=changed_contract):
                with self.assertRaisesRegex(ValueError, "Integration contract changed"):
                    verify.receipt(self.write_manifest(root))


if __name__ == "__main__":
    unittest.main()
