"""Write the .mcpb manifest for one platform's release.

    python packaging/mcpb/make_manifest.py windows 0.1.2 dist/columbus/manifest.json
    python packaging/mcpb/make_manifest.py macos 0.1.2 dist/columbus/manifest.json

manifest.json beside this script is the Windows manifest; macOS changes the
program name, the platform and the folder names people are shown.
"""

import json
import sys
from pathlib import Path

MACOS_ORG = "/Library/Application Support/Columbus"


def manifest(platform: str, version: str) -> dict:
    m = json.loads((Path(__file__).parent / "manifest.json").read_text(encoding="utf-8"))
    m["version"] = version
    if platform == "windows":
        return m
    if platform != "macos":
        raise SystemExit(f"unknown platform {platform!r}: expected windows or macos")
    m["server"]["entry_point"] = "columbus-mcp"
    m["server"]["mcp_config"]["command"] = "${__dirname}/columbus-mcp"
    m["compatibility"]["platforms"] = ["darwin"]
    m["long_description"] = m["long_description"].replace(
        "engine\\setup-engine.ps1", "engine/setup-engine.sh"
    )
    uc = m["user_config"]
    uc["connections"]["description"] = (
        "Your connection files. Leave empty to use ~/Library/Application Support/Columbus/connections."
    )
    uc["context_pack"]["description"] = (
        f"Your organization's GIS context pack. Leave empty to use {MACOS_ORG}/context."
    )
    uc["python"]["description"] = (
        "The engine's Python: engine/setup-engine.sh prints the path to paste here."
    )
    return m


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    platform, version, out = sys.argv[1:]
    text = json.dumps(manifest(platform, version), indent=2, ensure_ascii=False) + "\n"
    if "%ProgramData%" in text and platform == "macos":
        raise SystemExit("a Windows folder name is left in the macOS manifest")
    Path(out).write_text(text, encoding="utf-8")


if __name__ == "__main__":
    main()
