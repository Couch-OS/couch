#!/usr/bin/env python3
"""Validate the integration admission catalog and optionally run its exact tests."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import tomllib
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CATALOG = ROOT / "integrations/catalog.json"
REQUIRED_TESTS = {"conformance", "failure", "timeout_no_retry", "spike"}
TIERS = {"test-only", "preview", "production"}
HARDWARE_STATES = {"not-applicable", "not-tested", "validated"}
IDENTIFIER = re.compile(r"[a-z0-9][a-z0-9_-]{0,63}")
RUST_IDENTIFIER = re.compile(r"[a-z_][a-z0-9_]*")


class InvalidCatalog(ValueError):
    pass


class AdmissionTestFailed(RuntimeError):
    pass


def _exact_keys(value: dict, expected: set[str], context: str) -> None:
    actual = set(value)
    if actual != expected:
        raise InvalidCatalog(
            f"{context}: keys must be exactly {sorted(expected)}, got {sorted(actual)}"
        )


def _relative_file(path: str, context: str) -> Path:
    candidate = Path(path)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise InvalidCatalog(f"{context}: must be a repository-relative path")
    resolved = ROOT / candidate
    if not resolved.is_file():
        raise InvalidCatalog(f"{context}: file does not exist: {path}")
    return resolved


def _hardware(entry: dict, context: str) -> None:
    value = entry["hardware_validation"]
    if not isinstance(value, dict):
        raise InvalidCatalog(f"{context}.hardware_validation: must be an object")
    _exact_keys(value, {"status", "evidence", "notes"}, f"{context}.hardware_validation")
    status = value["status"]
    evidence = value["evidence"]
    if (
        status not in HARDWARE_STATES
        or not isinstance(value["notes"], str)
        or not value["notes"].strip()
    ):
        raise InvalidCatalog(f"{context}.hardware_validation: invalid status or notes")
    if not isinstance(evidence, list):
        raise InvalidCatalog(f"{context}.hardware_validation.evidence: must be a list")
    if entry["tier"] == "test-only" and status != "not-applicable":
        raise InvalidCatalog(f"{context}: a synthetic test-only entry uses not-applicable hardware status")
    if entry["tier"] == "production" and status != "validated":
        raise InvalidCatalog(f"{context}: production requires validated hardware")
    if status == "validated" and not evidence:
        raise InvalidCatalog(f"{context}: validated hardware requires evidence")
    if status != "validated" and evidence:
        raise InvalidCatalog(f"{context}: evidence is allowed only for validated hardware")
    for at, item in enumerate(evidence):
        where = f"{context}.hardware_validation.evidence[{at}]"
        if not isinstance(item, dict):
            raise InvalidCatalog(f"{where}: must be an object")
        _exact_keys(
            item,
            {"device", "firmware", "date", "behaviors", "source", "version", "source_commit"},
            where,
        )
        if not all(
            isinstance(item[key], str) and item[key].strip()
            for key in ("device", "firmware", "source", "version")
        ):
            raise InvalidCatalog(f"{where}: device, firmware, source, and version are required")
        if not isinstance(item["source_commit"], str) or not re.fullmatch(
            r"[0-9a-f]{40}", item["source_commit"]
        ):
            raise InvalidCatalog(f"{where}.source_commit: must be a full Git commit SHA")
        try:
            dt.date.fromisoformat(item["date"])
        except (TypeError, ValueError) as error:
            raise InvalidCatalog(f"{where}.date: must be an ISO date") from error
        if not isinstance(item["behaviors"], list) or not item["behaviors"] or not all(
            isinstance(behavior, str) and behavior.strip() for behavior in item["behaviors"]
        ):
            raise InvalidCatalog(f"{where}.behaviors: must be a nonempty string list")
        source = item["source"]
        parsed = urlparse(source)
        if parsed.scheme:
            if parsed.scheme != "https" or not parsed.netloc:
                raise InvalidCatalog(f"{where}.source: external evidence must use HTTPS")
        else:
            _relative_file(source, f"{where}.source")


def _test_case(crate_path: Path, record: dict, context: str) -> None:
    if not isinstance(record, dict):
        raise InvalidCatalog(f"{context}: must be an object")
    _exact_keys(record, {"target", "case"}, context)
    target = record["target"]
    case = record["case"]
    if not isinstance(target, str) or not RUST_IDENTIFIER.fullmatch(target):
        raise InvalidCatalog(f"{context}.target: invalid Rust test target")
    if not isinstance(case, str) or not RUST_IDENTIFIER.fullmatch(case):
        raise InvalidCatalog(f"{context}.case: invalid Rust test name")
    source = crate_path / "tests" / f"{target}.rs"
    if not source.is_file():
        raise InvalidCatalog(f"{context}: test target is missing: {source.relative_to(ROOT)}")
    text = source.read_text(encoding="utf-8")
    match = re.search(
        rf"#\[test\]\s*(?:#\[[^\]]+\]\s*)*(?:pub\s+)?fn\s+{re.escape(case)}\s*\(",
        text,
    )
    if not match:
        raise InvalidCatalog(f"{context}: #[test] fn {case} is missing from {source.relative_to(ROOT)}")


def validate(path: Path = DEFAULT_CATALOG) -> dict:
    try:
        catalog = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise InvalidCatalog(f"cannot read catalog: {error}") from error
    if not isinstance(catalog, dict):
        raise InvalidCatalog("catalog must be an object")
    _exact_keys(catalog, {"schema", "protocol_version", "integrations"}, "catalog")
    if catalog["schema"] != 1 or catalog["protocol_version"] != 1:
        raise InvalidCatalog("catalog supports schema 1 and integration protocol 1")
    entries = catalog["integrations"]
    if not isinstance(entries, list) or not entries:
        raise InvalidCatalog("catalog.integrations must be a nonempty list")
    ids = []
    for at, entry in enumerate(entries):
        context = f"integrations[{at}]"
        if not isinstance(entry, dict):
            raise InvalidCatalog(f"{context}: must be an object")
        _exact_keys(
            entry,
            {
                "id", "manifest", "crate", "cargo_package", "binary", "tier",
                "synthetic", "limitations", "tests", "hardware_validation",
            },
            context,
        )
        integration_id = entry["id"]
        if not isinstance(integration_id, str) or not IDENTIFIER.fullmatch(integration_id):
            raise InvalidCatalog(f"{context}.id: invalid integration ID")
        ids.append(integration_id)
        if entry["tier"] not in TIERS:
            raise InvalidCatalog(f"{context}.tier: must be one of {sorted(TIERS)}")
        if not isinstance(entry["synthetic"], bool):
            raise InvalidCatalog(f"{context}.synthetic: must be boolean")
        if entry["tier"] == "test-only" and not entry["synthetic"]:
            raise InvalidCatalog(f"{context}: test-only entries must be explicitly synthetic")
        if entry["tier"] != "test-only" and entry["synthetic"]:
            raise InvalidCatalog(f"{context}: synthetic entries must remain test-only")
        for field in ("manifest", "crate", "cargo_package", "binary"):
            if not isinstance(entry[field], str) or not entry[field].strip():
                raise InvalidCatalog(f"{context}.{field}: must be a nonempty string")
        if not isinstance(entry["limitations"], list) or not all(
            isinstance(item, str) and item.strip() for item in entry["limitations"]
        ):
            raise InvalidCatalog(f"{context}.limitations: must be a string list")
        if entry["tier"] == "preview" and not entry["limitations"]:
            raise InvalidCatalog(f"{context}: preview entries must state their limitations")

        manifest_path = _relative_file(entry["manifest"], f"{context}.manifest")
        crate_manifest = _relative_file(entry["crate"] + "/Cargo.toml", f"{context}.crate")
        crate_path = crate_manifest.parent
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            cargo = tomllib.loads(crate_manifest.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
            raise InvalidCatalog(f"{context}: manifest or Cargo.toml cannot be parsed: {error}") from error
        if manifest.get("id") != integration_id:
            raise InvalidCatalog(f"{context}: manifest ID does not match catalog ID")
        if cargo.get("package", {}).get("name") != entry["cargo_package"]:
            raise InvalidCatalog(f"{context}: Cargo package name does not match catalog")
        if Path(str(manifest.get("executable", ""))).name != entry["binary"]:
            raise InvalidCatalog(f"{context}: manifest executable does not match catalog binary")
        binary_source = crate_path / "src" / "bin" / f"{entry['binary']}.rs"
        if not binary_source.is_file():
            raise InvalidCatalog(f"{context}: binary source is missing: {binary_source.relative_to(ROOT)}")

        tests = entry["tests"]
        if not isinstance(tests, dict) or set(tests) != REQUIRED_TESTS:
            raise InvalidCatalog(f"{context}.tests: must define exactly {sorted(REQUIRED_TESTS)}")
        cases = []
        for category in sorted(REQUIRED_TESTS):
            _test_case(crate_path, tests[category], f"{context}.tests.{category}")
            cases.append((tests[category]["target"], tests[category]["case"]))
        if len(set(cases)) != len(cases):
            raise InvalidCatalog(f"{context}.tests: each admission category needs a distinct test")
        _hardware(entry, context)
        if entry["tier"] == "production" and any(
            evidence["version"] != manifest.get("version")
            for evidence in entry["hardware_validation"]["evidence"]
        ):
            raise InvalidCatalog(f"{context}: hardware evidence version must match the manifest")
    if len(set(ids)) != len(ids):
        raise InvalidCatalog("integration IDs must be unique")
    if ids != sorted(ids):
        raise InvalidCatalog("integrations must be sorted by ID")
    return catalog


def run_tests(catalog: dict) -> None:
    for entry in catalog["integrations"]:
        for category in sorted(REQUIRED_TESTS):
            record = entry["tests"][category]
            command = [
                "cargo", "test", "--locked", "--manifest-path", "clients/Cargo.toml",
                "-p", entry["cargo_package"], "--test", record["target"],
                record["case"], "--", "--exact",
            ]
            print(f"== {entry['id']} / {category}: {record['case']}", flush=True)
            try:
                completed = subprocess.run(
                    command,
                    cwd=ROOT,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    timeout=180,
                )
            except subprocess.TimeoutExpired as error:
                raise AdmissionTestFailed(
                    f"{entry['id']} / {category} exceeded the 180-second deadline"
                ) from error
            print(completed.stdout, end="")
            if completed.returncode != 0:
                raise AdmissionTestFailed(
                    f"{entry['id']} / {category} exited {completed.returncode}"
                )
            result = re.findall(
                r"test result: ok\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;",
                completed.stdout,
            )
            if result != [("1", "0", "0")]:
                raise AdmissionTestFailed(
                    f"{entry['id']} / {category} did not execute exactly one non-ignored test"
                )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument("--run-tests", action="store_true")
    args = parser.parse_args()
    try:
        loaded = validate(args.catalog.resolve())
        if args.run_tests:
            run_tests(loaded)
    except (InvalidCatalog, AdmissionTestFailed) as error:
        parser.exit(1, f"integration admission failed: {error}\n")
    if args.run_tests:
        print(f"Integration admission passed for {len(loaded['integrations'])} entries.")
    else:
        print(f"Integration catalog valid: {len(loaded['integrations'])} entries.")
