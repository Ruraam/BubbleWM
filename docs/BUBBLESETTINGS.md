# BubbleSettings

BubbleSettings is the independent GTK4 graphical settings application for the
BubbleOS desktop. It is a separate process and does not run inside the compositor.

## Current features

- GTK4 application with Appearance and About pages.
- Validated Appearance controls for the color scheme, BubbleColor palette
  variant, contrast enforcement, BubbleColor enablement and wallpaper path.
- Loads and saves the shared BubbleWM TOML configuration through the same
  schema and validation code as the compositor.
- Saves through the existing atomic, private-file persistence routine.
- Clearly reports that these settings require a BubbleWM restart; it does not
  claim to apply them live.
- Reads the versioned `GetThemePalette` IPC request and displays the semantic
  BubbleColor roles published by BubbleWM.
- Performs palette IPC on a dedicated worker thread; GTK updates stay on the
  main thread.
- Shows explicit unavailable, empty-palette and protocol-error states.
- Does not read, decode or watch wallpaper files and does not run BubbleColor.

If the existing configuration cannot be loaded or its path cannot be resolved,
the save control is disabled rather than risking overwriting an unreadable file.

## Build and run

On Debian 13:

```sh
sudo apt install libgtk-4-dev pkg-config
cargo build -p bubblesettings --locked
cargo run -p bubblesettings --locked
```

BubbleWM must be running in the same user session for live palette data. If it
is unavailable, the settings window remains usable and reports the connection
state.

## Architecture boundary

- BubbleColor owns wallpaper analysis and semantic token generation.
- BubbleWM owns the configuration schema, validation, atomic persistence and
  accepted theme snapshot.
- BubbleSettings edits only supported configuration fields and reports the
  restart requirement; it does not mutate compositor state.
- BubblePanel and BubbleSettings consume the same read-only semantic palette.
- Live reload, per-setting restart metadata and additional settings categories
  remain future work.
