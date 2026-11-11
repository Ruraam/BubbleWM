# Architecture

BubbleWM is a standalone Wayland compositor. BubbleOS integration must remain optional.

## Layers

- **Compositor core:** Smithay state, event loop, protocol globals and lifecycle.
- **Backend:** Winit for nested development; DRM/KMS for native sessions.
- **Desktop:** surfaces, toplevel management, focus, placement and workspaces.
- **BubbleDecor:** server-side titlebars, borders and window controls.
- **BubbleFX:** effect scheduling, animation and GPU shader pipeline.
- **BubbleColor:** wallpaper analysis, palette generation and contrast validation.
- **BubblePanel:** independent, modular Wayland panel for workspaces, task lists, launchers and system indicators.
- **BubbleSettings:** dedicated graphical control center for discovering and editing supported user-facing options.
- **Configuration service:** typed configuration, validation, persistence, reload and migration.
- **IPC:** versioned local control interfaces for the panel, settings application, shell and external tools.

## Process boundaries

BubbleWM, BubblePanel and BubbleSettings are separate processes. A failure or restart in BubblePanel must not terminate the compositor or disrupt client windows. BubbleWM must remain usable without either BubblePanel or BubbleSettings.

BubblePanel is a Wayland client. It should use layer-shell when available for panel placement, anchors and exclusive zones, and detect optional protocols before enabling dependent features. It must not assume every Wayland compositor exposes the same protocol set.

The panel owns its module lifecycle and presentation. It obtains compositor information through supported Wayland protocols and communicates with BubbleWM through a versioned local IPC interface only for BubbleWM-specific controls that are not covered by standard protocols. The current panel uses capability discovery and workspace-state requests when the BubbleWM socket is available; it remains a visual-only panel on other compositors.

### Local IPC contract

The shared protocol lives in the `bubble-ipc` workspace crate and is consumed by BubbleWM and BubblePanel. Protocol version 1 uses typed JSON request/response envelopes with a monotonically assigned request ID, explicit success/failure responses and stable error categories.

- Commands are deliberately narrow: capability discovery, workspace-state queries, previous/next workspace requests and activation of a workspace by index.
- Frames are newline-delimited UTF-8 JSON, limited to 64 KiB, and must carry the supported protocol version.
- The Linux transport binds a Unix-domain socket at `$XDG_RUNTIME_DIR/bubblewm/control.sock`; no TCP listener or network-accessible endpoint is provided.
- The runtime directory and application subdirectory are checked without following symlinks. The application directory is restricted to mode `0700`, and the socket to `0600`.
- The server checks Linux `SO_PEERCRED` and accepts only clients with the same effective UID by default. Stale sockets may be removed; regular files and active sockets are never replaced.
- Frames are bounded, each connection is limited to one request/response exchange, and client reads/writes have timeouts. The accept loop must run on a dedicated worker thread, never on the compositor event loop.
- The server transport and tests are implemented in `bubble-ipc::unix`. BubbleWM exposes an application-side bridge in `bubblewm::ipc`: a dedicated worker forwards requests through a bounded channel (64 entries) and waits up to two seconds for the compositor thread to respond.
- The compositor thread calls `CoreCommandReceiver::dispatch_pending` from the Winit event loop. This keeps state access on the owner thread and prevents the IPC worker from touching Smithay state.
- `IpcRuntime` starts after Wayland/Winit initialization and is shut down when the nested compositor exits. It also joins the worker on drop. The live endpoint reports four workspaces, supports previous/next navigation with wraparound and validates indexed activation requests. BubblePanel queries the same endpoint for its active-workspace indicator and sends click/scroll navigation commands. See [IPC lifecycle](IPC.md).
- Unsupported protocol versions receive structured errors. The application-level handler remains responsible for capability checks, workspace bounds and command execution.


## Initial compositor milestone

The current executable starts a nested Winit/EGL backend and a Wayland server socket. It advertises the core compositor, shared-memory, seat, xdg-shell, xdg-activation, the standard data-device manager, primary-selection, `wlr-layer-shell` and one `wl_output` global. The virtual output follows Winit window resizes and currently advertises a 60 Hz development mode.

Each xdg toplevel has a stable managed-window entry, independent of whether its surface is currently mapped. The compositor preserves its last floating position across null-buffer unmaps and remaps, removes the entry on toplevel destruction, and restores keyboard focus and activation to the topmost remaining window. Mapped windows from the active virtual workspace are represented as Smithay `Window` elements in a `Space<Window>`; each managed toplevel retains its workspace assignment, stacking order and floating geometry while hidden. Four workspaces are initialized. Transient descendants inherit their parent's workspace, minimized families remain hidden across switches, and switching re-maps the target workspace in stacking order without destroying clients. The compositor refreshes output enter/leave state and renders the mapped scene.

Winit absolute pointer motion, mouse buttons and scroll-axis events are routed to the surface under the pointer. Clicking a window raises and activates it, and transfers keyboard focus to its toplevel and known transient descendants. Smithay's data-device and primary-selection states expose the standard Wayland clipboard and primary-selection protocols; both selection foci follow the client owning the keyboard-focused surface. Compositor-owned clipboard contents are not implemented yet. XDG interactive move and resize requests are accepted only when the request serial matches an active pointer grab whose focused surface belongs to the requesting client. The compositor owns the grab while moving/resizing; resize requests respect the client's min/max size hints, send the `Resizing` state and requested size through XDG configure events, and send a final configure when the grab is released.

XDG maximize requests configure the toplevel to the single output's non-exclusive work area and retain the previous floating location and acknowledged size. The work area is derived from Smithay's layer map, including exclusive zones and margins. Unmaximize restores that saved geometry. Minimize requests remove a toplevel and its transient descendants from the visible space without destroying their clients; the compositor can restore the most recently minimized family. The initial keyboard controls are `Super+H` to minimize, `Super+Tab` to restore the most recently minimized family, `Super+Q` to send an XDG close request, `Super+Left`/`Super+Right` to switch virtual workspaces with wraparound, and `Super+T` to toggle between floating and Master/Stack layouts. Floating is the default. Master/Stack tiles eligible top-level windows in the active workspace, respects layer-shell exclusive work areas, excludes transient and maximized windows, and restores each window's previous floating position and size when toggled off. The `core.layout` configuration accepts `floating` or `tiling` and selects the initial mode. In Master/Stack mode, `Super+F` toggles the topmost eligible top-level window between automatic tiling and a floating override. The window's geometry is saved on entry to tiling and restored when it is switched back to floating; the remaining tiled windows are then rearranged. Maximized and transient windows are not affected. A window explicitly marked floating remains excluded from subsequent Master/Stack arrangements. Intercepted key presses and releases are both consumed so clients do not retain a stuck modifier/key state.

Transient toplevels are positioned relative to their known parent and participate in family raising/minimization. XDG activation tokens are accepted only when they are short-lived, single-use and backed by a recent input serial from BubbleWM's seat and the requesting surface. Activation is restricted to mapped windows on the active workspace, so untrusted clients cannot use the protocol to steal focus or force workspace switches. XDG popups are managed by Smithay's `PopupManager`: they are tracked across commits, rendered with their toplevel, constrained to the output bounds and repositioned in response to the XDG reposition request. Popup grabs use Smithay's keyboard and pointer grab implementations, with serial validation before a grab is installed.

The compositor implements the `wlr-layer-shell` server and maps Background, Bottom, Top and Overlay surfaces onto the output's Smithay `LayerMap`. It handles initial and subsequent configure events, layer arrangement, anchors, margins, exclusive zones, output enter/leave, popup parenting and destruction. Layer surfaces participate in scene rendering and pointer hit-testing in the correct z-order. Exclusive keyboard-interactive surfaces on Top/Overlay receive keyboard focus; OnDemand surfaces can receive focus through pointer interaction. Layer popups use the same popup manager and serial-validated grabs as XDG toplevel popups. The exclusive work area is recalculated after layer commits and output resizes, and maximized windows are reconfigured to respect it.

The compositor advertises XDG minimize/maximize capabilities only. Clients that negotiate `xdg-decoration` default to server-side decoration; an explicit client-side request is honored. The current server-side decoration is intentionally minimal: a 32-pixel titlebar with close, maximize/restore and minimize hit targets, plus titlebar dragging. Close controls send the standard XDG close event and do not kill the client process. The titlebar is currently rendered as a simple bar with colored controls; text, icons, theme integration and refined accessibility remain future work. Window geometry is refreshed on root and subsurface commits so the Smithay space uses current surface bounds.

This remains a bring-up milestone, not a complete desktop. Pointer cursor rendering, touch routing, a fully polished focus/grab policy, tiling, workspaces, output-management protocols, multiple outputs and DRM/KMS are not implemented yet. BubblePanel can now be tested directly against BubbleWM's nested Winit compositor.

## Settings-first design

User-facing behavior must be configurable through a clear graphical Settings application. Editing TOML by hand remains an optional advanced path, never a requirement for ordinary users.

The settings application and compositor are separate processes. The UI edits a validated configuration model and requests a live apply through versioned local IPC. The compositor remains authoritative: it validates every request, reports unsupported capabilities and returns actionable errors. Settings that cannot safely be applied live are clearly marked as requiring a restart.

Settings are organized by task rather than by internal Rust module:

- Appearance: theme, wallpaper-derived colors, accent, contrast and transitions.
- Windows: floating/tiling behavior, focus, placement, snapping and per-app rules.
- Decorations: titlebar, buttons, borders, corner radius and client/server decoration preference.
- Effects: blur, transparency, shadows, animations, wobble and workspace transitions.
- Workspaces: count, names, navigation and layout behavior.
- Panel: enablement, position, size, monitors, profiles, modules, styling and interactions.
- Input: keyboard shortcuts, pointer behavior, gestures and repeat settings.
- Displays: scale, refresh, output arrangement and per-monitor preferences.
- Performance: effect quality, frame pacing, reduced motion and low-resource profile.
- Advanced: backend diagnostics, configuration paths, IPC status and logs.

The interface should provide search, sensible presets, contextual explanations, reset-to-default controls and immediate preview where safe. Every setting needs a stable key, a default, validation, a documented live/restart policy and a capability fallback.

## Configuration and reliability

- Store user configuration under `$XDG_CONFIG_HOME/bubblewm/` (default: `~/.config/bubblewm/`).
- Keep compositor and panel configuration logically separate, with shared theme tokens and versioning conventions.
- Use a versioned schema and migrate older configuration safely.
- Preserve comments and unknown extension keys when writing configuration where practical.
- Apply changes transactionally: validate the complete candidate before replacing the active configuration.
- Use atomic file replacement and retain a recoverable previous configuration.
- Never block the compositor event loop with disk I/O, image analysis or IPC work.
- Treat IPC clients, module output and configuration files as untrusted input.
- Bound custom module execution time, output size and environment.
- Make optional effects and panel modules fail safely when a protocol, renderer or hardware capability is unavailable.
- Track damage and avoid repainting unchanged regions.

## Decoration negotiation

BubbleWM will prefer server-side decorations through the xdg-decoration protocol where supported. Some clients draw their own client-side decorations; those clients must remain usable and must not be forcibly stripped.
