# BubblePanel implementation notes

## First runnable client milestone

The first implementation lives in the `bubblepanel/` workspace member. It is intentionally separate from the `bubblewm` compositor package.

### Implemented in the initial prototype

- Wayland connection through `wayland-client`.
- Registry and compositor integration through Smithay Client Toolkit.
- `wlr-layer-shell` top-layer surface anchored to the bottom edge.
- A 40-pixel exclusive zone.
- Software drawing through `wl_shm` and an SCTK `SlotPool`.
- Three visual zones: BubbleOS mark and workspace placeholders on the left, reserved center area, and status placeholders plus local-time digits on the right.
- Graceful error messages when Wayland, shared memory or layer-shell support is unavailable.
- Low-overhead clock refresh: the event loop polls Wayland with a one-second timeout and redraws only when the displayed minute changes.
- Pointer enter/motion/leave tracking and left-click press/release handling. A click is confirmed only when press and release target the same panel action.

### Explicitly not implemented yet

- Actual workspace state or task-list integration.
- Scroll, keyboard and popup interactions.
- Real network, audio, Bluetooth, battery or notification modules.
- Multi-monitor surface management.
- User-facing panel configuration and IPC.
- Blur, transparency and GPU rendering.

### Development order after the prototype

1. Introduce a typed panel configuration and configurable top/bottom placement, dimensions and colors.
2. Add protocol capability discovery and real workspace state.
3. Add scroll/keyboard/popup interactions, then define interactive module contracts and status providers.
4. Add per-output management and hotplug handling.
5. Connect the panel configuration to BubbleSettings.

The panel must remain an optional Wayland client. A missing panel or panel crash must never terminate BubbleWM or its clients.
