# BubblePanel implementation notes

## First runnable client milestone

The first implementation lives in the bubblepanel/ workspace member. It is intentionally separate from the bubblewm compositor package.

### Implemented in the initial prototype

- Wayland connection through wayland-client.
- Registry and compositor integration through Smithay Client Toolkit.
- wlr-layer-shell top-layer surface anchored to the bottom edge.
- A 40-pixel exclusive zone.
- Software drawing through wl_shm and an SCTK SlotPool.
- Three visual zones: BubbleOS mark and workspace indicators on the left, reserved center area, and Wi-Fi/audio indicators plus local-time digits on the right.
- Graceful error messages when Wayland, shared memory or layer-shell support is unavailable.
- Low-overhead clock refresh: the event loop polls Wayland with a one-second timeout and redraws only when the displayed minute changes.
- Pointer enter/motion/leave tracking and left-click press/release handling. A click is confirmed only when press and release target the same panel action.
- Pointer scroll detection for vertical, horizontal and diagonal movement. Direction is resolved from high-resolution, discrete or continuous axis data.
- Workspace state and capabilities over Bubble IPC. The panel displays the active workspace, renders the configured number of indicators and activates a workspace when its indicator is clicked.
- Workspace navigation over Bubble IPC. Vertical scrolling over the workspace region selects the previous or next workspace; horizontal scrolling and scrolling over other modules are ignored.
- Bubble IPC client support for fetching the current BubbleThemeEngine palette. The panel uses the engine's glass tint, opacity, primary, surface, text and outline roles and polls every five seconds so wallpaper changes propagate without decoding images in the client.
- Compact Wi-Fi signal and audio volume/mute indicators from BubbleWM's status snapshot. These indicators update with the existing five-second IPC poll and show a muted style when the data is unavailable or disabled.
- A live task list for up to six windows, showing titles and highlighting the active window. Clicking a task asks BubbleWM to restore/focus it and switch workspaces when needed.

### Explicitly not implemented yet

- Richer task-list presentation and interactive status modules.
- Bluetooth, battery, notification and media modules.
- Multi-monitor surface management.
- User-facing panel configuration.
- Blur and GPU rendering. The panel's themed background can be translucent when its compositor supports alpha blending.

### Development order after the prototype

1. Add richer task-list presentation and interactive system-status modules.
2. Introduce typed panel configuration, configurable placement and per-output management.
3. Add keyboard/popup interactions, module contracts and BubbleSettings integration.

The panel must remain an optional Wayland client. A missing panel or panel crash must never terminate BubbleWM or its clients.
