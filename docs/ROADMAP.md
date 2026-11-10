# Roadmap

## 0.1.0 — Foundation
- [x] Establish repository metadata and Rust project.
- [x] Add typed TOML configuration with validation.
- [x] Add architecture and roadmap documentation.
- [ ] Confirm dependency feature set and compile in CI.

## 0.2.0 — Nested compositor
- Smithay Winit backend.
- Wayland socket and protocol globals.
- Basic client surfaces and input routing.

## 0.3.0 — Window management
- Focus, move, resize, maximize and close.
- Floating layout and workspace foundations.

## 0.4.0 — BubbleDecor
- Server-side decoration negotiation.
- Titlebar, buttons, borders and focus states.

## 0.5.0 — Configuration engine and live IPC
- Versioned configuration schema and migrations.
- Safe atomic persistence and rollback.
- Validated live reload and capability reporting.
- Per-setting restart requirements.

## 0.6.0 — BubbleSettings
- Dedicated graphical settings application.
- Searchable categories, descriptions, presets and reset controls.
- Live previews and clear apply/restart feedback.
- Panel settings and module-management interface.
- The compositor remains functional without the settings app.

## 0.7.0 — BubbleColor
- Wallpaper palette extraction.
- Contrast-aware adaptive theme and manual tuning controls.

## 0.8.0 — BubbleFX
- Animation scheduler and easing.
- Fade, slide, zoom, shadows, transparency and blur.
- Wobble and workspace transitions, with quality profiles.

## 0.9.0 — BubblePanel
- Independent Rust Wayland panel process.
- Layer-shell integration and capability detection.
- Three-zone layout, clock, workspaces and system modules.
- Multi-monitor support, profiles and validated configuration.
- BubbleSettings integration and graceful degradation.

## 0.10.0 — Native backend
- DRM/KMS session support.
- Multi-output and per-monitor settings.
- Robustness, accessibility and performance testing.

## 1.0.0 — BubbleOS integration
- Packaging, session integration, documentation and release hardening.
