# BubblePanel

BubblePanel is BubbleOS's modular Wayland panel, inspired by the flexibility and module-based configuration of Waybar. It is an independent desktop component: it must not be part of BubbleWM's compositor process.

The goal is to provide a familiar status bar that can be rearranged and restyled from BubbleSettings, while keeping a clean configuration format for advanced users.

## Design principles

- **Independent process:** BubblePanel can be restarted or upgraded without restarting BubbleWM.
- **Wayland-native:** use layer-shell when available for panel surfaces, anchors, exclusive zones, margins and keyboard interaction. Detect protocol availability and provide clear diagnostics instead of assuming every compositor exposes the same extensions.
- **Modular:** each module has a defined data model, rendering, events and optional settings.
- **GUI-first configuration:** BubbleSettings is the normal way to configure the panel. Manual TOML editing remains available for advanced users.
- **Low overhead:** event-driven updates, configurable polling intervals, bounded subprocess execution and minimal redraws.
- **Graceful degradation:** missing protocols, unavailable sensors and unsupported hardware must hide or disable only the affected feature, not crash the panel.
- **Multi-monitor:** independent panel instances or surfaces per output, with per-output configuration and predictable hotplug behavior.

## Layout model

A panel contains three independently arranged zones:

- **Start / left:** BubbleOS launcher, workspaces, pinned applications and running applications.
- **Center:** active-window title, clock, media controls or user-selected modules.
- **End / right:** system indicators such as network, audio, Bluetooth, battery, notifications and session controls.

The zones support ordered modules, spacing, separators, alignment and optional overflow handling. The user may choose a single continuous bar or separated groups.

## Built-in modules

Initial module set, implemented incrementally:

- Launcher / main menu.
- Workspaces.
- Active window title.
- Task list, when a supported toplevel-management protocol is available.
- Clock and calendar.
- Network state.
- Audio output and volume.
- Battery and power state, when exposed by the system.
- Bluetooth state, when available.
- Notifications indicator, integrated with the notification service when available.
- System monitor: CPU and memory initially; temperatures only when reliable sensors are exposed.
- Media player controls through MPRIS where available.
- Custom command module for advanced users.

Modules must declare their required protocols or system interfaces, update policy, configuration schema and supported interactions. A missing dependency must not prevent unrelated modules from loading.

## Appearance and profiles

Provide three initial presets:

- **Classic:** compact, continuous, conventional panel.
- **Floating:** detached groups, rounded corners and configurable margins.
- **Compact:** reduced height and icon-focused presentation.

All presets are starting points, not locked themes. Expose at least:

- Screen edge / position: top, bottom, left or right.
- Height, width, margins, padding and spacing.
- Background color, opacity and optional blur when supported.
- Corner radius, border and separators.
- Font family, size, weight and icon theme.
- Module order, visibility, alignment and per-module style.
- Autohide, reveal behavior and exclusive-zone behavior.
- Animation and reduced-motion preferences.
- Per-output visibility, scale and profile.

The visual language should fit BubbleOS and work with BubbleColor's adaptive palette. Contrast must remain readable when wallpaper colors change; users must be able to override the palette.

## Interaction model

Modules may define left-click, middle-click, right-click, scroll and tooltip actions. Actions should be discoverable in BubbleSettings. Avoid hard-coded desktop assumptions: launchers and commands must be configurable, and missing applications must produce a useful error.

Custom command modules must have execution timeouts, output limits, controlled environment variables and explicit refresh intervals. Do not execute arbitrary configuration strings through a shell by default. Provide an opt-in shell mode for advanced users and document its security implications.

## BubbleSettings integration

Add a dedicated **Panel** section to BubbleSettings:

- Enable / disable BubblePanel.
- Choose Classic, Floating or Compact preset.
- Configure position, dimensions, spacing and appearance.
- Select the target monitor(s).
- Add, remove, reorder and configure modules.
- Configure module interactions and custom commands.
- Preview changes before applying.
- Show unavailable protocol or hardware features with an explanation.
- Indicate whether a change applies live or requires a panel restart.

BubbleSettings communicates with BubblePanel through a versioned local IPC interface. BubblePanel must also be able to start with its last valid configuration when BubbleSettings is absent.

## Configuration and IPC

Use a versioned configuration schema under:

`$XDG_CONFIG_HOME/bubblewm/panel.toml`, falling back to `~/.config/bubblewm/panel.toml`.

Keep panel settings logically separate from compositor settings while sharing common theme tokens and validation conventions. Apply candidate configurations transactionally, validate module settings before activation, write files atomically and retain a recoverable last-known-good version.

IPC must support at least:

- Query panel status and detected outputs.
- Query available modules and their capabilities.
- Validate a candidate configuration.
- Apply a validated configuration.
- Reload configuration and report structured errors.
- Restart or gracefully quit the panel.

IPC is local, versioned and permission-restricted. The panel must not expose an unauthenticated network control endpoint.

## Implementation phases

1. **Panel foundation:** independent Rust executable, layer-shell surface, basic layout, clock and clean shutdown.
2. **Output and workspace support:** multi-monitor lifecycle, workspace indicators and protocol capability detection.
3. **Task and system modules:** active-window/task information, network, audio and battery where available.
4. **Styling engine:** profiles, theme tokens, per-module styling, adaptive contrast and reduced-motion support.
5. **Configuration and IPC:** validated schema, atomic persistence, reload and structured status.
6. **BubbleSettings integration:** visual editor, module management, live preview and restart feedback.
7. **Advanced modules:** MPRIS, notifications, sensors and bounded custom-command modules.
8. **Hardening:** accessibility, performance, hotplug, missing-protocol and failure-path testing.

BubblePanel is optional. BubbleWM must remain fully usable without it.
