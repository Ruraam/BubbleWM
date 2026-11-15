# BubblePanel

BubblePanel is the independent panel process for BubbleWM and BubbleOS.

## First prototype

The initial executable:

- Connects to the active Wayland display.
- Requires wl_compositor, wl_shm and wlr-layer-shell.
- Creates a top-layer surface anchored to the bottom edge.
- Reserves a 40-pixel exclusive zone.
- Draws a compact three-zone prototype using shared-memory buffers.
- Shows live workspace indicators, status placeholders and a local-time seven-segment clock when BubbleWM IPC is available.
- Refreshes the clock when the displayed minute changes.
- Tracks pointer hover and left-click press/release targets.
- Activates a workspace when its indicator is clicked and sends vertical workspace navigation requests over IPC.
- Detects vertical, horizontal and diagonal scroll gestures; horizontal movement and scrolling over other regions do not switch workspaces.
- Reads BubbleWM's semantic theme palette over the authenticated local IPC socket and refreshes it every five seconds.
- Uses BubbleThemeEngine glass tint and opacity, primary, surface, text and outline roles; falls back to its built-in palette on other compositors.
- Shows live Wi-Fi signal and audio volume/mute indicators from BubbleWM's compact status snapshot.

Workspace state and commands are processed by BubbleWM on its compositor thread. BubblePanel sends requests from a separate worker so local socket delays do not block Wayland rendering.

## Build and run

From the repository root:

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

Run it inside a Wayland session whose compositor supports wlr-layer-shell. BubbleWM implements layer-shell for the panel. The BubbleThemeEngine palette is available when BubblePanel shares the same user session and runtime directory as BubbleWM.

## Current limitations

- Software rendering through wl_shm; no GPU renderer.
- Fixed bottom placement and 40-pixel height.
- Single surface; multi-monitor lifecycle is not implemented.
- No task list or interactive system modules yet; the current Wi-Fi and audio indicators are read-only.
- No Bluetooth, battery, notification or media indicators.
- No user-facing configuration or blur. The themed panel background can use alpha transparency.

These are intentional scope boundaries for the first milestone, not claims of completed functionality.
