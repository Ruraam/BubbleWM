#!/usr/bin/env python3
"""Validate Bubble's freedesktop icon theme using only the Python standard library."""

from __future__ import annotations

import configparser
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "data/icons/Bubble"


def main() -> int:
    index = ROOT / "index.theme"
    if not index.is_file():
        print(f"ERROR: missing {index}", file=sys.stderr)
        return 1

    parser = configparser.ConfigParser(interpolation=None, strict=True)
    try:
        with index.open(encoding="utf-8") as stream:
            parser.read_file(stream)
    except (OSError, configparser.Error) as error:
        print(f"ERROR: invalid index.theme: {error}", file=sys.stderr)
        return 1

    if not parser.has_section("Icon Theme"):
        print("ERROR: missing [Icon Theme] section", file=sys.stderr)
        return 1

    theme = parser["Icon Theme"]
    if not theme.get("Name", "").strip():
        print("ERROR: theme Name is empty", file=sys.stderr)
        return 1

    directories = [item.strip() for item in theme.get("Directories", "").split(",") if item.strip()]
    if not directories:
        print("ERROR: no icon directories declared", file=sys.stderr)
        return 1

    errors: list[str] = []
    for directory in directories:
        if not parser.has_section(directory):
            errors.append(f"missing [{directory}] section")
            continue
        metadata = parser[directory]
        if metadata.get("Type") != "Scalable":
            errors.append(f"[{directory}] must declare Type=Scalable")
        if metadata.get("Size") != "scalable":
            errors.append(f"[{directory}] must declare Size=scalable")
        folder = ROOT / directory
        if not folder.is_dir():
            errors.append(f"declared directory does not exist: {directory}")
            continue
        for svg in sorted(folder.glob("*.svg")):
            try:
                root = ET.parse(svg).getroot()
            except ET.ParseError as error:
                errors.append(f"{svg.relative_to(ROOT)}: invalid SVG XML ({error})")
                continue
            if not root.tag.endswith("svg"):
                errors.append(f"{svg.relative_to(ROOT)}: root element is not svg")
            if not root.get("viewBox"):
                errors.append(f"{svg.relative_to(ROOT)}: missing viewBox")

    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1

    icons = sum(len(list((ROOT / directory).glob("*.svg"))) for directory in directories)
    print(f"OK: {theme['Name']} theme; {len(directories)} directories; {icons} SVG icons validated")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
