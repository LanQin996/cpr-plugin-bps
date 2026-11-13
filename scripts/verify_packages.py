"""Verify local package checksums, resources and executable architectures."""

import hashlib
import json
from pathlib import Path
import struct
import tarfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    version = json.loads((ROOT / "plugin.json").read_text(encoding="utf-8"))["version"]
    packages = sorted((ROOT / "dist").glob(f"local.bps-{version}-*.tar.gz"))
    if not packages:
        raise SystemExit("No packages found in dist; build a package first.")
    for path in packages:
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        expected = path.with_name(path.name + ".sha256").read_text().split()[0]
        assert digest == expected, f"Archive checksum mismatch: {path.name}"
        with tarfile.open(path) as archive:
            assert all(member.isfile() for member in archive.getmembers())
            manifest = json.load(archive.extractfile("plugin.json"))
            package = manifest["package"]
            assert package["protocolVersion"] == 7
            assert set(archive.getnames()) == {"plugin.json", *package["files"]}
            for name, expected in package["files"].items():
                raw = archive.extractfile(name).read()
                assert hashlib.sha256(raw).hexdigest() == expected, name
                if name != manifest["main"]:
                    source = ROOT / "frontend" / "dist" / name[4:] if name.startswith("web/") else ROOT / name
                    assert raw == source.read_bytes(), f"Stale resource: {name}"
            binary = archive.extractfile(manifest["main"]).read()
            architecture = package["target"]["architecture"]
            if binary[:4] == b"\x7fELF":
                assert binary[:6] == b"\x7fELF\x02\x01"
                assert struct.unpack_from("<H", binary, 18)[0] == {
                    "x86_64": 62, "aarch64": 183,
                }[architecture]
                target = f"{architecture}-unknown-linux-gnu"
            else:
                assert binary[:4] == b"\xcf\xfa\xed\xfe"
                assert struct.unpack_from("<I", binary, 4)[0] == 0x0100000C
                assert architecture == "aarch64"
                target = "aarch64-apple-darwin"
            current = ROOT / "target" / target / "release" / "cpr-plugin-bps"
            assert binary == current.read_bytes(), f"Stale executable: {path.name}"
        print(f"Verified {path.name} ({path.stat().st_size} bytes)\nSHA-256 {digest}")


if __name__ == "__main__":
    main()
