# BubbleSettings

BubbleSettings is the independent GTK4 graphical settings application for the
BubbleOS desktop. It is a client of BubbleWM, not part of the compositor process.

## Current foundation

- GTK4 application with independent process lifetime.
- Appearance and About navigation pages.
- Reads the versioned `GetThemePalette` IPC request.
- Displays the semantic BubbleColor roles published by BubbleWM.
- Performs IPC on a dedicated worker thread; GTK updates stay on the main thread.
- Shows explicit unavailable, empty-palette and protocol-error states.
- Does not read, decode or watch wallpaper files and does not run the BubbleColor engine.

The first version is intentionally read-only. It does not write compositor
configuration or claim that settings are applied. Configuration editing,
validated persistence, live apply/restart feedback and previews will be added
only after the configuration IPC contract is designed.

## Build and run

On Debian 13:

```sh
sudo apt install libgtk-4-dev pkg-config
cargo build -p bubblesettings
cargo run -p bubblesettings
```

BubbleWM must be running in the same user session for live palette data. If it
is unavailable, the settings window remains usable and reports the connection
state.

## Architecture boundary

- BubbleColor owns wallpaper analysis and semantic token generation.
- BubbleWM owns the accepted theme snapshot and publishes it over authenticated IPC.
- BubblePanel and BubbleSettings consume the same read-only semantic snapshot.
- BubbleSettings will own user interaction and validated settings requests, not palette generation.
