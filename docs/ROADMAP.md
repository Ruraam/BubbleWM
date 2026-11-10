# Roadmap

## 0.1.0 — Foundation
- [x] Establish repository metadata and Rust project.
- [x] Add typed TOML configuration with validation.
- [x] Add architecture and roadmap documentation.
- [x] Confirm the workspace dependency feature set and compile/test it in CI.

## 0.2.0 — Nested compositor (initial bring-up complete)
- [x] Smithay Winit/EGL development backend.
- [x] Automatic Wayland socket creation and client acceptance.
- [x] Core compositor, shm, seat and xdg-shell globals.
- [x] Basic xdg-toplevel surface rendering and frame callbacks.
- [x] Initial keyboard event forwarding with a temporary first-toplevel focus policy.
- [x] Versioned IPC server startup, compositor-thread dispatch and orderly shutdown.
- [ ] Output-management globals and output state.
- [ ] Pointer/touch routing, proper focus policy and event-loop frame pacing improvements.
- [ ] wlr-layer-shell server support for BubblePanel.

## 0.3.0 — Window management
- [ ] Surface lifecycle and stable window map.
- [ ] Focus, move, resize, maximize and close.
- [ ] Floating layout and workspace foundations.
- [ ] Connect workspace IPC commands to authoritative workspace state.
- [ ] Integrate BubblePanel's IPC client for workspace state and navigation.

## 0.4.0 — BubbleDecor
- [ ] Server-side decoration negotiation.
- [ ] Titlebar, buttons, borders and focus states.

## 0.5.0 — Configuration engine and live IPC
- [ ] Versioned configuration schema and migrations.
- [ ] Safe atomic persistence and rollback.
- [ ] Validated live reload and capability reporting.
- [ ] Per-setting restart requirements.

## 0.6.0 — BubbleSettings
- [ ] Dedicated graphical settings application.
- [ ] Searchable categories, descriptions, presets and reset controls.
- [ ] Live previews and clear apply/restart feedback.
- [ ] Panel settings and module-management interface.
- [ ] The compositor remains functional without the settings app.

## 0.7.0 — BubbleColor
- [ ] Wallpaper palette extraction.
- [ ] Contrast-aware adaptive theme and manual tuning controls.

## 0.8.0 — BubbleFX
- [ ] Animation scheduler and easing.
- [ ] Fade, slide, zoom, shadows, transparency and blur.
- [ ] Wobble and workspace transitions, with quality profiles.

## 0.9.0 — BubblePanel
- [x] Independent Rust Wayland panel process.
- [x] Layer-shell integration and capability detection on the client side.
- [x] Three-zone layout, placeholder workspaces, pointer hit testing and clock.
- [ ] Real workspace state, task list and system modules.
- [ ] Multi-monitor support, profiles and validated configuration.
- [ ] BubbleSettings integration and graceful degradation.

## 0.10.0 — Native backend
- [ ] DRM/KMS session support.
- [ ] Multi-output and per-monitor settings.
- [ ] Robustness, accessibility and performance testing.

## 1.0.0 — BubbleOS integration
- [ ] Packaging, session integration, documentation and release hardening.