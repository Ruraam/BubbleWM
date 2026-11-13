# Bubble icon theme

Bubble is the original freedesktop-compatible icon theme for BubbleOS. Artwork is designed for this project; Papirus and other themes may be used as visual research references, but their artwork is not copied.

## Install for a user

Copy the `Bubble` directory to:

```text
~/.local/share/icons/Bubble
```

Then select **Bubble** in the desktop environment's icon-theme setting. The theme inherits from `hicolor` for icons not yet provided.

## Current coverage

The first seed contains original scalable SVG icons for BubbleWM, BubblePanel, BubbleSettings, Terminal, system preferences, folders and information status. This is a deliberately small starting set, not a claim of complete application coverage.

## Validation

From the repository root:

```sh
python3 tools/check-icons.py
```

The validator checks the theme metadata, declared directories and SVG XML/viewBox. Desktop-entry name coverage and visual variant previews will be added as the icon inventory grows.
