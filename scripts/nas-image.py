#!/usr/bin/env python3
"""Assemble the NAS image for a commit without Docker.

Usage: nas-image.py COMMIT [--out DIR]

Cross-compiles monica-server for the NAS (cargo-zigbuild, glibc 2.36), builds
apps/web from the same commit, and stacks both on the Debian/ffmpeg layers of
the last image built by infra/Dockerfile. Those base layers are only valid
while infra/Dockerfile is unchanged since that image.

The toolchain and the base layers live in the main checkout's
.local-validation/nas-migration-20261113 directory.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
import subprocess
import tarfile
from pathlib import Path

BASE_COMMIT = "5b95f20257f39b450f3f3b3d1bc955af8e448361"
TARGET = "x86_64-unknown-linux-gnu.2.36"


def git(*args: str, cwd: Path) -> str:
    return subprocess.check_output(["git", *args], cwd=cwd, text=True).strip()


def sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def add_bytes(tar: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.mtime = len(data), 0o644, 0
    tar.addfile(info, io.BytesIO(data))


def add_file(tar: tarfile.TarFile, name: str, path: Path, mode: int) -> None:
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.mtime = path.stat().st_size, mode, 0
    with path.open("rb") as data:
        tar.addfile(info, data)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("commit")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    here = Path(__file__).resolve().parents[1]
    main_checkout = Path(git("rev-parse", "--path-format=absolute", "--git-common-dir", cwd=here)).parent
    assets = main_checkout / ".local-validation/nas-migration-20261113"
    tools = assets / "linux-build-tools"
    commit = git("rev-parse", args.commit, cwd=here)
    subprocess.run(["git", "diff", "--quiet", BASE_COMMIT, commit, "--", "infra/Dockerfile"], cwd=here, check=True)
    out = args.out or main_checkout / ".local-validation/nas-images"
    out.mkdir(parents=True, exist_ok=True)

    source = out / f"source-{commit[:12]}"
    if not source.exists():
        source.mkdir()
        archive = subprocess.run(
            ["git", "archive", commit, "Cargo.toml", "Cargo.lock", "crates", "migrations", "apps/web", "packages"],
            cwd=here, check=True, capture_output=True).stdout
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(source)

    env = dict(
        os.environ,
        RUSTC=str(tools / "sysroot-rust-1.92.0/bin/rustc"),
        RUSTDOC=str(tools / "sysroot-rust-1.92.0/bin/rustdoc"),
        CARGO_TARGET_DIR=str(assets / "linux-target-rust192"),
        CARGO_ZIGBUILD_CACHE_DIR=str(tools / "cargo-zigbuild-cache"),
        CARGO_ZIGBUILD_ZIG_PATH=str(tools / "zig-aarch64-macos-0.15.2/zig"),
    )
    subprocess.run(
        [str(tools / "cargo-zigbuild-aarch64-apple-darwin/cargo-zigbuild"), "zigbuild", "--release", "--locked",
         "--target", TARGET, "-p", "monica-server"], cwd=source, env=env, check=True)
    binary = assets / "linux-target-rust192/x86_64-unknown-linux-gnu/release/monica-server"
    kind = subprocess.check_output(["file", str(binary)], text=True)
    assert "ELF 64-bit" in kind and "x86-64" in kind, kind

    web = source / "apps/web"
    subprocess.run(["npm", "ci", "--no-audit", "--no-fund"], cwd=web, check=True)
    subprocess.run(["npm", "run", "build"], cwd=web, check=True)

    base = assets / f"image-build-{BASE_COMMIT[:12]}"
    with tarfile.open(assets / f"monica-rs-validated-{BASE_COMMIT[:12]}-linux-amd64.tar.gz", "r:gz") as previous:
        manifest = json.load(previous.extractfile("manifest.json"))[0]
        config = json.loads(previous.extractfile(manifest["Config"]).read())

    binary_layer = out / f"binary-{commit[:12]}.tar"
    with tarfile.open(binary_layer, "w") as layer:
        add_file(layer, "usr/local/bin/monica-server", binary, 0o755)
    web_layer = out / f"web-{commit[:12]}.tar"
    with tarfile.open(web_layer, "w") as layer:
        for path in sorted(p for p in (web / "dist").rglob("*") if p.is_file()):
            add_file(layer, f"opt/monica/web/{path.relative_to(web / 'dist').as_posix()}", path, 0o644)

    layers = [base / "base-0.tar", base / "base-1.tar", binary_layer, web_layer]
    diff_ids = ["sha256:" + sha(path) for path in layers]
    assert diff_ids[:2] == config["rootfs"]["diff_ids"][:2], "base layers differ from the reference image"
    config["rootfs"]["diff_ids"] = diff_ids
    config["history"] = config["history"][:-2] + [
        {"created_by": f"COPY monica-server {commit}"}, {"created_by": f"COPY Vite dist {commit}"}]
    config["config"]["Labels"].update({
        "org.opencontainers.image.revision": commit, "org.opencontainers.image.version": commit[:12],
        "monica.binary-sha256": sha(binary)})
    config["config"]["Env"] = [v for v in config["config"]["Env"] if not v.startswith("MONICA_BUILD_COMMIT=")] + [f"MONICA_BUILD_COMMIT={commit}"]
    config_bytes = json.dumps(config, sort_keys=True, separators=(",", ":")).encode()
    image = hashlib.sha256(config_bytes).hexdigest()
    names = [d.split(":")[1] + "/layer.tar" for d in diff_ids]
    package = out / f"monica-rs-{commit[:12]}-linux-amd64.tar.gz"
    with package.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=1, mtime=0) as packed:
        with tarfile.open(fileobj=packed, mode="w|") as tar:
            add_bytes(tar, image + ".json", config_bytes)
            add_bytes(tar, "manifest.json", json.dumps([{"Config": image + ".json", "RepoTags": [f"monica-rs:nas-{commit}"], "Layers": names}]).encode())
            for path, name in zip(layers, names):
                add_file(tar, name, path, 0o644)
    result = {"commit": commit, "image": "sha256:" + image, "tag": f"monica-rs:nas-{commit}", "archive": str(package),
              "archive_sha256": sha(package), "binary_sha256": sha(binary)}
    (out / f"image-{commit[:12]}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
