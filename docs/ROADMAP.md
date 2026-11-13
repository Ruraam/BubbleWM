# Roadmap

## 0.1.0 — Foundation
- [x] Establish repository metadata and Rust project.
- [x] Add typed TOML configuration with validation.
- [x] Add architecture and roadmap documentation.
- [x] Confirm the workspace dependency feature set and compile/test it in CI.

## 0.2.0 — Nested compositor (initial bring-up complete)
- [x] Smithay Winit/EGL development backend.
- [x] Automatic Wayland socket creation and client acceptance.
- [x] Core compositor, shm, seat, xdg-shell and single wl_output global.
- [x] One virtual output with a mode that follows Winit window resizes.
- [x] Smithay Space<Window> with mapped xdg-toplevel windows, cascading initial placement and output enter/leave tracking.
- [x] Basic xdg-toplevel scene rendering and frame callbacks.
- [x] Absolute pointer motion, mouse button and scroll-axis routing to the surface under the pointer.
- [x] Initial click-to-focus policy: raise/activate the clicked window and give its toplevel keyboard focus.
- [x] Initial activation and keyboard focus for newly mapped toplevels.
- [x] Bounded frame pacing, versioned IPC startup, compositor-thread dispatch and orderly shutdown.
- [x] Composite client-provided cursor surfaces at the requested pointer hotspot; hide the nested host cursor for custom/hidden cursor states.
- [ ] Named cursor-theme rendering, touch routing and robust keyboard/pointer grabs.
- [ ] Output-management protocols, multi-output layout and hotplug handling.
- [x] wlr-layer-shell server support for BubblePanel, including layer arrangement, exclusive zones, rendering, input and keyboard focus.

## 0.3.0 — Window management
- [x] Stable toplevel registry and lifecycle handling for creation, map, unmap, remap and destruction.
- [x] Preserve floating window positions across unmap/remap transitions.
- [x] Restore keyboard focus and activation to the topmost remaining window after unmap or destruction.
- [x] XDG interactive move requests guarded by the matching pointer-grab serial and requesting client.
- [x] XDG interactive resize requests with client min/max size constraints and final configure on release.
- [x] Refresh window geometry after root-surface and subsurface commits.
- [x] XDG maximize/unmaximize handling with saved floating position and size restoration.
- [x] Transient toplevel parent tracking, relative initial placement and family stacking.
- [x] XDG popup tracking/rendering, output-constrained placement and reposition requests.
- [x] XDG popup keyboard/pointer grabs with serial validation.
- [x] XDG minimize requests and minimized state for toplevels and transient descendants.
- [x] Restore the most recently minimized window family.
- [x] Super+H minimize, Super+Tab restore and Super+Q request-close shortcuts, with matching key-release suppression.
- [x] XDG close event from a prototype server-side titlebar; applications remain responsible for handling the close request.
- [x] XDG decoration negotiation, default server-side mode and honoring explicit client-side mode requests.
- [x] Prototype 32-pixel server-side titlebar with close, maximize/restore, minimize and move interactions.
- [ ] Window title text, icons, themed controls, borders and accessibility refinements.
- [x] Four virtual workspaces with per-window membership and preserved stacking order.
- [x] Keep inactive-workspace windows unmapped while preserving client state and floating geometry.
- [x] Inherit workspace membership across transient toplevel families and preserve minimized state across switches.
- [x] Connect workspace IPC commands to authoritative compositor state, including bounds validation and wraparound navigation.
- [x] Integrate BubblePanel's IPC client for capability discovery, live workspace state, click activation and scroll navigation.
- [ ] Tiling layout policy and configurable floating/tiling behavior.

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
- [x] Three-zone layout, workspace indicators, pointer hit testing and clock.
- [x] Live workspace state and workspace navigation through BubbleWM's authenticated IPC endpoint.
- [ ] Task list and real system modules.
- [ ] Multi-monitor support, profiles and validated configuration.
- [ ] BubbleSettings integration and graceful degradation.

## 0.10.0 — Native backend
- [x] Add an opt-in Smithay feature set for DRM, GBM, libinput, udev and libseat.
- [x] Add CI compile coverage for the optional native backend dependencies.
- [x] Add a read-only libseat/udev probe for seat-assigned DRM card nodes.
- [x] Open, validate and close one seat-assigned DRM card node through libseat without modesetting.
- [x] Initialize and drop Smithay's DrmDevice while returning its descriptor through libseat; do not disable connectors or create DRM surfaces.
- [x] Add a hardware-independent, idempotent policy state machine for libseat pause/activation events and test device-access gating.
- [x] Register the real libseat notifier in a separate, explicit diagnostic calloop monitor and test lifecycle policy without DRM hardware.
- [ ] Integrate session events into the native compositor and pause/drop/reopen live DRM resources on session events.
- [ ] DRM/KMS surface creation, connector/mode management and output lifecycle.
- [ ] Multi-output and per-monitor settings.
- [ ] Robustness, accessibility and performance testing.

## 0.11.0 — Session Manager foundation
- [x] Add typed, versioned IPC requests for session state, capabilities and actions.
- [x] Add a backend-independent Rust session policy layer.
- [x] Disable host session and power actions in the nested Winit development backend.
- [x] Test that every session action is rejected when no trusted provider is installed.
- [x] Add a read-only systemd-logind probe over the system D-Bus.
- [x] Parse logind's explicit yes/no/challenge/na capability responses conservatively.
- [x] Validate the current logind session ID, owner UID, Wayland type, user class and active state.
- [x] Add a one-shot logind worker with a bounded result channel and non-blocking polling.
- [ ] Integrate native session detection into the native backend through the worker, outside the compositor event loop.
- [ ] Add per-session lock/logout provider discovery.
- [ ] Add authorization, confirmation and action execution flows.
- [ ] Add an independent graphical Session Menu with keyboard accessibility and BubbleColor styling.

## 1.0.0 — BubbleOS integration
- [ ] Packaging, session integration, documentation and release hardening.

## Bubble Icons — coverage and diagnostics
- [x] Add original scalable SVG icons and a freedesktop-compatible theme.
- [x] Add a standard-library validator for theme metadata and SVG structure.
- [x] Add common application-name aliases as relative SVG links and validate their targets.
- [x] Add a read-only auditor for installed XDG desktop entries with JSON and strict modes.
- [ ] Expand the icon inventory from real Debian/Flatpak/AppImage desktop-entry usage.
- [ ] Design and validate Bubble Dark, Bubble Light, Bubble Neon and Bubble Glass variants.
- [ ] Integrate icon-theme selection and previews into BubbleSettings.
