# BubbleWM

A native, lightweight and customizable Wayland desktop for BubbleOS, written in Rust and built on Smithay. Its target role includes the Wayland login and session lifecycle currently handled by display managers such as greetd or LightDM.

> **Status:** early development. BubbleWM has a nested Winit compositor and an initial native DRM/KMS backend for one primary GPU and one connected output, using libseat, GBM/EGL and libinput. It also has core Wayland globals, tracked xdg-toplevel lifecycle, pointer routing, interactive move/resize, maximize/restore, minimize/restore, transient stacking, XDG popup management and prototype server-side decorations. The built-in PAM lock screen locks an already-running user session; a graphical login greeter and system user-session lifecycle are not implemented. The native backend still needs broader hardware validation, multi-output and hotplug support. BubbleWM is not yet a complete or daily-usable desktop environment.

## Project principles

- Develop and test independently from BubbleOS while keeping BubbleOS integration a first-class target.
- Use Smithay as the Wayland compositor foundation.
- Keep Winit as the nested development backend and DRM/KMS as the native session backend.
- Keep the compositor core independent from BubbleOS internals; integrate through documented packaging, configuration and session interfaces.
- Manage graphical login and user-session start/stop for Wayland sessions only. Do not offer Xorg desktop sessions.
- Make visual effects optional and degrade gracefully when unsupported.
- Prefer explicit, documented behavior over placeholder feature claims.

## Workspace

- **bubblewm:** Smithay compositor, configuration, login/session lifecycle and application lifecycle (login/session management is planned, not implemented).
- **bubblepanel:** independent Wayland panel client using Smithay Client Toolkit.
- **bubble-ipc:** shared, versioned local-control protocol and Linux Unix-socket transport.
- **bubble-theme-engine:** vendored BubbleOS wallpaper palette generator used by BubbleWM and companion clients.

## Run the nested compositor

The current Winit backend opens a development window, creates a Wayland socket in `XDG_RUNTIME_DIR`, accepts Wayland clients, advertises the core compositor, shm, seat, xdg-shell, the standard data-device manager and one `wl_output` global, then renders mapped xdg toplevel windows through a Smithay `Space<Window>`.

```sh
cargo run
```

The log prints the selected `WAYLAND_DISPLAY` name. In another terminal, point a Wayland client at that socket:

```sh
WAYLAND_DISPLAY=wayland-1 <your-wayland-client>
```

Replace `wayland-1` with the exact display name printed by BubbleWM. Close the Winit window to stop the nested compositor.

The Winit backend uses the host session's EGL/OpenGL environment. The native DRM/KMS backend uses libseat to open the primary GPU, GBM/EGL to render and libinput for physical input. It currently selects one connected output at startup; multi-output and connector hotplug are not implemented. Select `core.backend = "drm"` in the BubbleWM config and launch it from a VT where a libseat provider can grant seat access. This starts the compositor only; it does not show a login greeter or create a user session.

### Current compositor capabilities

- One nested Winit window and one Wayland display socket.
- Initial DRM/KMS backend for one primary GPU and one connected output, with libseat session pause/resume and libinput input.
- Core `wl_compositor`, `wl_shm`, `wl_seat`, `xdg-shell`, `wl_data_device_manager` and `wl_output` globals.
- One virtual output whose advertised pixel mode follows Winit window resize events. The development mode currently advertises 60 Hz.
- A stable toplevel registry tracks creation, mapping, unmapping, remapping and destruction while preserving each window's last floating position.
- A Smithay `Space<Window>` handles mapped xdg toplevels, initial cascading placement, output enter/leave tracking and scene rendering.
- Absolute pointer motion, mouse buttons and scroll-axis events are routed to the surface under the pointer.
- Standard Wayland clipboard selection is available between clients; data-device focus follows the keyboard-focused client. Primary-selection extensions are not advertised yet.
- A click raises and activates the clicked window and moves keyboard focus to its toplevel. Focus and activation are restored to the topmost remaining window after an unmap or destruction.
- XDG interactive move and resize requests are accepted only when their serial matches an active pointer grab for the requesting client. Resize requests use the client's minimum/maximum size hints and complete with a final configure after release.
- XDG maximize and unmaximize requests are supported. BubbleWM saves the pre-maximized floating position and size, then restores them when the client requests unmaximize.
- XDG transient toplevels are positioned relative to their known parent and raised with their parent window family.
- XDG popups are tracked and rendered with their owning toplevel. Popup placement is constrained to the output, reposition requests are handled, and protocol popup grabs route keyboard and pointer focus through Smithay's popup-grab helpers.
- XDG minimize requests unmap the window and its transient descendants. `Super+H` minimizes the focused window and `Super+Tab` restores the most recently minimized window family.
- `Super+Q` and the server-side titlebar close control send the standard XDG close event to the client; BubbleWM does not forcibly terminate the application.
- BubbleWM advertises XDG minimize/maximize capabilities only. Fullscreen and compositor window-menu capabilities remain unadvertised.
- Clients negotiating XDG server-side decorations receive a prototype 32-pixel titlebar with close, maximize/restore and minimize controls. Client-side decoration requests are respected. The titlebar can also initiate an interactive move.
- Window geometry is refreshed after root-surface and subsurface commits.
- Frame pacing is throttled to approximately 60 updates per second in the nested development loop.
- Configurable virtual workspaces keep independent window sets. `Super+Left` and `Super+Right` switch between them; `Super+1` through `Super+9` select one, and adding Shift moves the focused window.
- The IPC server starts and stops with the compositor. It reports the active workspace and count, accepts navigation requests, publishes the BubbleThemeEngine palette and exposes compact Wi-Fi/audio status.
- BubbleThemeEngine generates wallpaper-based semantic roles. BubbleWM uses them in glass panels, notifications, launcher, lock screen and window surfaces; BubblePanel reads the full palette over local IPC and refreshes it every five seconds.

### Not implemented yet

- Pointer cursor rendering, touch input, and a complete focus/grab policy.
- Polished decorations: the current server-side titlebar uses BubbleThemeEngine surfaces but remains a prototype with colored controls and no title text, icons or full accessibility behavior.
- Tiling layout policy and workspace-specific layout behavior.
- Output-management protocols, multiple outputs and connector hotplug.
- A login greeter and session supervisor: BubbleWM cannot yet authenticate a login, create a PAM/logind user session, launch the compositor as that user, or cleanly end and reap it. The current built-in lock screen only unlocks the user who already owns the running compositor; it does not replace greetd or LightDM today.
- Richer BubblePanel task-list presentation, Bluetooth and battery modules.

## BubblePanel prototype

The independent panel connects to a Wayland compositor, creates a bottom `wlr-layer-shell` surface, reserves a 40-pixel exclusive zone and renders a three-zone layout with workspace and task indicators, live Wi-Fi/audio indicators and a seven-segment local-time clock. Through BubbleWM's local IPC endpoint it reads the semantic palette, current workspace, window list, status and Wi-Fi network list; focuses windows when a task is clicked; opens a network popover to connect to nearby or saved profiles; toggles Wi-Fi or audio mute; adjusts volume with vertical scrolling over the speaker; and switches workspaces. Snapshots refresh every five seconds, with network probes refreshed when the popover opens.

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

BubblePanel requires a compositor that supports `wlr-layer-shell`; live theme colors, workspace/window control and system-status modules require BubbleWM's local IPC endpoint. Bluetooth, battery and notification indicators remain future work.

See [bubblepanel/README.md](bubblepanel/README.md) and [docs/PANEL.md](docs/PANEL.md).

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A Wayland or X11 host session with working EGL/OpenGL drivers for the nested Winit development backend. This is a development host requirement; BubbleWM's managed desktop sessions will be Wayland-only.
- Native development libraries required by the selected Smithay backend.
- A separate compositor with `wlr-layer-shell` to run the current BubblePanel prototype.

## Planned subsystems

- **Core:** compositor state, event loop and protocol lifecycle.
- **Backend:** Winit for nested development; initial DRM/KMS/libseat implementation for native sessions, with multi-output and hotplug still planned.
- **BubbleSession:** Wayland-only greeter, authentication boundary, user-session startup, logout and child-process cleanup; designed to replace greetd/tuigreet in BubbleOS and integrate with other distributions without requiring Xorg sessions.
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
