#!/usr/bin/env python3
"""Validate Bubble's freedesktop icon theme and explicit aliases."""
from __future__ import annotations
import configparser, json, sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "data/icons/Bubble"

def main() -> int:
    index = ROOT / "index.theme"
    if not index.is_file():
        print(f"ERROR: missing {index}", file=sys.stderr); return 1
    parser = configparser.ConfigParser(interpolation=None, strict=True)
    try:
        with index.open(encoding="utf-8") as stream: parser.read_file(stream)
    except (OSError, configparser.Error) as error:
        print(f"ERROR: invalid index.theme: {error}", file=sys.stderr); return 1
    if not parser.has_section("Icon Theme"):
        print("ERROR: missing [Icon Theme] section", file=sys.stderr); return 1
    theme = parser["Icon Theme"]
    if not theme.get("Name", "").strip():
        print("ERROR: theme Name is empty", file=sys.stderr); return 1
    directories = [item.strip() for item in theme.get("Directories", "").split(",") if item.strip()]
    if not directories:
        print("ERROR: no icon directories declared", file=sys.stderr); return 1
    errors, icon_count = [], 0
    for directory in directories:
        if not parser.has_section(directory):
            errors.append(f"missing [{directory}] section"); continue
        metadata = parser[directory]
        if metadata.get("Type") != "Scalable": errors.append(f"[{directory}] must declare Type=Scalable")
        if metadata.get("Size") != "scalable": errors.append(f"[{directory}] must declare Size=scalable")
        folder = ROOT / directory
        if not folder.is_dir():
            errors.append(f"declared directory does not exist: {directory}"); continue
        for svg in sorted(folder.glob("*.svg")):
            icon_count += 1
            try: root = ET.parse(svg).getroot()
            except (ET.ParseError, OSError) as error:
                errors.append(f"{svg.relative_to(ROOT)}: invalid SVG ({error})"); continue
            if not root.tag.endswith("svg"): errors.append(f"{svg.relative_to(ROOT)}: root element is not svg")
            if not root.get("viewBox"): errors.append(f"{svg.relative_to(ROOT)}: missing viewBox")
    try:
        data = json.loads((ROOT / "aliases.json").read_text(encoding="utf-8"))
        aliases = data["aliases"]
        if data.get("version") != 1 or not isinstance(aliases, dict): raise ValueError("expected version 1 and an aliases object")
    except (OSError, json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        errors.append(f"invalid aliases.json: {error}"); aliases = {}
    apps = ROOT / "scalable/apps"
    for alias, target in aliases.items():
        if not isinstance(alias, str) or not isinstance(target, str) or not alias or not target:
            errors.append(f"invalid alias entry: {alias!r} -> {target!r}"); continue
        alias_path, target_path = apps / f"{alias}.svg", apps / f"{target}.svg"
        if not target_path.is_file(): errors.append(f"alias target does not exist: {target}.svg")
        if not alias_path.is_symlink(): errors.append(f"alias must be a relative symlink: {alias}.svg")
        elif alias_path.readlink().as_posix() != f"{target}.svg": errors.append(f"alias {alias}.svg must point to {target}.svg")
    if errors:
        for error in errors: print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(f"OK: {theme['Name']} theme; {len(directories)} directories; {icon_count} SVG names; {len(aliases)} aliases validated")
    return 0

if __name__ == "__main__": raise SystemExit(main())
