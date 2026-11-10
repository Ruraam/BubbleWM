# BubblePanel implementation notes

## First runnable client milestone

The first implementation lives in the bubblepanel/ workspace member. It is intentionally separate from the bubblewm compositor package.

### Implemented in the initial prototype

- Wayland connection through wayland-client.
- Registry and compositor integration through Smithay Client Toolkit.
- wlr-layer-shell top-layer surface anchored to the bottom edge.
- A 40-pixel exclusive zone.
- Software drawing through wl_shm and an SCTK SlotPool.
- Three visual zones: BubbleOS mark and workspace placeholders on the left, reserved center area, and status placeholders plus local-time digits on the right.
- Graceful error messages when Wayland, shared memory or layer-shell support is unavailable.
- Low-overhead clock refresh: the event loop polls Wayland with a one-second timeout and redraws only when the displayed minute changes.
- Pointer enter/motion/leave tracking and left-click press/release handling. A click is confirmed only when press and release target the same panel action.
- Pointer scroll detection for vertical, horizontal and diagonal movement. Direction is resolved from high-resolution, discrete or continuous axis data.
- Workspace scroll intent: vertical scrolling over a workspace hit region identifies previous/next navigation; horizontal scrolling and scrolling over other modules are ignored. The panel still logs this intent and does not yet send it over IPC.

### Explicitly not implemented yet

- Actual workspace state or task-list integration.
- BubblePanel's IPC client, capability discovery and live workspace state. BubbleWM now starts its authenticated IPC server with the Winit compositor, but the panel is not connected to it yet.
- Actual workspace switching. BubbleWM currently returns unavailable for workspace commands because no workspace manager exists.
- Real network, audio, Bluetooth, battery or notification modules.
- Multi-monitor surface management.
- User-facing panel configuration.
- Blur, transparency and GPU rendering.

### Development order after the prototype

1. Add the missing compositor output globals and wlr-layer-shell server so BubblePanel can run directly on BubbleWM.
2. Implement BubbleWM workspace state and connect the existing IPC commands to that authoritative state.
3. Implement the BubblePanel IPC client and display real workspace/capability state.
4. Introduce typed panel configuration, configurable placement and per-output management.
5. Add keyboard/popup interactions, module contracts and BubbleSettings integration.

The panel must remain an optional Wayland client. A missing panel or panel crash must never terminate BubbleWM or its clients.