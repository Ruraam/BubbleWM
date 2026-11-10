# Architecture

BubbleWM is a standalone Wayland compositor. BubbleOS integration must remain optional.

## Layers

- **Compositor core:** Smithay state, event loop, protocol globals and lifecycle.
- **Backend:** Winit for nested development; DRM/KMS for native sessions.
- **Desktop:** surfaces, toplevel management, focus, placement and workspaces.
- **BubbleDecor:** server-side titlebars, borders and window controls.
- **BubbleFX:** effect scheduling, animation and GPU shader pipeline.
- **BubbleColor:** wallpaper analysis, palette generation and contrast validation.
- **Settings application:** a dedicated graphical application for discovering and editing every supported user-facing option.
- **Configuration service:** typed configuration, validation, persistence, reload and migration.
- **IPC:** versioned local control interface for the shell, settings application and external tools.

## Settings-first design

User-facing behavior must be configurable through a clear graphical Settings application. Editing TOML by hand remains an optional advanced path, never a requirement for ordinary users.

The settings application and compositor are separate processes. The UI edits a validated configuration model and requests a live apply through versioned local IPC. The compositor remains authoritative: it validates every request, reports unsupported capabilities and returns actionable errors. Settings that cannot safely be applied live are clearly marked as requiring a restart.

Settings are organized by task rather than by internal Rust module:

- Appearance: theme, wallpaper-derived colors, accent, contrast and transitions.
- Windows: floating/tiling behavior, focus, placement, snapping and per-app rules.
- Decorations: titlebar, buttons, borders, corner radius and client/server decoration preference.
- Effects: blur, transparency, shadows, animations, wobble and workspace transitions.
- Workspaces: count, names, navigation and layout behavior.
- Input: keyboard shortcuts, pointer behavior, gestures and repeat settings.
- Displays: scale, refresh, output arrangement and per-monitor preferences.
- Performance: effect quality, frame pacing, reduced motion and low-resource profile.
- Advanced: backend diagnostics, configuration paths, IPC status and logs.

The interface should provide search, sensible presets, contextual explanations, reset-to-default controls and immediate preview where safe. Every setting needs a stable key, a default, validation, a documented live/restart policy and a capability fallback.

## Configuration and reliability

- Store user configuration under `$XDG_CONFIG_HOME/bubblewm/` (default: `~/.config/bubblewm/`).
- Keep a versioned schema and migrate older configuration safely.
- Preserve comments and unknown extension keys when writing configuration where practical.
- Apply changes transactionally: validate the complete candidate before replacing the active configuration.
- Use atomic file replacement and retain a recoverable previous configuration.
- Never block the compositor event loop with disk I/O, image analysis or IPC work.
- Treat IPC clients and configuration files as untrusted input.
- Make optional effects fail safely when a renderer capability is unavailable.
- Track damage and avoid repainting unchanged regions.
- Keep BubbleWM usable when the settings application is not installed or running.

## Decoration negotiation

BubbleWM will prefer server-side decorations through the xdg-decoration protocol where supported. Some clients draw their own client-side decorations; those clients must remain usable and must not be forcibly stripped.
