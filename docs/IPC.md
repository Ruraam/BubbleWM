# BubbleWM IPC lifecycle

The Linux socket transport and reusable request/response client are implemented by bubble-ipc::unix. BubbleWM's application-side bridge is exposed by bubblewm::ipc and is owned by the Winit compositor lifecycle. BubblePanel and the `bubblectl` terminal utility both use the shared client, which validates protocol versions and request IDs and opens one connection per request.

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

- GetCapabilities reports workspace_control = true.
- GetWorkspaceState returns the active zero-based workspace index and the current workspace count.
- WorkspacePrevious and WorkspaceNext switch to the adjacent workspace and wrap at either end.
- ActivateWorkspace validates the requested zero-based index and switches to that workspace.
- Successful state-changing requests return Acknowledged; invalid indices return InvalidRequest.

The compositor currently initializes four workspaces. Each managed toplevel belongs to one workspace; transient descendants inherit their parent's workspace. Switching unmaps the previous workspace and maps the target workspace in its saved stacking order without destroying clients or losing floating geometry. Minimized windows remain minimized, and restoring a minimized family on another workspace first activates that workspace.

## Current implementation boundary

The compositor is still an early nested prototype. It advertises wl_compositor, wl_shm, wl_seat, xdg-shell, xdg-decoration, wlr-layer-shell and one wl_output, accepts clients and renders mapped xdg toplevels and layer surfaces through Smithay's desktop space. Its window manager tracks toplevel map/unmap/remap/destruction, supports XDG interactive move/resize using validated pointer grabs, preserves floating geometry across maximize/unmaximize, tracks transient families, supports minimizing/restoring whole families, manages XDG popup placement/repositioning/grabs, and implements four virtual workspaces. BubblePanel uses the same versioned IPC protocol for live workspace state and switching. A prototype server-side titlebar is available to clients negotiating XDG decorations.

Protocol v1 intentionally exposes only capability discovery and workspace control; window-management operations remain compositor-local. Output-management protocols, multiple outputs, pointer cursor rendering, touch, polished/title-bearing decorations and tiling are still future work.

See docs/ARCHITECTURE.md and docs/ROADMAP.md for the next integration milestones.

## Session Manager protocol foundation

Protocol v1 also defines typed session requests:

- GetSessionState reports whether the compositor is running as a nested development session or a native session, plus whether an action is in progress.
- GetSessionCapabilities returns explicit flags for lock, logout, suspend, reboot and power-off.
- RequestSessionAction carries one typed SessionAction value; arbitrary shell commands are never accepted.

The Winit backend identifies itself as NestedDevelopment, advertises every host-session action as unavailable and rejects every action request with a structured Unavailable response. It never invokes loginctl, systemctl, pkexec or any other host command.

The native session work now includes a read-only systemd-logind probe implemented with zbus over the system D-Bus. It queries CanSuspend, CanReboot and CanPowerOff. The probe is not wired into the compositor, does not execute actions, and does not change the public capability flags. Its blocking D-Bus calls must run outside the compositor event loop. A separate authorization, confirmation and execution flow is required before any action can be advertised.
