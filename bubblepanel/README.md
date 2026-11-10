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

The panel currently logs workspace navigation intent. It does not yet send commands to BubbleWM or switch workspaces.

## Build and run

From the repository root:

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

Run it inside a Wayland session whose compositor supports wlr-layer-shell. BubbleWM's initial Winit server does not yet implement the compositor side of layer-shell, so use a compatible existing Wayland compositor for development and visual testing.

## Current limitations

- Software rendering through wl_shm; no GPU renderer.
- Fixed bottom placement and 40-pixel height.
- Single surface; multi-monitor lifecycle is not implemented.
- No live workspace data, task list or IPC client yet. BubbleWM has a running authenticated IPC endpoint, but BubblePanel is not connected to it.
- No real network, audio, Bluetooth, battery, notification or media modules.
- No user-facing configuration, blur or transparency.

These are intentional scope boundaries for the first milestone, not claims of completed functionality.