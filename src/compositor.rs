//! Nested Smithay compositor backend.
//!
//! The Winit development backend exports the core Wayland globals, xdg-shell,
//! server-side decorations and wlr-layer-shell. It maps and renders windows,
//! popups and desktop layers, routes pointer/keyboard events, and accounts for
//! layer-shell exclusive zones when positioning maximized windows. Workspaces,
//! multiple outputs and native DRM/KMS remain future work.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use bubble_ipc::{ErrorCode, ErrorResponse, Request, Response, ResponseData, ThemePalette};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, ButtonState, Event as BackendInputEvent, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    backend::{
        renderer::{
            Color32F, Frame, Renderer,
            element::{
                Kind,
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            utils::{draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state},
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        LayerSurface as DesktopLayerSurface, PopupKeyboardGrab, PopupKind, PopupManager,
        PopupPointerGrab, PopupUngrabStrategy, Space, Window, WindowSurfaceType,
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle, Keysym, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, CursorImageSurfaceData, Focus,
            GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
            GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
            PointerHandle, PointerInnerHandle, RelativeMotionEvent,
        },
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            Client, Display, DisplayHandle, ListeningSocket, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer, wl_seat,
                wl_surface::{self, WlSurface},
            },
        },
        winit::platform::pump_events::PumpStatus,
    },
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, add_post_commit_hook, get_parent, with_states,
            with_surface_tree_downward,
        },
        output::{OutputHandler, OutputManagerState},
        seat::WaylandFocus,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
            primary_selection::{
                PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
            },
        },
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler,
            WlrLayerShellState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState, XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};
use tracing::info;

use crate::layout::{MasterStackConfig, Rect as LayoutRect, master_stack};
use crate::session::SessionManager;
use crate::theme::{Rgb, ThemeDocument, ThemeOptions};
use crate::theme_runtime::ThemeRuntime;

use crate::ipc::IpcRuntime;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_REFRESH_MILLIHZ: i32 = 60_000;
const WINDOW_CASCADE_STEP: i32 = 32;
const WINDOW_CASCADE_COUNT: u32 = 8;
const WORKSPACE_COUNT: u32 = 4;
const TITLEBAR_HEIGHT: i32 = 32;
const DECORATION_BUTTON_SIZE: i32 = 16;
const DECORATION_BUTTON_MARGIN: i32 = 8;
const DECORATION_BUTTON_GAP: i32 = 6;
const RESIZE_EDGE_TOP: u32 = 1;
const RESIZE_EDGE_BOTTOM: u32 = 2;
const RESIZE_EDGE_LEFT: u32 = 4;
const RESIZE_EDGE_RIGHT: u32 = 8;

#[derive(Clone, Copy)]
struct RestoreGeometry {
    location: Point<i32, Logical>,
    size: Option<Size<i32, Logical>>,
}

struct ManagedWindow {
    window: Window,
    /// Position of the outer window frame, not the client surface.
    location: Point<i32, Logical>,
    mapped_once: bool,
    restore_geometry: Option<RestoreGeometry>,
    parent_surface: Option<WlSurface>,
    minimized: bool,
    minimized_order: Option<u64>,
    workspace: u32,
    /// Stable order used by the tiler; unlike stacking_order, focus changes never modify it.
    tiling_order: u64,
    stacking_order: u64,
    tiling_restore: Option<RestoreGeometry>,
    layout_override: WindowLayoutOverride,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum WindowLayoutOverride {
    #[default]
    Automatic,
    Floating,
    Tiled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecorationAction {
    Close,
    Maximize,
    Minimize,
    Move,
}

#[derive(Clone)]
struct ResizeSession {
    surface: WlSurface,
    edges: u32,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
    released: bool,
}

enum WindowGrabKind {
    Move,
    Resize {
        edges: u32,
        initial_size: Size<i32, Logical>,
        last_size: Size<i32, Logical>,
    },
}

struct WindowPointerGrab {
    start_data: GrabStartData<CompositorStateData>,
    window: Window,
    initial_location: Point<i32, Logical>,
    kind: WindowGrabKind,
}

struct CompositorStateData {
    display_handle: DisplayHandle,
    compositor_state: CompositorState,
    layer_shell_state: WlrLayerShellState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    shm_state: ShmState,
    data_device_state: DataDeviceState,
    primary_selection_state: PrimarySelectionState,
    xdg_activation_state: XdgActivationState,
    recent_input_serials: Vec<(Serial, Instant, ClientId)>,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    keyboard: Option<KeyboardHandle<Self>>,
    pointer: Option<PointerHandle<Self>>,
    pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    logo_modifier_pressed: bool,
    pointer_cursor_status: CursorImageStatus,
    output: Output,
    output_manager_state: OutputManagerState,
    space: Space<Window>,
    windows: Vec<ManagedWindow>,
    popups: PopupManager,
    resize_session: Option<ResizeSession>,
    next_window_position: u32,
    next_minimized_order: u64,
    next_stacking_order: u64,
    active_workspace: u32,
    workspace_count: u32,
    suppressed_keys: Vec<u32>,
    layout_mode: LayoutMode,
    theme_runtime: Option<ThemeRuntime>,
    theme_generation: u64,
    active_theme: Option<Arc<ThemeDocument>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMode {
    Floating,
    MasterStack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompositorShortcut {
    Minimize,
    Restore,
    Close,
    WorkspacePrevious,
    WorkspaceNext,
    ToggleLayout,
    ToggleWindowLayout,
}

fn compositor_shortcut(modifiers: ModifiersState, keysym: Keysym) -> Option<CompositorShortcut> {
    // Keep restore off the host's Super-based global shortcuts (XFCE uses
    // Super+R for its application finder in common configurations).
    if modifiers.ctrl && modifiers.alt && matches!(keysym, Keysym::r | Keysym::R) {
        return Some(CompositorShortcut::Restore);
    }

    if !modifiers.logo {
        return None;
    }

    match keysym {
        Keysym::h => Some(CompositorShortcut::Minimize),
        Keysym::q => Some(CompositorShortcut::Close),
        Keysym::Left => Some(CompositorShortcut::WorkspacePrevious),
        Keysym::Right => Some(CompositorShortcut::WorkspaceNext),
        Keysym::t | Keysym::T => Some(CompositorShortcut::ToggleLayout),
        Keysym::f | Keysym::F => Some(CompositorShortcut::ToggleWindowLayout),
        _ => None,
    }
}

fn suppress_shortcut_press(suppressed: &mut Vec<u32>, key_code: u32) -> bool {
    if suppressed.contains(&key_code) {
        return false;
    }
    suppressed.push(key_code);
    true
}

fn consume_shortcut_release(suppressed: &mut Vec<u32>, key_code: u32) -> bool {
    if !suppressed.contains(&key_code) {
        return false;
    }
    suppressed.retain(|key| *key != key_code);
    true
}

impl CompositorStateData {
    fn poll_bubblecolor(&mut self) {
        loop {
            let update = self
                .theme_runtime
                .as_ref()
                .and_then(ThemeRuntime::try_take_update);
            let Some(update) = update else {
                break;
            };

            if !is_current_theme_generation(update.generation, self.theme_generation) {
                tracing::debug!(
                    received_generation = update.generation,
                    current_generation = self.theme_generation,
                    "Ignoring stale BubbleColor result"
                );
                continue;
            }

            match update.result {
                Ok(theme) => {
                    tracing::info!(
                        generation = update.generation,
                        schema_version = theme.schema_version,
                        source = %theme.source_file,
                        sampled_pixels = theme.diagnostics.sampled_pixels,
                        "BubbleColor palette snapshot accepted"
                    );
                    self.active_theme = Some(Arc::new(theme));
                }
                Err(error) => {
                    tracing::warn!(
                        generation = update.generation,
                        %error,
                        "BubbleColor palette generation failed; keeping previous snapshot"
                    );
                }
            }
        }
    }

    fn remember_input_serial(&mut self, serial: Serial, surface: Option<WlSurface>) {
        let Some(surface) = surface else {
            return;
        };
        let Some(client_id) = self
            .display_handle
            .get_client(surface.id())
            .ok()
            .map(|client| client.id())
        else {
            return;
        };
        let now = Instant::now();
        self.recent_input_serials
            .retain(|(_, timestamp, _)| now.duration_since(*timestamp) <= Duration::from_secs(5));
        self.recent_input_serials.push((serial, now, client_id));
        if self.recent_input_serials.len() > 64 {
            self.recent_input_serials.remove(0);
        }
    }

    fn activation_token_is_valid(&self, data: &XdgActivationTokenData) -> bool {
        let Some((serial, seat)) = data.serial.as_ref() else {
            return false;
        };
        let Some(source) = data.surface.as_ref() else {
            return false;
        };
        let Some(requesting_client) = data.client_id.as_ref() else {
            return false;
        };
        let source_client = self
            .display_handle
            .get_client(source.id())
            .ok()
            .map(|client| client.id());
        if data.timestamp.elapsed() > Duration::from_secs(5)
            || !self.seat.owns(seat)
            || source_client.as_ref() != Some(requesting_client)
        {
            return false;
        }
        self.recent_input_serials
            .iter()
            .any(|(known, timestamp, client_id)| {
                known == serial
                    && client_id == requesting_client
                    && timestamp.elapsed() <= Duration::from_secs(5)
            })
    }

    fn layer_under_pointer(
        &self,
        location: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(DesktopLayerSurface, WlSurface, Point<f64, Logical>)> {
        let output_geometry = self.space.output_geometry(&self.output)?;
        let output_location = location - output_geometry.loc.to_f64();
        let map = layer_map_for_output(&self.output);

        for layer in layers {
            let Some(layer_surface) = map.layer_under(*layer, output_location) else {
                continue;
            };
            let Some(layer_geometry) = map.layer_geometry(layer_surface) else {
                continue;
            };
            let layer_origin = layer_geometry.loc - layer_surface.bbox().loc;
            let local_location = output_location - layer_origin.to_f64();
            let Some((surface, surface_location)) =
                layer_surface.surface_under(local_location, WindowSurfaceType::ALL)
            else {
                continue;
            };

            let absolute_location =
                output_geometry.loc.to_f64() + layer_origin.to_f64() + surface_location.to_f64();
            return Some((layer_surface.clone(), surface, absolute_location));
        }

        None
    }

    fn pointer_focus(
        &self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        if let Some((_, surface, surface_location)) =
            self.layer_under_pointer(location, &[Layer::Overlay, Layer::Top])
        {
            return Some((surface, surface_location));
        }

        if let Some((window, window_location)) = self.space.element_under(location) {
            let local_location = location - window_location.to_f64();
            if let Some((surface, surface_location)) =
                window.surface_under(local_location, WindowSurfaceType::ALL)
            {
                return Some((
                    surface,
                    window_location.to_f64() + surface_location.to_f64(),
                ));
            }
        }

        self.layer_under_pointer(location, &[Layer::Bottom, Layer::Background])
            .map(|(_, surface, surface_location)| (surface, surface_location))
    }

    fn exclusive_layer_focus(&self) -> Option<WlSurface> {
        let map = layer_map_for_output(&self.output);
        map.layers_on(Layer::Overlay)
            .rev()
            .chain(map.layers_on(Layer::Top).rev())
            .find(|layer| {
                layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && with_renderer_surface_state(layer.wl_surface(), |state| {
                        state.buffer().is_some()
                    })
                    .unwrap_or(false)
            })
            .map(|layer| layer.wl_surface().clone())
    }

    fn set_keyboard_focus(&mut self, requested: Option<WlSurface>, serial: Serial) {
        let focus = self.exclusive_layer_focus().or(requested);
        if let Some(keyboard) = self.keyboard.clone() {
            keyboard.set_focus(self, focus, serial);
        }
    }

    fn refresh_exclusive_layer_focus(&mut self) {
        let Some(keyboard) = self.keyboard.clone() else {
            return;
        };
        if let Some(surface) = self.exclusive_layer_focus() {
            self.set_keyboard_focus(Some(surface), SERIAL_COUNTER.next_serial());
            return;
        }

        let focused_layer = keyboard.current_focus().and_then(|focused| {
            let map = layer_map_for_output(&self.output);
            map.layer_for_surface(&focused, WindowSurfaceType::ALL)
                .map(|layer| {
                    let interactivity = layer.cached_state().keyboard_interactivity;
                    let has_buffer = with_renderer_surface_state(layer.wl_surface(), |state| {
                        state.buffer().is_some()
                    })
                    .unwrap_or(false);
                    (interactivity, has_buffer)
                })
        });

        if focused_layer.is_some_and(|(interactivity, has_buffer)| {
            should_fallback_from_layer_focus(interactivity, has_buffer)
        }) {
            self.focus_topmost_window();
        }
    }

    fn reconfigure_maximized_windows(&mut self) {
        let windows = self
            .windows
            .iter()
            .map(|entry| entry.window.clone())
            .collect::<Vec<_>>();
        let maximized = windows
            .iter()
            .filter_map(|window| {
                window.toplevel().filter(|surface| {
                    surface
                        .current_state()
                        .states
                        .contains(xdg_toplevel::State::Maximized)
                })
            })
            .collect::<Vec<_>>();

        for surface in maximized {
            self.maximize_window(&surface);
        }
    }

    fn handle_layer_commit(&mut self, surface: &WlSurface) {
        let layer = {
            let mut map = layer_map_for_output(&self.output);
            let layer = map
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .cloned();
            if layer.is_some() {
                map.arrange();
            }
            layer
        };

        if let Some(layer) = layer {
            // Send the initial configure after the client's first commit;
            // arrange() handles subsequent size changes.
            layer.layer_surface().send_pending_configure();
            self.reconfigure_maximized_windows();
            self.arrange_tiled_windows();
            self.refresh_exclusive_layer_focus();
            self.space.refresh();
        }
    }

    fn work_area(&self) -> Option<Rectangle<i32, Logical>> {
        let output_geometry = self.space.output_geometry(&self.output)?;
        let zone = layer_map_for_output(&self.output).non_exclusive_zone();
        Some(Rectangle::new(output_geometry.loc + zone.loc, zone.size))
    }

    fn arrange_tiled_windows(&mut self) {
        self.arrange_tiled_windows_excluding(None);
    }

    fn arrange_tiled_windows_excluding(&mut self, excluded: Option<&Window>) {
        if self.layout_mode != LayoutMode::MasterStack {
            return;
        }
        let Some(area) = self.work_area() else {
            return;
        };

        let mut tiled = self
            .windows
            .iter()
            .filter(|entry| {
                !entry.minimized
                    && entry.workspace == self.active_workspace
                    && entry.parent_surface.is_none()
                    // A window can be temporarily absent from Space's location
                    // lookup while a configure/map transition is in flight.
                    // mapped_once is the compositor's stable eligibility marker;
                    // using the transient Space lookup here could silently drop
                    // one of three visible windows from the tiling calculation.
                    && entry.mapped_once
                    // A previously mapped toplevel may currently be unmapped
                    // after committing a null buffer. Do not let that stale
                    // handle consume a tile or get remapped by set_window_location.
                    && entry.window.toplevel().is_some_and(|surface| {
                        with_renderer_surface_state(surface.wl_surface(), |state| {
                            state.buffer().is_some()
                        })
                        .unwrap_or(false)
                    })
                    && excluded != Some(&entry.window)
                    && entry.layout_override != WindowLayoutOverride::Floating
            })
            .map(|entry| (entry.tiling_order, entry.window.clone()))
            .collect::<Vec<_>>();
        tiled.sort_by_key(|(order, _)| std::cmp::Reverse(*order));

        for (_, window) in &tiled {
            if let Some(entry) = self
                .windows
                .iter_mut()
                .find(|entry| entry.window == *window)
            {
                if entry.tiling_restore.is_none() {
                    entry.tiling_restore = Some(RestoreGeometry {
                        location: entry.location,
                        size: Some(window.geometry().size),
                    });
                }
            }
        }

        let rectangles = master_stack(
            LayoutRect::new(area.loc.x, area.loc.y, area.size.w, area.size.h),
            tiled.len(),
            MasterStackConfig::default(),
        );

        for ((_, window), rectangle) in tiled.into_iter().zip(rectangles) {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let client_height = if Self::uses_server_decorations(&window) {
                (rectangle.height - TITLEBAR_HEIGHT).max(1)
            } else {
                rectangle.height.max(1)
            };
            let requested_size = (rectangle.width.max(1), client_height).into();
            if toplevel.current_state().size != Some(requested_size) {
                toplevel.with_pending_state(|state| {
                    state.size = Some(requested_size);
                });
                toplevel.send_pending_configure();
            }
            self.set_window_location(&window, (rectangle.x, rectangle.y).into(), false);
        }
    }

    fn toggle_focused_window_layout(&mut self) {
        if self.layout_mode != LayoutMode::MasterStack {
            return;
        }

        // Resolve the actual keyboard-focused toplevel. Space element order is
        // stacking order, which can differ from keyboard focus after floating,
        // workspace changes, or client-side activation requests.
        let Some(focused_surface) = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus())
        else {
            return;
        };
        let Some(window) = self.managed_window_for_surface(&focused_surface) else {
            return;
        };
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if toplevel
            .current_state()
            .states
            .contains(xdg_toplevel::State::Maximized)
        {
            return;
        }

        let Some(index) = self.windows.iter().position(|entry| {
            entry.window == window
                && entry.workspace == self.active_workspace
                && !entry.minimized
                && entry.parent_surface.is_none()
        }) else {
            return;
        };

        let currently_tiled = self.windows[index].tiling_restore.is_some()
            && self.windows[index].layout_override != WindowLayoutOverride::Floating;

        if currently_tiled {
            let restore = self.windows[index].tiling_restore.take();
            self.windows[index].layout_override = WindowLayoutOverride::Floating;
            if let Some(restore) = restore {
                toplevel.with_pending_state(|state| {
                    state.size = restore.size;
                });
                toplevel.send_pending_configure();
                self.set_window_location(&window, restore.location, false);
            }
            info!("Focused window switched to floating mode");
        } else {
            self.windows[index].layout_override = WindowLayoutOverride::Tiled;
            // The arranger captures the floating geometry exactly once before
            // applying the new tiled rectangle.
            info!("Focused window switched to tiled mode");
        }

        // A window detached from the tiling set must also become visible
        // above the remaining tiled windows. Re-raise its complete transient
        // family after changing the override; this updates z-order only and
        // leaves the stable tiling_order untouched.
        self.raise_window_family(&window);
        // Reflow the remaining tiled windows immediately and apply the newly
        // tiled window's geometry without waiting for another compositor event.
        self.arrange_tiled_windows();
        self.space.refresh();
    }

    fn toggle_layout_mode(&mut self) {
        match self.layout_mode {
            LayoutMode::Floating => {
                // A maximized xdg-toplevel is a different client state from a
                // tiled window. Clear that state before handing the window to
                // the tiler; otherwise it is permanently excluded from
                // arrange_tiled_windows() and repeated Super+T transitions can
                // leave windows stacked as full-screen maximized surfaces.
                self.layout_mode = LayoutMode::MasterStack;
                // Preserve the geometry saved when a client was maximized.
                // Waiting for an asynchronous xdg configure acknowledgement here
                // would let the still-fullscreen client geometry be captured as
                // its tiling restore size.
                let maximized = self
                    .windows
                    .iter()
                    .filter(|entry| {
                        !entry.minimized
                            && entry.workspace == self.active_workspace
                            && entry.parent_surface.is_none()
                    })
                    .filter_map(|entry| {
                        entry.window.toplevel().and_then(|surface| {
                            surface
                                .current_state()
                                .states
                                .contains(xdg_toplevel::State::Maximized)
                                .then(|| {
                                    let restore =
                                        entry.restore_geometry.unwrap_or(RestoreGeometry {
                                            location: entry.location,
                                            size: Some(entry.window.geometry().size),
                                        });
                                    (surface.clone(), entry.window.clone(), restore)
                                })
                        })
                    })
                    .collect::<Vec<_>>();

                for (surface, window, restore) in maximized {
                    if let Some(entry) =
                        self.windows.iter_mut().find(|entry| entry.window == window)
                    {
                        entry.restore_geometry = None;
                        if entry.tiling_restore.is_none() {
                            entry.tiling_restore = Some(restore);
                        }
                    }
                    surface.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Maximized);
                        state.size = restore.size;
                    });
                    surface.send_pending_configure();
                    self.set_window_location(&window, restore.location, false);
                }

                // Super+T is a global layout transition: windows previously
                // detached with Super+F must rejoin the tiling set.
                for entry in self.windows.iter_mut().filter(|entry| {
                    !entry.minimized
                        && entry.workspace == self.active_workspace
                        && entry.parent_surface.is_none()
                }) {
                    entry.layout_override = WindowLayoutOverride::Tiled;
                }

                // Do not wait for clients to acknowledge the unmaximize
                // configure before arranging: that acknowledgement is
                // asynchronous, while arrange_tiled_windows() must include
                // every eligible window immediately.
                self.arrange_tiled_windows();
                info!("Master/Stack layout enabled");
            }
            LayoutMode::MasterStack => {
                self.layout_mode = LayoutMode::Floating;
                let restores = self
                    .windows
                    .iter_mut()
                    .filter_map(|entry| {
                        entry
                            .tiling_restore
                            .take()
                            .map(|restore| (entry.window.clone(), restore))
                    })
                    .collect::<Vec<_>>();
                for (window, restore) in restores {
                    // A layout transition must never preserve a compositor
                    // maximized state. Clear it first, then restore the exact
                    // pre-tiling geometry.
                    if let Some(toplevel) = window.toplevel() {
                        let maximized = toplevel
                            .current_state()
                            .states
                            .contains(xdg_toplevel::State::Maximized);
                        if maximized {
                            toplevel.with_pending_state(|state| {
                                state.states.unset(xdg_toplevel::State::Maximized);
                            });
                        }
                        toplevel.with_pending_state(|state| {
                            state.size = restore.size;
                        });
                        toplevel.send_pending_configure();
                    }
                    self.set_window_location(&window, restore.location, false);
                }
                info!("Floating layout enabled");
            }
        }
        self.space.refresh();
    }

    fn configure_toplevels(&self) {
        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
        }
    }

    fn managed_window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.windows
            .iter()
            .find(|entry| {
                entry
                    .window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .map(|entry| entry.window.clone())
    }

    fn uses_server_decorations(window: &Window) -> bool {
        window.toplevel().is_some_and(|surface| {
            surface.current_state().decoration_mode == Some(DecorationMode::ServerSide)
        })
    }

    fn content_location(&self, window: &Window, outer: Point<i32, Logical>) -> Point<i32, Logical> {
        if Self::uses_server_decorations(window) {
            outer + Size::from((0, TITLEBAR_HEIGHT))
        } else {
            outer
        }
    }

    fn update_transient_parent(&mut self, surface: &ToplevelSurface) {
        let root = surface.wl_surface().clone();
        let parent = surface.parent();
        let parent_info = parent.as_ref().and_then(|parent| {
            self.windows
                .iter()
                .find(|entry| {
                    entry
                        .window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.wl_surface() == parent)
                })
                .map(|entry| (entry.location, entry.workspace))
        });
        info!(
            "Transient parent update parent_present={} parent_managed={}",
            parent.is_some(),
            parent_info.is_some()
        );

        let Some(index) = self.windows.iter().position(|entry| {
            entry
                .window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == &root)
        }) else {
            return;
        };

        self.windows[index].parent_surface = parent;
        let Some((parent_location, parent_workspace)) = parent_info else {
            info!("Transient parent not managed yet; deferring child placement");
            return;
        };

        self.windows[index].workspace = parent_workspace;
        let location = parent_location + Size::from((WINDOW_CASCADE_STEP, WINDOW_CASCADE_STEP));
        self.windows[index].location = location;
        let window = self.windows[index].window.clone();
        if parent_workspace != self.active_workspace {
            self.space.unmap_elem(&window);
        } else if !self.windows[index].minimized {
            let has_buffer = window.toplevel().is_some_and(|toplevel| {
                with_renderer_surface_state(toplevel.wl_surface(), |state| state.buffer().is_some())
                    .unwrap_or(false)
            });
            if has_buffer {
                let content_location = self.content_location(&window, location);
                self.space.map_element(window, content_location, false);

            }
        }
    }

    fn raise_window_family(&mut self, window: &Window) {
        let Some(root) = window
            .toplevel()
            .map(|surface| surface.wl_surface().clone())
        else {
            return;
        };

        let mut family = vec![root];
        let mut index = 0;
        while index < family.len() {
            let parent = family[index].clone();
            for entry in &self.windows {
                if entry.parent_surface.as_ref() == Some(&parent) {
                    if let Some(child) = entry.window.toplevel().map(|s| s.wl_surface().clone()) {
                        if !family.contains(&child) {
                            family.push(child);
                        }
                    }
                }
            }
            index += 1;
        }

        let mut family_windows = self
            .windows
            .iter()
            .filter(|entry| {
                !entry.minimized
                    && entry.workspace == self.active_workspace
                    && entry
                        .window
                        .toplevel()
                        .is_some_and(|surface| family.contains(surface.wl_surface()))
            })
            .map(|entry| {
                let surface = entry.window.toplevel()?.wl_surface();
                let family_position = family.iter().position(|member| member == surface)?;
                Some((family_position, entry.window.clone()))
            })
            .flatten()
            .collect::<Vec<_>>();
        family_windows.sort_by_key(|(position, _)| *position);

        // Always raise a transient family from its root to its deepest
        // descendants. Smithay's Space order is back-to-front, so this
        // guarantees that children remain above their parent even when the
        // parent itself receives focus.
        let target_is_root = window
            .toplevel()
            .is_some_and(|surface| surface.wl_surface() == &family[0]);
        if !target_is_root
            && let Some(position) = family_windows
                .iter()
                .position(|(_, member)| member == window)
        {
            let target = family_windows.remove(position);
            family_windows.push(target);
        }

        // Reflow tiled windows before changing z-order. Mapping/configuring
        // their new rectangles can affect Space ordering, so the focused
        // window family must be raised only after the layout pass is complete.
        self.arrange_tiled_windows();

        for (_, member) in family_windows {
            if let Some(entry) = self.windows.iter_mut().find(|entry| entry.window == member) {
                entry.stacking_order = self.next_stacking_order;
                self.next_stacking_order = self.next_stacking_order.wrapping_add(1);
            }
            self.space.raise_element(&member, member == *window);
        }

        let managed_order = self
            .windows
            .iter()
            .map(|entry| {
                let title = entry
                    .window
                    .toplevel()
                    .and_then(|toplevel| toplevel_title(&toplevel))
                    .unwrap_or_else(|| "<untitled>".into());
                let parent = entry.parent_surface.is_some();
                format!("{title}:parent={parent}:stack={}", entry.stacking_order)
            })
            .collect::<Vec<_>>();
        let space_order = self
            .space
            .elements()
            .map(|member| {
                member
                    .toplevel()
                    .and_then(|toplevel| toplevel_title(&toplevel))
                    .unwrap_or_else(|| "<untitled>".into())
            })
            .collect::<Vec<_>>();
        info!(
            target = ?window.toplevel().and_then(|toplevel| toplevel_title(&toplevel)),
            managed_order = ?managed_order,
            space_back_to_front = ?space_order,
            "Transient stacking diagnostic"
        );
        self.configure_toplevels();
    }

    fn window_family_surfaces(&self, root: &WlSurface) -> Vec<WlSurface> {
        let mut family = vec![root.clone()];
        let mut index = 0;
        while index < family.len() {
            let parent = family[index].clone();
            for entry in &self.windows {
                if entry.parent_surface.as_ref() == Some(&parent) {
                    if let Some(child) = entry.window.toplevel().map(|s| s.wl_surface().clone()) {
                        if !family.contains(&child) {
                            family.push(child);
                        }
                    }
                }
            }
            index += 1;
        }
        family
    }

    fn root_surface_for_toplevel(&self, surface: &WlSurface) -> WlSurface {
        let mut root = surface.clone();
        loop {
            let parent = self
                .windows
                .iter()
                .find(|entry| {
                    entry
                        .window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                })
                .and_then(|entry| entry.parent_surface.clone());
            let Some(parent) = parent else {
                break;
            };
            root = parent;
        }
        root
    }

    fn minimize_window(&mut self, surface: &ToplevelSurface) {
        let root = surface.wl_surface().clone();
        let family = self.window_family_surfaces(&root);
        let order = self.next_minimized_order;
        self.next_minimized_order = self.next_minimized_order.wrapping_add(1);

        let mut removed_any = false;
        for entry in &mut self.windows {
            let belongs_to_family = entry
                .window
                .toplevel()
                .is_some_and(|toplevel| family.contains(toplevel.wl_surface()));
            if !belongs_to_family || entry.minimized {
                continue;
            }

            if self.space.element_location(&entry.window).is_some() {
                self.space.unmap_elem(&entry.window);
                removed_any = true;
            }
            entry.minimized = true;
            entry.minimized_order = Some(order);
        }

        // A minimized window must not keep a compositor-owned move/resize
        // grab alive. Otherwise restoring it before the next pointer event
        // could reactivate the stale interaction.
        let has_window_grab = self
            .pointer
            .as_ref()
            .and_then(|pointer| {
                pointer.with_grab(|_, grab| {
                    grab.downcast_ref::<WindowPointerGrab>()
                        .is_some_and(|grab| {
                            grab.window
                                .toplevel()
                                .is_some_and(|toplevel| family.contains(toplevel.wl_surface()))
                        })
                })
            })
            .unwrap_or(false);
        if has_window_grab {
            if let Some(pointer) = self.pointer.clone() {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), 0);
            }
        }

        if removed_any {
            self.focus_topmost_window();
            self.arrange_tiled_windows();
        }
    }

    fn minimize_focused_window(&mut self) {
        let target = self.space.elements().last().cloned();
        if let Some(window) = target {
            if let Some(toplevel) = window.toplevel() {
                self.minimize_window(&toplevel);
            }
        }
    }

    fn restore_last_minimized_window(&mut self) {
        let target = self
            .windows
            .iter()
            .filter(|entry| entry.minimized)
            .max_by_key(|entry| entry.minimized_order)
            .and_then(|entry| {
                entry
                    .window
                    .toplevel()
                    .map(|surface| (surface.wl_surface().clone(), entry.workspace))
            });

        let Some((target, workspace)) = target else {
            return;
        };
        if workspace != self.active_workspace && self.activate_workspace(workspace).is_err() {
            return;
        }
        let root = self.root_surface_for_toplevel(&target);
        let family = self.window_family_surfaces(&root);
        let mut restored = Vec::new();

        for entry in &mut self.windows {
            let belongs_to_family = entry
                .window
                .toplevel()
                .is_some_and(|toplevel| family.contains(toplevel.wl_surface()));
            if !belongs_to_family || !entry.minimized {
                continue;
            }
            entry.minimized = false;
            entry.minimized_order = None;
            restored.push((entry.window.clone(), entry.location));
        }

        for (window, location) in &restored {
            let content_location = self.content_location(window, *location);
            self.space
                .map_element(window.clone(), content_location, false);
        }

        self.arrange_tiled_windows();
        // Restore and focus the originally minimized toplevel, not the
        // family root. When restoring a transient child, raising the root
        // would put its parent above the child and hide the restored dialog.
        if let Some(window) = self.managed_window_for_surface(&target) {
            self.raise_window_family(&window);
            if let Some(toplevel) = window.toplevel() {
                self.set_keyboard_focus(
                    Some(toplevel.wl_surface().clone()),
                    SERIAL_COUNTER.next_serial(),
                );
            }
        }
    }

    fn activate_workspace(&mut self, index: u32) -> Result<(), Response> {
        if index >= self.workspace_count {
            return Err(Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::InvalidRequest,
                    message: format!(
                        "workspace index {index} is out of range (valid range: 0..{})",
                        self.workspace_count
                    ),
                },
            });
        }

        if index == self.active_workspace {
            return Ok(());
        }

        let windows = self
            .windows
            .iter()
            .map(|entry| (entry.window.clone(), entry.workspace, entry.minimized))
            .collect::<Vec<_>>();
        for (window, workspace, _) in &windows {
            if *workspace != index && self.space.element_location(window).is_some() {
                self.space.unmap_elem(window);
            }
        }

        self.active_workspace = index;

        let mut visible = self
            .windows
            .iter()
            .filter(|entry| entry.workspace == index && !entry.minimized)
            .filter_map(|entry| {
                let has_buffer = entry.window.toplevel().is_some_and(|toplevel| {
                    with_renderer_surface_state(toplevel.wl_surface(), |state| {
                        state.buffer().is_some()
                    })
                    .unwrap_or(false)
                });
                has_buffer.then_some((entry.stacking_order, entry.window.clone(), entry.location))
            })
            .collect::<Vec<_>>();
        visible.sort_by_key(|(order, _, _)| *order);

        for (_, window, location) in visible {
            let content_location = self.content_location(&window, location);
            self.space.map_element(window, content_location, false);
        }

        // Rebuild transient stacking after all workspace members have been
        // mapped. Mapping in stacking_order alone is not sufficient: Space
        // may otherwise leave a transient child below its parent.
        let mut transient_roots = self
            .windows
            .iter()
            .filter(|entry| {
                entry.workspace == index
                    && !entry.minimized
                    && entry.parent_surface.is_none()
                    && entry.window.toplevel().is_some_and(|toplevel| {
                        with_renderer_surface_state(toplevel.wl_surface(), |state| {
                            state.buffer().is_some()
                        })
                        .unwrap_or(false)
                    })
            })
            .map(|entry| (entry.stacking_order, entry.window.clone()))
            .collect::<Vec<_>>();
        transient_roots.sort_by_key(|(order, _)| *order);
        for (_, root) in transient_roots {
            self.raise_window_family(&root);
        }

        self.arrange_tiled_windows();
        self.focus_topmost_window();
        self.refresh_exclusive_layer_focus();
        self.space.refresh();
        Ok(())
    }

    fn handle_ipc_request(&mut self, request: Request, session: &SessionManager) -> Response {
        match request {
            Request::GetCapabilities => Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: true,
                },
            },
            Request::GetWorkspaceState => Response::Success {
                data: ResponseData::WorkspaceState {
                    active: self.active_workspace,
                    count: self.workspace_count,
                },
            },
            Request::GetThemePalette => Response::Success {
                data: ResponseData::ThemePalette {
                    palette: self.active_theme.as_ref().map(|theme| ThemePalette {
                        schema_version: theme.schema_version,
                        generation: self.theme_generation,
                        primary: [
                            theme.colors.primary.r,
                            theme.colors.primary.g,
                            theme.colors.primary.b,
                        ],
                        primary_container: [
                            theme.colors.primary_container.r,
                            theme.colors.primary_container.g,
                            theme.colors.primary_container.b,
                        ],
                        on_primary_container: [
                            theme.colors.on_primary_container.r,
                            theme.colors.on_primary_container.g,
                            theme.colors.on_primary_container.b,
                        ],
                        surface: [
                            theme.colors.surface.r,
                            theme.colors.surface.g,
                            theme.colors.surface.b,
                        ],
                        on_surface: [
                            theme.colors.on_surface.r,
                            theme.colors.on_surface.g,
                            theme.colors.on_surface.b,
                        ],
                        surface_variant: [
                            theme.colors.surface_variant.r,
                            theme.colors.surface_variant.g,
                            theme.colors.surface_variant.b,
                        ],
                        outline: [
                            theme.colors.outline.r,
                            theme.colors.outline.g,
                            theme.colors.outline.b,
                        ],
                        warning: [
                            theme.colors.warning.r,
                            theme.colors.warning.g,
                            theme.colors.warning.b,
                        ],
                        success: [
                            theme.colors.success.r,
                            theme.colors.success.g,
                            theme.colors.success.b,
                        ],
                        error: [
                            theme.colors.error.r,
                            theme.colors.error.g,
                            theme.colors.error.b,
                        ],
                    }),
                },
            },
            Request::WorkspacePrevious => {
                let index = previous_workspace(self.active_workspace, self.workspace_count);
                match self.activate_workspace(index) {
                    Ok(()) => Response::Success {
                        data: ResponseData::Acknowledged,
                    },
                    Err(response) => response,
                }
            }
            Request::WorkspaceNext => {
                let index = next_workspace(self.active_workspace, self.workspace_count);
                match self.activate_workspace(index) {
                    Ok(()) => Response::Success {
                        data: ResponseData::Acknowledged,
                    },
                    Err(response) => response,
                }
            }
            Request::ActivateWorkspace { index } => match self.activate_workspace(index) {
                Ok(()) => Response::Success {
                    data: ResponseData::Acknowledged,
                },
                Err(response) => response,
            },
            Request::GetSessionState => Response::Success {
                data: ResponseData::SessionState {
                    state: session.state(),
                },
            },
            Request::GetSessionCapabilities => Response::Success {
                data: ResponseData::SessionCapabilities {
                    capabilities: session.capabilities(),
                },
            },
            Request::RequestSessionAction { action } => session.request_action(action),
        }
    }

    fn close_focused_window(&self) {
        let focused = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus());
        if let Some(surface) = focused
            && let Some(window) = self.managed_window_for_surface(&surface)
            && let Some(toplevel) = window.toplevel()
        {
            let title = toplevel_title(&toplevel).unwrap_or_else(|| "<untitled>".into());
            info!("Sending close request to focused toplevel title={title}");
            toplevel.send_close();
        } else {
            info!("Close shortcut ignored because no managed toplevel has keyboard focus");
        }
    }

    fn decoration_hit_test(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(Window, DecorationAction)> {
        for window in self.space.elements().rev() {
            let title = window
                .toplevel()
                .and_then(|surface| toplevel_title(&surface))
                .unwrap_or_else(|| "<untitled>".into());
            let decoration_mode = window
                .toplevel()
                .map(|surface| surface.current_state().decoration_mode);
            let Some(entry) = self.windows.iter().find(|entry| entry.window == *window) else {
                info!("Decoration candidate title={title} mode={decoration_mode:?} managed=false");
                continue;
            };
            info!(
                "Decoration candidate title={title} mode={decoration_mode:?} location=({}, {}) geometry={:?}",
                entry.location.x,
                entry.location.y,
                window.geometry().size
            );
            if decoration_mode != Some(Some(DecorationMode::ServerSide)) {
                continue;
            }
            let width = window.geometry().size.w;
            if width <= 0
                || location.x < entry.location.x as f64
                || location.x >= (entry.location.x + width) as f64
                || location.y < entry.location.y as f64
                || location.y >= (entry.location.y + TITLEBAR_HEIGHT) as f64
            {
                continue;
            }

            let close_x =
                entry.location.x + width - DECORATION_BUTTON_MARGIN - DECORATION_BUTTON_SIZE;
            let maximize_x = close_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let minimize_x = maximize_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let button_y = entry.location.y + (TITLEBAR_HEIGHT - DECORATION_BUTTON_SIZE) / 2;
            let in_button_y = location.y >= button_y as f64
                && location.y < (button_y + DECORATION_BUTTON_SIZE) as f64;

            let action = if in_button_y
                && location.x >= close_x as f64
                && location.x < (close_x + DECORATION_BUTTON_SIZE) as f64
            {
                DecorationAction::Close
            } else if in_button_y
                && location.x >= maximize_x as f64
                && location.x < (maximize_x + DECORATION_BUTTON_SIZE) as f64
            {
                DecorationAction::Maximize
            } else if in_button_y
                && location.x >= minimize_x as f64
                && location.x < (minimize_x + DECORATION_BUTTON_SIZE) as f64
            {
                DecorationAction::Minimize
            } else {
                DecorationAction::Move
            };

            return Some((window.clone(), action));
        }
        None
    }

    fn decoration_render_elements(&self) -> Vec<SolidColorRenderElement> {
        let mut elements = Vec::new();
        let scale = self.output.current_scale().fractional_scale();
        let focused_surface = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus());

        for window in self.space.elements() {
            if !Self::uses_server_decorations(window) {
                continue;
            }
            let Some(entry) = self.windows.iter().find(|entry| entry.window == *window) else {
                continue;
            };
            let width = window.geometry().size.w;
            if width <= 0 {
                continue;
            }
            let focused = focused_surface.as_ref().is_some_and(|focused| {
                window
                    .toplevel()
                    .is_some_and(|surface| surface.wl_surface() == focused)
            });

            let palette = self.active_theme.as_deref();
            let titlebar_color = palette
                .map(|theme| {
                    color32_from_rgb(if focused {
                        theme.colors.primary_container
                    } else {
                        theme.colors.surface
                    })
                })
                .unwrap_or_else(|| {
                    if focused {
                        Color32F::new(0.16, 0.20, 0.29, 1.0)
                    } else {
                        Color32F::new(0.10, 0.12, 0.16, 1.0)
                    }
                });
            let title_color = palette
                .map(|theme| {
                    color32_from_rgb(if focused {
                        theme.colors.on_primary_container
                    } else {
                        theme.colors.on_surface
                    })
                })
                .unwrap_or(Color32F::new(0.88, 0.91, 0.96, 1.0));
            append_solid_decoration(
                &mut elements,
                entry.location,
                (width, TITLEBAR_HEIGHT).into(),
                titlebar_color,
                scale,
            );
            if let Some(title) = window
                .toplevel()
                .and_then(|surface| toplevel_title(&surface))
                .or_else(|| {
                    window
                        .toplevel()
                        .and_then(|surface| toplevel_app_id(&surface))
                })
            {
                append_decoration_title(
                    &mut elements,
                    &title,
                    entry.location,
                    width,
                    title_color,
                    scale,
                );
            }
            if focused {
                append_solid_decoration(
                    &mut elements,
                    (entry.location.x, entry.location.y + TITLEBAR_HEIGHT - 2).into(),
                    (width, 2).into(),
                    palette
                        .map(|theme| color32_from_rgb(theme.colors.primary))
                        .unwrap_or(Color32F::new(0.25, 0.62, 0.95, 1.0)),
                    scale,
                );
            }

            let close_x =
                entry.location.x + width - DECORATION_BUTTON_MARGIN - DECORATION_BUTTON_SIZE;
            let maximize_x = close_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let minimize_x = maximize_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let button_y = entry.location.y + (TITLEBAR_HEIGHT - DECORATION_BUTTON_SIZE) / 2;

            append_solid_decoration(
                &mut elements,
                (minimize_x, button_y).into(),
                (DECORATION_BUTTON_SIZE, DECORATION_BUTTON_SIZE).into(),
                palette
                    .map(|theme| color32_from_rgb(theme.colors.warning))
                    .unwrap_or(Color32F::new(0.86, 0.68, 0.24, 1.0)),
                scale,
            );
            append_solid_decoration(
                &mut elements,
                (maximize_x, button_y).into(),
                (DECORATION_BUTTON_SIZE, DECORATION_BUTTON_SIZE).into(),
                palette
                    .map(|theme| color32_from_rgb(theme.colors.success))
                    .unwrap_or(Color32F::new(0.24, 0.68, 0.48, 1.0)),
                scale,
            );
            append_solid_decoration(
                &mut elements,
                (close_x, button_y).into(),
                (DECORATION_BUTTON_SIZE, DECORATION_BUTTON_SIZE).into(),
                palette
                    .map(|theme| color32_from_rgb(theme.colors.error))
                    .unwrap_or(Color32F::new(0.82, 0.28, 0.31, 1.0)),
                scale,
            );
        }

        elements
    }

    fn handle_decoration_action(&mut self, window: &Window, action: DecorationAction) {
        self.raise_window_family(window);

        let Some(surface) = window.toplevel() else {
            return;
        };
        match action {
            DecorationAction::Close => surface.send_close(),
            DecorationAction::Minimize => self.minimize_window(&surface),
            DecorationAction::Maximize => {
                if surface
                    .current_state()
                    .states
                    .contains(xdg_toplevel::State::Maximized)
                {
                    self.unmaximize_window(&surface);
                } else {
                    self.maximize_window(&surface);
                }
            }
            DecorationAction::Move => {}
        }
    }

    fn maximize_window(&mut self, surface: &ToplevelSurface) {
        let current = surface.current_state();
        if !current
            .capabilities
            .contains(xdg_toplevel::WmCapabilities::Maximize)
        {
            if surface.is_initial_configure_sent() {
                surface.send_configure();
            }
            return;
        }

        let Some(window) = self.managed_window_for_surface(surface.wl_surface()) else {
            if surface.is_initial_configure_sent() {
                surface.send_configure();
            }
            return;
        };
        let Some(bounds) = self.work_area() else {
            if surface.is_initial_configure_sent() {
                surface.send_configure();
            }
            return;
        };

        if !current.states.contains(xdg_toplevel::State::Maximized) {
            if let Some(entry) = self.windows.iter_mut().find(|entry| entry.window == window) {
                if entry.restore_geometry.is_none() {
                    let location = entry.location;
                    let size = self
                        .space
                        .element_location(&window)
                        .map(|_| window.geometry().size)
                        .or(current.size);
                    entry.restore_geometry = Some(RestoreGeometry { location, size });
                }
            }
        }

        let client_height = if Self::uses_server_decorations(&window) {
            (bounds.size.h - TITLEBAR_HEIGHT).max(1)
        } else {
            bounds.size.h
        };
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some((bounds.size.w, client_height).into());
        });
        self.set_window_location(&window, bounds.loc, true);

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
        self.arrange_tiled_windows_excluding(Some(&window));
    }

    fn unmaximize_window(&mut self, surface: &ToplevelSurface) {
        if !surface
            .current_state()
            .states
            .contains(xdg_toplevel::State::Maximized)
        {
            return;
        }

        let Some(window) = self.managed_window_for_surface(surface.wl_surface()) else {
            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Maximized);
                state.size = None;
            });
            surface.send_pending_configure();
            return;
        };

        let restore = self
            .windows
            .iter_mut()
            .find(|entry| entry.window == window)
            .and_then(|entry| entry.restore_geometry.take());

        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
            state.size = restore.and_then(|geometry| geometry.size);
        });

        if let Some(restore) = restore {
            self.set_window_location(&window, restore.location, true);
        }

        surface.send_pending_configure();
        self.arrange_tiled_windows();
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(mut target) = self.space.output_geometry(&self.output) else {
            return;
        };

        let parent_location = if let Some(window) = self.managed_window_for_surface(&root) {
            let Some(window_geometry) = self.space.element_geometry(&window) else {
                return;
            };
            window_geometry.loc
        } else {
            let Some(layer_location) = self.space.outputs().find_map(|output| {
                let map = layer_map_for_output(output);
                map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                    .and_then(|layer| map.layer_geometry(layer))
                    .and_then(|geometry| {
                        self.space
                            .output_geometry(output)
                            .map(|output_geometry| output_geometry.loc + geometry.loc)
                    })
            }) else {
                return;
            };
            layer_location
        };

        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= parent_location;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    fn set_window_location(
        &mut self,
        window: &Window,
        location: Point<i32, Logical>,
        activate: bool,
    ) {
        let visible = if let Some(entry) = self
            .windows
            .iter_mut()
            .find(|entry| entry.window == *window)
        {
            entry.location = location;
            !entry.minimized && entry.workspace == self.active_workspace
        } else {
            false
        };
        if visible {
            let content_location = self.content_location(window, location);
            self.space
                .map_element(window.clone(), content_location, activate);
        }
    }

    fn focus_topmost_window(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let exclusive_layer = self.exclusive_layer_focus();
        let target = self.space.elements().last().cloned();

        if exclusive_layer.is_none() {
            if let Some(window) = target.as_ref() {
                self.space.raise_element(window, true);
                self.configure_toplevels();
            }
        }

        let focus = exclusive_layer.or_else(|| {
            target.and_then(|window| {
                window
                    .toplevel()
                    .map(|toplevel| toplevel.wl_surface().clone())
            })
        });
        self.set_keyboard_focus(focus, serial);
    }

    fn handle_toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(index) = self.windows.iter().position(|entry| {
            entry
                .window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        }) else {
            return;
        };

        let window = self.windows[index].window.clone();
        let was_mapped = self.space.element_location(&window).is_some();
        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        info!(
            "Toplevel commit state workspace={} active_workspace={} minimized={} has_buffer={} mapped={}",
            self.windows[index].workspace,
            self.active_workspace,
            self.windows[index].minimized,
            has_buffer,
            self.space.element_location(&window).is_some()
        );

        if !has_buffer {
            if self.space.element_location(&window).is_some() {
                self.space.unmap_elem(&window);
                self.focus_topmost_window();
                // Reflow the remaining windows immediately. The unmapped
                // toplevel stays in the management list for a possible remap,
                // but must not retain a tile while it has no buffer.
                self.arrange_tiled_windows();
            }
            if self
                .resize_session
                .as_ref()
                .is_some_and(|session| session.surface == *surface)
            {
                self.resize_session = None;
            }
            return;
        }

        window.on_commit();

        if self.space.element_location(&window).is_none()
            && !self.windows[index].minimized
            && self.windows[index].workspace == self.active_workspace
        {
            let location = self.windows[index].location;
            let content_location = self.content_location(&window, location);
            let first_map = !self.windows[index].mapped_once;
            let activate = first_map || self.space.elements().next().is_none();
            self.windows[index].mapped_once = true;
            if activate {
                let order = self.next_stacking_order;
                self.windows[index].stacking_order = order;
                if first_map {
                    self.windows[index].tiling_order = order;
                }
                self.next_stacking_order = self.next_stacking_order.wrapping_add(1);
            }
            self.space
                .map_element(window.clone(), content_location, activate);
            if activate {
                if let Some(toplevel) = window.toplevel() {
                    self.set_keyboard_focus(
                        Some(toplevel.wl_surface().clone()),
                        SERIAL_COUNTER.next_serial(),
                    );
                }
            }
        } else if !self.windows[index].minimized
            && self.windows[index].workspace == self.active_workspace
        {
            // Decoration mode can change after the client acknowledges a
            // configure. Keep the client surface below/without the titlebar.
            let expected = self.content_location(&window, self.windows[index].location);
            if self.space.element_location(&window) != Some(expected) {
                self.space.map_element(window.clone(), expected, false);
            }
        }

        if let Some(session) = self
            .resize_session
            .clone()
            .filter(|session| session.surface == *surface)
        {
            let geometry = window.geometry();
            let mut location = session.initial_location;
            if session.edges & RESIZE_EDGE_LEFT != 0 {
                location.x =
                    session.initial_location.x + (session.initial_size.w - geometry.size.w);
            }
            if session.edges & RESIZE_EDGE_TOP != 0 {
                location.y =
                    session.initial_location.y + (session.initial_size.h - geometry.size.h);
            }
            self.set_window_location(&window, location, false);

            let resize_finished = session.released
                && window.toplevel().is_some_and(|toplevel| {
                    !toplevel
                        .current_state()
                        .states
                        .contains(xdg_toplevel::State::Resizing)
                });
            if resize_finished {
                self.resize_session = None;
            }
        }
        self.arrange_tiled_windows();

        // Mapping order is client-dependent: a parent may become visible
        // after its transient child and otherwise cover it on the first frame.
        // Rebuild the family stack exactly once for each newly mapped member.
        if !was_mapped {
            let is_transient = self.windows[index].parent_surface.is_some();
            let has_transient_children = self.windows.iter().any(|entry| {
                entry.parent_surface.as_ref() == Some(surface)
                    && !entry.minimized
                    && entry.workspace == self.active_workspace
                    && self.space.element_location(&entry.window).is_some()
            });
            if is_transient || has_transient_children {
                self.raise_window_family(&window);

                // If the parent maps after its transient child, the parent
                // mapping path may have taken keyboard focus. Keep the
                // topmost mapped descendant focused along with the family.
                if has_transient_children {
                    let mut family = vec![surface.clone()];
                    let mut position = 0;
                    while position < family.len() {
                        let parent = family[position].clone();
                        for entry in &self.windows {
                            if entry.parent_surface.as_ref() == Some(&parent)
                                && !entry.minimized
                                && entry.workspace == self.active_workspace
                                && self.space.element_location(&entry.window).is_some()
                                && let Some(child) = entry.window.toplevel()
                                    .map(|toplevel| toplevel.wl_surface().clone())
                                && !family.contains(&child)
                            {
                                family.push(child);
                            }
                        }
                        position += 1;
                    }

                    if let Some(topmost) = family.last()
                        && topmost != surface
                    {
                        self.set_keyboard_focus(
                            Some(topmost.clone()),
                            SERIAL_COUNTER.next_serial(),
                        );
                    }
                }
            }
        }
    }

    fn begin_move(&mut self, surface: &ToplevelSurface, seat: &Seat<Self>, serial: Serial) {
        let title = toplevel_title(surface).unwrap_or_else(|| "<untitled>".into());
        info!("XDG move request title={title} serial={serial:?}");

        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        if !pointer.has_grab(serial) {
            return;
        }

        let Some(start_data) = pointer.grab_start_data() else {
            return;
        };
        if !start_data
            .focus
            .as_ref()
            .is_some_and(|(focus, _)| focus.same_client_as(&surface.wl_surface().id()))
        {
            return;
        }

        let Some(window) = self.managed_window_for_surface(surface.wl_surface()) else {
            return;
        };
        let Some(initial_location) = self
            .windows
            .iter()
            .find(|entry| entry.window == window)
            .map(|entry| entry.location)
        else {
            return;
        };

        pointer.set_grab(
            self,
            WindowPointerGrab {
                start_data,
                window,
                initial_location,
                kind: WindowGrabKind::Move,
            },
            serial,
            Focus::Clear,
        );
    }

    fn begin_resize(
        &mut self,
        surface: &ToplevelSurface,
        seat: &Seat<Self>,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        if !pointer.has_grab(serial) {
            return;
        }

        let Some(start_data) = pointer.grab_start_data() else {
            return;
        };
        if !start_data
            .focus
            .as_ref()
            .is_some_and(|(focus, _)| focus.same_client_as(&surface.wl_surface().id()))
        {
            return;
        }

        let Some(window) = self.managed_window_for_surface(surface.wl_surface()) else {
            return;
        };
        let Some(initial_location) = self
            .windows
            .iter()
            .find(|entry| entry.window == window)
            .map(|entry| entry.location)
        else {
            return;
        };

        let initial_size = window.geometry().size;
        let edge_bits = edges as u32;
        if edge_bits == 0 {
            return;
        }

        self.resize_session = Some(ResizeSession {
            surface: surface.wl_surface().clone(),
            edges: edge_bits,
            initial_location,
            initial_size,
            released: false,
        });

        pointer.set_grab(
            self,
            WindowPointerGrab {
                start_data,
                window,
                initial_location,
                kind: WindowGrabKind::Resize {
                    edges: edge_bits,
                    initial_size,
                    last_size: initial_size,
                },
            },
            serial,
            Focus::Clear,
        );
    }

    fn update_output_mode(&mut self, size: smithay::utils::Size<i32, smithay::utils::Physical>) {
        if size.w <= 0 || size.h <= 0 {
            return;
        }

        let mode = Mode {
            size,
            refresh: OUTPUT_REFRESH_MILLIHZ,
        };
        self.output.set_preferred(mode);
        self.output
            .change_current_state(Some(mode), None, None, None);
        layer_map_for_output(&self.output).arrange();
        self.reconfigure_maximized_windows();
        self.arrange_tiled_windows();
        self.space.refresh();
        info!(width = size.w, height = size.h, "Winit output mode changed");
    }

    fn focus_window_under_pointer(&mut self, serial: Serial) {
        if let Some((layer, surface, _)) =
            self.layer_under_pointer(self.pointer_location, &[Layer::Overlay, Layer::Top])
        {
            if layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None {
                self.set_keyboard_focus(Some(surface), serial);
            }
            return;
        }

        let target = self
            .space
            .element_under(self.pointer_location)
            .map(|(window, _location)| window.clone());
        if let Some(window) = target {
            self.raise_window_family(&window);

            if let Some(toplevel) = window.toplevel() {
                self.set_keyboard_focus(Some(toplevel.wl_surface().clone()), serial);
            }
            return;
        }

        if let Some((layer, surface, _)) =
            self.layer_under_pointer(self.pointer_location, &[Layer::Bottom, Layer::Background])
            && layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None
        {
            self.set_keyboard_focus(Some(surface), serial);
        } else {
            // Clicking empty desktop space must not leave keyboard focus on
            // the previously focused toplevel. Exclusive layers are still
            // enforced centrally by set_keyboard_focus().
            self.set_keyboard_focus(None, serial);
        }
    }
}

impl BufferHandler for CompositorStateData {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for CompositorStateData {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<WaylandClientState>()
            .expect("BubbleWM client data is installed at connection time")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popups.commit(surface);

        // A subsurface commit can change the toplevel's bounding box too.
        // Walk to its root and refresh the owning Window's cached geometry.
        let mut root_surface = surface.clone();
        while let Some(parent) = get_parent(&root_surface) {
            root_surface = parent;
        }
        if let Some(window) = self.managed_window_for_surface(&root_surface) {
            window.on_commit();

            // The first xdg_toplevel commit is normally the client's initial
            // null-buffer commit. Send the initial configure only after
            // Smithay has processed that commit and initialized renderer state.
            if root_surface == *surface {
                if let Some(toplevel) = window.toplevel() {
                    if !toplevel.is_initial_configure_sent() {
                        toplevel.send_configure();
                    }
                }
            }
        }

        // PopupManager::commit promotes newly parented popups into the popup
        // tree. Their first configure must be sent after that transition.
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                if let Err(error) = popup.send_configure() {
                    info!(?error, "Unable to send initial xdg popup configure");
                }
            }
        }

        self.handle_layer_commit(&root_surface);
        self.popups.cleanup();
        self.space.refresh();
    }
}

impl XdgDecorationHandler for CompositorStateData {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

impl ShmHandler for CompositorStateData {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SelectionHandler for CompositorStateData {
    type SelectionUserData = ();
}

impl PrimarySelectionHandler for CompositorStateData {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl XdgActivationHandler for CompositorStateData {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        self.activation_token_is_valid(&data)
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Tokens are single-use in BubbleWM, including denied requests.
        self.xdg_activation_state.remove_token(&token);

        if !self.activation_token_is_valid(&token_data) {
            return;
        }

        let Some(window) = self.managed_window_for_surface(&surface) else {
            return;
        };
        let Some(entry) = self.windows.iter().find(|entry| entry.window == window) else {
            return;
        };
        if entry.minimized
            || entry.workspace != self.active_workspace
            || self.space.element_location(&window).is_none()
        {
            return;
        }

        self.raise_window_family(&window);
        self.set_keyboard_focus(Some(surface), SERIAL_COUNTER.next_serial());
    }
}

impl ClientDndGrabHandler for CompositorStateData {}

impl ServerDndGrabHandler for CompositorStateData {}

impl DataDeviceHandler for CompositorStateData {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl OutputHandler for CompositorStateData {}

impl XdgShellHandler for CompositorStateData {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("New xdg_toplevel surface");

        let window = Window::new_wayland_window(surface.clone());
        let cascade = self.next_window_position % WINDOW_CASCADE_COUNT;
        self.next_window_position = self.next_window_position.wrapping_add(1);
        let offset = cascade as i32 * WINDOW_CASCADE_STEP;
        let location = (64 + offset, 64 + offset).into();

        // Keep a stable handle and its last position even while the client
        // temporarily unmaps the surface with a null buffer.
        let stacking_order = self.next_stacking_order;
        self.next_stacking_order = self.next_stacking_order.wrapping_add(1);
        self.windows.push(ManagedWindow {
            window,
            location,
            mapped_once: false,
            restore_geometry: None,
            parent_surface: surface.parent(),
            minimized: false,
            minimized_order: None,
            workspace: self.active_workspace,
            tiling_order: stacking_order,
            stacking_order,
            tiling_restore: None,
            layout_override: WindowLayoutOverride::Automatic,
        });
        self.update_transient_parent(&surface);

        // The XDG role's first configure/commit cycle is managed by Smithay.
        // This post-commit hook maps the window only once it has a real buffer
        // and handles subsequent unmap/remap transitions.
        add_post_commit_hook(surface.wl_surface(), |state: &mut Self, _, committed| {
            state.handle_toplevel_commit(committed);
        });
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        self.begin_move(&surface, &seat, serial);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        self.begin_resize(&surface, &seat, serial, edges);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.maximize_window(&surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.unmaximize_window(&surface);
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        self.update_transient_parent(&surface);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.minimize_window(&surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let root = surface.wl_surface().clone();
        if let Some(index) = self.windows.iter().position(|entry| {
            entry
                .window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == &root)
        }) {
            let entry = self.windows.remove(index);
            self.space.unmap_elem(&entry.window);

            // A compositor-owned move/resize grab holds a clone of its window.
            // Cancel it immediately when that toplevel is destroyed instead
            // of waiting for the next pointer event to notice the dead window.
            let has_window_grab = self
                .pointer
                .as_ref()
                .and_then(|pointer| {
                    pointer.with_grab(|_, grab| {
                        grab.downcast_ref::<WindowPointerGrab>()
                            .is_some_and(|grab| grab.window == entry.window)
                    })
                })
                .unwrap_or(false);
            if has_window_grab {
                if let Some(pointer) = self.pointer.clone() {
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), 0);
                }
            }
        }

        if self
            .resize_session
            .as_ref()
            .is_some_and(|session| session.surface == root)
        {
            self.resize_session = None;
        }

        self.arrange_tiled_windows();
        self.focus_topmost_window();
        self.space.refresh();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            info!(?error, "Unable to track xdg popup");
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Some(root) = find_popup_root_surface(&kind).ok() else {
            return;
        };
        let mapped_window = self
            .managed_window_for_surface(&root)
            .is_some_and(|window| self.space.element_location(&window).is_some());
        let mapped_layer = self.space.outputs().any(|output| {
            layer_map_for_output(output)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        });
        if !mapped_window && !mapped_layer {
            return;
        }

        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else {
            return;
        };

        if let Some(keyboard) = seat.get_keyboard() {
            let previous_serial = grab.previous_serial().unwrap_or(serial);
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(previous_serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }

        if let Some(pointer) = seat.get_pointer() {
            let previous_serial = grab.previous_serial().unwrap_or_else(|| grab.serial());
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(previous_serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.popups.cleanup();
    }
}

impl WlrLayerShellHandler for CompositorStateData {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        requested_output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = requested_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());

        let Some(output) = output else {
            tracing::warn!(
                namespace,
                "Ignoring layer surface because no output is available"
            );
            return;
        };

        let layer = DesktopLayerSurface::new(surface, namespace);
        let mut map = layer_map_for_output(&output);
        if let Err(error) = map.map_layer(&layer) {
            tracing::warn!(?error, "Unable to map layer surface");
            return;
        }
        drop(map);

        self.refresh_exclusive_layer_focus();
        self.space.refresh();
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.unconstrain_popup(&popup);
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!(?error, "Unable to track layer-shell popup");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let focused = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focused| {
                let map = layer_map_for_output(&self.output);
                map.layer_for_surface(&focused, WindowSurfaceType::ALL)
                    .is_some_and(|layer| layer.layer_surface() == &surface)
            });

        {
            let mut map = layer_map_for_output(&self.output);
            let layer = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
            map.cleanup();
        }

        if focused {
            self.focus_topmost_window();
        }
        self.reconfigure_maximized_windows();
        // Reclaim or release the layer's exclusive zone immediately so tiled
        // windows do not keep geometry from the previous work area.
        self.arrange_tiled_windows();
        self.space.refresh();
    }
}

impl SeatHandler for CompositorStateData {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.pointer_cursor_status = image;
    }
}

fn toplevel_title(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|attributes| attributes.title.clone())
    })
}

fn toplevel_app_id(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|attributes| attributes.app_id.clone())
    })
}

fn append_decoration_title(
    elements: &mut Vec<SolidColorRenderElement>,
    title: &str,
    window_location: Point<i32, Logical>,
    window_width: i32,
    title_color: Color32F,
    scale: f64,
) {
    const GLYPH_WIDTH: i32 = 5;
    const GLYPH_HEIGHT: i32 = 7;
    const GLYPH_ADVANCE: i32 = 6;
    const TITLE_LEFT_PADDING: i32 = 12;
    const BUTTONS_RESERVED_WIDTH: i32 = 88;

    let available_width = (window_width - TITLE_LEFT_PADDING - BUTTONS_RESERVED_WIDTH).max(0);
    let max_chars = (available_width / GLYPH_ADVANCE) as usize;
    let mut cursor_x = window_location.x + TITLE_LEFT_PADDING;
    let cursor_y = window_location.y + (TITLEBAR_HEIGHT - GLYPH_HEIGHT) / 2;

    for character in title
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
    {
        let rows = decoration_glyph(character);
        for (row_index, bits) in rows.into_iter().enumerate() {
            let mut column = 0;
            while column < GLYPH_WIDTH {
                if bits & (1 << (GLYPH_WIDTH - 1 - column)) == 0 {
                    column += 1;
                    continue;
                }
                let start = column;
                while column < GLYPH_WIDTH && bits & (1 << (GLYPH_WIDTH - 1 - column)) != 0 {
                    column += 1;
                }
                append_solid_decoration(
                    elements,
                    (cursor_x + start, cursor_y + row_index as i32).into(),
                    (column - start, 1).into(),
                    title_color,
                    scale,
                );
            }
        }
        cursor_x += GLYPH_ADVANCE;
    }
}

fn decoration_glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 4, 4, 8],
        ':' => [0, 4, 4, 0, 4, 4, 0],
        ';' => [0, 4, 4, 0, 4, 4, 8],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '\\' => [16, 16, 8, 4, 2, 1, 1],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '=' => [0, 31, 0, 31, 0, 0, 0],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

fn rgb_channels(color: Rgb) -> [f32; 3] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
    ]
}

fn color32_from_rgb(color: Rgb) -> Color32F {
    let [red, green, blue] = rgb_channels(color);
    Color32F::new(red, green, blue, 1.0)
}

fn append_solid_decoration(
    elements: &mut Vec<SolidColorRenderElement>,
    location: Point<i32, Logical>,
    size: Size<i32, Logical>,
    color: Color32F,
    scale: f64,
) {
    if size.w <= 0 || size.h <= 0 {
        return;
    }
    let buffer = SolidColorBuffer::new(size, color);
    let physical_location = location.to_physical_precise_round(scale);
    elements.push(SolidColorRenderElement::from_buffer(
        &buffer,
        physical_location,
        scale,
        1.0,
        Kind::Unspecified,
    ));
}

fn should_fallback_from_layer_focus(
    interactivity: KeyboardInteractivity,
    has_buffer: bool,
) -> bool {
    interactivity != KeyboardInteractivity::OnDemand || !has_buffer
}

fn calculate_resize_size(
    initial_size: Size<i32, Logical>,
    delta: Point<f64, Logical>,
    edges: u32,
    min_size: Size<i32, Logical>,
    max_size: Size<i32, Logical>,
) -> Size<i32, Logical> {
    let mut width = initial_size.w as f64;
    let mut height = initial_size.h as f64;

    if edges & RESIZE_EDGE_LEFT != 0 {
        width -= delta.x;
    } else if edges & RESIZE_EDGE_RIGHT != 0 {
        width += delta.x;
    }
    if edges & RESIZE_EDGE_TOP != 0 {
        height -= delta.y;
    } else if edges & RESIZE_EDGE_BOTTOM != 0 {
        height += delta.y;
    }

    let min_width = min_size.w.max(1);
    let min_height = min_size.h.max(1);
    let max_width = if max_size.w <= 0 {
        i32::MAX
    } else {
        max_size.w.max(min_width)
    };
    let max_height = if max_size.h <= 0 {
        i32::MAX
    } else {
        max_size.h.max(min_height)
    };

    (
        (width.round() as i32).clamp(min_width, max_width),
        (height.round() as i32).clamp(min_height, max_height),
    )
        .into()
}

impl PointerGrab<CompositorStateData> for WindowPointerGrab {
    fn motion(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Keep clients from receiving pointer focus while the compositor owns
        // the interactive move/resize operation.
        handle.motion(data, None, event);

        if !self.window.alive() || data.space.element_location(&self.window).is_none() {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        }

        let delta = event.location - self.start_data.location;
        match &mut self.kind {
            WindowGrabKind::Move => {
                let location = self.initial_location.to_f64() + delta;
                data.set_window_location(&self.window, location.to_i32_round(), true);
            }
            WindowGrabKind::Resize {
                edges,
                initial_size,
                last_size,
            } => {
                let (min_size, max_size) = self
                    .window
                    .wl_surface()
                    .map(|surface| {
                        with_states(&surface, |states| {
                            let mut cached = states.cached_state.get::<SurfaceCachedState>();
                            let current = cached.current();
                            (current.min_size, current.max_size)
                        })
                    })
                    .unwrap_or(((0, 0).into(), (0, 0).into()));
                let next_size =
                    calculate_resize_size(*initial_size, delta, *edges, min_size, max_size);
                *last_size = next_size;

                if let Some(toplevel) = self.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.states.set(xdg_toplevel::State::Resizing);
                        state.size = Some(next_size);
                    });
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            if !self.window.alive() {
                handle.unset_grab(self, data, event.serial, event.time, true);
                return;
            }

            if let WindowGrabKind::Resize { last_size, .. } = &self.kind {
                let last_size = *last_size;
                if let Some(toplevel) = self.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Resizing);
                        state.size = Some(last_size);
                    });
                    toplevel.send_pending_configure();
                }
                if let Some(session) = data.resize_session.as_mut() {
                    session.released = true;
                }
            }
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
    ) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut CompositorStateData,
        handle: &mut PointerInnerHandle<'_, CompositorStateData>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<CompositorStateData> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut CompositorStateData) {}
}

struct WaylandClientState {
    compositor_state: CompositorClientState,
    connected: AtomicBool,
}

impl Default for WaylandClientState {
    fn default() -> Self {
        Self {
            compositor_state: CompositorClientState::default(),
            connected: AtomicBool::new(true),
        }
    }
}

impl ClientData for WaylandClientState {
    fn initialized(&self, _client_id: ClientId) {
        info!("Wayland client connected");
    }

    fn disconnected(&self, _client_id: ClientId, reason: DisconnectReason) {
        self.connected.store(false, Ordering::Release);
        info!(?reason, "Wayland client disconnected");
    }
}

delegate_compositor!(CompositorStateData);
delegate_output!(CompositorStateData);
delegate_seat!(CompositorStateData);
delegate_shm!(CompositorStateData);
delegate_data_device!(CompositorStateData);
delegate_primary_selection!(CompositorStateData);
delegate_xdg_activation!(CompositorStateData);
delegate_xdg_decoration!(CompositorStateData);
delegate_xdg_shell!(CompositorStateData);

delegate_layer_shell!(CompositorStateData);

/// Starts the initial Winit-backed Wayland compositor and owns IPC for its
/// complete lifetime.
pub fn run_winit(initial_layout: &str) -> Result<()> {
    run_winit_with_theme(initial_layout, None, ThemeOptions::default())
}

/// Starts the Winit compositor and optionally generates a wallpaper palette
/// on the dedicated BubbleColor worker. Palette polling stays on the
/// compositor thread and never waits for image decoding.
pub fn run_winit_with_theme(
    initial_layout: &str,
    wallpaper: Option<&str>,
    theme_options: ThemeOptions,
) -> Result<()> {
    let (mut backend, mut winit_loop) =
        winit::init::<smithay::backend::renderer::gles::GlesRenderer>()
            .map_err(|error| anyhow!("cannot initialize the Winit/EGL backend: {error}"))?;
    let initial_size = backend.window_size();

    let mut display: Display<CompositorStateData> =
        Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

    let output_manager_state =
        OutputManagerState::new_with_xdg_output::<CompositorStateData>(&display_handle);
    let compositor_state = CompositorState::new::<CompositorStateData>(&display_handle);
    let layer_shell_state = WlrLayerShellState::new::<CompositorStateData>(&display_handle);
    let shm_state = ShmState::new::<CompositorStateData>(&display_handle, vec![]);
    let data_device_state = DataDeviceState::new::<CompositorStateData>(&display_handle);
    let primary_selection_state =
        PrimarySelectionState::new::<CompositorStateData>(&display_handle);
    let xdg_activation_state = XdgActivationState::new::<CompositorStateData>(&display_handle);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&display_handle, "bubblewm");

    let output = Output::new(
        "WL-1".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "BubbleOS".into(),
            model: "BubbleWM Winit output".into(),
        },
    );
    let initial_mode = Mode {
        size: initial_size,
        refresh: OUTPUT_REFRESH_MILLIHZ,
    };
    output.set_preferred(initial_mode);
    output.change_current_state(
        Some(initial_mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.create_global::<CompositorStateData>(&display_handle);

    let mut theme_runtime = None;
    let mut theme_generation = 0;
    if let Some(wallpaper) = wallpaper.filter(|path| !path.trim().is_empty()) {
        match ThemeRuntime::start() {
            Ok(runtime) => {
                let generation = 1;
                match runtime.request(generation, wallpaper, theme_options) {
                    Ok(()) => {
                        theme_generation = generation;
                        theme_runtime = Some(runtime);
                        info!(
                            wallpaper,
                            generation, "Queued initial BubbleColor palette generation"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Could not queue initial BubbleColor palette generation");
                        let _ = runtime.shutdown();
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Could not start BubbleColor worker; continuing without wallpaper palette");
            }
        }
    }

    let mut state = CompositorStateData {
        display_handle: display_handle.clone(),
        compositor_state,
        layer_shell_state,
        xdg_shell_state: XdgShellState::new_with_capabilities::<CompositorStateData>(
            &display_handle,
            [
                xdg_toplevel::WmCapabilities::Maximize,
                xdg_toplevel::WmCapabilities::Minimize,
            ],
        ),
        _xdg_decoration_state: XdgDecorationState::new::<CompositorStateData>(&display_handle),
        shm_state,
        data_device_state,
        primary_selection_state,
        xdg_activation_state,
        recent_input_serials: Vec::new(),
        seat_state,
        seat,
        keyboard: None,
        pointer: None,
        pointer_location: (0.0, 0.0).into(),
        logo_modifier_pressed: false,
        pointer_cursor_status: CursorImageStatus::default_named(),
        output: output.clone(),
        output_manager_state,
        space: Space::default(),
        windows: Vec::new(),
        popups: PopupManager::default(),
        resize_session: None,
        next_window_position: 0,
        next_minimized_order: 0,
        next_stacking_order: 0,
        active_workspace: 0,
        workspace_count: WORKSPACE_COUNT,
        suppressed_keys: Vec::new(),
        layout_mode: if initial_layout == "tiling" {
            LayoutMode::MasterStack
        } else {
            LayoutMode::Floating
        },
        theme_runtime,
        theme_generation,
        active_theme: None,
    };
    state.space.map_output(&output, (0, 0));

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 600, 25)
        .context("cannot initialize the virtual keyboard")?;
    let pointer = state.seat.add_pointer();
    state.keyboard = Some(keyboard);
    state.pointer = Some(pointer);

    let wayland_listener = ListeningSocket::bind_auto("wayland", 1..33)
        .context("cannot create a Wayland socket in XDG_RUNTIME_DIR")?;
    let wayland_display = wayland_listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("Wayland listener did not provide a display name")?;

    // The socket is advertised before starting the IPC endpoint. If IPC setup
    // fails, dropping the listener closes the Wayland socket as startup unwinds.
    // The Winit backend is a nested development compositor. Its session
    // manager deliberately exposes no host power or logout actions.
    let session_manager = SessionManager::nested_development();
    let (ipc_runtime, ipc_commands) =
        IpcRuntime::start_default().context("cannot initialize BubbleWM IPC")?;

    info!(
        wayland_display = %wayland_display,
        output = %output.name(),
        width = initial_size.w,
        height = initial_size.h,
        "BubbleWM nested Wayland compositor is ready"
    );
    info!(
        "To launch a client in this nested session, use WAYLAND_DISPLAY={} <program>",
        wayland_display
    );

    let start_time = Instant::now();
    let mut clients = Vec::new();
    let mut running = true;
    // Capture a small sample of the first frames after a client toplevel
    // receives a real buffer. This distinguishes missing Space elements from
    // elements that are present but omitted by output rendering.
    let mut initial_render_diagnostics_remaining = 8;

    while running {
        let status = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => running = false,
            WinitEvent::Resized { size, .. } => state.update_output_mode(size),
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                let key_code = event.key_code();
                if let Some(keyboard) = state.keyboard.clone() {
                    let serial = SERIAL_COUNTER.next_serial();
                    if event.state() == smithay::backend::input::KeyState::Pressed {
                        let focus = keyboard.current_focus();
                        state.remember_input_serial(serial, focus);
                    }
                    keyboard.input::<(), _>(
                        &mut state,
                        event.key_code(),
                        event.state(),
                        serial,
                        (event.time() / 1000) as u32,
                        |compositor, modifiers, key| {
                            compositor.logo_modifier_pressed = modifiers.logo;
                            let keysym = key.modified_sym();
                            if event.state() == smithay::backend::input::KeyState::Pressed {
                                if let Some(action) = compositor_shortcut(*modifiers, keysym) {
                                    if suppress_shortcut_press(
                                        &mut compositor.suppressed_keys,
                                        key_code.into(),
                                    ) {
                                        match action {
                                            CompositorShortcut::Minimize => {
                                                compositor.minimize_focused_window();
                                            }
                                            CompositorShortcut::Restore => {
                                                compositor.restore_last_minimized_window();
                                            }
                                            CompositorShortcut::Close => {
                                                compositor.close_focused_window();
                                            }
                                            CompositorShortcut::WorkspacePrevious => {
                                                let index = previous_workspace(
                                                    compositor.active_workspace,
                                                    compositor.workspace_count,
                                                );
                                                let _ = compositor.activate_workspace(index);
                                            }
                                            CompositorShortcut::WorkspaceNext => {
                                                let index = next_workspace(
                                                    compositor.active_workspace,
                                                    compositor.workspace_count,
                                                );
                                                let _ = compositor.activate_workspace(index);
                                            }
                                            CompositorShortcut::ToggleLayout => {
                                                compositor.toggle_layout_mode();
                                            }
                                            CompositorShortcut::ToggleWindowLayout => {
                                                compositor.toggle_focused_window_layout();
                                            }
                                        }
                                    }
                                    FilterResult::Intercept(())
                                } else {
                                    FilterResult::Forward
                                }
                            } else if consume_shortcut_release(
                                &mut compositor.suppressed_keys,
                                key_code.into(),
                            ) {
                                FilterResult::Intercept(())
                            } else {
                                FilterResult::Forward
                            }
                        },
                    );
                }
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                let logical_size = state
                    .output
                    .current_mode()
                    .map(|mode| {
                        mode.size
                            .to_f64()
                            .to_logical(state.output.current_scale().fractional_scale())
                            .to_i32_ceil()
                    })
                    .unwrap_or_else(|| (0, 0).into());
                if logical_size.w > 0 && logical_size.h > 0 {
                    let location = event.position_transformed(logical_size);
                    let serial = SERIAL_COUNTER.next_serial();
                    let pointer_event = MotionEvent {
                        location,
                        serial,
                        time: (event.time() / 1000) as u32,
                    };
                    state.pointer_location = location;
                    if let Some(pointer) = state.pointer.clone() {
                        let focus = state.pointer_focus(location);
                        pointer.motion(&mut state, focus, &pointer_event);
                        pointer.frame(&mut state);
                    }
                }
            }
            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = (event.time() / 1000) as u32;
                if event.state() == ButtonState::Pressed {
                    let focus = state
                        .pointer_focus(state.pointer_location)
                        .map(|(surface, _)| surface);
                    state.remember_input_serial(serial, focus);
                }

                let mut decoration_move = None;
                let mut modifier_move = None;
                if event.state() == ButtonState::Pressed {
                    let hit = state.decoration_hit_test(state.pointer_location);
                    info!(
                        "Pointer press button={:#x} location=({}, {}) decoration_hit={}",
                        event.button_code(),
                        state.pointer_location.x,
                        state.pointer_location.y,
                        hit.as_ref()
                            .map(|(_, action)| format!("{action:?}"))
                            .unwrap_or_else(|| "none".into())
                    );
                    if let Some((window, action)) = hit {
                        let title = window
                            .toplevel()
                            .and_then(|surface| toplevel_title(&surface))
                            .unwrap_or_else(|| "<untitled>".into());
                        info!("Decoration action={action:?} target={title}");
                        if action == DecorationAction::Move {
                            decoration_move = Some(window.clone());
                        }
                        state.handle_decoration_action(&window, action);
                    } else if state.logo_modifier_pressed && event.button_code() == 0x110 {
                        if let Some((window, _)) = state.space.element_under(state.pointer_location)
                        {
                            let window = window.clone();
                            if state.windows.iter().any(|entry| {
                                entry.window == window
                                    && !entry.minimized
                                    && entry.workspace == state.active_workspace
                            }) {
                                let title = window
                                    .toplevel()
                                    .and_then(|surface| toplevel_title(&surface))
                                    .unwrap_or_else(|| "<untitled>".into());
                                info!("Modifier move grab requested title={title}");
                                state.focus_window_under_pointer(serial);
                                modifier_move = Some(window);
                            } else {
                                state.focus_window_under_pointer(serial);
                            }
                        } else {
                            state.focus_window_under_pointer(serial);
                        }
                    } else {
                        state.focus_window_under_pointer(serial);
                    }
                }

                if let Some(pointer) = state.pointer.clone() {
                    pointer.button(
                        &mut state,
                        &ButtonEvent {
                            serial,
                            time,
                            button: event.button_code(),
                            state: event.state(),
                        },
                    );
                    pointer.frame(&mut state);

                    if let Some(window) = modifier_move {
                        if event.state() == ButtonState::Pressed && event.button_code() == 0x110 {
                            if let Some(location) = state
                                .windows
                                .iter()
                                .find(|entry| entry.window == window)
                                .map(|entry| entry.location)
                            {
                                let start_data = GrabStartData {
                                    focus: None,
                                    button: event.button_code(),
                                    location: state.pointer_location,
                                };
                                info!(
                                    "Starting modifier move grab at ({}, {}) initial=({}, {})",
                                    state.pointer_location.x,
                                    state.pointer_location.y,
                                    location.x,
                                    location.y
                                );
                                pointer.set_grab(
                                    &mut state,
                                    WindowPointerGrab {
                                        start_data,
                                        window,
                                        initial_location: location,
                                        kind: WindowGrabKind::Move,
                                    },
                                    serial,
                                    Focus::Clear,
                                );
                            }
                        }
                    }

                    if let Some(window) = decoration_move {
                        if event.state() == ButtonState::Pressed && event.button_code() == 0x110 {
                            if let Some(location) = state
                                .windows
                                .iter()
                                .find(|entry| entry.window == window)
                                .map(|entry| entry.location)
                            {
                                let start_data = GrabStartData {
                                    focus: None,
                                    button: event.button_code(),
                                    location: state.pointer_location,
                                };
                                info!(
                                    "Starting decoration move grab at ({}, {}) initial=({}, {})",
                                    state.pointer_location.x,
                                    state.pointer_location.y,
                                    location.x,
                                    location.y
                                );
                                pointer.set_grab(
                                    &mut state,
                                    WindowPointerGrab {
                                        start_data,
                                        window,
                                        initial_location: location,
                                        kind: WindowGrabKind::Move,
                                    },
                                    serial,
                                    Focus::Clear,
                                );
                            }
                        }
                    }
                }
            }
            WinitEvent::Input(InputEvent::PointerAxis { event }) => {
                let time = (event.time() / 1000) as u32;
                let mut axis_frame = AxisFrame::new(time).source(event.source());
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    if let Some(value) = event.amount(axis) {
                        axis_frame = axis_frame.value(axis, value);
                    }
                    if let Some(value) = event.amount_v120(axis) {
                        axis_frame = axis_frame.v120(axis, value.round() as i32);
                    }
                    axis_frame =
                        axis_frame.relative_direction(axis, event.relative_direction(axis));
                }

                if let Some(pointer) = state.pointer.clone() {
                    pointer.axis(&mut state, axis_frame);
                    pointer.frame(&mut state);
                }
            }
            WinitEvent::Focus(_) | WinitEvent::Redraw | WinitEvent::Input(_) => {}
        });

        if matches!(status, PumpStatus::Exit(_)) || !running {
            break;
        }

        // BubbleColor results are consumed on the compositor thread. Image
        // decoding and palette generation remain isolated on its worker.
        state.poll_bubblecolor();

        // IPC commands are consumed on the compositor thread. No IPC worker
        // callback accesses Smithay state directly.
        ipc_commands
            .dispatch_pending(|request| state.handle_ipc_request(request, &session_manager));

        while let Some(stream) = wayland_listener
            .accept()
            .context("cannot accept a Wayland client")?
        {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(WaylandClientState::default()))
                .context("cannot register a Wayland client")?;
            clients.push(client);
        }

        display
            .dispatch_clients(&mut state)
            .context("Wayland client dispatch failed")?;

        // Some clients expose their committed buffer to the renderer only
        // after the post-commit hook has run. Retry the normal toplevel
        // mapping path once the client dispatch has completed, rather than
        // waiting for a workspace switch to remap otherwise-ready windows.
        let ready_unmapped_toplevels = state
            .windows
            .iter()
            .filter(|entry| {
                !entry.minimized
                    && entry.workspace == state.active_workspace
                    && state.space.element_location(&entry.window).is_none()
            })
            .filter_map(|entry| {
                let surface = entry.window.toplevel()?.wl_surface().clone();
                let has_buffer = with_renderer_surface_state(&surface, |surface_state| {
                    surface_state.buffer().is_some()
                })
                .unwrap_or(false);
                has_buffer.then_some(surface)
            })
            .collect::<Vec<_>>();
        for surface in ready_unmapped_toplevels {
            state.handle_toplevel_commit(&surface);
        }

        state.popups.cleanup();
        state.space.refresh();
        display
            .flush_clients()
            .context("cannot flush Wayland client events")?;

        // Client handles keep their ClientData alive. Drop disconnected handles
        // so repeated client connections cannot accumulate stale client state.
        clients.retain(|client| {
            client
                .get_data::<WaylandClientState>()
                .is_some_and(|data| data.connected.load(Ordering::Acquire))
        });

        // Winit supplies the fallback named cursor. Hide it when a client asks
        // for a custom surface or explicitly hides the pointer; the custom
        // surface is composited into BubbleWM's own scene below.
        backend
            .window()
            .set_cursor_visible(host_cursor_visible(&state.pointer_cursor_status));

        let size = backend.window_size();
        if size.w <= 0 || size.h <= 0 {
            thread::sleep(FRAME_INTERVAL);
            continue;
        }
        let damage = Rectangle::from_size(size);

        {
            let (renderer, mut framebuffer) = backend
                .bind()
                .context("cannot bind the Winit render target")?;
            let elements = state
                .space
                .render_elements_for_output(renderer, &state.output, 1.0)
                .map_err(|error| anyhow!("cannot assemble output scene: {error}"))?;

            if initial_render_diagnostics_remaining > 0
                && state.windows.iter().any(|entry| {
                    entry.window.toplevel().is_some_and(|toplevel| {
                        with_renderer_surface_state(toplevel.wl_surface(), |surface_state| {
                            surface_state.buffer().is_some()
                        })
                        .unwrap_or(false)
                    })
                })
            {
                let managed = state
                    .windows
                    .iter()
                    .map(|entry| {
                        let title = entry
                            .window
                            .toplevel()
                            .and_then(|toplevel| toplevel_title(&toplevel))
                            .unwrap_or_else(|| "<untitled>".into());
                        let has_buffer = entry.window.toplevel().is_some_and(|toplevel| {
                            with_renderer_surface_state(
                                toplevel.wl_surface(),
                                |surface_state| surface_state.buffer().is_some(),
                            )
                            .unwrap_or(false)
                        });
                        format!(
                            "{title}:buffer={has_buffer},mapped={},workspace={},minimized={},location={:?}",
                            state.space.element_location(&entry.window).is_some(),
                            entry.workspace,
                            entry.minimized,
                            entry.location
                        )
                    })
                    .collect::<Vec<_>>();
                info!(
                    output_size = ?size,
                    managed_windows = ?managed,
                    space_elements = state.space.elements().count(),
                    rendered_elements = elements.len(),
                    "Initial output render diagnostic"
                );
                initial_render_diagnostics_remaining -= 1;
            }

            let cursor_elements: Vec<
                WaylandSurfaceRenderElement<smithay::backend::renderer::gles::GlesRenderer>,
            > = match &state.pointer_cursor_status {
                CursorImageStatus::Surface(surface) if surface.alive() => {
                    let hotspot = with_states(surface, |states| {
                        states
                            .data_map
                            .get::<CursorImageSurfaceData>()
                            .and_then(|data| data.lock().ok().map(|attributes| attributes.hotspot))
                    })
                    .unwrap_or((0, 0).into());
                    let scale = state.output.current_scale().fractional_scale();
                    let location = (state.pointer_location - hotspot.to_f64())
                        .to_physical_precise_round(scale);
                    render_elements_from_surface_tree(
                        renderer,
                        surface,
                        location,
                        scale,
                        1.0,
                        Kind::Cursor,
                    )
                }
                _ => Vec::new(),
            };
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .context("cannot begin a Winit frame")?;
            frame
                .clear(Color32F::new(0.075, 0.09, 0.12, 1.0), &[damage])
                .context("cannot clear the Winit frame")?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .context("cannot render Wayland surfaces")?;
            let decorations = state.decoration_render_elements();
            draw_render_elements::<
                smithay::backend::renderer::gles::GlesRenderer,
                f64,
                SolidColorRenderElement,
            >(&mut frame, 1.0, &decorations, &[damage])
            .context("cannot render server-side decorations")?;
            draw_render_elements(&mut frame, 1.0, &cursor_elements, &[damage])
                .context("cannot render the pointer cursor")?;

            let sync_point = frame.finish().context("cannot finish the Winit frame")?;
            loop {
                if sync_point.wait().is_ok() {
                    break;
                }
            }
        }

        let frame_time = start_time.elapsed().as_millis() as u32;
        for surface in state.xdg_shell_state.toplevel_surfaces() {
            send_frames_surface_tree(surface.wl_surface(), frame_time);
        }
        for surface in state.layer_shell_state.layer_surfaces() {
            send_frames_layer_surface_tree(surface.wl_surface(), frame_time);
        }
        if let CursorImageStatus::Surface(surface) = &state.pointer_cursor_status
            && surface.alive()
        {
            send_frames_surface_tree(surface, frame_time);
        }

        backend
            .submit(Some(&[damage]))
            .context("cannot present the Winit frame")?;

        // Smithay's Winit event source is pumped with a zero timeout. A small
        // frame interval avoids spinning at full CPU when the host backend has
        // vsync disabled, while keeping input latency bounded.
        thread::sleep(FRAME_INTERVAL);
    }

    // Stop accepting IPC requests before shutting down the theme worker.
    // The IPC worker may still be waiting for an in-flight compositor reply.
    ipc_runtime
        .shutdown()
        .context("cannot shut down the BubbleWM IPC worker")?;
    if let Some(runtime) = state.theme_runtime.take() {
        runtime
            .shutdown()
            .map_err(|_| anyhow!("BubbleColor worker panicked during shutdown"))?;
    }
    info!("BubbleWM compositor is shutting down");
    Ok(())
}

fn is_current_theme_generation(update_generation: u64, current_generation: u64) -> bool {
    update_generation == current_generation
}

fn host_cursor_visible(status: &CursorImageStatus) -> bool {
    matches!(status, CursorImageStatus::Named(_))
}

fn previous_workspace(current: u32, count: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    (current + count - 1) % count
}

fn next_workspace(current: u32, count: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    (current + 1) % count
}

/// Sends frame callbacks to a layer surface, its popups and their subsurfaces.
fn send_frames_layer_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    for (popup, _) in PopupManager::popups_for_surface(surface) {
        send_frames_surface_tree(popup.wl_surface(), time);
    }
    send_frames_surface_tree(surface, time);
}

/// Sends frame callbacks to every surface in a tree after the nested frame.
fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_channels_are_normalized_for_the_renderer() {
        assert_eq!(
            rgb_channels(Rgb::new(128, 64, 255)),
            [128.0 / 255.0, 64.0 / 255.0, 1.0]
        );
    }

    #[test]
    fn current_theme_generation_accepts_only_the_latest_result() {
        assert!(is_current_theme_generation(4, 4));
        assert!(!is_current_theme_generation(3, 4));
        assert!(!is_current_theme_generation(5, 4));
    }

    #[test]
    fn host_cursor_is_visible_only_for_named_fallback_shapes() {
        assert!(host_cursor_visible(&CursorImageStatus::default_named()));
        assert!(!host_cursor_visible(&CursorImageStatus::Hidden));
    }

    #[test]
    fn compositor_shortcuts_require_the_logo_modifier() {
        assert_eq!(
            compositor_shortcut(ModifiersState::default(), Keysym::h),
            None
        );
        assert_eq!(
            compositor_shortcut(
                ModifiersState {
                    logo: true,
                    ..ModifiersState::default()
                },
                Keysym::h
            ),
            Some(CompositorShortcut::Minimize)
        );
    }

    #[test]
    fn held_shortcut_repeats_do_not_repeat_compositor_actions() {
        let mut suppressed = Vec::new();
        const TAB_KEY_CODE: u32 = 15;
        assert!(suppress_shortcut_press(&mut suppressed, TAB_KEY_CODE));
        assert!(!suppress_shortcut_press(&mut suppressed, TAB_KEY_CODE));
        assert_eq!(suppressed, vec![TAB_KEY_CODE]);
        assert!(consume_shortcut_release(&mut suppressed, TAB_KEY_CODE));
        assert!(suppressed.is_empty());
        assert!(suppress_shortcut_press(&mut suppressed, TAB_KEY_CODE));
    }

    #[test]
    fn compositor_shortcuts_map_restore_and_close_keys() {
        let modifiers = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            compositor_shortcut(
                ModifiersState {
                    ctrl: true,
                    alt: true,
                    ..ModifiersState::default()
                },
                Keysym::r
            ),
            Some(CompositorShortcut::Restore)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::q),
            Some(CompositorShortcut::Close)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::Left),
            Some(CompositorShortcut::WorkspacePrevious)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::Right),
            Some(CompositorShortcut::WorkspaceNext)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::t),
            Some(CompositorShortcut::ToggleLayout)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::T),
            Some(CompositorShortcut::ToggleLayout)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::f),
            Some(CompositorShortcut::ToggleWindowLayout)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::F),
            Some(CompositorShortcut::ToggleWindowLayout)
        );
    }

    #[test]
    fn layer_focus_falls_back_when_surface_cannot_receive_keyboard_input() {
        assert!(should_fallback_from_layer_focus(
            KeyboardInteractivity::None,
            true
        ));
        assert!(should_fallback_from_layer_focus(
            KeyboardInteractivity::Exclusive,
            false
        ));
        assert!(should_fallback_from_layer_focus(
            KeyboardInteractivity::OnDemand,
            false
        ));
        assert!(!should_fallback_from_layer_focus(
            KeyboardInteractivity::OnDemand,
            true
        ));
    }

    #[test]
    fn resizing_from_the_right_and_bottom_tracks_pointer_delta() {
        let size = calculate_resize_size(
            (800, 600).into(),
            (40.0, 25.0).into(),
            RESIZE_EDGE_RIGHT | RESIZE_EDGE_BOTTOM,
            (1, 1).into(),
            (0, 0).into(),
        );
        assert_eq!(size, (840, 625).into());
    }

    #[test]
    fn resizing_from_the_left_and_top_keeps_opposite_edges_fixed() {
        let size = calculate_resize_size(
            (800, 600).into(),
            (40.0, 25.0).into(),
            RESIZE_EDGE_LEFT | RESIZE_EDGE_TOP,
            (1, 1).into(),
            (0, 0).into(),
        );
        assert_eq!(size, (760, 575).into());
    }

    #[test]
    fn resize_respects_client_minimum_and_maximum_sizes() {
        let size = calculate_resize_size(
            (800, 600).into(),
            (-500.0, -500.0).into(),
            RESIZE_EDGE_LEFT | RESIZE_EDGE_TOP,
            (320, 240).into(),
            (700, 500).into(),
        );
        assert_eq!(size, (700, 500).into());
    }

    #[test]
    fn output_mode_uses_millihertz_refresh_units() {
        let mode = Mode {
            size: (1280, 800).into(),
            refresh: OUTPUT_REFRESH_MILLIHZ,
        };
        assert_eq!(mode.size.w, 1280);
        assert_eq!(mode.size.h, 800);
        assert_eq!(mode.refresh, 60_000);
    }

    #[test]
    fn window_cascade_is_bounded_to_eight_positions() {
        let positions = (0..WINDOW_CASCADE_COUNT * 2)
            .map(|index| (index % WINDOW_CASCADE_COUNT) as i32 * WINDOW_CASCADE_STEP)
            .collect::<Vec<_>>();
        assert_eq!(positions[0], positions[8]);
        assert_eq!(positions[1], positions[9]);
        assert!(positions.iter().all(|offset| (0..=224).contains(offset)));
    }

    #[test]
    fn workspace_navigation_wraps_in_both_directions() {
        assert_eq!(previous_workspace(0, WORKSPACE_COUNT), WORKSPACE_COUNT - 1);
        assert_eq!(previous_workspace(2, WORKSPACE_COUNT), 1);
        assert_eq!(next_workspace(WORKSPACE_COUNT - 1, WORKSPACE_COUNT), 0);
        assert_eq!(next_workspace(1, WORKSPACE_COUNT), 2);
    }

    #[test]
    fn workspace_navigation_handles_zero_count_without_panicking() {
        assert_eq!(previous_workspace(0, 0), 0);
        assert_eq!(next_workspace(0, 0), 0);
    }

    #[test]
    fn workspace_count_starts_with_four_workspaces() {
        assert_eq!(WORKSPACE_COUNT, 4);
    }
}
