"""Build the native plugin and package it with the matching official CLI."""

import argparse
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
SUPPORTED = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
)


def run(*args, env=None):
    command = (shutil.which(args[0]) or args[0], *args[1:])
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=SUPPORTED, required=True)
    parser.add_argument("--zig", action="store_true", help="Cross-compile Linux using cargo-zigbuild")
    parser.add_argument("--install-cli", action="store_true", help="Install the pinned CLI into .tools")
    parser.add_argument("--cli", type=Path, help="Use an already installed matching cpr-plugin CLI")
    args = parser.parse_args()
    dependency = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["dependencies"]["gateway-plugin-sdk"]
    cli = args.cli or ROOT / ".tools" / "bin" / ("cpr-plugin.exe" if os.name == "nt" else "cpr-plugin")
    if args.install_cli:
        run("cargo", "install", "--locked", "--git", dependency["git"], "--rev", dependency["rev"],
            "codex-proxy-plugin-cli", "--root", str(ROOT / ".tools"))
    if not cli.is_file():
        parser.error("Matching cpr-plugin CLI not found; use --install-cli or --cli.")
    run("pnpm", "--dir", "frontend", "install", "--frozen-lockfile")
    run("pnpm", "--dir", "frontend", "build")
    env = os.environ.copy()
    if args.zig:
        if "linux" not in args.target:
            parser.error("--zig supports the Linux package targets only.")
        if not shutil.which("zig"):
            spec = importlib.util.find_spec("ziglang")
            if spec is None or not spec.origin:
                parser.error("Install Zig or the Python ziglang package before using --zig.")
            env["PATH"] = str(Path(spec.origin).parent) + os.pathsep + env.get("PATH", "")
        run("cargo", "zigbuild", "--release", "--locked", "--target", args.target + ".2.28", env=env)
    else:
        run("cargo", "build", "--release", "--locked", "--target", args.target)
    target_dir = Path(env.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    binary = target_dir / args.target / "release" / "cpr-plugin-bps"
    if not binary.is_file():
        raise SystemExit(f"Built binary not found: {binary}")
    run(str(cli.resolve()), "package", "--manifest", str(ROOT / "plugin.json"), "--binary", str(binary),
        "--target", args.target, "--resource-map", "web=frontend/dist", "--output-dir", str(ROOT / "dist"))


if __name__ == "__main__":
    main()
