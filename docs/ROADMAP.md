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
- [ ] Pointer cursor rendering, touch routing and robust keyboard/pointer grabs.
- [ ] Output-management protocols, multi-output layout and hotplug handling.
- [x] wlr-layer-shell server support for BubblePanel.

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
- [x] Apply BubbleThemeEngine surface roles to window decorations.
- [ ] Window title text, icons and accessibility refinements.
- [ ] Tiling layout policy and virtual workspace foundations.
- [ ] Floating layout policy and virtual workspace foundations.
- [x] Connect workspace IPC commands to authoritative workspace state.
- [x] Integrate BubblePanel's IPC client for the BubbleThemeEngine palette.
- [x] Integrate workspace state and navigation over IPC.
- [x] Publish Wi-Fi and audio state to BubblePanel over IPC.
- [x] Publish managed window titles and focus/restore requests over IPC.
- [x] Queue Wi-Fi and audio mute toggles on the background status worker.
- [x] Adjust audio volume from BubblePanel using bounded five-percent scroll steps.

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
- [x] Wallpaper palette extraction through BubbleThemeEngine.
- [x] Contrast-aware adaptive theme generation.
- [x] Share semantic roles with BubbleWM surfaces and BubblePanel over IPC.
- [ ] Manual theme tuning controls.

## 0.8.0 — BubbleFX
- [ ] Animation scheduler and easing.
- [ ] Fade, slide, zoom, shadows, transparency and blur.
- [ ] Wobble and workspace transitions, with quality profiles.

## 0.9.0 — BubblePanel
- [x] Independent Rust Wayland panel process.
- [x] Layer-shell integration and capability detection on the client side.
- [x] Three-zone layout, placeholder workspaces, pointer hit testing and clock.
- [x] Consume and refresh BubbleThemeEngine colors over Bubble IPC.
- [x] Display live workspace state and handle click/scroll navigation over IPC.
- [x] Read-only Wi-Fi signal and audio volume/mute indicators.
- [x] Show live window task buttons and focus/restore on click.
- [x] Toggle Wi-Fi and audio mute from BubblePanel.
- [x] Adjust volume from the BubblePanel speaker indicator.
- [x] Select nearby and saved Wi-Fi networks from a themed panel popover.
- [ ] Richer task-list presentation.
- [ ] Multi-monitor support, profiles and validated configuration.
- [ ] BubbleSettings integration and graceful degradation.

## 0.10.0 — Native backend bring-up
- [x] Initial DRM/KMS output through libseat, GBM and EGL for one primary GPU and one connected output.
- [x] libinput input and libseat pause/resume handling for VT/session switches.
- [ ] Multi-output, connector hotplug and per-monitor settings.
- [ ] Hardware validation, robustness, accessibility and performance review.

## 0.11.0 — Wayland login and session manager
- [x] Add a fixed-service PAM authentication/account-check helper for an explicitly selected login account.
- [x] Add a root-only PAM session handle that establishes/deletes PAM credentials, opens/closes the PAM session and restores supervisor groups.
- [x] Add a trusted-command launcher that drops to the selected user and passes a filtered Wayland session environment.
- [x] Add a managed process handle that signals a dedicated session process group and reaps the direct session leader.
- [x] Compose PAM open/close, command execution and process-group cleanup into a one-login helper flow.
- [x] Add a root-only `bubblewm-session` entry point with bounded stdin password input, VT validation and a fixed compositor/configuration target.
- [ ] Add a production greeter and a system-facing BubbleWM session supervisor that securely invokes the helper.
- [ ] Install a distro-reviewed `bubblewm` PAM service (a Debian-compatible profile is available) and connect greeter login to the helper and account/session policy.
- [ ] Start the selected user's PAM/logind session through a reviewed stack with `pam_systemd` exactly once; validate the session scope, `XDG_RUNTIME_DIR`, seat and VT metadata before starting the compositor.
- [ ] Run the compositor and desktop as the selected user, with a correct `XDG_RUNTIME_DIR` and user-manager environment; never run desktop clients as root.
- [ ] Split the root supervisor from the single-threaded PAM worker; keep the supervisor outside the user session cgroup and make it own final cleanup.
- [ ] Track the full session lifecycle and handle compositor crash, user switching and greeter recovery.
- [ ] On logout and every startup-failure path, have the external supervisor stop the retained logind session scope through systemd and verify cgroup emptiness; process-group signaling remains best-effort only.
- [ ] Add shutdown/reboot actions and safe cleanup of session resources, device access and VT ownership.
- [ ] Keep the managed login and desktop session Wayland-only; do not add Xorg session entries. Optional XWayland is limited to legacy clients inside Wayland.
- [ ] Add security review and virtual-machine coverage for successful/failed login and each session termination path.

## 1.0.0 — Distribution integration
- [ ] Package BubbleWM as the Wayland-only graphical login/session manager and compositor for BubbleOS, replacing greetd/tuigreet and Wayfire.
- [ ] Document integration with other distributions as a replacement for LightDM or their existing display manager.
- [ ] Provide install, upgrade, recovery and disable paths, then complete release hardening.
