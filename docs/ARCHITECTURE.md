# Architecture

BubbleWM is a standalone Wayland compositor. BubbleOS integration must remain optional.

## Layers

- **Compositor core:** Smithay state, event loop, protocol globals and lifecycle.
- **Backend:** Winit for nested development; DRM/KMS for native sessions.
- **Desktop:** surfaces, toplevel management, focus, placement and workspaces.
- **BubbleDecor:** server-side titlebars, borders and window controls.
- **BubbleFX:** effect scheduling, animation and GPU shader pipeline.
- **BubbleColor:** wallpaper analysis, palette generation and contrast validation.
- **IPC:** versioned local control protocol for shell/settings integration.

## Engineering constraints

- Begin with a minimal nested compositor; keep native DRM/KMS work separate.
- Never block the compositor event loop with image analysis or IPC work.
- Make optional effects fail safely when a renderer capability is unavailable.
- Track damage and avoid repainting unchanged regions.
- Validate configuration and treat IPC/client input as untrusted.
- Preserve client-side decoration compatibility.

## Decoration negotiation

BubbleWM will prefer server-side decorations through the xdg-decoration protocol where supported. Some clients draw their own client-side decorations; those clients must remain usable and must not be forcibly stripped.
