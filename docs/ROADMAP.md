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

## 0.5.0 — Configuration and IPC
- TOML loading, validation and reload.
- Versioned local IPC.

## 0.6.0 — BubbleColor
- Wallpaper palette extraction.
- Contrast-aware adaptive theme.

## 0.7.0 — BubbleFX
- Animation scheduler and easing.
- Fade, slide, zoom, shadows and transparency.

## 0.8.0 — Advanced effects
- Blur pipeline, wobble and workspace transitions.
- Capability detection and quality profiles.

## 0.9.0 — Native backend
- DRM/KMS session support.
- Multi-output and robustness testing.

## 1.0.0 — BubbleOS integration
- Packaging, session integration, documentation and release hardening.
