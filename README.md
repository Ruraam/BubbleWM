# BubbleWM

A native, lightweight and customizable Wayland compositor for BubbleOS, written in Rust and built on Smithay.

> **Status:** early development. This repository is being established incrementally. It is not yet a usable Wayland compositor.

## Project principles

- Develop and test independently from BubbleOS.
- Use Smithay as the Wayland compositor foundation.
- Start with a nested Winit backend before native DRM/KMS support.
- Keep the compositor core independent from the BubbleOS shell.
- Make visual effects optional and degrade gracefully when unsupported.
- Prefer explicit, documented behavior over placeholder feature claims.

## Current milestone: 0.1.0

The initial milestone establishes the project structure, configuration model, documentation and CI. A working nested compositor is the next milestone.

## Requirements

- Rust 1.87 or newer.
- Cargo.
- A graphical session for future nested-backend testing.
- Native development libraries required by the selected Smithay backend.

## Planned subsystems

- **Core:** compositor state, event loop and protocol lifecycle.
- **Backend:** Winit for nested development; DRM/KMS for native sessions.
- **Desktop:** surfaces, focus, placement, workspaces and optional tiling.
- **BubbleDecor:** server-side titlebars, borders and controls.
- **BubbleFX:** animation scheduler, effects and GPU shader pipeline.
- **BubbleColor:** wallpaper-derived palettes and contrast validation.
- **IPC:** versioned local control interface for shell/settings integration.

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build

Build instructions will be finalized alongside the first implemented Smithay backend. Do not treat this milestone as a functioning compositor.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
