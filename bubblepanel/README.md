# BubblePanel

BubblePanel is the independent panel process for BubbleWM and BubbleOS.

## First prototype

The initial executable:

- Connects to the active Wayland display.
- Requires wl_compositor, wl_shm and wlr-layer-shell.
- Creates a top-layer surface anchored to the bottom edge.
- Reserves a 40-pixel exclusive zone.
- Draws a compact three-zone prototype using shared-memory buffers.
- Shows workspace placeholders, status placeholders and a local-time seven-segment clock.
- Refreshes the clock when the displayed minute changes.
- Tracks pointer hover and left-click press/release targets.
- Detects vertical, horizontal and diagonal scroll gestures, including workspace navigation intent.
- Reads BubbleWM's semantic theme palette over the authenticated local IPC socket and refreshes it every five seconds.
- Uses BubbleThemeEngine glass tint and opacity, primary, surface, text and outline roles; falls back to its built-in palette on other compositors.

The panel currently logs workspace navigation intent. It does not yet send commands to BubbleWM or switch workspaces.

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
- No live workspace data or task list yet. The IPC client currently reads the theme palette; workspace commands remain unavailable until BubbleWM implements workspaces.
- No real network, audio, Bluetooth, battery, notification or media modules.
- No user-facing configuration or blur. The themed panel background can use alpha transparency.

These are intentional scope boundaries for the first milestone, not claims of completed functionality.
