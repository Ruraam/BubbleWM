# BubbleWM

A native, lightweight and customizable Wayland compositor for BubbleOS, written in Rust and built on Smithay.

> **Status:** early development. BubbleWM now starts a nested Winit compositor with a single `wl_output`, core Wayland globals, tracked xdg-toplevel lifecycle, pointer routing, interactive move/resize, maximize/restore, minimize/restore, four virtual workspaces, transient stacking, XDG popup management, server-side decorations and a working `wlr-layer-shell` server. BubblePanel can query and control workspaces through the authenticated local IPC endpoint. It remains a development milestone, not yet a complete or daily-usable desktop environment.

## Project principles

- Develop and test independently from BubbleOS.
- Use Smithay as the Wayland compositor foundation.
- Start with a nested Winit backend before native DRM/KMS support.
- Keep the compositor core independent from the BubbleOS shell.
- Make visual effects optional and degrade gracefully when unsupported.
- Prefer explicit, documented behavior over placeholder feature claims.

## Workspace

- **bubblewm:** Smithay compositor, configuration and application lifecycle.
- **bubblepanel:** independent Wayland panel client using Smithay Client Toolkit.
- **bubble-ipc:** shared, versioned local-control protocol, Linux Unix-socket transport and reusable client.
- **bubblectl:** terminal utility for workspace status and navigation through the local IPC endpoint.

## Run the nested compositor

The current Winit backend opens a development window, creates a Wayland socket in `XDG_RUNTIME_DIR`, accepts Wayland clients, advertises the core compositor, shm, seat, xdg-shell, the standard data-device manager, `wlr-layer-shell` and one `wl_output` global, then renders mapped windows and layer surfaces through Smithay's desktop space.

```sh
cargo run
```

The log prints the selected `WAYLAND_DISPLAY` name. In another terminal, point a Wayland client at that socket:

```sh
WAYLAND_DISPLAY=wayland-1 <your-wayland-client>
```

Replace `wayland-1` with the exact display name printed by BubbleWM. Close the Winit window to stop the nested compositor.

The Winit backend uses the host session's EGL/OpenGL environment. The native DRM/KMS renderer and output lifecycle are not implemented yet. The native backend has a hardware-independent, tested state machine for libseat pause/activation events and an explicit diagnostic monitor (`cargo run --features native-backend -- --native-session-monitor`) that registers the real libseat notifier in a separate Smithay/calloop event loop. It logs pause/activation transitions and keeps the Winit compositor loop untouched. This monitor does not initialize DRM devices or run a native compositor; live DRM descriptors are not yet paused, closed or reopened on VT/session switches. Selecting `core.backend = "drm"` with the optional native feature acquires a libseat session, enumerates DRM card nodes assigned to that seat, then opens at most one node through `Session::open`. It validates the opened descriptor against the udev device identity and initializes Smithay's `DrmDevice` using a reference-counted descriptor. The recovery handle ensures the descriptor is returned through `Session::close` after Smithay drops its device and notifier, including when initialization fails. Connector disabling is explicitly disabled during this probe. Smithay briefly attempts to acquire DRM master while initializing the device, but BubbleWM does not create a `DrmSurface`, select or change display modes, submit KMS commits, or start a native compositor. The diagnostic command exits afterwards.

### Current compositor capabilities

- One nested Winit window and one Wayland display socket.
- Core `wl_compositor`, `wl_shm`, `wl_seat`, `xdg-shell`, `xdg-activation`, `wl_data_device_manager`, primary-selection, `zwlr_layer_shell_v1` and `wl_output` globals.
- One virtual output whose advertised pixel mode follows Winit window resize events. The development mode currently advertises 60 Hz.
- A stable toplevel registry tracks creation, mapping, unmapping, remapping and destruction while preserving each window's last floating position.
- A Smithay `Space<Window>` handles the active workspace's mapped xdg toplevels, initial cascading placement, output enter/leave tracking and scene rendering. `Super+T` toggles the hybrid floating/Master-Stack layout; floating remains the default and previous floating geometry is restored when switching back. In Master/Stack mode, `Super+F` toggles the topmost eligible top-level window between automatic tiling and a floating override, restoring its pre-tiling geometry when it becomes floating.
- Four virtual workspaces keep their own window membership and stacking order. Switching workspaces preserves each window's floating geometry, minimized state and transient family; previous/next navigation wraps around.
- Absolute pointer motion, mouse buttons and scroll-axis events are routed to the surface under the pointer. Client-provided cursor surfaces are composited at the pointer hotspot; the nested Winit window's default cursor is hidden while a custom cursor or hidden-pointer state is active.
- Standard Wayland clipboard and primary-selection protocols are available between clients; both selection foci follow the keyboard-focused client.
- A click raises and activates the clicked window and moves keyboard focus to its toplevel. Focus and activation are restored to the topmost remaining window after an unmap or destruction.
- XDG interactive move and resize requests are accepted only when their serial matches an active pointer grab for the requesting client. Resize requests use the client's minimum/maximum size hints and complete with a final configure after release.
- XDG maximize and unmaximize requests are supported. BubbleWM saves the pre-maximized floating position and size, then restores them when the client requests unmaximize.
- XDG transient toplevels are positioned relative to their known parent and raised with their parent window family.
- XDG activation accepts only short-lived, single-use tokens backed by a recent input serial from the compositor seat and the requesting surface; activation is limited to already-mapped windows on the active workspace, preventing arbitrary clients from stealing focus or forcing workspace changes.
- XDG popups are tracked and rendered with their owning toplevel or layer surface. Popup placement is constrained to the appropriate output, reposition requests are handled, and protocol popup grabs route keyboard and pointer focus through Smithay's popup-grab helpers.
- The `wlr-layer-shell` server maps Background, Bottom, Top and Overlay surfaces, arranges anchors and margins, honors exclusive zones, renders layers in the desktop scene, routes pointer input and handles exclusive/on-demand keyboard focus. Layer-shell popups are tracked and constrained as well.
- Maximized windows use the remaining non-exclusive work area reserved by layer surfaces, and the work area is recalculated when layer surfaces change or the output is resized.
- XDG minimize requests unmap the window and its transient descendants. `Super+H` minimizes the focused window and `Super+Tab` restores the most recently minimized window family.
- `Super+Left` and `Super+Right` switch to the previous and next virtual workspace, wrapping at either end.
- Master/Stack arranges eligible top-level windows in the active workspace, respects layer-shell exclusive work areas, and leaves transient and maximized windows floating. Set `core.layout = "tiling"` in the TOML configuration to start in tiling mode.
- `Super+Q` and the server-side titlebar close control send the standard XDG close event to the client; BubbleWM does not forcibly terminate the application.
- BubbleWM advertises XDG minimize/maximize capabilities only. Fullscreen and compositor window-menu capabilities remain unadvertised.
- Clients negotiating XDG server-side decorations receive a prototype 32-pixel titlebar with close, maximize/restore and minimize controls. Client-side decoration requests are respected. The titlebar can also initiate an interactive move.
- Window geometry is refreshed after root-surface and subsurface commits.
- Frame pacing is throttled to approximately 60 updates per second in the nested development loop.
- The authenticated IPC server starts and stops with the compositor. It reports workspace capabilities/state and accepts previous, next and indexed workspace activation requests on the compositor thread.
- The `bubblectl` utility can query capabilities/workspace state and switch workspaces from a terminal without requiring BubblePanel.

### Not implemented yet

- Named cursor-theme rendering, touch input, and a complete focus/grab policy. Named cursor requests currently use the nested Winit window's default arrow as a fallback.
- Polished decorations: the current server-side titlebar is a basic prototype with colored controls, not yet a themed titlebar with title text, icons, borders or full accessibility behavior.
- Workspace names, per-workspace layout configuration and dynamic workspace counts.
- Output-management protocols, multiple outputs, native DRM/KMS surface creation, connector/mode management and hotplug handling.
- Real system status modules, task-list integration and user-facing panel configuration.

## Terminal control

Build and use the `bubblectl` client while BubbleWM is running:

```sh
cargo run --bin bubblectl -- capabilities
cargo run --bin bubblectl -- workspace
cargo run --bin bubblectl -- workspace next
cargo run --bin bubblectl -- workspace prev
cargo run --bin bubblectl -- workspace go 3
```

Workspace numbers are one-based in the CLI. The same shared IPC client implementation is used by BubblePanel and `bubblectl`.

## BubblePanel prototype

The independent panel connects to BubbleWM (or another compatible Wayland compositor), creates a bottom `wlr-layer-shell` surface, reserves a 40-pixel exclusive zone and renders a three-zone layout with live workspace indicators, status placeholders and a seven-segment local-time clock. When BubbleWM IPC is available, workspace clicks and vertical scrolling switch workspaces; the panel remains usable as a visual-only panel on other compositors.

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

BubbleWM implements the server side of `wlr-layer-shell` and exposes workspace state/control over its authenticated IPC socket, so BubblePanel can be tested directly in the nested session. Real system modules and task-list integration remain future work.

See [bubblepanel/README.md](bubblepanel/README.md) and [docs/PANEL.md](docs/PANEL.md).

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A Wayland or X11 host session with working EGL/OpenGL drivers for the Winit development backend.
- Native development libraries required by the selected Smithay backend.

### Optional native backend build dependencies

The `native-backend` Cargo feature enables Smithay's DRM, GBM, libinput, udev and libseat dependencies. It also enables a diagnostic libseat/udev probe that initializes and releases one Smithay `DrmDevice` for a seat-assigned DRM card node; it is not yet a runnable DRM/KMS compositor.

On Debian 13, install the development packages before checking this feature:

```sh
sudo apt install libwayland-dev libxkbcommon-dev libegl1-mesa-dev libgles2-mesa-dev \
  libinput-dev libudev-dev libgbm-dev libdrm-dev libseat-dev pkg-config
cargo check -p bubblewm --features native-backend
```

The default build remains Winit-only and does not enable native session/device access.

## Planned subsystems

- **Core:** compositor state, event loop and protocol lifecycle.
- **Backend:** Winit for nested development; DRM/KMS for native sessions.
- **Desktop:** surfaces, focus, placement, workspaces and optional tiling.
- **BubbleDecor:** server-side titlebars, borders and controls.
- **BubbleFX:** animation scheduler, effects and GPU shader pipeline.
- **BubbleColor:** wallpaper-derived palettes and contrast validation.
- **BubblePanel:** modular panel, task list, workspace indicators and system modules.
- **BubbleSettings:** graphical configuration for compositor and desktop components.
- **IPC:** versioned local control interface for shell/settings integration.

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/IPC.md](docs/IPC.md) and [docs/PANEL.md](docs/PANEL.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
