#!/usr/bin/env python3
"""Record explicit compiler/source provenance for downloadable installer binaries."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


TARGETS = {
    "linux-x64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "macos-x64": "x86_64-apple-darwin",
    "macos-arm64": "aarch64-apple-darwin",
    "windows-x64": "x86_64-pc-windows-msvc",
}

VERSION = re.compile(
    r"^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$"
)


def command(*args):
    return subprocess.check_output(args, text=True, timeout=60).strip()


def file_receipt(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError("receipt input must be a regular file")
    size = path.stat().st_size
    if size > 512 * 1024 * 1024:
        raise ValueError("receipt input exceeds size bound")
    digest, count = hashlib.sha256(), 0
    with path.open("rb") as file:
        while block := file.read(1024 * 1024):
            digest.update(block)
            count += len(block)
    if count != size:
        raise ValueError("receipt input changed size")
    return {"size": size, "sha256": digest.hexdigest()}


def source_commit():
    source = command("git", "rev-parse", "HEAD")
    if not re.fullmatch(r"[a-f0-9]{40}", source):
        raise ValueError("invalid source commit")
    subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], check=True, timeout=60)
    return source


def validate_installer_version(version):
    if not isinstance(version, str) or len(version) > 128 or not VERSION.fullmatch(version):
        raise ValueError("invalid installer VERSION")
    return version


def read_installer_version(root):
    path = Path(root) / "VERSION"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 129:
        raise ValueError("installer VERSION must be a small regular file")
    version = path.read_text(encoding="utf-8").strip()
    return validate_installer_version(version)


def native(platform, source, root, installer_version=None):
    installer_version = (read_installer_version(root) if installer_version is None
                         else validate_installer_version(installer_version))
    target = TARGETS[platform]
    suffix = ".exe" if platform == "windows-x64" else ""
    compiler = Path(command("rustup", "which", "rustc"))
    cargo = Path(command("rustup", "which", "cargo"))
    libdir = Path(command("rustc", "--print", "target-libdir", "--target", target))
    libraries = sorted(path for path in libdir.iterdir() if path.is_file())
    if not libraries or len(libraries) > 256:
        raise ValueError("unexpected target sysroot inventory")
    return {
        "schema": 1, "kind": "couch-installer-native-build", "source_commit": source,
        "installer_version": installer_version,
        "platform": platform, "target": target,
        "toolchain": {
            "rustc_verbose": command("rustc", "-vV"),
            "cargo_verbose": command("cargo", "-vV"),
            "active_toolchain": command("rustup", "show", "active-toolchain").split()[0],
            "rustc": file_receipt(compiler), "cargo": file_receipt(cargo),
            "target_sysroot_files": {path.name: file_receipt(path) for path in libraries},
        },
        "binaries": {
            component: file_receipt(root / component / "target" / target / "release" /
                                    f"couch-installer-{component}{suffix}")
            for component in ("host", "tui")
        },
    }


def universal(source, inputs, output, installer_version):
    installer_version = validate_installer_version(installer_version)
    receipts = {}
    for platform in ("macos-x64", "macos-arm64"):
        path = inputs / f"couch-installer-build-{platform}" / "build.json"
        if path.stat().st_size > 1024 * 1024:
            raise ValueError("native receipt exceeds bound")
        raw = path.read_bytes()
        receipt = json.loads(raw)
        if (receipt.get("schema") != 1 or receipt.get("kind") != "couch-installer-native-build"
                or receipt.get("source_commit") != source
                or receipt.get("installer_version") != installer_version
                or receipt.get("platform") != platform
                or receipt.get("target") != TARGETS[platform]
                or set(receipt.get("binaries", {})) != {"host", "tui"}):
            raise ValueError("native receipt source/platform differs")
        for component in ("host", "tui"):
            binary = inputs / f"couch-installer-{component}-{platform}" / f"couch-installer-{component}"
            if file_receipt(binary) != receipt["binaries"][component]:
                raise ValueError("downloaded native binary differs from build receipt")
        receipts[platform] = {
            "receipt": {"size": len(raw), "sha256": hashlib.sha256(raw).hexdigest()},
            "build": receipt,
        }
    return {
        "schema": 1, "kind": "couch-installer-universal-build", "source_commit": source,
        "installer_version": installer_version,
        "platform": "macos-universal", "architectures": ["x86_64", "arm64"],
        "inputs": receipts,
        "binaries": {component: file_receipt(output / f"couch-installer-{component}")
                     for component in ("host", "tui")},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=[*TARGETS, "macos-universal"], required=True)
    parser.add_argument("--root", type=Path, default=Path("tools/installer"))
    parser.add_argument("--inputs", type=Path, default=Path("binaries"))
    parser.add_argument("--universal-output", type=Path, default=Path("universal"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = source_commit()
    installer_version = read_installer_version(args.root)
    if args.platform == "macos-universal":
        receipt = universal(source, args.inputs, args.universal_output, installer_version)
        receipt["xcode_version"] = command("xcodebuild", "-version")
    else:
        receipt = native(args.platform, source, args.root, installer_version)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8") as file:
        json.dump(receipt, file, indent=2, sort_keys=True)
        file.write("\n")


if __name__ == "__main__":
    main()
