#!/usr/bin/env python3
"""Verify a pinned core/integration set and optionally its public feed bytes.

This writes release provenance. It never downloads, signs, publishes, installs,
or adds an integration package to a runtime or installer payload.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile

REPO = Path(__file__).resolve().parents[2]
DEFAULT = Path(__file__).with_name("tested-integrations.json")
HOST_HARNESS = REPO / "tools/tests/denon-v1-host-compatibility.py"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
LEGACY_EVIDENCE_COMMIT = "b9eb59fd0a180fd3ae2d7b2ed27a61920cb5f6cb"
# Integrations whose source left this repository for their own, and so have no
# entry in integrations/catalog.json. Only these may be absent from it: any
# other id without an entry is a typo or an integration nobody admitted.
OUT_OF_TREE = frozenset({"denon"})
# Schema 2 cannot drop a changed contract path to retain stale evidence. The
# SDK and new model types affect the wire contract even outside couch-plugin.
CONTRACT_PATHS = (
    "clients/couch-plugin", "clients/couch-control", "clients/couch-sdk",
    "daemon/couch-integrations", "daemon/couch-confd/src/api.rs",
    "daemon/couch-confd/src/api/connections.rs",
    "daemon/couch-confd/src/api/integration_migrations.rs",
    "daemon/couch-confd/src/api/integration_packages.rs",
    "daemon/couch-confd/src/api/plugins.rs", "daemon/couch-confd/src/main.rs",
    "daemon/couch-confd/src/plugins.rs", "daemon/couch-confd/src/store.rs",
    "model/couch-model/src/integration_migration.rs", "model/couch-model/src/lib.rs",
    "model/couch-model/src/seed.rs", "model/couch-model/src/storage.rs",
    "model/couch-model/src/validate.rs", "model/couch-model/src/volume.rs",
    "model/couch-model/src/commands.rs", "model/couch-model/src/connection.rs",
    "model/couch-model/src/device.rs", "model/couch-model/src/buttons.rs",
)
HOST_CHECKS = (
    "signed_package_lifecycle", "v1_handshake", "fake_receiver_status",
    "fake_receiver_inputs", "fake_receiver_command", "shared_transport_ownership",
    "v2_action_refused_for_v1", "v4_protocol_probe",
)
# The admission harness is scaffolding a published package never links: its
# module is compiled out of every shipped build, so it cannot alter the wire
# protocol, the manifest format or host behavior of an APK already in the feed.
# It does decide what "passed admission" means, so it is tracked by digest
# instead of being frozen: changing one is a recorded one-line manifest edit
# after a rerun of the admission suite, not a repeat of hardware validation.
# Only a path named here may be exempted, and only while the gate below still
# compiles it out, so no manifest edit alone can launder contract code out of
# the freeze. Each entry maps the harness file to the source that declares it
# and the declaration that keeps it out of shipped builds.
HARNESS_PATHS = {
    "clients/couch-plugin/src/testing.rs": (
        "clients/couch-plugin/src/lib.rs",
        r'#\[cfg\(feature\s*=\s*"testing"\)\]\s*pub mod testing;',
    ),
}


def require(value, message):
    if not value:
        raise ValueError(message)


def exact(value, keys, where):
    require(isinstance(value, dict) and set(value) == set(keys),
            f"{where} has missing or unexpected fields")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def regular(path, limit=128 * 1024 * 1024):
    require(path.is_file() and not path.is_symlink(), f"Expected regular file: {path}")
    require(0 < path.stat().st_size <= limit, f"Invalid file size: {path}")
    return path.read_bytes()


def pinned_file(pin, path, where):
    exact(pin, ("file", "size", "sha256"), where)
    require(isinstance(pin["file"], str) and pin["file"] and not pin["file"].startswith("/")
            and ".." not in Path(pin["file"]).parts, f"{where}.file is not a safe relative path")
    require(type(pin["size"]) is int and pin["size"] > 0 and HEX64.fullmatch(pin["sha256"]),
            f"{where} has an invalid size or SHA-256")
    data = regular(path)
    require(len(data) == pin["size"] and digest(data) == pin["sha256"],
            f"{where} bytes differ from the tested set")
    return data


def git(*args, check=True):
    return subprocess.run(("git", "-C", str(REPO), *args), check=check,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def commit_exists(commit):
    return git("cat-file", "-e", commit + "^{commit}", check=False).returncode == 0


def ancestor(older, newer):
    return git("merge-base", "--is-ancestor", older, newer, check=False).returncode == 0


def read_index(data):
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = archive.getmember("APKINDEX")
            require(member.isfile() and member.size <= 1024 * 1024, "Invalid APKINDEX member")
            return archive.extractfile(member).read().decode("utf-8")
    except (KeyError, OSError, UnicodeDecodeError, tarfile.TarError) as error:
        raise ValueError("Expected a bounded signed APKINDEX archive") from error


def index_records(text):
    records = []
    for paragraph in text.strip().split("\n\n"):
        record = {}
        for line in paragraph.splitlines():
            if len(line) >= 3 and line[1] == ":":
                record[line[0]] = line[2:]
        if record:
            records.append(record)
    return records


def github_repositories(name):
    """A Couch repository under either owner; they move to the Couch-OS organization."""
    return tuple(f"https://github.com/{owner}/{name}.git" for owner in ("dangerouslaser", "Couch-OS"))


def validate_protocol(core, schema):
    supported = [1] if schema == 1 else core["supported_protocol_versions"]
    require(type(supported) is list and supported
            and all(type(version) is int for version in supported)
            and supported == list(range(1, max(supported) + 1)), "Invalid core protocol versions")
    protocol = (REPO / "clients/couch-plugin/src/protocol.rs").read_text()
    require(re.search(r"pub const PROTOCOL_VERSION:\s*u32\s*=\s*" + str(max(supported)) + r"\s*;", protocol),
            "Current plugin protocol maximum differs from tested core")
    main = (REPO / "daemon/couch-confd/src/main.rs").read_text()
    require(all(f'"--supports-integration-protocol={version}"' in main for version in supported),
            "Core host support probe is absent")


def validate_harness(core, schema):
    """Digest-track the compiled-out admission harness; never exempt contract code."""
    harness = core.get("harness_paths", {}) if schema == 2 else {}
    require(isinstance(harness, dict), "Core harness paths must be a path to SHA-256 object")
    for item, pinned in harness.items():
        require(isinstance(item, str) and item and not item.startswith("/")
                and ".." not in PurePosixPath(item).parts, "Invalid core harness path")
        require(item in HARNESS_PATHS,
                f"Only a recorded compiled-out harness may leave the contract freeze: {item}")
        require(any(item == path or item.startswith(path + "/") for path in core["contract_paths"]),
                f"Harness path is not inside a frozen contract path: {item}")
        require(isinstance(pinned, str) and HEX64.fullmatch(pinned),
                f"Invalid harness SHA-256 for {item}")
        declaring, gate = HARNESS_PATHS[item]
        require(re.search(gate, (REPO / declaring).read_text()),
                f"Harness is no longer compiled out of shipped builds: {item}")
        actual = digest(regular(REPO / item, 1024 * 1024))
        require(actual == pinned,
                f"Admission harness changed: rerun the admission suite and record {item} = {actual}")
    return harness


def validate_host_compatibility(manifest, path):
    """Validate trusted, hash-pinned host test evidence, never hardware evidence."""
    raw = pinned_file(manifest["host_compatibility"], path, "host_compatibility")
    require(len(raw) <= 64 * 1024, "Host compatibility receipt exceeds bound")
    evidence = json.loads(raw)
    exact(evidence, ("schema", "kind", "evidence_level", "core", "integrations", "checks",
                     "hardware_validation", "harness_sha256", "report_sha256"), "host compatibility receipt")
    require(type(evidence["schema"]) is int and evidence["schema"] == 1
            and evidence["kind"] == "couch-integration-host-compatibility"
            and evidence["evidence_level"] == "host-protocol-compatibility"
            and evidence["hardware_validation"] is False,
            "Host compatibility receipt must not claim hardware validation")
    for field in ("harness_sha256", "report_sha256"):
        require(isinstance(evidence[field], str) and HEX64.fullmatch(evidence[field]),
                "Missing host compatibility harness/report digest")
    require(path.name.endswith("-host-compatibility.json"), "Unexpected host compatibility receipt filename")
    report = path.with_name(path.name.removesuffix("-host-compatibility.json") + "-host-report.json")
    require(digest(regular(HOST_HARNESS, 128 * 1024)) == evidence["harness_sha256"],
            "Host compatibility harness bytes differ from executed evidence")
    report_bytes = regular(report, 64 * 1024)
    require(digest(report_bytes) == evidence["report_sha256"],
            "Host compatibility report bytes differ from executed evidence")
    core = evidence["core"]
    exact(core, ("source_commit", "supported_protocol_versions", "target", "binary_sha256"), "host compatibility core")
    require(core["source_commit"] == manifest["core"]["tested_commit"]
            and core["supported_protocol_versions"] == manifest["core"]["supported_protocol_versions"]
            and all(type(version) is int for version in core["supported_protocol_versions"])
            and core["target"] == "armv7-unknown-linux-musleabihf"
            and isinstance(core["binary_sha256"], str) and HEX64.fullmatch(core["binary_sha256"]),
            "Host compatibility core differs from frozen tested core")
    exact(evidence["checks"], HOST_CHECKS, "host compatibility checks")
    require(all(value == "passed" for value in evidence["checks"].values()),
            "Host compatibility checks did not all pass")
    report_data = json.loads(report_bytes)
    exact(report_data, ("schema", "kind", "core_commit", "hardware_validation", "checks",
                       "http_panel_device_connections", "maximum_simultaneous_device_connections",
                       "signature_checks", "lifecycle", "wire_requests"), "host compatibility report")
    require(type(report_data.get("schema")) is int
            and report_data["schema"] == 1 and report_data.get("kind") == "couch-integration-host-test-report"
            and report_data.get("core_commit") == core["source_commit"]
            and report_data.get("checks") == evidence["checks"]
            and report_data.get("hardware_validation") is False,
            "Host compatibility report identity or results differ from receipt")
    require(all(type(report_data[field]) is int and report_data[field] == 1 for field in (
                "http_panel_device_connections", "maximum_simultaneous_device_connections"))
            and report_data["signature_checks"] == {
                "trusted_apk": "passed", "untrusted_key": "rejected", "tampered_apk": "rejected"}
            and report_data["lifecycle"] == ["signed_install", "same_version_readmission",
                "removal_preserves_config_and_settings", "signed_reinstall_preserves_settings"]
            and report_data["wire_requests"] == ["ZM?", "MV?", "MU?", "SI?", "SSFUN ?", "MVUP",
                "MV?", "MUON", "MU?", "ZM?", "MV?", "MU?", "SI?"],
            "Host compatibility report lacks exact signature, lifecycle or shared-owner results")
    require(type(evidence["integrations"]) is list
            and len(evidence["integrations"]) == len(manifest["integrations"]),
            "Host compatibility integration set differs")
    by_id = {item["id"]: item for item in manifest["integrations"]}
    seen = set()
    for item in evidence["integrations"]:
        exact(item, ("id", "version", "protocol_version", "apk_sha256", "manifest_sha256",
                     "binary_sha256", "provenance_sha256"), "host compatibility integration")
        require(isinstance(item["id"], str) and item["id"] in by_id and item["id"] not in seen,
                "Unknown or duplicate host compatibility integration")
        seen.add(item["id"])
        pin = by_id[item["id"]]
        require(type(item["protocol_version"]) is int and item == {
            "id": pin["id"], "version": pin["version"], "protocol_version": pin["protocol_version"],
            "apk_sha256": pin["artifact"]["sha256"],
            "manifest_sha256": pin["provenance"]["manifest_sha256"],
            "binary_sha256": pin["provenance"]["binary_sha256"],
            "provenance_sha256": pin["provenance"]["sha256"],
        }, "Host compatibility package bytes differ from immutable feed pins")
    return evidence


def verify_package_protocol(data, pin):
    """Inspect the hash-pinned APK's actual manifest and executable, without extraction."""
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
            unpacked = compressed.read(64 * 1024 * 1024 + 1)
        require(len(unpacked) <= 64 * 1024 * 1024, "Tested APK decompressed size exceeds bound")
        base = "usr/lib/couch/integrations/" + pin["id"] + "/"
        names = {base + "manifest.json": "manifest_sha256",
                 base + "bin/" + pin["provenance"]["binary"]: "binary_sha256"}
        found = {}
        with tarfile.open(fileobj=io.BytesIO(unpacked), mode="r:", ignore_zeros=True) as archive:
            for count, member in enumerate(archive, 1):
                require(count <= 4096, "Tested APK entry count exceeds bound")
                name = str(PurePosixPath(member.name))
                if name not in names:
                    continue
                limit = 128 * 1024 if name.endswith("/manifest.json") else 32 * 1024 * 1024
                require(name not in found and member.type in (tarfile.REGTYPE, tarfile.AREGTYPE)
                        and not member.sparse and 0 < member.size <= limit,
                        "Duplicate, nonregular or oversized tested APK member")
                content = archive.extractfile(member).read()
                require(digest(content) == pin["provenance"][names[name]], "Tested APK member differs from provenance")
                found[name] = content
        require(set(found) == set(names), "Tested APK lacks manifest or executable")
        package = json.loads(found[base + "manifest.json"])
        require(type(package.get("protocol_version")) is int
                and package["protocol_version"] == pin["protocol_version"]
                and package.get("id") == pin["id"] and package.get("version") == pin["version"]
                and package.get("executable") == "bin/" + pin["provenance"]["binary"],
                "Tested package protocol or identity differs from actual APK manifest")
    except (OSError, EOFError, tarfile.TarError) as error:
        raise ValueError("Invalid bounded tested APK archive") from error


def validate_manifest(path=DEFAULT):
    raw = regular(path, 1024 * 1024)
    manifest = json.loads(raw)
    schema = manifest.get("schema")
    require(type(schema) is int and schema in (1, 2), "Unsupported tested-set schema")
    exact(manifest, ("schema", "kind", "name", "core", "feed", "integrations", "rollout",
                     *(("host_compatibility",) if schema == 2 else ())), "manifest")
    require(manifest["kind"] == "couch-tested-integration-set"
            and isinstance(manifest["name"], str) and manifest["name"], "Unsupported tested-set identity")
    core = manifest["core"]
    # harness_paths is optional: absent means the freeze covers every contract byte.
    declared_harness = ("harness_paths",) if isinstance(core, dict) and "harness_paths" in core else ()
    exact(core, ("repository", "tested_commit", "compatibility_floor", "contract_paths",
                 "protocol_version" if schema == 1 else "supported_protocol_versions",
                 *(declared_harness if schema == 2 else ())), "core")
    require(core["repository"] in github_repositories("couch")
            and HEX40.fullmatch(core["tested_commit"])
            and (schema == 2 or type(core["protocol_version"]) is int and core["protocol_version"] == 1),
            "Invalid core identity")
    require(commit_exists(core["tested_commit"]), "Tested core commit is absent from this checkout")
    head = git("rev-parse", "HEAD").stdout.strip()
    require(ancestor(core["tested_commit"], head), "Current checkout does not contain the tested core commit")
    require(isinstance(core["contract_paths"], list) and core["contract_paths"], "Core contract paths are empty")
    for item in core["contract_paths"]:
        require(isinstance(item, str) and item and not item.startswith("/") and ".." not in Path(item).parts,
                "Invalid core contract path")
    if schema == 2:
        require(len(set(core["contract_paths"])) == len(core["contract_paths"])
                and set(CONTRACT_PATHS) <= set(core["contract_paths"]), "Schema 2 omits required contract paths")
    harness = validate_harness(core, schema)
    changed = [path for path in git("diff", "--name-only", core["tested_commit"], head,
                                    "--", *core["contract_paths"]).stdout.splitlines()
               if path not in harness]
    require(not changed, "Integration contract changed after its tested commit: " + ", ".join(changed))

    validate_protocol(core, schema)

    feed = manifest["feed"]
    exact(feed, ("repository", "source_commit", "base_url", "channel", "architecture", "public_key", "index"), "feed")
    require(feed["repository"] in github_repositories("couch-integrations")
            and HEX40.fullmatch(feed["source_commit"])
            and feed["base_url"].startswith("https://")
            and feed["channel"] == "preview" and feed["architecture"] == "armv7", "Invalid feed identity")
    exact(feed["public_key"], ("file", "size", "sha256"), "feed.public_key")
    exact(feed["index"], ("file", "size", "sha256"), "feed.index")
    embedded = regular(REPO / "daemon/couch-integrations/src/official.rsa.pub", 64 * 1024)
    require(len(embedded) == feed["public_key"]["size"] and digest(embedded) == feed["public_key"]["sha256"],
            "Embedded official feed key differs from the tested set")

    catalog = json.loads((REPO / "integrations/catalog.json").read_text())
    catalog_by_id = {item["id"]: item for item in catalog["integrations"]}
    require(isinstance(manifest["integrations"], list) and manifest["integrations"], "No tested integrations")
    seen = set()
    for position, item in enumerate(manifest["integrations"]):
        where = f"integrations[{position}]"
        exact(item, ("id", "package", "version", "release", "tier", "evidence_level",
                     "validated_behaviors", "not_validated", "artifact", "provenance",
                     *(("protocol_version", "hardware_evidence_core_commit") if schema == 2 else ())), where)
        if schema == 2:
            require(type(item["protocol_version"]) is int and item["protocol_version"] == 1
                    and item["protocol_version"] in core["supported_protocol_versions"],
                    "Renewed host evidence covers the original protocol-1 package only")
            require(item["hardware_evidence_core_commit"] == LEGACY_EVIDENCE_COMMIT
                    and commit_exists(LEGACY_EVIDENCE_COMMIT)
                    and ancestor(LEGACY_EVIDENCE_COMMIT, core["tested_commit"]),
                    "Original hardware evidence core identity must be preserved")
        require(re.fullmatch(r"[a-z0-9][a-z0-9-]*", item["id"] or "") and item["id"] not in seen,
                f"{where}.id is invalid or duplicated")
        seen.add(item["id"])
        require(item["release"] == item["version"] + "-r0" and item["package"] == "couch-integration-" + item["id"],
                f"{where} package version identity is inconsistent")
        require(item["tier"] == "preview"
                and item["evidence_level"] == "package-lifecycle-and-read-only-device"
                and item["validated_behaviors"] == [
                    "signed package admission and lifecycle", "read-only receiver status", "receiver input enumeration"]
                and item["not_validated"] == [
                    "full receiver command behavior", "receiver model and firmware compatibility"],
                f"{where} overstates or changes the Denon pilot evidence")
        # The catalog lists only integrations whose source is still in this
        # repository. One named in OUT_OF_TREE lives in its own repository and
        # needs no entry: its tier is the tested set's own, checked above. One
        # that is listed must not claim more there than the tested set does here.
        entry = catalog_by_id.get(item["id"])
        require(entry is not None or item["id"] in OUT_OF_TREE,
                f"{where} has no catalog entry and is not a known out-of-tree integration")
        require(entry is None
                or entry["tier"] == "preview" and entry["hardware_validation"]["status"] == "not-tested",
                f"{where} is not a not-tested preview catalog entry")
        exact(item["artifact"], ("file", "size", "sha256"), where + ".artifact")
        provenance = item["provenance"]
        exact(provenance, ("file", "size", "sha256", "source_repository", "source_commit",
                           "sdk_repository", "sdk_commit", "tooling_repository", "tooling_commit",
                           "binary", "binary_sha256", "manifest_sha256"), where + ".provenance")
        for key in ("source_commit", "sdk_commit", "tooling_commit"):
            require(HEX40.fullmatch(provenance[key]), f"{where}.provenance.{key} is invalid")
        for key in ("sha256", "binary_sha256", "manifest_sha256"):
            require(HEX64.fullmatch(provenance[key]), f"{where}.provenance.{key} is invalid")
        # Packages built before the Couch-OS move record the previous owner.
        require(provenance["sdk_repository"] in github_repositories("couch")
                and provenance["tooling_repository"] in github_repositories("couch")
                and commit_exists(provenance["sdk_commit"])
                and commit_exists(provenance["tooling_commit"])
                and ancestor(provenance["sdk_commit"], core["tested_commit"])
                and ancestor(provenance["tooling_commit"], core["tested_commit"]),
                f"{where} SDK/tooling is not contained by the tested core")

    rollout = manifest["rollout"]
    exact(rollout, ("bundle_packages_in_runtime", "bundle_packages_in_installer",
                    "automatic_install", "automatic_configuration_migration"), "rollout")
    # A package never travels inside a runtime archive or an installer image.
    # The two automatic fields may be true: a core may install the package for,
    # and convert, a connection saved while that integration was built in, from
    # an official repository. They say what the tested core does; nothing here
    # makes either mean that anything else installs by itself.
    require(all(type(value) is bool for value in rollout.values()), "Rollout fields must be booleans")
    require(rollout["bundle_packages_in_runtime"] is False and rollout["bundle_packages_in_installer"] is False,
            "Packages must not be bundled in the runtime or the installer")
    if schema == 2:
        evidence_pin = manifest["host_compatibility"]
        exact(evidence_pin, ("file", "size", "sha256"), "host_compatibility")
        require(type(evidence_pin["size"]) is int and 0 < evidence_pin["size"] <= 64 * 1024,
                "Host compatibility receipt exceeds bound")
        validate_host_compatibility(manifest, path.parent / evidence_pin["file"])
    return manifest, raw, head


def require_clean_contract(manifest):
    paths = manifest["core"]["contract_paths"]
    dirty = git("status", "--porcelain=v1", "--untracked-files=all", "--", *paths).stdout.splitlines()
    require(not dirty, "Integration contract has uncommitted changes: " + ", ".join(dirty))


def verify_artifacts(manifest, key_path=None, index_path=None, packages=None, provenance=None):
    supplied = [key_path is not None, index_path is not None, bool(packages), bool(provenance)]
    require(all(supplied) or not any(supplied), "Supply key, index, package, and provenance inputs together")
    if not any(supplied):
        return False
    packages, provenance = packages or {}, provenance or {}
    feed = manifest["feed"]
    pinned_file(feed["public_key"], key_path, "feed.public_key")
    index = pinned_file(feed["index"], index_path, "feed.index")
    records = index_records(read_index(index))
    expected_ids = {item["id"] for item in manifest["integrations"]}
    require(set(packages) == expected_ids and set(provenance) == expected_ids,
            "Package/provenance inputs must cover exactly the tested integrations")
    for item in manifest["integrations"]:
        name = item["id"]
        package = pinned_file(item["artifact"], packages[name], f"{name}.artifact")
        if manifest["schema"] == 2:
            verify_package_protocol(package, item)
        raw = pinned_file({key: item["provenance"][key] for key in ("file", "size", "sha256")},
                          provenance[name], f"{name}.provenance")
        published = json.loads(raw)
        expected = {key: value for key, value in item["provenance"].items()
                    if key not in ("file", "size", "sha256")}
        expected.update({"schema": 2, "id": name, "version": item["version"]})
        require(published == expected, f"{name} provenance fields differ from the tested set")
        matches = [record for record in records if record.get("P") == item["package"]
                   and record.get("V") == item["release"] and record.get("A") == feed["architecture"]]
        require(len(matches) == 1 and matches[0].get("S") == str(item["artifact"]["size"]),
                f"{name} tested package is absent from the pinned index")
    return True


def parse_bindings(values, flag):
    result = {}
    for value in values:
        name, separator, path = value.partition("=")
        require(separator and name and path and name not in result, f"Invalid or duplicate {flag} binding")
        result[name] = Path(path)
    return result


def receipt(manifest_path=DEFAULT, key_path=None, index_path=None, packages=None, provenance=None):
    manifest, raw, head = validate_manifest(manifest_path)
    if any((key_path is not None, index_path is not None, bool(packages), bool(provenance))):
        require_clean_contract(manifest)
    artifacts = verify_artifacts(manifest, key_path, index_path, packages, provenance)
    result = {
        "schema": manifest["schema"],
        "kind": "couch-tested-integration-set-verification",
        "name": manifest["name"],
        "manifest_sha256": digest(raw),
        "core_tested_commit": manifest["core"]["tested_commit"],
        "candidate_commit": head,
        "integration_versions": {item["id"]: item["version"] for item in manifest["integrations"]},
        "artifact_bytes_verified": artifacts,
        "rollout": manifest["rollout"],
    }
    if manifest["schema"] == 1:
        result["protocol_version"] = manifest["core"]["protocol_version"]
    else:
        result.update(
            core_supported_protocol_versions=manifest["core"]["supported_protocol_versions"],
            integration_protocol_versions={item["id"]: item["protocol_version"] for item in manifest["integrations"]},
            hardware_evidence_core_commits={item["id"]: item["hardware_evidence_core_commit"] for item in manifest["integrations"]},
            host_compatibility_sha256=manifest["host_compatibility"]["sha256"],
            core_harness_paths=manifest["core"].get("harness_paths", {}),
        )
    return result


def verify_receipt(manifest_path, receipt_path, require_artifacts=True):
    expected = receipt(manifest_path)
    if require_artifacts:
        manifest, _, _ = validate_manifest(manifest_path)
        require_clean_contract(manifest)
    saved = json.loads(regular(receipt_path, 1024 * 1024))
    exact(saved, expected.keys(), "verification receipt")
    for key in expected:
        if key == "artifact_bytes_verified":
            require(saved[key] is True if require_artifacts else saved[key] in (True, False),
                    "Verification receipt does not cover the pinned feed bytes")
        else:
            require(saved[key] == expected[key], f"Verification receipt {key} differs from this candidate")
    return saved


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", nargs="?", type=Path, default=DEFAULT)
    parser.add_argument("--public-key", type=Path)
    parser.add_argument("--index", type=Path)
    parser.add_argument("--package", action="append", default=[], metavar="ID=PATH")
    parser.add_argument("--provenance", action="append", default=[], metavar="ID=PATH")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = receipt(args.manifest, args.public_key, args.index,
                     parse_bindings(args.package, "package"),
                     parse_bindings(args.provenance, "provenance"))
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        require(not args.output.exists() and not args.output.is_symlink(), "Output must be new")
        args.output.write_text(encoded)
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
