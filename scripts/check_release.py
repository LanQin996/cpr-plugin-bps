"""Validate release versions and the complete Linux release asset set."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {"x86_64": 62, "aarch64": 183}


def check_tag(tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError("Run the release workflow on a v<version> tag, not on a branch.")
    version = tag[1:]
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    plugin = json.loads((ROOT / "plugin.json").read_text(encoding="utf-8"))
    if cargo["package"]["version"] != version or plugin["version"] != version:
        raise ValueError("The tag, Cargo.toml and plugin.json versions must match.")
    return version


def check_assets(directory, version):
    expected = {
        f"local.bps-{version}-{arch}-unknown-linux-gnu.tar.gz{suffix}"
        for arch in TARGETS for suffix in ("", ".sha256")
    }
    if {path.name for path in directory.iterdir()} != expected:
        raise ValueError("Both Linux packages and their checksum files are required.")
    for arch, machine in TARGETS.items():
        path = directory / f"local.bps-{version}-{arch}-unknown-linux-gnu.tar.gz"
        checksum = path.with_name(path.name + ".sha256").read_text().split()
        if checksum != [hashlib.sha256(path.read_bytes()).hexdigest(), path.name]:
            raise ValueError(f"Archive checksum mismatch: {path.name}")
        with tarfile.open(path) as archive:
            manifest = json.load(archive.extractfile("plugin.json"))
            package = manifest["package"]
            if (manifest["publisher"], manifest["name"], manifest["version"]) != ("local", "bps", version):
                raise ValueError(f"Unexpected plugin identity or version: {path.name}")
            if package["protocolVersion"] != 7 or package["target"] != {"os": "linux", "architecture": arch}:
                raise ValueError(f"Unexpected package target or RPC version: {path.name}")
            if not all(member.isfile() for member in archive.getmembers()):
                raise ValueError("Package must contain only regular files.")
            if set(archive.getnames()) != {"plugin.json", *package["files"]}:
                raise ValueError("Unexpected package members.")
            for name, digest in package["files"].items():
                if hashlib.sha256(archive.extractfile(name).read()).hexdigest() != digest:
                    raise ValueError(f"Package member checksum mismatch: {name}")
            binary = archive.extractfile(manifest["main"]).read()
            if binary[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", binary, 18)[0] != machine:
                raise ValueError(f"Executable architecture mismatch: {path.name}")
        print(f"Verified {path.name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--assets", type=Path)
    args = parser.parse_args()
    version = check_tag(args.tag)
    if args.assets:
        check_assets(args.assets, version)
    print(f"Release {args.tag} validated")


if __name__ == "__main__":
    main()
