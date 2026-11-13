# BubbleSettings

BubbleSettings is the graphical control center for BubbleWM and its optional desktop components. It is a separate application, not a window embedded into the compositor.

## User experience

The application must make common changes discoverable without requiring users to know configuration keys or edit files manually.

### Main sections

1. **Appearance** — wallpaper, adaptive palette, accent color, contrast, light/dark mode and transitions.
2. **Windows** — floating or tiling layout, focus policy, placement, snapping and application rules.
3. **Decorations** — titlebar visibility, button layout, borders, corner radius and client/server decoration preference.
4. **Effects** — blur, transparency, shadows, fade, zoom, slide, wobble and workspace transitions.
5. **Workspaces** — number, names, navigation and per-workspace layout.
6. **Panel** — enablement, Classic/Floating/Compact profiles, position, dimensions, monitors, modules, ordering, styling and interactions.
7. **Input** — keyboard shortcuts, pointer options, gestures and repeat behavior.
8. **Displays** — scale, refresh rate, arrangement and per-monitor preferences.
9. **Performance** — quality presets, reduced motion, frame pacing and low-resource mode.
10. **Advanced** — diagnostics, configuration location, IPC state and logs.

## Panel settings

The Panel section manages BubblePanel without requiring manual configuration editing:

- Enable or disable the independent panel process.
- Choose Classic, Floating or Compact as a starting preset.
- Set top, bottom, left or right placement and configure dimensions, margins and spacing.
- Select outputs and configure per-monitor behavior.
- Add, remove, reorder and configure available modules.
- Configure module visibility, alignment, icons, labels, tooltips and supported click/scroll actions.
- Adjust colors, opacity, blur where supported, borders, radius, typography and separators.
- Configure autohide, exclusive zone and reduced-motion behavior.
- Preview changes and apply them live when safe.
- Display clear explanations when a module requires an unavailable protocol or system service.

## Interaction requirements

- Search settings by name and keyword.
- Explain technical options in plain language.
- Provide sensible presets without hiding individual controls.
- Offer reset for a setting, section and complete configuration.
- Preview visual changes before committing when practical.
- Clearly distinguish immediate changes from changes requiring restart.
- Show a useful error and preserve the last known-good configuration if an apply fails.
- Support keyboard navigation, accessible labels and scalable UI.

## Configuration contract

Each setting must have:
- A stable, documented key.
- A type, default and validation rule.
- A supported-value range or enumeration.
- A live-apply or restart-required policy.
- A capability fallback when hardware or protocol support is missing.

The GUI edits a candidate configuration. The owning component validates the candidate, applies it transactionally, and acknowledges success or returns a structured error. Invalid changes must never partially corrupt the active state.

## Persistence and safety

- Compositor configuration: `$XDG_CONFIG_HOME/bubblewm/config.toml`, falling back to `~/.config/bubblewm/config.toml`.
- Panel configuration: `$XDG_CONFIG_HOME/bubblewm/panel.toml`, falling back to `~/.config/bubblewm/panel.toml`.
- Write atomically; never truncate the active file before validation succeeds.
- Keep a recoverable backup of the last valid configuration.
- Version the schema and provide explicit migrations.
- Do not require the GUI or panel to be running for the compositor to start.
- Do not require manual TOML editing for normal operation.

## Implementation order

1. Typed schemas and validation.
2. Atomic persistence, backup and migration.
3. Versioned IPC with structured acknowledgements.
4. Settings UI and searchable categories, including Panel.
5. Live previews and per-setting restart indicators.


## Appearance foundation

The initial persisted schema reserves stable settings for the desktop appearance:

- Wallpaper path and placement mode (center, contain, cover, zoom or tile).
- Icon theme name, cursor theme and cursor size.
- Interface and monospace font families, plus the base font size.
- Dark, light or system color scheme and a validated hexadecimal accent color.
- Bubble Color engine selection, palette source, palette variant and contrast enforcement.

The default icon theme identifier is `Bubble`. Bubble Icons is intended to follow the freedesktop icon-theme layout and naming conventions, with scalable SVG assets, application aliases and inherited fallback themes. The first implementation must include an inventory/test tool that reports unresolved desktop-entry icon names rather than silently claiming full coverage.

Bubble Color owns palette generation. Its planned inputs are the selected wallpaper or a manually chosen accent; its outputs must be shared theme tokens consumed by BubbleWM, BubblePanel and BubbleSettings. Contrast enforcement must check foreground/background pairs before a generated palette is accepted.

These keys are configuration foundations only. Wallpaper rendering, font application, icon-theme installation/selection and palette generation must be integrated by their owning services before the corresponding controls are advertised as active in the GUI.

## Configuration persistence milestone

The compositor configuration uses schema version 1. Missing appearance sections in existing configuration files receive defaults, preserving compatibility with the pre-appearance configuration format. Unknown keys and unsupported schema versions are rejected rather than guessed.

Saving is validate-first and uses a temporary file in the destination directory followed by an atomic rename. On Unix, the temporary file is created with owner-only permissions. A failed validation or temporary-file write must leave the active configuration untouched.
