# Bubble icon theme

Bubble is the original freedesktop-compatible icon theme for BubbleOS. Artwork is designed for this project; Papirus and other themes may be used as visual research references, but their artwork is not copied.

## Install for a user

Copy the `Bubble` directory to `~/.local/share/icons/Bubble`, then select **Bubble** in the desktop environment's icon-theme setting. The theme inherits from `hicolor` for icons not yet provided.

## Coverage and aliases

The scalable SVG set includes first-party BubbleOS artwork and generic names for common application and system roles. `aliases.json` documents common application icon names; matching relative SVG symlinks make those names resolvable by standard icon-theme lookup. Coverage remains incremental.

## Validate

From the repository root run `python3 tools/check-icons.py`. This validates theme metadata, SVGs and every alias target/link.

## Audit installed applications

Run `python3 tools/audit_icons.py` to inspect XDG application desktop entries against the repository theme. For an installed copy, pass `--theme-dir ~/.local/share/icons/Bubble`. Use `--applications-dir PATH` repeatedly for explicit application directories, `--json` for machine-readable output, or `--strict` to return status 1 when icons are missing.

The auditor is read-only: it does not edit desktop entries, install icons or change system settings.
