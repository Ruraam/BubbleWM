#!/usr/bin/env python3
"""Unit tests for Bubble's desktop-entry icon audit."""
from __future__ import annotations
import sys, tempfile, unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from audit_icons import audit, icon_is_available, theme_icon_names

class IconAuditTests(unittest.TestCase):
    def test_alias_is_found_and_missing_icon_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); theme=root/"Bubble"; apps=root/"applications"
            (theme/"scalable/apps").mkdir(parents=True); apps.mkdir()
            (theme/"scalable/apps/browser.svg").write_text('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>', encoding="utf-8")
            (theme/"scalable/apps/firefox.svg").symlink_to("browser.svg")
            (apps/"browser.desktop").write_text("[Desktop Entry]\nType=Application\nName=Browser\nIcon=firefox\n", encoding="utf-8")
            (apps/"missing.desktop").write_text("[Desktop Entry]\nType=Application\nName=Missing\nIcon=unknown-app\n", encoding="utf-8")
            missing, checked=audit(theme,[apps])
            self.assertEqual(checked,2); self.assertEqual([item["name"] for item in missing],["Missing"])
    def test_hidden_entries_are_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); theme=root/"Bubble"; apps=root/"applications"; theme.mkdir(); apps.mkdir()
            (apps/"hidden.desktop").write_text("[Desktop Entry]\nType=Application\nName=Hidden\nIcon=none\nHidden=true\n", encoding="utf-8")
            missing,checked=audit(theme,[apps]); self.assertEqual(checked,0); self.assertEqual(missing,[])
    def test_symlink_alias_is_in_theme_inventory(self):
        with tempfile.TemporaryDirectory() as tmp:
            theme=Path(tmp)/"Bubble"; (theme/"scalable/apps").mkdir(parents=True)
            (theme/"scalable/apps/target.svg").write_text("<svg/>",encoding="utf-8")
            (theme/"scalable/apps/alias.svg").symlink_to("target.svg")
            self.assertIn("alias",theme_icon_names(theme))
    def test_absolute_icon_path_is_supported(self):
        with tempfile.TemporaryDirectory() as tmp:
            icon=Path(tmp)/"custom.svg"; icon.write_text("<svg/>",encoding="utf-8")
            self.assertTrue(icon_is_available(str(icon),Path(tmp),set()))

if __name__ == "__main__": unittest.main()
