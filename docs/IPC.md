# BubbleWM IPC lifecycle

The Linux socket transport is implemented by bubble-ipc::unix. BubbleWM's application-side bridge is exposed by bubblewm::ipc and is now owned by the Winit compositor lifecycle.

## Thread ownership

The IPC runtime owns one named worker thread. It accepts local connections, authenticates peer credentials, validates the protocol envelope and sends commands through a bounded channel with a capacity of 64 requests.

The compositor owns the receiving half. It calls CoreCommandReceiver::dispatch_pending from its event loop; the supplied handler therefore runs on the compositor thread and is the only place where requests may inspect or mutate compositor state.

The IPC worker waits up to two seconds for a response. Queue saturation, a stopped receiver, or a compositor that does not dispatch requests in time returns a structured unavailable response rather than blocking indefinitely.

## Current lifecycle

1. BubbleWM loads and validates its configuration.
2. It creates the Wayland display, core compositor, shm, seat and xdg-shell state, then advertises the single Winit-backed wl_output.
3. It binds a Wayland listening socket and initializes the Winit/EGL backend.
4. It starts IpcRuntime::start_default() and retains the runtime owner for the compositor lifetime.
5. Each Winit event-loop iteration calls CoreCommandReceiver::dispatch_pending on the compositor thread.
6. Closing the Winit window ends the loop; BubbleWM calls IpcRuntime::shutdown() before returning. Dropping the runtime also requests shutdown and joins its worker as a fallback.

The event-loop handler must remain quick and must not perform blocking I/O. It must validate workspace indices and feature availability against authoritative compositor state before returning success.

## Current command capabilities

- GetCapabilities succeeds and currently reports workspace_control = false.
- GetWorkspaceState returns unavailable because workspace state does not exist yet.
- WorkspacePrevious, WorkspaceNext and ActivateWorkspace return unavailable for the same reason.

This is intentional. The endpoint is live and correctly reports the current feature boundary rather than pretending that workspace switching works.

## Current implementation boundary

The compositor is still an early nested prototype. It advertises wl_compositor, wl_shm, wl_seat, xdg-shell, xdg-decoration and one wl_output, accepts clients and renders mapped xdg toplevel windows through a Smithay Space. Its current window manager tracks toplevel map/unmap/remap/destruction, supports XDG interactive move/resize using validated pointer grabs, preserves floating geometry across maximize/unmaximize, tracks transient toplevel relationships, supports minimizing/restoring whole transient families and manages XDG popup placement, repositioning and grabs. A prototype server-side titlebar is available to clients negotiating XDG decorations.

These desktop operations are not exposed over IPC yet: protocol v1 still provides capability discovery and workspace requests only, and workspace requests correctly return unavailable. The compositor also does not yet implement workspace management, output-management protocols or multiple outputs, pointer cursor rendering, touch, polished/title-bearing decorations, tiling or the layer-shell server protocol.

See docs/ARCHITECTURE.md and docs/ROADMAP.md for the next integration milestones.