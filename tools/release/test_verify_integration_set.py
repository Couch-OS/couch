import copy
import gzip
import hashlib
import io
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
    HARNESS = "clients/couch-plugin/src/testing.rs"

    def setUp(self):
        self.support = tempfile.TemporaryDirectory()
        self.addCleanup(self.support.cleanup)
        self.harness = Path(self.support.name) / "harness.py"
        self.harness.write_bytes(b"# synthetic test harness; never executed\n")
        self.report_bytes = b'{"checks":"synthetic passing fixture","hardware_validation":false}\n'
        patched = mock.patch.object(verify, "HOST_HARNESS", self.harness)
        patched.start()
        self.addCleanup(patched.stop)
        self.manifest = json.loads(verify.DEFAULT.read_text())
        self.manifest["schema"] = 2
        self.manifest["core"].pop("protocol_version", None)
        self.manifest["core"]["supported_protocol_versions"] = [1, 2, 3, 4, 5]
        self.manifest["core"]["tested_commit"] = verify.git("rev-parse", "HEAD").stdout.strip()
        self.manifest["core"]["contract_paths"] = list(verify.CONTRACT_PATHS)
        self.manifest["core"]["harness_paths"] = {
            self.HARNESS: self.in_tree_digest(self.HARNESS)
        }
        for item in self.manifest["integrations"]:
            item["protocol_version"] = 1
            item["hardware_evidence_core_commit"] = verify.LEGACY_EVIDENCE_COMMIT
        self.reset_evidence()

    def reset_evidence(self):
        self.report_bytes = json.dumps({"schema": 1, "kind": "couch-integration-host-test-report",
            "core_commit": self.manifest["core"]["tested_commit"], "hardware_validation": False,
            "checks": {name: "passed" for name in verify.HOST_CHECKS},
            "http_panel_device_connections": 1, "maximum_simultaneous_device_connections": 1,
            "signature_checks": {"trusted_apk": "passed", "untrusted_key": "rejected", "tampered_apk": "rejected"},
            "lifecycle": ["signed_install", "same_version_readmission", "removal_preserves_config_and_settings",
                          "signed_reinstall_preserves_settings"],
            "wire_requests": ["ZM?", "MV?", "MU?", "SI?", "SSFUN ?", "MVUP", "MV?", "MUON", "MU?", "ZM?", "MV?", "MU?", "SI?"]}).encode()
        self.evidence = {
            "schema": 1, "kind": "couch-integration-host-compatibility",
            "evidence_level": "host-protocol-compatibility", "hardware_validation": False,
            "harness_sha256": verify.digest(self.harness.read_bytes()),
            "report_sha256": verify.digest(self.report_bytes),
            "core": {"source_commit": self.manifest["core"]["tested_commit"],
                     "supported_protocol_versions": [1, 2, 3, 4, 5],
                     "target": "armv7-unknown-linux-musleabihf", "binary_sha256": "c" * 64},
            "checks": {name: "passed" for name in verify.HOST_CHECKS},
            "integrations": [{
                "id": item["id"], "version": item["version"], "protocol_version": 1,
                "apk_sha256": item["artifact"]["sha256"],
                "manifest_sha256": item["provenance"]["manifest_sha256"],
                "binary_sha256": item["provenance"]["binary_sha256"],
                "provenance_sha256": item["provenance"]["sha256"],
            } for item in self.manifest["integrations"]],
        }

    @staticmethod
    def in_tree_digest(path):
        return verify.digest(verify.regular(verify.REPO / path))

    @staticmethod
    def diff_reports(tested, *paths):
        """Pretend the contract diff since `tested` listed exactly these paths."""
        real_git = verify.git

        def patched(*args, **kwargs):
            if args[:3] == ("diff", "--name-only", tested):
                return subprocess.CompletedProcess(args, 0, "".join(path + "\n" for path in paths), "")
            return real_git(*args, **kwargs)

        return mock.patch.object(verify, "git", side_effect=patched)

    def write_manifest(self, root):
        if self.manifest["schema"] == 2:
            evidence = root / "fixture-host-compatibility.json"
            (root / "fixture-host-report.json").write_bytes(self.report_bytes)
            evidence.write_text(json.dumps(self.evidence))
            self.manifest["host_compatibility"] = {
                "file": evidence.name, "size": evidence.stat().st_size,
                "sha256": verify.digest(evidence.read_bytes()),
            }
        path = root / "set.json"
        path.write_text(json.dumps(self.manifest))
        return path

    @staticmethod
    def apk(manifest, binary, duplicate=False):
        chunks = []
        for files in [
            [(".SIGN.RSA.fixture", b"fixture signature")],
            [(".PKGINFO", b"pkgname = couch-integration-denon\n")],
            [("usr/lib/couch/integrations/denon/manifest.json", manifest),
             ("usr/lib/couch/integrations/denon/bin/couch-plugin-denon", binary)]
        ]:
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode="w") as archive:
                for name, data in files:
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))
                    if duplicate and name.endswith("manifest.json"):
                        member.name = "./" + name
                        archive.addfile(member, io.BytesIO(data))
            chunks.append(gzip.compress(raw.getvalue()))
        return b"".join(chunks)

    def test_schema2_separates_supported_core_versions_package_protocol_and_old_hardware(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = verify.receipt(self.write_manifest(Path(directory)))
        self.assertEqual(receipt["schema"], 2)
        self.assertNotIn("protocol_version", receipt)
        self.assertEqual(receipt["core_supported_protocol_versions"], [1, 2, 3, 4, 5])
        self.assertEqual(receipt["integration_protocol_versions"], {"denon": 1})
        self.assertEqual(receipt["hardware_evidence_core_commits"], {"denon": verify.LEGACY_EVIDENCE_COMMIT})
        self.assertFalse(receipt["artifact_bytes_verified"])

    def test_an_integration_in_its_own_repository_needs_no_catalog_entry_and_a_listed_one_cannot_overclaim(self):
        real = json.loads

        def catalog_with(entry):
            def loads(text, *args, **kwargs):
                value = real(text, *args, **kwargs)
                if isinstance(value, dict) and "kind" not in value and "protocol_version" in value \
                        and isinstance(value.get("integrations"), list):
                    value["integrations"] = [e for e in value["integrations"] if e["id"] != "denon"] + entry
                return value
            return mock.patch.object(verify.json, "loads", loads)

        listed = {"id": "denon", "tier": "preview", "hardware_validation": {"status": "not-tested"}}
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_manifest(Path(directory))
            # Denon's source left this repository, and the catalog with it.
            with catalog_with([]):
                self.assertEqual(verify.receipt(path)["integration_versions"], {"denon": "0.1.1"})
            with catalog_with([listed]):
                verify.receipt(path)
            # Only a named out-of-tree integration may be absent from the catalog.
            with catalog_with([]), mock.patch.object(verify, "OUT_OF_TREE", frozenset({"kodi"})), \
                    self.assertRaisesRegex(ValueError, "not a known out-of-tree integration"):
                verify.receipt(path)
            for claim in ({"tier": "production"}, {"hardware_validation": {"status": "validated"}}):
                with self.subTest(claim=claim), catalog_with([{**listed, **claim}]), \
                        self.assertRaisesRegex(ValueError, "not a not-tested preview catalog entry"):
                    verify.receipt(path)

    def test_legacy_schema1_remains_readable_only_with_its_protocol_contract(self):
        self.manifest["schema"] = 1
        self.manifest["core"].pop("supported_protocol_versions")
        self.manifest["core"]["protocol_version"] = 1
        harness = self.manifest["core"].pop("harness_paths")
        self.manifest.pop("host_compatibility", None)
        for item in self.manifest["integrations"]:
            item.pop("protocol_version")
            item.pop("hardware_evidence_core_commit")
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_manifest(Path(directory))
            with self.assertRaisesRegex(ValueError, "protocol maximum differs"):
                verify.receipt(path)
            # Historical checkout source validation is independent of parsing
            # the old metadata/receipt shape, which remains byte-compatible.
            with mock.patch.object(verify, "validate_protocol"):
                receipt = verify.receipt(path)
            self.assertEqual(receipt["schema"], 1)
            self.assertEqual(receipt["protocol_version"], 1)
            self.assertNotIn("host_compatibility_sha256", receipt)
            self.assertNotIn("core_harness_paths", receipt)
            # Schema 1 predates the harness exemption and refuses it outright,
            # so no legacy set can acquire one by editing its manifest.
            self.manifest["core"]["harness_paths"] = harness
            with self.assertRaisesRegex(ValueError, "core has missing or unexpected fields"):
                verify.validate_manifest(self.write_manifest(Path(directory)))

    def test_receipt_exact_types_scope_checks_and_artifact_identity_are_required(self):
        mutations = [
            lambda e: e.update(schema=True),
            lambda e: e.update(hostname="not-public-evidence"),
            lambda e: e.update(hardware_validation=True),
            lambda e: e.update(hardware_validation=0),
            lambda e: e.update(evidence_level="receiver-command-hardware"),
            lambda e: e.update(harness_sha256="missing"),
            lambda e: e["core"].update(source_commit="0" * 40),
            lambda e: e["core"].update(supported_protocol_versions=[True, 2]),
            lambda e: e["core"].update(target="x86_64-unknown-linux-gnu"),
            lambda e: e["checks"].update(fake_receiver_command="not-run"),
            lambda e: e["checks"].update(fake_receiver_command=True),
            lambda e: e["checks"].pop("v2_action_refused_for_v1"),
            lambda e: e["integrations"][0].update(protocol_version=2),
            lambda e: e["integrations"][0].update(protocol_version=True),
            lambda e: e["integrations"][0].update(apk_sha256="0" * 64),
            lambda e: e["integrations"][0].update(manifest_sha256="0" * 64),
            lambda e: e["integrations"].append(copy.deepcopy(e["integrations"][0])),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for mutate in mutations:
                self.reset_evidence()
                mutate(self.evidence)
                with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                    verify.receipt(self.write_manifest(root))

    def test_receipt_file_hash_missing_file_and_symlink_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for mutation in ("changed", "missing", "symlink"):
                path = self.write_manifest(root)
                evidence = root / "fixture-host-compatibility.json"
                if mutation == "changed":
                    evidence.write_text(evidence.read_text() + "\n")
                elif mutation == "missing":
                    evidence.unlink()
                else:
                    evidence.rename(root / "original.json")
                    evidence.symlink_to(root / "original.json")
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    verify.receipt(path)
                evidence.unlink(missing_ok=True)

    def test_changed_harness_or_report_cannot_reuse_prior_success_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = self.harness.read_bytes()
            for kind in ("harness", "report"):
                manifest = self.write_manifest(root)
                changed = self.harness if kind == "harness" else root / "fixture-host-report.json"
                changed.write_bytes(changed.read_bytes() + b"changed")
                with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, kind + " bytes differ"):
                    verify.receipt(manifest)
                self.harness.write_bytes(original)

    def test_rewritten_receipt_cannot_relabel_unchanged_report_to_another_core(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.manifest["core"]["tested_commit"] = "f" * 40
            self.evidence["core"]["source_commit"] = "f" * 40
            self.write_manifest(root)
            with self.assertRaisesRegex(ValueError, "report identity or results differ"):
                verify.validate_host_compatibility(self.manifest, root / "fixture-host-compatibility.json")

    def test_report_rejects_extra_private_fields_or_missing_signature_and_owner_evidence(self):
        mutations = [
            lambda report: report.update(hostname="private-host"),
            lambda report: report.update(http_panel_device_connections=True),
            lambda report: report.update(maximum_simultaneous_device_connections=2),
            lambda report: report["signature_checks"].update(untrusted_key="not-run"),
            lambda report: report.update(lifecycle=["signed_install"]),
            lambda report: report.update(wire_requests=["MVUP", "MVUP"]),
        ]
        with tempfile.TemporaryDirectory() as directory:
            for mutate in mutations:
                self.reset_evidence()
                report = json.loads(self.report_bytes)
                mutate(report)
                self.report_bytes = json.dumps(report).encode()
                self.evidence["report_sha256"] = verify.digest(self.report_bytes)
                with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                    verify.receipt(self.write_manifest(Path(directory)))

    def test_contract_paths_and_original_hardware_identity_cannot_be_relabelled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in ("clients/couch-sdk", "model/couch-model/src/volume.rs", "model/couch-model/src/commands.rs"):
                self.manifest["core"]["contract_paths"].remove(path)
                with self.assertRaisesRegex(ValueError, "required contract paths"):
                    verify.receipt(self.write_manifest(root))
                self.manifest["core"]["contract_paths"].append(path)
            self.manifest["integrations"][0]["hardware_evidence_core_commit"] = self.manifest["core"]["tested_commit"]
            with self.assertRaisesRegex(ValueError, "Original hardware evidence"):
                verify.receipt(self.write_manifest(root))

    def test_package_protocol_is_read_from_exact_apk_members(self):
        pin = copy.deepcopy(self.manifest["integrations"][0])
        manifest = json.dumps({"id": "denon", "version": "0.1.1", "protocol_version": 1,
                               "executable": "bin/couch-plugin-denon"}).encode()
        binary = b"fixture ARM executable"
        pin["provenance"]["manifest_sha256"] = verify.digest(manifest)
        pin["provenance"]["binary_sha256"] = verify.digest(binary)
        data = self.apk(manifest, binary)
        verify.verify_package_protocol(data, pin)
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            verify.verify_package_protocol(self.apk(manifest, binary, duplicate=True), pin)
        pin["protocol_version"] = 2
        with self.assertRaisesRegex(ValueError, "protocol or identity differs"):
            verify.verify_package_protocol(data, pin)
        pin["protocol_version"] = 1
        with self.assertRaisesRegex(ValueError, "differs from provenance"):
            verify.verify_package_protocol(self.apk(manifest, b"other executable"), pin)

    def test_saved_verification_receipt_requires_same_candidate_and_compatibility_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.write_manifest(root)
            receipt = verify.receipt(manifest)
            path = root / "verification.json"
            for field in ("candidate_commit", "host_compatibility_sha256"):
                bad = {**receipt, field: "0" * len(receipt[field])}
                path.write_text(json.dumps(bad))
                with self.assertRaisesRegex(ValueError, "differs from this candidate"):
                    verify.verify_receipt(manifest, path, require_artifacts=False)
            # A saved receipt cannot quietly drop the harness exemption it was cut with.
            path.write_text(json.dumps({**receipt, "core_harness_paths": {}}))
            with self.assertRaisesRegex(ValueError, "core_harness_paths differs from this candidate"):
                verify.verify_receipt(manifest, path, require_artifacts=False)
            path.write_text(json.dumps(receipt))
            self.assertEqual(verify.verify_receipt(manifest, path, require_artifacts=False), receipt)
            with self.assertRaisesRegex(ValueError, "does not cover"):
                verify.verify_receipt(manifest, path, require_artifacts=True)

    def test_committed_manifest_is_refused_until_protocol_5_evidence_is_cut(self):
        with mock.patch.object(verify, "HOST_HARNESS", verify.REPO / "tools/tests/denon-v1-host-compatibility.py"):
            with self.assertRaisesRegex(ValueError, "Integration contract changed after its tested commit"):
                verify.receipt()

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

    def test_rollout_may_be_automatic_but_never_bundled_and_is_always_boolean(self):
        automatic = ("automatic_install", "automatic_configuration_migration")
        bundled = ("bundle_packages_in_runtime", "bundle_packages_in_installer")
        self.assertEqual(set(self.manifest["rollout"]), set(automatic + bundled))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for field in bundled:
                with self.subTest(field=field):
                    self.manifest["rollout"][field] = True
                    with self.assertRaisesRegex(ValueError, "must not be bundled"):
                        verify.receipt(self.write_manifest(root))
                    self.manifest["rollout"][field] = False
            # A core that converts a saved built-in connection by itself says so.
            for value in (True, False):
                for field in automatic:
                    self.manifest["rollout"][field] = value
                receipt = verify.receipt(self.write_manifest(root))
                self.assertEqual([receipt["rollout"][field] for field in automatic], [value, value])
            for field in automatic + bundled:
                for invalid in (0, 1, "false", None):
                    with self.subTest(field=field, invalid=invalid):
                        original = self.manifest["rollout"][field]
                        self.manifest["rollout"][field] = invalid
                        with self.assertRaisesRegex(ValueError, "must be booleans"):
                            verify.receipt(self.write_manifest(root))
                        self.manifest["rollout"][field] = original
            del self.manifest["rollout"]["automatic_install"]
            with self.assertRaisesRegex(ValueError, "missing or unexpected fields"):
                verify.receipt(self.write_manifest(root))

    def test_feed_bytes_and_provenance_are_bound_into_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            key = root / "key"
            shutil.copyfile(verify.REPO / "daemon/couch-integrations/src/official.rsa.pub", key)
            item = self.manifest["integrations"][0]
            package = root / "package.apk"
            package_manifest = json.dumps({"id": "denon", "version": "0.1.1", "protocol_version": 1,
                "executable": "bin/couch-plugin-denon"}).encode()
            binary = b"fixture ARM executable bytes"
            package.write_bytes(self.apk(package_manifest, binary))
            item["provenance"]["manifest_sha256"] = verify.digest(package_manifest)
            item["provenance"]["binary_sha256"] = verify.digest(binary)
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
            self.reset_evidence()
            manifest = self.write_manifest(root)
            with mock.patch.object(verify, "require_clean_contract"):
                receipt = verify.receipt(manifest, key, index, {"denon": package}, {"denon": provenance})
            self.assertTrue(receipt["artifact_bytes_verified"])

    def test_rejects_partial_artifact_inputs(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(verify, "require_clean_contract"):
            with self.assertRaisesRegex(ValueError, "Supply key, index"):
                verify.receipt(self.write_manifest(Path(directory)),
                               key_path=verify.REPO / "daemon/couch-integrations/src/official.rsa.pub")

    def test_rejects_changed_contract_after_tested_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tested = verify.git("rev-parse", "HEAD").stdout.strip()
            changed = self.manifest["core"]["contract_paths"][0]
            self.manifest["core"]["tested_commit"] = tested
            real_git = verify.git

            def changed_contract(*args, **kwargs):
                if args[:3] == ("diff", "--name-only", tested):
                    return subprocess.CompletedProcess(args, 0, changed + "\n", "")
                return real_git(*args, **kwargs)

            with mock.patch.object(verify, "git", side_effect=changed_contract):
                with self.assertRaisesRegex(ValueError, "Integration contract changed"):
                    verify.receipt(self.write_manifest(root))

    def test_recorded_harness_change_keeps_device_evidence_without_a_hardware_rerun(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tested = self.manifest["core"]["tested_commit"]
            pinned = {self.HARNESS: self.in_tree_digest(self.HARNESS)}
            self.manifest["core"]["harness_paths"] = pinned
            with self.diff_reports(tested, self.HARNESS):
                receipt = verify.receipt(self.write_manifest(root))
            self.assertEqual(receipt["core_harness_paths"], pinned)
            self.assertEqual(receipt["hardware_evidence_core_commits"], {"denon": verify.LEGACY_EVIDENCE_COMMIT})
            # Undeclared, the same compiled-out file still invalidates the set.
            self.manifest["core"].pop("harness_paths")
            with self.diff_reports(tested, self.HARNESS):
                with self.assertRaisesRegex(ValueError, "Integration contract changed"):
                    verify.receipt(self.write_manifest(root))

    def test_declared_harness_never_excuses_a_real_contract_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tested = self.manifest["core"]["tested_commit"]
            self.manifest["core"]["harness_paths"] = {self.HARNESS: self.in_tree_digest(self.HARNESS)}
            path = self.write_manifest(root)
            for contract in ("clients/couch-plugin/src/protocol.rs", "clients/couch-plugin/src/lib.rs",
                             "clients/couch-sdk/src/lib.rs", "daemon/couch-confd/src/main.rs",
                             "daemon/couch-integrations/src/lib.rs", "model/couch-model/src/validate.rs"):
                with self.subTest(contract=contract), self.diff_reports(tested, self.HARNESS, contract):
                    with self.assertRaises(ValueError) as caught:
                        verify.receipt(path)
                    self.assertEqual(str(caught.exception),
                                     "Integration contract changed after its tested commit: " + contract)

    def test_contract_file_cannot_be_laundered_into_the_harness_exemption(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tested = self.manifest["core"]["tested_commit"]
            for contract in ("clients/couch-plugin/src/protocol.rs", "clients/couch-plugin/src/lib.rs",
                             "clients/couch-plugin", "daemon/couch-confd/src/main.rs",
                             "model/couch-model/src/validate.rs"):
                # A genuine digest of a genuine contract file buys nothing: only
                # the in-code allowlist decides what may leave the freeze.
                self.manifest["core"]["harness_paths"] = {contract: self.in_tree_digest(contract)
                    if (verify.REPO / contract).is_file() else "a" * 64}
                with self.subTest(contract=contract), self.diff_reports(tested, contract):
                    with self.assertRaisesRegex(ValueError, "Only a recorded compiled-out harness"):
                        verify.receipt(self.write_manifest(root))

    def test_harness_exemption_needs_current_bytes_a_gated_module_and_a_contract_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            actual = self.in_tree_digest(self.HARNESS)
            self.manifest["core"]["harness_paths"] = {self.HARNESS: "0" * 64}
            with self.assertRaisesRegex(ValueError, "rerun the admission suite and record .*" + actual):
                verify.receipt(self.write_manifest(root))
            self.manifest["core"]["harness_paths"] = {self.HARNESS: actual}
            ungated = {self.HARNESS: ("clients/couch-plugin/src/lib.rs", r"pub mod testing;\s*// shipped")}
            with mock.patch.object(verify, "HARNESS_PATHS", ungated):
                with self.assertRaisesRegex(ValueError, "no longer compiled out of shipped builds"):
                    verify.receipt(self.write_manifest(root))
            outside = "tools/tests/denon-v1-host-compatibility.py"
            self.manifest["core"]["harness_paths"] = {outside: self.in_tree_digest(outside)}
            with mock.patch.object(verify, "HARNESS_PATHS", {outside: verify.HARNESS_PATHS[self.HARNESS]}):
                with self.assertRaisesRegex(ValueError, "not inside a frozen contract path"):
                    verify.receipt(self.write_manifest(root))

    def test_harness_paths_reject_traversal_absolute_and_malformed_digests(self):
        actual = self.in_tree_digest(self.HARNESS)
        cases = [
            ({"/" + self.HARNESS: actual}, "Invalid core harness path"),
            ({"../couch/" + self.HARNESS: actual}, "Invalid core harness path"),
            ({"clients/couch-plugin/../couch-plugin/src/testing.rs": actual}, "Invalid core harness path"),
            ({"": actual}, "Invalid core harness path"),
            ({self.HARNESS: "not-a-sha256"}, "Invalid harness SHA-256"),
            ({self.HARNESS: actual.upper()}, "Invalid harness SHA-256"),
            ({self.HARNESS: None}, "Invalid harness SHA-256"),
            ([self.HARNESS], "path to SHA-256 object"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for harness, message in cases:
                self.manifest["core"]["harness_paths"] = harness
                with self.subTest(harness=harness), self.assertRaisesRegex(ValueError, message):
                    verify.receipt(self.write_manifest(root))

    def test_deleted_harness_cannot_pass_as_an_exempt_change(self):
        gone = "clients/couch-plugin/src/never-written.rs"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.manifest["core"]["harness_paths"] = {gone: "b" * 64}
            with mock.patch.object(verify, "HARNESS_PATHS", {gone: verify.HARNESS_PATHS[self.HARNESS]}):
                with self.diff_reports(self.manifest["core"]["tested_commit"], gone):
                    with self.assertRaisesRegex(ValueError, "Expected regular file"):
                        verify.receipt(self.write_manifest(root))


if __name__ == "__main__":
    unittest.main()
