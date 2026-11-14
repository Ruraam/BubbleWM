# BubblePanel

BubblePanel is the independent panel process for BubbleWM and BubbleOS.

## First prototype

The initial executable:

- Connects to the active Wayland display.
- Requires wl_compositor, wl_shm and wlr-layer-shell.
- Creates a top-layer surface anchored to the bottom edge.
- Reserves a 40-pixel exclusive zone.
- Draws a compact three-zone prototype using shared-memory buffers.
- Shows live workspace indicators when BubbleWM IPC is available, plus status placeholders and a local-time seven-segment clock.
- Refreshes the clock when the displayed minute changes.
- Tracks pointer hover and left-click press/release targets.
- Queries BubbleWM's authenticated local IPC endpoint for workspace capabilities and active state.
- Sends workspace activation requests on click and previous/next requests when scrolling vertically over a workspace indicator.
- Remains a visual-only panel when the host compositor does not expose BubbleWM's IPC endpoint.

## Build and run

From the repository root:

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

Run it inside a Wayland session whose compositor supports wlr-layer-shell. BubbleWM implements the server side in its nested Winit backend, so it can be tested directly with BubblePanel.

## Current limitations

- Software rendering through wl_shm; no GPU renderer.
- Fixed bottom placement and 40-pixel height.
- Single surface; multi-monitor lifecycle is not implemented.
- Four fixed workspace indicators; workspace names, dynamic counts and task-list integration are not implemented yet.
- No real network, audio, Bluetooth, battery, notification or media modules.
- No user-facing configuration, blur or transparency.

These are intentional scope boundaries for the first milestone, not claims of completed functionality.