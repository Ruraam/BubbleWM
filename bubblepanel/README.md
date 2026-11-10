# BubblePanel

BubblePanel is the independent panel process for BubbleWM and BubbleOS.

## First prototype

The initial executable:

- Connects to the active Wayland display.
- Requires `wl_compositor`, `wl_shm` and `wlr-layer-shell`.
- Creates a top-layer surface anchored to the bottom edge.
- Reserves a 40-pixel exclusive zone.
- Draws a compact three-zone prototype using shared-memory buffers.
- Shows workspace placeholders, status placeholders and a local-time seven-segment clock.

The clock is sampled on surface configuration in this first rendering pass. Periodic refresh, interactive workspaces and live system modules are not implemented yet.

## Build and run

From the repository root:

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

Run it inside a Wayland session whose compositor supports `wlr-layer-shell`. BubbleWM does not yet implement the compositor side of that protocol, so this prototype currently targets a compatible existing Wayland compositor for development and visual testing.

## Current limitations

- Software rendering through `wl_shm`; no GPU renderer.
- Fixed bottom placement and 40-pixel height.
- Single surface; multi-monitor lifecycle is not implemented.
- No module discovery, user configuration, input handling or IPC yet.
- No periodic clock refresh yet.

These are intentional scope boundaries for the first milestone, not claims of completed functionality.
