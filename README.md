# BubbleWM

A native, lightweight and customizable Wayland compositor for BubbleOS, written in Rust and built on Smithay.

> **Status:** early development. BubbleWM now starts an initial nested Winit compositor with core Wayland, shared-memory, seat and xdg-shell support. It is a development milestone, not yet a complete or daily-usable desktop environment.

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

The current Winit backend opens a development window, creates a Wayland socket in XDG_RUNTIME_DIR, accepts Wayland clients, exposes the core compositor, shm, seat and xdg-shell globals, and renders committed xdg toplevel surfaces.

```sh
cargo run
```

The log prints the selected WAYLAND_DISPLAY name. In another terminal, point a Wayland client at that socket:

```sh
WAYLAND_DISPLAY=wayland-1 <your-wayland-client>
```

Replace wayland-1 with the exact display name printed by BubbleWM. Close the Winit window to stop the nested compositor.

The Winit backend uses the host session's EGL/OpenGL environment. The native DRM/KMS backend is not implemented yet; selecting core.backend = drm exits with an explicit error.

### Current compositor boundaries

- One nested Winit window and one Wayland display socket.
- Core wl_compositor, wl_shm, wl_seat and xdg-shell support.
- Basic keyboard event forwarding with a temporary first-toplevel focus policy.
- Basic xdg-toplevel surface rendering.
- No output-management globals, pointer/touch routing, proper focus policy, move/resize, decorations, workspaces, layer-shell or multi-monitor support yet.
- The IPC server starts and is shut down with the compositor. Capability discovery works, but workspace commands correctly return unavailable until workspace management exists.

## BubblePanel prototype

The first panel milestone connects to a Wayland compositor, creates a bottom wlr-layer-shell surface, reserves a 40-pixel exclusive zone and renders a three-zone layout with workspace/status placeholders and a seven-segment local-time clock. It also detects pointer clicks and scroll gestures and refreshes the clock once per minute.

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

BubblePanel currently requires a compositor that supports wlr-layer-shell. BubbleWM does not yet implement the server side of that protocol, so use another compatible Wayland compositor to test the panel for now. Real workspace state, IPC client integration and system modules remain future work.

See [bubblepanel/README.md](bubblepanel/README.md) and [docs/PANEL.md](docs/PANEL.md).

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A Wayland or X11 host session with working EGL/OpenGL drivers for the Winit development backend.
- Native development libraries required by the selected Smithay backend.
- A separate compositor with wlr-layer-shell to run the current BubblePanel prototype.

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