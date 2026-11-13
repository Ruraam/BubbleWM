# Bubble icon theme

Bubble is the original freedesktop-compatible icon theme for BubbleOS. Artwork is designed for this project; Papirus and other themes may be used as visual research references, but their artwork is not copied.

## Install for a user

Copy the `Bubble` directory to:

```text
~/.local/share/icons/Bubble
```

Then select **Bubble** in the desktop environment's icon-theme setting. The theme inherits from `hicolor` for icons not yet provided.

## Current coverage

The original scalable SVG set includes BubbleWM, BubblePanel and BubbleSettings, plus generic desktop icon names for a file manager, web browser, text editor, calculator, software installation, system monitoring, display settings, appearance settings, wireless networking and audio volume. Starter category, place and status icons are also included.

This is a growing foundation, not a claim of complete coverage for every installed application. Themes should provide generic names and common desktop-entry aliases before adding application-specific artwork.

## Validation

From the repository root:

```sh
python3 tools/check-icons.py
```

The validator checks theme metadata, declared directories and SVG XML/viewBox. Desktop-entry coverage auditing, missing-icon reporting and visual variant previews remain planned work.
