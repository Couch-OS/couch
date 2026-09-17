import copy
import json
import tempfile
import unittest
from pathlib import Path
from subprocess import CompletedProcess
from unittest import mock

from tools.integrations import validate_catalog


class CatalogPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.catalog = json.loads(validate_catalog.DEFAULT_CATALOG.read_text(encoding="utf-8"))

    def validate_copy(self, mutate=lambda value: None):
        value = copy.deepcopy(self.catalog)
        mutate(value)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            return validate_catalog.validate(path)

    def test_repository_catalog_is_valid(self):
        loaded = validate_catalog.validate()
        ids = [entry["id"] for entry in loaded["integrations"]]
        self.assertEqual(ids, sorted(ids))
        self.assertTrue({"denon", "echo"}.issubset(ids))

    def test_v2_catalog_contains_v1_and_v2_packages_but_old_core_refuses_v2(self):
        loaded = self.validate_copy()
        protocols = {json.loads((validate_catalog.ROOT / entry["manifest"]).read_text())["protocol_version"]
                     for entry in loaded["integrations"]}
        self.assertEqual(protocols, {1, 2})
        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "manifest protocol is incompatible"):
            self.validate_copy(lambda catalog: catalog.update(protocol_version=1))
        for invalid in (True, 0, 3, "2"):
            with self.subTest(invalid=invalid), self.assertRaises(validate_catalog.InvalidCatalog):
                self.validate_copy(lambda catalog: catalog.update(protocol_version=invalid))

    def test_production_requires_hardware_evidence(self):
        def mutate(value):
            value["integrations"][0]["tier"] = "production"

        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "production requires validated hardware"):
            self.validate_copy(mutate)

    def test_production_evidence_is_bound_to_manifest_version(self):
        def mutate(value):
            entry = value["integrations"][0]
            entry["tier"] = "production"
            entry["hardware_validation"] = {
                "status": "validated",
                "notes": "Deliberately mismatched fixture.",
                "evidence": [{
                    "device": "Example AVR",
                    "firmware": "1.0",
                    "date": "2026-09-16",
                    "behaviors": ["Power"],
                    "source": "docs/development/admission.md",
                    "version": "9.9.9",
                    "source_commit": "0" * 40,
                }],
            }

        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "version must match"):
            self.validate_copy(mutate)

    def test_every_required_test_category_is_mandatory(self):
        def mutate(value):
            del value["integrations"][1]["tests"]["spike"]

        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "must define exactly"):
            self.validate_copy(mutate)

    def test_categories_cannot_point_to_one_token_test(self):
        def mutate(value):
            tests = value["integrations"][1]["tests"]
            tests["spike"] = copy.deepcopy(tests["conformance"])

        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "distinct test"):
            self.validate_copy(mutate)

    def test_catalog_identity_must_match_the_manifest(self):
        def mutate(value):
            value["integrations"][1]["id"] = "forged"
            value["integrations"].sort(key=lambda entry: entry["id"])

        with self.assertRaisesRegex(validate_catalog.InvalidCatalog, "manifest ID does not match"):
            self.validate_copy(mutate)

    def test_runner_rejects_a_filter_that_executes_no_test(self):
        completed = CompletedProcess([], 0, "test result: ok. 0 passed; 0 failed; 0 ignored; 4 filtered out\n")
        with (
            mock.patch.object(validate_catalog.subprocess, "run", return_value=completed),
            mock.patch("builtins.print"),
        ):
            with self.assertRaisesRegex(validate_catalog.AdmissionTestFailed, "exactly one"):
                validate_catalog.run_tests(self.catalog)

    def test_runner_rejects_an_ignored_test(self):
        completed = CompletedProcess([], 0, "test result: ok. 0 passed; 0 failed; 1 ignored; 3 filtered out\n")
        with (
            mock.patch.object(validate_catalog.subprocess, "run", return_value=completed),
            mock.patch("builtins.print"),
        ):
            with self.assertRaisesRegex(validate_catalog.AdmissionTestFailed, "exactly one"):
                validate_catalog.run_tests(self.catalog)


if __name__ == "__main__":
    unittest.main()
