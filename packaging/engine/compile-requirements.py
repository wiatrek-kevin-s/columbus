"""Regenerates Columbus's hash-pinned requirement files from their sources.

Each workers/python/requirements-<name>.in (the human-readable intent: ranges
and why each package is there) becomes requirements-<name>.txt: every
package, dependencies included, at one exact version, with the SHA-256 of
every file PyPI has for it, resolved once for every system Columbus supports
(uv's universal resolution, CPython 3.12 and later). The engine is then
installed from the .txt files with pip's --require-hashes, so a file whose
bytes differ from the pinned hash is refused.

    python packaging/engine/compile-requirements.py            # keep current pins; add or drop what the .in files changed
    python packaging/engine/compile-requirements.py --upgrade  # move every package to the newest version its range allows

It runs Columbus's own uv (the one requirements-uv.txt installs in the
engine), or `--uv <path>`. After resolving, it checks that every pinned
version has a wheel for Windows x64, macOS arm64 (15 or later) and Linux x64
on CPython 3.12 and 3.13, and stops with the package's name if one doesn't.
"""

import argparse
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
REQ = os.path.join(ROOT, "workers", "python")
COMMAND = "python packaging/engine/compile-requirements.py"

# In order: each file is resolved with the ones before it (listed here) as
# constraints, so packages they share get the same version and the files
# install side by side.
FILES = [
    ("gis", []),
    ("db", ["gis"]),
    ("projectdb", []),
    ("uv", []),
    ("ci", ["gis", "db"]),
]

PLATFORMS = ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
# The oldest macOS the wheels must install on (docs/install-macos.md):
# rasterio 1.5 publishes macOS wheels for 15 and later only (pyproj 3.7+
# for 14 and later).
MACOS = "15.0"
PYTHONS = ["3.12", "3.13"]


def find_uv(given):
    if given:
        return given
    try:
        import uv  # Columbus's own, from requirements-uv.txt

        return uv.find_uv_bin()
    except Exception:
        found = shutil.which("uv")
        if found:
            return found
    sys.exit("uv not found: install workers/python/requirements-uv.txt into this Python, or pass --uv <path>")


def run(cmd, env=None):
    print("+", " ".join(cmd), flush=True)
    result = subprocess.run(cmd, cwd=REQ, env=env)
    if result.returncode != 0:
        sys.exit(f"stopped: {cmd[0]} exited with {result.returncode}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--upgrade", action="store_true", help="move every package to the newest allowed version")
    parser.add_argument("--uv", help="the uv to run (default: the engine's own)")
    parser.add_argument("--no-check", action="store_true", help="skip the per-system wheel check")
    args = parser.parse_args()
    uv = find_uv(args.uv)
    env_note = os.environ.get("UV_INDEX_URL") or os.environ.get("UV_DEFAULT_INDEX")
    if env_note:
        print(f"note: uv's index is set by the environment ({env_note})")

    for name, constraints in FILES:
        source = f"requirements-{name}.in"
        output = f"requirements-{name}.txt"
        cmd = [
            uv, "pip", "compile", source,
            "--output-file", output,
            "--universal",
            "--python-version", PYTHONS[0],
            "--generate-hashes",
            "--no-config",
            "--quiet",
            "--custom-compile-command", COMMAND,
        ]
        for c in constraints:
            cmd += ["--constraint", f"requirements-{c}.txt"]
        if args.upgrade:
            cmd.append("--upgrade")
        run(cmd)

    if args.no_check:
        return
    # Every pinned version must have a wheel for each system and Python
    # Columbus supports: resolving the .txt again for that one target, wheels
    # only, fails naming the package that has none.
    env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET=MACOS)
    for name, _ in FILES:
        output = f"requirements-{name}.txt"
        for platform in PLATFORMS:
            for python in PYTHONS:
                run([
                    uv, "pip", "compile", output,
                    "--python-platform", platform,
                    "--python-version", python,
                    "--only-binary", ":all:",
                    "--no-deps" if name in ("projectdb", "uv") else "--no-annotate",
                    "--no-config",
                    "--quiet",
                    "--output-file", os.devnull,
                ], env=env)
    print("done: the requirement files are pinned and every system has wheels")


if __name__ == "__main__":
    main()
