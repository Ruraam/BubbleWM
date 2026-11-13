# BubblePanel implementation notes

## First runnable client milestone

The first implementation lives in the bubblepanel/ workspace member. It is intentionally separate from the bubblewm compositor package.

### Implemented in the initial prototype

- Wayland connection through wayland-client.
- Registry and compositor integration through Smithay Client Toolkit.
- wlr-layer-shell Top-layer surface anchored to the bottom edge.
- A 40-pixel exclusive zone.
- Direct compatibility with BubbleWM's nested compositor, which now implements the wlr-layer-shell server.
- Software drawing through wl_shm and an SCTK SlotPool.
- Three visual zones: BubbleOS mark and live workspace indicators on the left, reserved center area, and status placeholders plus local-time digits on the right.
- Graceful error messages when Wayland, shared memory or layer-shell support is unavailable.
- Low-overhead clock refresh: the event loop polls Wayland with a one-second timeout and redraws only when the displayed minute changes.
- Pointer enter/motion/leave tracking and left-click press/release handling. A click is confirmed only when press and release target the same panel action.
- Pointer scroll detection for vertical, horizontal and diagonal movement. Direction is resolved from high-resolution, discrete or continuous axis data.
- BubbleWM IPC capability discovery and workspace-state queries over the authenticated per-user Unix socket.
- Read-only polling of the compositor's versioned BubbleColor semantic palette; the panel consumes RGB tokens and never analyzes wallpaper images.
- Workspace clicks send indexed activation requests; vertical scrolling over a workspace hit region sends previous/next requests. The panel refreshes the active indicator after successful commands.
- IPC is optional: on other Wayland compositors, or when BubbleWM's socket is unavailable, the panel remains a visual-only client and reports that workspace control is unavailable.

### Explicitly not implemented yet

- Task-list integration and workspace names/configuration.
- Dynamic workspace counts and workspace state from compositors other than BubbleWM.
- Real network, audio, Bluetooth, battery or notification modules.
- Multi-monitor surface management.
- User-facing panel configuration.
- Live palette editing or wallpaper analysis (the panel only consumes the compositor snapshot).
- Blur, transparency and GPU rendering.

### Development order after the prototype

1. Introduce typed panel configuration, configurable placement and per-output management.
2. Add task-list integration and real system status modules.
3. Add keyboard/popup interactions, module contracts and BubbleSettings integration.

The panel must remain an optional Wayland client. A missing panel or panel crash must never terminate BubbleWM or its clients.