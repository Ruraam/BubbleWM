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
- Pointer scroll detection for vertical, horizontal and diagonal movement. Direction is resolved from high-resolution, discrete or continuous axis data.
- Workspace scroll intent: vertical scrolling over a workspace hit region requests the previous/next workspace; horizontal scrolling and scrolling over other modules are ignored. It currently logs the request only; the shared IPC wire contract is now defined, but no socket transport/server is connected yet.

### Explicitly not implemented yet

- Actual workspace state or task-list integration.
- Actual workspace switching through BubbleWM integration/IPC, plus keyboard and popup interactions. The panel currently detects and logs a workspace navigation request but does not switch workspaces.
- Real network, audio, Bluetooth, battery or notification modules.
- Multi-monitor surface management.
- User-facing panel configuration.
- BubblePanel's IPC client and live workspace state integration. BubbleWM now has a tested worker-to-compositor bounded-channel bridge, but the bridge is not started by the executable until a real Smithay event loop exists.
- Blur, transparency and GPU rendering.

### Development order after the prototype

1. Implement the Smithay compositor lifecycle and event loop; start/stop IpcRuntime with that lifecycle and drain CoreCommandReceiver on the compositor thread.
2. Connect workspace commands to BubbleWM's authoritative workspace state and return capability/state responses.
3. Implement the BubblePanel IPC client, capability discovery and real workspace state.
4. Introduce typed panel configuration, configurable placement and per-output management.
5. Add keyboard/popup interactions, module contracts and BubbleSettings integration.

The panel must remain an optional Wayland client. A missing panel or panel crash must never terminate BubbleWM or its clients.
