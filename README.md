# BubbleWM

A native, lightweight and customizable Wayland compositor for BubbleOS, written in Rust and built on Smithay.

> **Status:** early development. The compositor is not yet a usable Wayland desktop. The separate BubblePanel prototype is the first runnable Wayland client component.

## Project principles

- Develop and test independently from BubbleOS.
- Use Smithay as the Wayland compositor foundation.
- Start with a nested Winit backend before native DRM/KMS support.
- Keep the compositor core independent from the BubbleOS shell.
- Make visual effects optional and degrade gracefully when unsupported.
- Prefer explicit, documented behavior over placeholder feature claims.

## Workspace

- **bubblewm:** compositor foundation, configuration and future Smithay server.
- **bubblepanel:** independent Wayland panel client using Smithay Client Toolkit.
- **bubble-ipc:** shared, versioned local-control protocol types and bounded JSON framing.

## BubblePanel prototype

The first panel milestone connects to a Wayland compositor, creates a bottom `wlr-layer-shell` surface, reserves a 40-pixel exclusive zone and renders a three-zone layout with workspace/status placeholders and a seven-segment local-time clock.

```sh
cargo build -p bubblepanel
cargo run -p bubblepanel
```

BubblePanel currently requires a compositor that supports `wlr-layer-shell`. BubbleWM does not yet implement that server protocol, so use an existing compatible Wayland compositor for this initial visual prototype. The clock is sampled when the surface is configured; periodic refresh and interactive modules are future work.

See [bubblepanel/README.md](bubblepanel/README.md) and [docs/PANEL.md](docs/PANEL.md).

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A Wayland session for running BubblePanel.
- `wlr-layer-shell` support in the compositor used to test the panel.
- Native development libraries required by the selected Smithay backend.

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

See [docs/ROADMAP.md](docs/ROADMAP.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/PANEL.md](docs/PANEL.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
