"""Build exact-source capture downloads and complete immutable-release assets."""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib

TARGET = "x86_64-unknown-linux-gnu"
TOOLCHAIN = "1.97.1"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_versions(root):
    root = Path(root)
    package = tomllib.loads((root / "capture-service/Cargo.toml").read_text())["package"]
    version = package["version"]
    locked = [item for item in tomllib.loads((root / "capture-service/Cargo.lock").read_text())["package"]
              if item["name"] == "louiselm-capture"]
    require(package["name"] == "louiselm-capture" and re.fullmatch(r"0\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version)
            and len(locked) == 1 and locked[0]["version"] == version
            and json.loads((root / ".release-please-manifest.json").read_text())["capture-service"] == version,
            "capture Cargo/lock/release manifest must agree and remain 0.x.y")
    return version


def package(binary, output, sha, version, metadata):
    require(re.fullmatch(r"[0-9a-f]{40}", sha), "invalid source commit")
    require(metadata["component"] == "capture" and metadata["version"] == version,
            "built binary has the wrong identity")
    output.mkdir(parents=True, exist_ok=True)
    name = f"louiselm-capture-{version}-{TARGET}"
    record = dict(metadata, source_commit=sha, target=TARGET, rust=TOOLCHAIN,
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
    encoded = (json.dumps(record, sort_keys=True, indent=2) + "\n").encode()
    archive = output / (name + ".tar.gz")
    # Fixed headers make interrupted-upload retries compare exact bytes.
    with archive.open("wb") as raw, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as bundle:
            for filename, data, mode in [("louiselm-capture", binary.read_bytes(), 0o755), ("metadata.json", encoded, 0o644)]:
                info = tarfile.TarInfo(filename)
                info.size, info.mode, info.mtime = len(data), mode, 0
                bundle.addfile(info, io.BytesIO(data))
    metadata_path = output / (name + ".json")
    metadata_path.write_bytes(encoded)
    checksum = output / (name + ".sha256")
    checksum.write_text("".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
                                for path in (archive, metadata_path)))
    return [archive, metadata_path, checksum]


def build(sha, output):
    require(re.fullmatch(r"[0-9a-f]{40}", sha), "build requires an exact source commit")
    with tempfile.TemporaryDirectory(prefix="capture-release-source-") as directory:
        root = Path(directory)
        # Only Git's approved tree enters the build, never the caller's worktree.
        archive = subprocess.check_output(["git", "archive", sha])
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            source.extractall(root, filter="data")
        version = check_versions(root)
        env = {key: value for key, value in os.environ.items()
               if key in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR")}
        env["RUSTFLAGS"] = f"--remap-path-prefix={root}=/source"
        env["SOURCE_DATE_EPOCH"] = "0"
        subprocess.run(["cargo", f"+{TOOLCHAIN}", "build", "--release", "--locked", "--target", TARGET],
                       cwd=root / "capture-service", env=env, check=True)
        binary = root / "capture-service/target" / TARGET / "release/louiselm-capture"
        require(subprocess.check_output([str(binary), "--version"], text=True, env=env).strip() == f"louiselm-capture {version}",
                "built binary version disagrees with source")
        metadata = json.loads(subprocess.check_output([str(binary), "metadata"], text=True, env=env))
        return package(binary, Path(output), sha, version, metadata)


def upload_missing(repository, release, files, api, upload):
    """Never replace an asset: validate all existing bytes before any upload."""
    expected = {path.name: (path, "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()) for path in files}
    assets = release["assets"]
    require(len({asset["name"] for asset in assets}) == len(assets), "duplicate capture asset names")
    for asset in assets:
        require(asset["name"] in expected and asset.get("state") == "uploaded"
                and asset.get("digest") == expected[asset["name"]][1],
                "existing capture asset differs; inspect draft, never overwrite")
    missing = set(expected) - {asset["name"] for asset in assets}
    require(release["draft"] or not missing, "published capture assets are incomplete")
    for name in sorted(missing):
        upload(repository, release["tag_name"], expected[name][0])
    actual = api(f"repos/{repository}/releases/{release['id']}")["assets"]
    require(len(actual) == len(expected) and {asset["name"] for asset in actual} == set(expected)
            and all(asset.get("state") == "uploaded" and asset.get("digest") == expected[asset["name"]][1] for asset in actual),
            "capture assets are incomplete or corrupt; leave draft unpublished")


def prepare_assets(repository, release, sha, version):
    def api(path):
        return json.loads(subprocess.check_output(["gh", "api", path], text=True))

    def upload(repo, tag, path):
        subprocess.run(["gh", "release", "upload", tag, str(path), "--repo", repo], check=True)

    with tempfile.TemporaryDirectory(prefix="capture-release-assets-") as output:
        files = build(sha, output)
        record = json.loads(next(path for path in files if path.suffix == ".json").read_text())
        require(record["version"] == version, "capture asset version disagrees with release")
        upload_missing(repository, release, files, api, upload)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--sha")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.check:
        print(check_versions(Path(__file__).resolve().parent.parent))
    else:
        require(args.sha and args.output, "building requires --sha and --output")
        print(json.dumps([str(path) for path in build(args.sha, args.output)]))
