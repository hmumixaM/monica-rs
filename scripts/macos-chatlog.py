#!/usr/bin/env python3
"""Build the pinned Chatlog Beta snapshot service and retain its dependency notices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PIN = ROOT / "apps/desktop/patches/chatlog/beta.json"
NOTICE_PREFIXES = ("LICENSE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")


def copy_notices(metadata, notices):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node["deps"] for node in metadata["resolve"]["nodes"]}
    pending = [metadata["resolve"]["root"]]
    visited = set()
    inventory = []
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        package = packages[package_id]
        pending.extend(
            dep["pkg"] for dep in nodes[package_id]
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
        )
        root = Path(package["manifest_path"]).parent
        files = {path for path in root.iterdir() if path.is_file() and path.name.upper().startswith(NOTICE_PREFIXES)}
        if license_file := package["license_file"]:
            files.add(root / license_file)
        if not files:
            raise RuntimeError(f"Missing dependency notice: {package['name']} {package['version']}")
        destination = notices / f"{package['name']}-{package['version']}"
        destination.mkdir(parents=True)
        for path in sorted(files):
            shutil.copy2(path, destination / path.name)
        inventory.append({key: package[key] for key in ("name", "version", "license")})
    return sorted(inventory, key=lambda package: (package["name"], package["version"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        parser.error("Output must be absent")

    pin = json.loads(PIN.read_text())
    with tempfile.TemporaryDirectory(prefix="monica-chatlog-beta-") as directory:
        checkout = Path(directory) / "source"
        subprocess.run(["git", "clone", "--quiet", "--no-checkout", pin["repository"], checkout], check=True)
        subprocess.run(["git", "-C", checkout, "checkout", "--quiet", "--detach", pin["commit"]], check=True)
        target = Path(directory) / "target"
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "--package", "chatlog-beta",
             "--target", pin["target"], "--target-dir", target],
            cwd=checkout, env={**os.environ, "MACOSX_DEPLOYMENT_TARGET": "26.0"}, check=True,
        )
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", pin["target"]],
            cwd=checkout,
        ))
        dependencies = copy_notices(metadata, output / "licenses")
        binary = output / "chatlog-beta"
        shutil.copy2(target / pin["target"] / "release" / binary.name, binary)
        shutil.copy2(checkout / "Cargo.lock", output / "Cargo.lock")
        manifest = {
            **pin, "binary": binary.name, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "cargo": subprocess.check_output(["cargo", "--version"], cwd=checkout, text=True).strip(),
            "dependencies": dependencies,
        }
        (output / "source.json").write_text(json.dumps(manifest, indent=2) + "\n")
    subprocess.run([binary, "--help"], check=True, stdout=subprocess.DEVNULL)
    print(json.dumps({"output": str(output), "commit": pin["commit"], "packages": len(dependencies)}))


if __name__ == "__main__":
    main()
