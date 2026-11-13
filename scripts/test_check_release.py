"""Exercise release tag and attachment validation with isolated fixtures."""

import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import check_release


class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n')
        (self.root / "plugin.json").write_text('{"version":"0.1.0"}')
        self.patch = patch.object(check_release, "ROOT", self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def packages(self):
        for arch, machine in check_release.TARGETS.items():
            binary = b"\x7fELF\x02\x01" + bytes(12) + machine.to_bytes(2, "little")
            manifest = {
                "publisher": "local", "name": "bps", "version": "0.1.0",
                "main": "bin/plugin", "package": {
                    "protocolVersion": 7, "target": {"os": "linux", "architecture": arch},
                    "files": {"bin/plugin": hashlib.sha256(binary).hexdigest()},
                },
            }
            path = self.root / f"local.bps-0.1.0-{arch}-unknown-linux-gnu.tar.gz"
            with tarfile.open(path, "w:gz") as archive:
                for name, content in {"plugin.json": json.dumps(manifest).encode(), "bin/plugin": binary}.items():
                    info = tarfile.TarInfo(name)
                    info.size = len(content)
                    archive.addfile(info, io.BytesIO(content))
            path.with_name(path.name + ".sha256").write_text(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n")

    def test_tag_must_match_both_source_versions(self):
        self.assertEqual(check_release.check_tag("v0.1.0"), "0.1.0")
        for tag in ("main", "v0.2.0", "v0.1.0/unsafe"):
            with self.assertRaises(ValueError):
                check_release.check_tag(tag)
        (self.root / "plugin.json").write_text('{"version":"0.2.0"}')
        with self.assertRaises(ValueError):
            check_release.check_tag("v0.1.0")

    def test_complete_release_verifies_and_missing_or_tampered_files_fail(self):
        self.packages()
        assets = self.root / "dist"
        assets.mkdir()
        for path in self.root.glob("*.tar.gz*"):
            path.rename(assets / path.name)
        check_release.check_assets(assets, "0.1.0")
        path = next(assets.glob("*.tar.gz"))
        path.write_bytes(path.read_bytes() + b"tampered")
        with self.assertRaises(ValueError):
            check_release.check_assets(assets, "0.1.0")
        path.unlink()
        with self.assertRaises(ValueError):
            check_release.check_assets(assets, "0.1.0")


if __name__ == "__main__":
    unittest.main()
