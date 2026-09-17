#!/usr/bin/env python3
"""Verify a pinned core/integration set and optionally its public feed bytes.

This writes release provenance. It never downloads, signs, publishes, installs,
or adds an integration package to a runtime or installer payload.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile

REPO = Path(__file__).resolve().parents[2]
DEFAULT = Path(__file__).with_name("tested-integrations.json")
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")


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


def validate_manifest(path=DEFAULT):
    raw = regular(path, 1024 * 1024)
    manifest = json.loads(raw)
    exact(manifest, ("schema", "kind", "name", "core", "feed", "integrations", "rollout"), "manifest")
    require(manifest["schema"] == 1 and manifest["kind"] == "couch-tested-integration-set"
            and isinstance(manifest["name"], str) and manifest["name"], "Unsupported tested-set identity")
    core = manifest["core"]
    exact(core, ("repository", "tested_commit", "protocol_version", "compatibility_floor", "contract_paths"), "core")
    require(core["repository"] in github_repositories("couch")
            and HEX40.fullmatch(core["tested_commit"]) and core["protocol_version"] == 1,
            "Invalid core identity")
    require(commit_exists(core["tested_commit"]), "Tested core commit is absent from this checkout")
    head = git("rev-parse", "HEAD").stdout.strip()
    require(ancestor(core["tested_commit"], head), "Current checkout does not contain the tested core commit")
    require(isinstance(core["contract_paths"], list) and core["contract_paths"], "Core contract paths are empty")
    for item in core["contract_paths"]:
        require(isinstance(item, str) and item and not item.startswith("/") and ".." not in Path(item).parts,
                "Invalid core contract path")
    changed = git("diff", "--name-only", core["tested_commit"], head, "--", *core["contract_paths"]).stdout.splitlines()
    require(not changed, "Integration contract changed after its tested commit: " + ", ".join(changed))

    protocol = (REPO / "clients/couch-plugin/src/protocol.rs").read_text()
    require(re.search(r"pub const PROTOCOL_VERSION:\s*u32\s*=\s*1\s*;", protocol),
            "Current plugin protocol is not version 1")
    main = (REPO / "daemon/couch-confd/src/main.rs").read_text()
    require('"--supports-integration-protocol=1"' in main, "Core host support probe is absent")

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
                     "validated_behaviors", "not_validated", "artifact", "provenance"), where)
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
        entry = catalog_by_id.get(item["id"])
        require(entry and entry["tier"] == "preview" and entry["hardware_validation"]["status"] == "not-tested",
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
    require(all(value is False for value in rollout.values()), "Pilot rollout must remain explicit and unbundled")
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
        pinned_file(item["artifact"], packages[name], f"{name}.artifact")
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
    return {
        "schema": 1,
        "kind": "couch-tested-integration-set-verification",
        "name": manifest["name"],
        "manifest_sha256": digest(raw),
        "core_tested_commit": manifest["core"]["tested_commit"],
        "candidate_commit": head,
        "protocol_version": manifest["core"]["protocol_version"],
        "integration_versions": {item["id"]: item["version"] for item in manifest["integrations"]},
        "artifact_bytes_verified": artifacts,
        "rollout": manifest["rollout"],
    }


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
