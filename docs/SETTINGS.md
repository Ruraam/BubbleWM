# BubbleSettings

BubbleSettings is the graphical control center for BubbleWM. It is a separate application, not a window embedded into the compositor.

## User experience

The application must make common changes discoverable without requiring users to know configuration keys or edit files manually.

### Main sections

1. **Appearance** — wallpaper, adaptive palette, accent color, contrast, light/dark mode and transitions.
2. **Windows** — floating or tiling layout, focus policy, placement, snapping and application rules.
3. **Decorations** — titlebar visibility, button layout, borders, corner radius and client/server decoration preference.
4. **Effects** — blur, transparency, shadows, fade, zoom, slide, wobble and workspace transitions.
5. **Workspaces** — number, names, navigation and per-workspace layout.
6. **Input** — keyboard shortcuts, pointer options, gestures and repeat behavior.
7. **Displays** — scale, refresh rate, arrangement and per-monitor preferences.
8. **Performance** — quality presets, reduced motion, frame pacing and low-resource mode.
9. **Advanced** — diagnostics, configuration location, IPC state and logs.

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
- A capability fallback when hardware or renderer support is missing.

The GUI edits a candidate configuration. The compositor validates the candidate, applies it transactionally, and acknowledges success or returns a structured error. Invalid changes must never partially corrupt the active state.

## Persistence and safety

- Default location: `$XDG_CONFIG_HOME/bubblewm/config.toml`, falling back to `~/.config/bubblewm/config.toml`.
- Write atomically; never truncate the active file before validation succeeds.
- Keep a recoverable backup of the last valid configuration.
- Version the schema and provide explicit migrations.
- Do not require the GUI to be running for the compositor to start.
- Do not require manual TOML editing for normal operation.

## Implementation order

1. Typed schema and validation.
2. Atomic persistence, backup and migration.
3. Versioned IPC with structured acknowledgements.
4. Settings UI and searchable categories.
5. Live previews and per-setting restart indicators.
