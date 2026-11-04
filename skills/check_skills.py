"""Check the skills library: every skill folder has a valid SKILL.md, and every
Columbus tool a skill names in backticks is a tool the server really has.

Run from the repository root:  python skills/check_skills.py
Uses only the standard library.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / "skills"
GATEWAY = ROOT / "crates" / "gateway" / "src"

NAME = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
ALLOWED_KEYS = {"name", "description", "license", "metadata", "compatibility", "allowed-tools"}
MAX_BODY_LINES = 500

# Tools the design plans but the server does not have yet. A skill may name
# them only to say they are not available.
PLANNED = {
    "pointcloud_pipeline", "run_python", "propose_edits",
    "apply_edits",
    "draft_context_from_geodatabase",
}


def server_tools() -> set[str]:
    tools: set[str] = set()
    for f in GATEWAY.glob("*.rs"):
        text = f.read_text(encoding="utf-8")
        for m in re.finditer(r"#\[tool\((?:[^\]]|\][^\n])*?\)\]\s*(?:pub(?:\([a-z]+\))?\s+)?async fn (\w+)", text, re.S):
            tools.add(m.group(1))
    return tools


def front_matter(text: str) -> tuple[dict[str, str], list[str]] | None:
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return None
    try:
        end = next(i for i in range(1, len(lines)) if lines[i].strip() == "---")
    except StopIteration:
        return None
    fields: dict[str, str] = {}
    key = None
    for raw in lines[1:end]:
        if not raw.strip():
            continue
        m = re.match(r"^([A-Za-z][\w-]*):\s*(.*)$", raw)
        if m and not raw.startswith((" ", "\t")):
            key = m.group(1)
            fields[key] = m.group(2).strip()
        elif key:
            fields[key] = (fields[key] + " " + raw.strip()).strip()
    for k, v in fields.items():
        if len(v) >= 2 and v[0] == v[-1] and v[0] in "'\"":
            fields[k] = v[1:-1]
        if v in (">", "|", ">-", "|-"):
            fields[k] = ""
    return fields, lines[end + 1:]


def check() -> list[str]:
    problems: list[str] = []
    tools = server_tools()
    if len(tools) < 15:
        problems.append(f"found only {len(tools)} server tools in {GATEWAY}; the tool scan is broken")
    known = tools | PLANNED
    # Words in backticks that look like tool names: snake_case with an underscore.
    tool_like = re.compile(r"`([a-z]+(?:_[a-z]+)+)(?:\s*\{[^`]*\})?`")
    # Prefixes Columbus tools use; plain snake_case words (field names, SQL,
    # library functions) with other prefixes are not checked.
    tool_prefixes = {"describe", "registry", "context", "approval", "vector", "sample",
                     "sql", "pointcloud", "raster", "export", "propose", "apply"}

    folders = sorted(p for p in SKILLS.iterdir() if p.is_dir() and not p.name.startswith((".", "_")))
    for folder in folders:
        skill = folder / "SKILL.md"
        where = skill.relative_to(ROOT)
        if not skill.is_file():
            problems.append(f"{folder.relative_to(ROOT)}: no SKILL.md")
            continue
        text = skill.read_text(encoding="utf-8")
        parsed = front_matter(text)
        if parsed is None:
            problems.append(f"{where}: no front matter between --- lines")
            continue
        fields, body = parsed
        extra = set(fields) - ALLOWED_KEYS
        if extra:
            problems.append(f"{where}: unexpected front matter keys {sorted(extra)}")
        name = fields.get("name", "")
        if name != folder.name:
            problems.append(f"{where}: name {name!r} does not match the folder {folder.name!r}")
        if not NAME.match(name) or len(name) > 64:
            problems.append(f"{where}: name must be lower-case words joined by '-', at most 64 characters")
        desc = fields.get("description", "")
        if not desc:
            problems.append(f"{where}: description is missing")
        elif len(desc) > 1024:
            problems.append(f"{where}: description is {len(desc)} characters (at most 1024)")
        if len(body) > MAX_BODY_LINES:
            problems.append(f"{where}: body is {len(body)} lines (at most {MAX_BODY_LINES}; move detail to references/)")

        for md in [skill, *sorted(folder.rglob("*.md"))]:
            if md != skill and md.name == "SKILL.md":
                continue
            content = md.read_text(encoding="utf-8")
            rel = md.relative_to(ROOT)
            for m in re.finditer(r"\]\(([^)\s#]+)(?:#[^)]*)?\)", content):
                target = m.group(1)
                if re.match(r"^[a-z]+:", target):
                    continue
                if not (md.parent / target).exists():
                    problems.append(f"{rel}: link to {target} does not resolve")
            for m in tool_like.finditer(content):
                word = m.group(1)
                if word.split("_")[0] in tool_prefixes and word not in known:
                    problems.append(f"{rel}: names a tool `{word}` the server does not have")
    return problems


def main() -> int:
    problems = check()
    for p in problems:
        print(p)
    if problems:
        print(f"{len(problems)} problem(s)")
        return 1
    print("skills ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
