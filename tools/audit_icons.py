#!/usr/bin/env python3
"""Report installed desktop applications whose icon is absent from Bubble."""
from __future__ import annotations
import argparse, configparser, json, os
from pathlib import Path

def default_application_dirs() -> list[Path]:
    home = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
    roots = [home / "applications", *(Path(p) / "applications" for p in os.environ.get("XDG_DATA_DIRS", "/usr/local/share:/usr/share").split(":") if p)]
    result, seen = [], set()
    for path in roots:
        path = path.expanduser()
        if path.is_dir() and path not in seen: result.append(path); seen.add(path)
    return result

def desktop_entries(directories: list[Path]) -> list[Path]:
    found: dict[str, Path] = {}
    for directory in directories:
        if directory.is_dir():
            for path in sorted(directory.rglob("*.desktop")):
                found.setdefault(path.relative_to(directory).as_posix(), path)
    return list(found.values())

def load_desktop_entry(path: Path):
    parser = configparser.ConfigParser(interpolation=None, strict=False, delimiters=("=",), comment_prefixes=("#",))
    parser.optionxform = str
    try:
        with path.open(encoding="utf-8", errors="replace") as stream: parser.read_file(stream)
    except (OSError, configparser.Error): return None
    if not parser.has_section("Desktop Entry"): return None
    entry = parser["Desktop Entry"]
    if entry.get("Type", "Application") != "Application" or entry.get("Hidden", "false").strip().lower() == "true": return None
    return entry

def theme_icon_names(theme_dir: Path) -> set[str]:
    return {p.stem for p in theme_dir.rglob("*") if p.is_file() and p.suffix.lower() in {".svg", ".png", ".xpm"}} if theme_dir.is_dir() else set()

def icon_is_available(icon: str, theme_dir: Path, names: set[str]) -> bool:
    value = icon.strip()
    if not value: return False
    candidate = Path(value).expanduser()
    if candidate.is_absolute() or "/" in value: return candidate.is_file()
    if value in names: return True
    return any((theme_dir / f"{value}{ext}").is_file() for ext in (".svg", ".png", ".xpm"))

def audit(theme_dir: Path, directories: list[Path]):
    names, missing, checked = theme_icon_names(theme_dir), [], 0
    for desktop_file in desktop_entries(directories):
        entry = load_desktop_entry(desktop_file)
        if entry is None: continue
        name = entry.get("Name", desktop_file.stem).strip() or desktop_file.stem
        icon = entry.get("Icon", "").strip()
        checked += 1
        if not icon_is_available(icon, theme_dir, names):
            missing.append({"name": name, "desktop_file": str(desktop_file), "icon": icon})
    return missing, checked

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--theme-dir", type=Path, default=Path(__file__).resolve().parents[1] / "data/icons/Bubble", help="Bubble theme directory")
    parser.add_argument("--applications-dir", type=Path, action="append", help="applications directory to scan; may be repeated")
    parser.add_argument("--json", action="store_true", help="print machine-readable JSON")
    parser.add_argument("--strict", action="store_true", help="exit 1 when missing icons are found")
    args = parser.parse_args()
    dirs = args.applications_dir if args.applications_dir is not None else default_application_dirs()
    missing, checked = audit(args.theme_dir.expanduser(), dirs)
    if args.json: print(json.dumps({"checked": checked, "missing": missing}, ensure_ascii=False, indent=2))
    else:
        print(f"Bubble icon audit: {checked} desktop applications checked.")
        if not missing: print("No missing icons found.")
        else:
            print(f"{len(missing)} application(s) need an icon or alias:")
            for item in missing: print(f"- {item['name']}: {item['icon'] or '(no Icon key)'} [{item['desktop_file']}]")
    return 1 if args.strict and missing else 0

if __name__ == "__main__": raise SystemExit(main())
