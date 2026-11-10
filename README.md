# BubbleWM

A native, lightweight and customizable Wayland compositor for BubbleOS, written in Rust and built on Smithay.

> **Status:** early development. BubbleWM now starts an initial nested Winit compositor with a single advertised `wl_output`, core Wayland globals, xdg-toplevel rendering, tracked window lifecycle, pointer routing, click-to-focus and interactive XDG move/resize. It is a development milestone, not yet a complete or daily-usable desktop environment.

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
- **bubble-ipc:** shared, versioned local-control protocol and Linux Unix-socket transport.

## Run the nested compositor

The current Winit backend opens a development window, creates a Wayland socket in `XDG_RUNTIME_DIR`, accepts Wayland clients, advertises the core compositor, shm, seat, xdg-shell and one `wl_output` global, then renders mapped xdg toplevel windows through a Smithay `Space<Window>`.

```sh
cargo run
```

The log prints the selected `WAYLAND_DISPLAY` name. In another terminal, point a Wayland client at that socket:

```sh
WAYLAND_DISPLAY=wayland-1 <your-wayland-client>
```

Replace `wayland-1` with the exact display name printed by BubbleWM. Close the Winit window to stop the nested compositor.

The Winit backend uses the host session's EGL/OpenGL environment. The native DRM/KMS backend is not implemented yet; selecting `core.backend = drm` exits with an explicit error.

### Current compositor capabilities

- One nested Winit window and one Wayland display socket.
- Core `wl_compositor`, `wl_shm`, `wl_seat`, `xdg-shell` and `wl_output` globals.
- One virtual output whose advertised pixel mode follows Winit window resize events. The development mode currently advertises 60 Hz.
- A stable toplevel registry tracks creation, mapping, unmapping, remapping and destruction while preserving each window's last floating position.
- A Smithay `Space<Window>` handles mapped xdg toplevels, initial cascading placement, output enter/leave tracking and scene rendering.
- Absolute pointer motion, mouse buttons and scroll-axis events are routed to the surface under the pointer.
- A click raises and activates the clicked window and moves keyboard focus to its toplevel. Focus and activation are restored to the topmost remaining window after an unmap or destruction.
- XDG interactive move and resize requests are accepted only when their serial matches an active pointer grab for the requesting client. Resize requests use the client's minimum/maximum size hints and complete with a final configure after release.
- Window geometry is refreshed after root-surface and subsurface commits.
- Frame pacing is throttled to approximately 60 updates per second in the nested development loop.
- The IPC server starts and stops with the compositor. Capability discovery works, but workspace commands correctly return `unavailable` until workspace management exists.

### Not implemented yet

- Pointer cursor rendering, touch input, keyboard shortcuts, popup grabs and a complete focus/grab policy.
- Maximize/minimize, compositor-provided close controls, server-side decorations and tiling.
- Virtual workspaces and workspace switching.
- `wlr-layer-shell`, output-management protocols, multiple outputs and native DRM/KMS.
- BubblePanel integration and real system status modules.

## BubblePanel prototype

The independent panel connects to a Wayland compositor, creates a bottom `wlr-layer-shell` surface, reserves a 40-pixel exclusive zone and renders a three-zone layout with workspace/status placeholders and a seven-segment local-time clock. It also detects pointer clicks and scroll gestures and refreshes the clock once per minute.

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

BubblePanel currently requires a compositor that supports `wlr-layer-shell`. BubbleWM does not yet implement the server side of that protocol, so use another compatible Wayland compositor to test the panel for now. Real workspace state, IPC client integration and system modules remain future work.

See [bubblepanel/README.md](bubblepanel/README.md) and [docs/PANEL.md](docs/PANEL.md).

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A Wayland or X11 host session with working EGL/OpenGL drivers for the Winit development backend.
- Native development libraries required by the selected Smithay backend.
- A separate compositor with `wlr-layer-shell` to run the current BubblePanel prototype.

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