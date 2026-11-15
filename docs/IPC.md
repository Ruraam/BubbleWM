# BubbleWM IPC lifecycle

The Linux socket transport is implemented by bubble-ipc::unix. BubbleWM's application-side bridge is exposed by bubblewm::ipc and is owned by both the Winit and native DRM compositor lifecycles.

## Thread ownership

The IPC runtime owns one named worker thread. It accepts local connections, authenticates peer credentials, validates the protocol envelope and sends commands through a bounded channel with a capacity of 64 requests.

The compositor owns the receiving half. It calls CoreCommandReceiver::dispatch_pending from its event loop; the supplied handler therefore runs on the compositor thread and is the only place where requests may inspect or mutate compositor state.

The IPC worker waits up to two seconds for a response. Queue saturation, a stopped receiver, or a compositor that does not dispatch requests in time returns a structured unavailable response rather than blocking indefinitely.

## Current lifecycle

1. BubbleWM loads and validates its configuration.
2. It creates the Wayland display and backend-independent compositor state.
3. It selects either the nested Winit/EGL backend or the native DRM/KMS backend. Both publish a Wayland output; the current DRM path supports one connected output and handles libseat pause/resume.
4. It starts IpcRuntime::start_default() and retains the runtime owner for the compositor lifetime.
5. The selected backend's event loop calls CoreCommandReceiver::dispatch_pending on the compositor thread.
6. Closing the Winit window, requesting shutdown, or leaving the DRM loop ends the compositor; BubbleWM shuts down IpcRuntime before returning. Dropping the runtime also requests shutdown and joins its worker as a fallback.

The event-loop handler must remain quick and must not perform blocking I/O. It must validate workspace indices and feature availability against authoritative compositor state before returning success.

## Current command capabilities

- GetCapabilities succeeds and reports workspace_control = true while the compositor is live.
- GetThemePalette returns the active BubbleThemeEngine snapshot, including container, elevated-surface, glass, status and neon roles. Older protocol-v1 clients ignore the additive fields; new clients default fields omitted by an older server.
- GetWorkspaceState returns the current zero-based active index and configured workspace count.
- GetSystemStatus returns a compact snapshot of NetworkManager/Wi-Fi availability, the active Wi-Fi signal, and current audio volume/mute state. Status probing runs independently of BubbleWM's built-in bar so BubblePanel can use it on its own.
- GetWindowList returns stable window IDs, titles, workspace indices, minimized state and active state; FocusWindow restores and focuses a listed window on the compositor thread.
- GetNetworkList returns nearby Wi-Fi access points and saved wireless profiles. ConnectWifi, ConnectSavedWifi and DisconnectWifi are validated against the compositor's latest status snapshot before being queued on the background status worker; RefreshNetworkStatus requests a new probe.
- ToggleWifi and ToggleMute queue radio and audio commands on the status worker, then refresh its snapshot. AdjustVolume accepts bounded changes from -20 to +20 percent. The compositor IPC handler only queues these actions and stays non-blocking.
- WorkspacePrevious, WorkspaceNext and ActivateWorkspace update compositor-owned state and return the resulting active index and count.

Workspace commands validate target indices and remap visible windows on the compositor thread. BubblePanel sends these requests from its own IPC worker, so its Wayland event loop remains responsive.

## Current implementation boundary

The compositor is still an early prototype: the nested Winit path is for development, while the native DRM path currently targets a single connected output. Both advertise wl_compositor, wl_shm, wl_seat, xdg-shell, xdg-decoration and wl_output, accept clients and render mapped xdg toplevel windows through a Smithay Space. The window manager tracks toplevel map/unmap/remap/destruction, supports XDG interactive move/resize using validated pointer grabs, preserves floating geometry across maximize/unmaximize, tracks transient toplevel relationships, supports minimizing/restoring whole transient families and manages XDG popup placement, repositioning and grabs. A prototype server-side titlebar is available to clients negotiating XDG decorations.

Protocol v1 provides capability discovery, theme-palette, system-status, window-list and Wi-Fi network-list reads, plus workspace, window focus, network connection and audio controls. The compositor does not yet implement output-management protocols or multiple outputs, pointer cursor rendering, touch, polished/title-bearing decorations or tiling.

See docs/ARCHITECTURE.md and docs/ROADMAP.md for the next integration milestones.
