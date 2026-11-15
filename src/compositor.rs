//! Initial nested Smithay compositor backend.
//!
//! This milestone creates a Winit window, exports the core compositor, shm,
//! seat, xdg-shell and wl_output globals, accepts clients, keeps a Smithay
//! desktop space, routes pointer/keyboard events, and renders mapped windows.
//! Decorations, workspaces, layer-shell and native DRM/KMS remain future work.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use bubble_ipc::{
    ErrorCode, ErrorResponse, Request, Response, ResponseData, SavedWifiSummary, SystemStatus,
    ThemePalette, WifiNetworkSummary, WindowSummary,
};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, ButtonState, Event as BackendInputEvent, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    backend::{
        renderer::{
            Color32F, Frame, Renderer,
            element::{
                Element, Kind,
                solid::{SolidColorBuffer, SolidColorRenderElement},
            },
            utils::{draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state},
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        LayerSurface, PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab,
        PopupUngrabStrategy, Space, Window, WindowSurfaceType, find_popup_root_surface,
        get_popup_toplevel_coords, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle, Keysym, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, Focus, GestureHoldBeginEvent,
            GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
            GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerHandle,
            PointerInnerHandle, RelativeMotionEvent,
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
        },
        shell::{
            wlr_layer::{
                Layer as WlrLayer, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::{
                PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface,
                XdgShellHandler, XdgShellState,
                decoration::{XdgDecorationHandler, XdgDecorationState},
            },
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::{info, warn};

use crate::{
    animation::{AnimationEngine, FinishAction, WindowTransform},
    bar::{self, BarAction, BarHit, BarModel},
    blur::BlurPipeline,
    config::Config,
    cursor::CursorThemeImages,
    idle::{IdleAction, IdleManager},
    ipc::IpcRuntime,
    launcher::{Launcher, LauncherAction},
    lock::{LockAction, LockScreen},
    menu::{Icon, MenuAction, PopupItem, PopupMenu},
    notifications::{NotificationCenter, NotificationEvent},
    render::{AnimatedElement, BubbleRenderElement, Pixmap, parse_color},
    status::{SessionStatus, StatusRuntime},
    theme::{ColorMode, PaletteStyle, Rgb, ThemeDocument, ThemeOptions},
    theme_runtime::ThemeRuntime,
    ui::{Canvas, Fonts, GlassRegion, UiColor},
    wallpaper::{Wallpaper, WallpaperSource},
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_REFRESH_MILLIHZ: i32 = 60_000;
const WINDOW_CASCADE_STEP: i32 = 32;
const WINDOW_CASCADE_COUNT: u32 = 8;
const TITLEBAR_HEIGHT: i32 = 32;
const DECORATION_BUTTON_SIZE: i32 = 16;
const DECORATION_BUTTON_MARGIN: i32 = 8;
const DECORATION_BUTTON_GAP: i32 = 6;
const RESIZE_EDGE_TOP: u32 = 1;
const RESIZE_EDGE_BOTTOM: u32 = 2;
const RESIZE_EDGE_LEFT: u32 = 4;
const RESIZE_EDGE_RIGHT: u32 = 8;

static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_reload(_signal: libc::c_int) {
    RELOAD_REQUESTED.store(true, Ordering::SeqCst);
}

extern "C" fn request_shutdown(_signal: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

pub(crate) fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGHUP,
            request_reload as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            request_shutdown as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            request_shutdown as *const () as libc::sighandler_t,
        );
    }
}

#[derive(Clone, Copy)]
struct RestoreGeometry {
    location: Point<i32, Logical>,
    size: Option<Size<i32, Logical>>,
}

struct ManagedWindow {
    /// Stable identifier used by the taskbar, the window menu and animations.
    id: u64,
    window: Window,
    /// Position of the outer window frame, not the client surface.
    location: Point<i32, Logical>,
    mapped_once: bool,
    restore_geometry: Option<RestoreGeometry>,
    parent_surface: Option<WlSurface>,
    minimized: bool,
    minimized_order: Option<u64>,
    workspace: u32,
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

struct OverlayCache {
    launcher: Option<Pixmap>,
    notifications: Option<Pixmap>,
    menu: Option<Pixmap>,
    bar: Option<Pixmap>,
    glass_regions: Vec<GlassRegion>,
    window_masks: Vec<Pixmap>,
    window_masks_dirty: bool,
    lock: Option<Pixmap>,
    dirty: bool,
    output_size: Size<i32, Logical>,
    lock_rendered_at: Option<Instant>,
    lock_minute: i64,
    bar_minute: String,
}

impl OverlayCache {
    fn new() -> Self {
        Self {
            launcher: None,
            notifications: None,
            menu: None,
            bar: None,
            glass_regions: Vec::new(),
            window_masks: Vec::new(),
            window_masks_dirty: true,
            lock: None,
            dirty: true,
            output_size: (0, 0).into(),
            lock_rendered_at: None,
            lock_minute: -1,
            bar_minute: String::new(),
        }
    }
}

pub(crate) struct CompositorStateData {
    display_handle: DisplayHandle,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    layer_shell_state: WlrLayerShellState,
    shm_state: ShmState,
    data_device_state: DataDeviceState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    keyboard: Option<KeyboardHandle<Self>>,
    pointer: Option<PointerHandle<Self>>,
    pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    output: Output,
    _output_manager_state: OutputManagerState,
    space: Space<Window>,
    windows: Vec<ManagedWindow>,
    popups: PopupManager,
    resize_session: Option<ResizeSession>,
    next_window_position: u32,
    next_window_id: u64,
    next_minimized_order: u64,
    active_workspace: u32,
    workspace_count: u32,
    suppressed_keys: Vec<Keysym>,
    context_menu: PopupMenu,
    animations: AnimationEngine,
    bar_hits: Vec<BarHit>,
    bar_status: SessionStatus,
    status_runtime: Option<StatusRuntime>,
    config: Config,
    config_path: Option<PathBuf>,
    theme_runtime: Option<ThemeRuntime>,
    theme_generation: u64,
    active_theme: Option<Arc<ThemeDocument>>,
    fonts: Fonts,
    wallpaper: Option<Wallpaper>,
    launcher: Launcher,
    notifications: NotificationCenter,
    notification_events: Option<mpsc::Receiver<NotificationEvent>>,
    lock: LockScreen,
    idle: IdleManager,
    cursor_theme: CursorThemeImages,
    cursor_hidden: bool,
    cursor_cache: Option<(f64, Pixmap, i32, i32)>,
    overlays: OverlayCache,
    real_blur: bool,
    gpu_blur_enabled: bool,
    blur_pipeline: BlurPipeline,
    blanked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompositorShortcut {
    Minimize,
    Restore,
    Close,
    Launcher,
    Lock,
    Terminal,
    WorkspacePrevious,
    WorkspaceNext,
    Workspace(u32),
    MoveToWorkspace(u32),
}

fn compositor_shortcut(modifiers: ModifiersState, keysym: Keysym) -> Option<CompositorShortcut> {
    if !modifiers.logo {
        return None;
    }

    match keysym {
        Keysym::h => Some(CompositorShortcut::Minimize),
        Keysym::Tab => Some(CompositorShortcut::Restore),
        Keysym::q => Some(CompositorShortcut::Close),
        Keysym::d => Some(CompositorShortcut::Launcher),
        Keysym::l => Some(CompositorShortcut::Lock),
        Keysym::Return | Keysym::KP_Enter => Some(CompositorShortcut::Terminal),
        _ => None,
    }
}

/// Workspace shortcuts use the unmodified key symbol so Shift+number still
/// selects a workspace on layouts where digits require Shift.
fn workspace_shortcut(modifiers: ModifiersState, raw_keysym: Keysym) -> Option<CompositorShortcut> {
    if !modifiers.logo {
        return None;
    }
    match raw_keysym {
        Keysym::_1 => Some(workspace_action(modifiers, 0)),
        Keysym::_2 => Some(workspace_action(modifiers, 1)),
        Keysym::_3 => Some(workspace_action(modifiers, 2)),
        Keysym::_4 => Some(workspace_action(modifiers, 3)),
        Keysym::_5 => Some(workspace_action(modifiers, 4)),
        Keysym::_6 => Some(workspace_action(modifiers, 5)),
        Keysym::_7 => Some(workspace_action(modifiers, 6)),
        Keysym::_8 => Some(workspace_action(modifiers, 7)),
        Keysym::_9 => Some(workspace_action(modifiers, 8)),
        Keysym::Left => Some(CompositorShortcut::WorkspacePrevious),
        Keysym::Right => Some(CompositorShortcut::WorkspaceNext),
        _ => None,
    }
}

fn workspace_action(modifiers: ModifiersState, index: u32) -> CompositorShortcut {
    if modifiers.shift {
        CompositorShortcut::MoveToWorkspace(index)
    } else {
        CompositorShortcut::Workspace(index)
    }
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

fn suppress_shortcut_press(suppressed: &mut Vec<Keysym>, keysym: Keysym) -> bool {
    if suppressed.contains(&keysym) {
        return false;
    }
    suppressed.push(keysym);
    true
}

fn consume_shortcut_release(suppressed: &mut Vec<Keysym>, keysym: Keysym) -> bool {
    if !suppressed.contains(&keysym) {
        return false;
    }
    suppressed.retain(|key| *key != keysym);
    true
}

impl CompositorStateData {
    /// Creates the backend-independent compositor state and Wayland globals.
    ///
    /// The caller owns the output and must have created its `wl_output` global
    /// with `CompositorStateData` as the handler type.
    pub(crate) fn create(
        display_handle: &DisplayHandle,
        config: Config,
        config_path: Option<PathBuf>,
        output: Output,
        start_locked: bool,
    ) -> Result<Self> {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display_handle);
        let layer_shell_state = WlrLayerShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(display_handle);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(display_handle, "bubblewm");

        let fonts = Fonts::load(Some(&config.theme.font));
        let cursor_theme = CursorThemeImages::load(&config);
        let launcher = Launcher::load();
        let (notification_sender, notification_events) = mpsc::channel();
        crate::notifications::start_service(notification_sender);
        let workspace_count = config.core.workspaces.max(1);
        // Keep session status available to independent clients such as
        // BubblePanel even when BubbleWM's built-in bar is disabled.
        let status_runtime = Some(StatusRuntime::start());

        let mut state = Self {
            display_handle: display_handle.clone(),
            compositor_state,
            xdg_shell_state: XdgShellState::new_with_capabilities::<Self>(
                display_handle,
                [
                    xdg_toplevel::WmCapabilities::Maximize,
                    xdg_toplevel::WmCapabilities::Minimize,
                ],
            ),
            _xdg_decoration_state: XdgDecorationState::new::<Self>(display_handle),
            layer_shell_state,
            shm_state,
            data_device_state,
            seat_state,
            seat,
            keyboard: None,
            pointer: None,
            pointer_location: (0.0, 0.0).into(),
            output: output.clone(),
            _output_manager_state: output_manager_state,
            space: Space::default(),
            windows: Vec::new(),
            popups: PopupManager::default(),
            resize_session: None,
            next_window_position: 0,
            next_window_id: 1,
            next_minimized_order: 0,
            active_workspace: 0,
            workspace_count,
            suppressed_keys: Vec::new(),
            context_menu: PopupMenu::hidden(),
            animations: AnimationEngine::new(&config.effects),
            bar_hits: Vec::new(),
            bar_status: SessionStatus::default(),
            status_runtime,
            config,
            config_path,
            theme_runtime: None,
            theme_generation: 0,
            active_theme: None,
            fonts,
            wallpaper: None,
            launcher,
            notifications: NotificationCenter::new(),
            notification_events: Some(notification_events),
            lock: LockScreen::new(start_locked),
            idle: IdleManager::new(),
            cursor_theme,
            cursor_hidden: false,
            cursor_cache: None,
            overlays: OverlayCache::new(),
            real_blur: false,
            gpu_blur_enabled: false,
            blur_pipeline: BlurPipeline::new(),
            blanked: false,
        };
        state.space.map_output(&output, (0, 0));
        state.reload_wallpaper();
        state.request_theme();

        let keyboard = state
            .seat
            .add_keyboard(Default::default(), 200, 200)
            .context("cannot initialize the virtual keyboard")?;
        let pointer = state.seat.add_pointer();
        state.keyboard = Some(keyboard);
        state.pointer = Some(pointer);
        state.apply_keyboard_config();

        Ok(state)
    }

    pub(crate) fn is_blanked(&self) -> bool {
        self.blanked
    }

    /// Enables the live GPU backdrop blur for backends that can draw the
    /// blurred regions themselves (currently the nested Winit backend).
    pub(crate) fn enable_gpu_blur(&mut self, enabled: bool) {
        self.gpu_blur_enabled = enabled;
    }

    /// Whether an overlay changed and the output must be repainted.
    pub(crate) fn overlays_dirty(&self) -> bool {
        self.overlays.dirty
    }

    /// Background color used when clearing an output frame.
    pub(crate) fn clear_color(&self) -> Color32F {
        if self.blanked {
            Color32F::new(0.0, 0.0, 0.0, 1.0)
        } else {
            self.wallpaper
                .as_ref()
                .map(|wallpaper| wallpaper.color)
                .or_else(|| {
                    self.active_theme
                        .as_ref()
                        .map(|theme| color32_from_rgb(theme.colors.background))
                })
                .unwrap_or_else(|| Color32F::new(0.075, 0.09, 0.12, 1.0))
        }
    }

    /// Registers a newly accepted Wayland client.
    pub(crate) fn insert_client(
        &mut self,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<Client> {
        self.display_handle
            .insert_client(stream, Arc::new(WaylandClientState::default()))
            .context("cannot register a Wayland client")
    }

    /// Flushes pending events to all connected Wayland clients.
    pub(crate) fn flush_clients(&mut self) -> Result<()> {
        self.display_handle
            .flush_clients()
            .context("cannot flush Wayland client events")
    }

    /// Advertises the shm formats supported by the active renderer.
    pub(crate) fn update_shm_formats(
        &mut self,
        formats: impl IntoIterator<Item = smithay::reexports::wayland_server::protocol::wl_shm::Format>,
    ) {
        self.shm_state.update_formats(formats);
    }

    /// Current keyboard LED state, if a keyboard has been initialized.
    pub(crate) fn keyboard_led_state(&self) -> Option<smithay::input::keyboard::LedState> {
        self.keyboard.as_ref().map(|keyboard| keyboard.led_state())
    }

    /// Sends frame callbacks to every mapped toplevel and layer surface.
    pub(crate) fn send_frame_callbacks(&self, time: u32) {
        for surface in self.xdg_shell_state.toplevel_surfaces() {
            send_frames_surface_tree(surface.wl_surface(), time);
        }
        let map = layer_map_for_output(&self.output);
        for layer in map.layers() {
            send_frames_surface_tree(layer.wl_surface(), time);
        }
    }

    /// Runs the periodic work shared by every backend: config reloads,
    /// notification delivery, idle policy and overlay invalidation.
    pub(crate) fn housekeeping(&mut self) {
        if RELOAD_REQUESTED.swap(false, Ordering::SeqCst) {
            self.reload_config();
        }

        self.poll_theme();

        if let Some(runtime) = self.status_runtime.as_ref() {
            while let Some(status) = runtime.poll() {
                self.bar_status = status;
                self.overlays.dirty = true;
            }
        }

        let mut notification_events = Vec::new();
        if let Some(receiver) = self.notification_events.as_ref() {
            while let Ok(event) = receiver.try_recv() {
                notification_events.push(event);
            }
        }
        for event in notification_events {
            self.notifications
                .handle_event(event, &self.config.notifications);
            self.overlays.dirty = true;
        }
        if self.notifications.expire() {
            self.overlays.dirty = true;
        }
        self.handle_idle();
        self.tick_animations();
    }

    /// True while a Compiz-style transition or a menu opening animation still
    /// needs fresh frames.
    pub(crate) fn animations_active(&self) -> bool {
        self.animations.active(Instant::now()) || self.context_menu.animating()
    }

    /// Refreshes cached window/popup state after dispatching Wayland clients.
    pub(crate) fn refresh(&mut self) {
        self.popups.cleanup();
        self.space.refresh();
    }

    fn layer_pointer_focus(
        &self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        let map = layer_map_for_output(&self.output);
        for layer in [
            WlrLayer::Overlay,
            WlrLayer::Top,
            WlrLayer::Bottom,
            WlrLayer::Background,
        ] {
            let Some(surface) = map.layer_under(layer, location) else {
                continue;
            };
            let Some(geometry) = map.layer_geometry(surface) else {
                continue;
            };
            let local = location - geometry.loc.to_f64();
            if let Some((surface, surface_location)) =
                surface.surface_under(local, WindowSurfaceType::ALL)
            {
                return Some((surface, geometry.loc.to_f64() + surface_location.to_f64()));
            }
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
        if let Some(focus) = self.layer_pointer_focus(location) {
            return Some(focus);
        }
        let (window, window_location) = self.space.element_under(location)?;
        let local_location = location - window_location.to_f64();
        let (surface, surface_location) =
            window.surface_under(local_location, WindowSurfaceType::ALL)?;
        Some((
            surface,
            window_location.to_f64() + surface_location.to_f64(),
        ))
    }

    fn fullscreen_window(&self) -> bool {
        self.space.elements().any(|window| {
            window.toplevel().is_some_and(|toplevel| {
                toplevel
                    .current_state()
                    .states
                    .contains(xdg_toplevel::State::Fullscreen)
            })
        })
    }

    fn output_size(&self) -> Size<i32, Logical> {
        self.output
            .current_mode()
            .map(|mode| {
                mode.size
                    .to_f64()
                    .to_logical(self.output.current_scale().fractional_scale())
                    .to_i32_round()
            })
            .unwrap_or_else(|| (0, 0).into())
    }

    fn reload_wallpaper(&mut self) {
        let size = self.output_size();
        if size.w <= 0 || size.h <= 0 {
            self.wallpaper = None;
            return;
        }
        match Wallpaper::load(&self.config.wallpaper, &self.config.effects, size) {
            Ok(wallpaper) => self.wallpaper = Some(wallpaper),
            Err(error) => {
                warn!(%error, "cannot load the configured wallpaper");
                self.wallpaper = None;
            }
        }
    }

    fn request_theme(&mut self) {
        if self.config.theme.source != "wallpaper" || !self.config.wallpaper.enabled {
            self.active_theme = None;
            return;
        }

        let path = PathBuf::from(&self.config.wallpaper.path);
        if !path.is_file() {
            warn!(
                path = %path.display(),
                "wallpaper palette source is missing; keeping the manual palette"
            );
            self.active_theme = None;
            return;
        }

        if self.theme_runtime.is_none() {
            match ThemeRuntime::start() {
                Ok(runtime) => self.theme_runtime = Some(runtime),
                Err(error) => {
                    warn!(%error, "cannot start the BubbleColor worker");
                    return;
                }
            }
        }

        self.theme_generation = self.theme_generation.wrapping_add(1).max(1);
        let generation = self.theme_generation;
        let options = self.theme_options();
        if let Some(runtime) = self.theme_runtime.as_ref() {
            if let Err(error) = runtime.request(generation, &path, options) {
                warn!(%error, "cannot queue wallpaper palette generation");
            }
        }
    }

    fn theme_options(&self) -> ThemeOptions {
        ThemeOptions {
            mode: match self.config.theme.palette_mode.as_str() {
                "light" => ColorMode::Light,
                _ => ColorMode::Dark,
            },
            style: match self.config.theme.palette_style.as_str() {
                "vivid" => PaletteStyle::Vivid,
                "muted" => PaletteStyle::Muted,
                _ => PaletteStyle::Balanced,
            },
            contrast_target: if self.config.theme.automatic_contrast {
                self.config.theme.contrast_target
            } else {
                4.5
            },
        }
    }

    fn poll_theme(&mut self) {
        let Some(update) = self
            .theme_runtime
            .as_ref()
            .and_then(ThemeRuntime::try_take_update)
        else {
            return;
        };
        if update.generation != self.theme_generation {
            return;
        }
        match update.result {
            Ok(theme) => {
                info!(
                    source = %theme.source_file,
                    sampled_pixels = theme.diagnostics.sampled_pixels,
                    "BubbleColor palette accepted"
                );
                // The compositor overlays (launcher, notifications, lock) all
                // read theme colors from the config. Apply the accepted
                // snapshot in memory without persisting it to disk.
                self.config.theme.accent = theme.colors.primary.to_hex();
                self.config.theme.background = theme.colors.background.to_hex();
                self.config.theme.foreground = theme.colors.on_background.to_hex();
                self.active_theme = Some(Arc::new(theme));
                if self
                    .cursor_theme
                    .apply_theme(self.active_theme.as_deref(), &self.config)
                {
                    self.cursor_cache = None;
                }
                self.overlays.dirty = true;
            }
            Err(error) => {
                warn!(%error, "BubbleColor palette generation failed; keeping the previous palette")
            }
        }
    }

    /// Returns the current palette snapshot in the IPC wire format.
    ///
    /// The compositor owns the only wallpaper analysis pass; clients such as
    /// BubblePanel and BubbleSettings receive this immutable snapshot instead
    /// of decoding the wallpaper themselves.
    pub(crate) fn theme_palette(&self) -> Option<ThemePalette> {
        let theme = self.active_theme.as_deref()?;
        let colors = &theme.colors;
        let rgb = |color: Rgb| [color.r, color.g, color.b];
        Some(ThemePalette {
            schema_version: theme.schema_version,
            generation: self.theme_generation,
            primary: rgb(colors.primary),
            on_primary: rgb(colors.on_primary),
            primary_container: rgb(colors.primary_container),
            on_primary_container: rgb(colors.on_primary_container),
            secondary: rgb(colors.secondary),
            on_secondary: rgb(colors.on_secondary),
            secondary_container: rgb(colors.secondary_container),
            on_secondary_container: rgb(colors.on_secondary_container),
            tertiary: rgb(colors.tertiary),
            on_tertiary: rgb(colors.on_tertiary),
            tertiary_container: rgb(colors.tertiary_container),
            on_tertiary_container: rgb(colors.on_tertiary_container),
            background: rgb(colors.background),
            on_background: rgb(colors.on_background),
            surface: rgb(colors.surface),
            on_surface: rgb(colors.on_surface),
            surface_variant: rgb(colors.surface_variant),
            on_surface_variant: rgb(colors.on_surface_variant),
            surface_elevated: rgb(colors.surface_elevated),
            surface_overlay: rgb(colors.surface_overlay),
            outline: rgb(colors.outline),
            outline_variant: rgb(colors.outline_variant),
            on_error: rgb(colors.on_error),
            warning: rgb(colors.warning),
            success: rgb(colors.success),
            error: rgb(colors.error),
            info: rgb(colors.info),
            glass_tint: rgb(colors.glass_tint),
            glass_opacity: colors.glass_opacity,
            glass_border: rgb(colors.glass_border),
            glass_highlight: rgb(colors.glass_highlight),
            glass_shadow: rgb(colors.glass_shadow),
            neon_cyan: rgb(colors.neon_cyan),
            neon_magenta: rgb(colors.neon_magenta),
            neon_violet: rgb(colors.neon_violet),
            neon_lime: rgb(colors.neon_lime),
        })
    }

    fn reload_config(&mut self) {
        match Config::load(self.config_path.as_deref()) {
            Ok(config) => {
                self.config = config;
                self.animations.configure(&self.config.effects);
                self.workspace_count = self.config.core.workspaces.max(1);
                if self.active_workspace >= self.workspace_count {
                    self.active_workspace = self.workspace_count - 1;
                }
                for entry in &mut self.windows {
                    if entry.workspace >= self.workspace_count {
                        entry.workspace = self.workspace_count - 1;
                    }
                }
                self.remap_active_workspace();
                if !self.config.bar.enabled {
                    self.bar_hits.clear();
                }
                self.fonts = Fonts::load(Some(&self.config.theme.font));
                self.cursor_theme = CursorThemeImages::load(&self.config);
                self.cursor_cache = None;
                self.reload_wallpaper();
                self.request_theme();
                if self
                    .cursor_theme
                    .apply_theme(self.active_theme.as_deref(), &self.config)
                {
                    self.cursor_cache = None;
                }
                self.overlays.dirty = true;
                self.apply_keyboard_config();
                info!("configuration reloaded");
            }
            Err(error) => warn!(%error, "cannot reload configuration"),
        }
    }

    fn apply_keyboard_config(&mut self) {
        if let Some(keyboard) = self.keyboard.clone() {
            let layout_name = self.config.input.keyboard_layout.clone();
            let variant = self.config.input.keyboard_variant.clone();
            let model = self.config.input.keyboard_model.clone();
            let options = self.config.input.keyboard_options.clone();
            let repeat_rate = self.config.input.repeat_rate as i32;
            let repeat_delay = self.config.input.repeat_delay as i32;
            let mut layout = smithay::input::keyboard::XkbConfig::default();
            layout.layout = &layout_name;
            layout.variant = &variant;
            layout.model = &model;
            layout.options = if options.is_empty() {
                None
            } else {
                Some(options)
            };
            if let Err(error) = keyboard.set_xkb_config(self, layout) {
                warn!(%error, "cannot apply keyboard configuration");
            }
            keyboard.change_repeat_info(repeat_rate, repeat_delay);
        }
    }

    fn lock_session(&mut self) {
        self.lock.lock(&self.config);
        self.overlays.dirty = true;
    }

    fn handle_idle(&mut self) -> bool {
        let inhibited = self.config.idle.inhibit_when_fullscreen && self.fullscreen_window();
        let action = self.idle.tick(&self.config.idle, inhibited);
        match action {
            IdleAction::Blank => {
                self.blanked = true;
                self.overlays.dirty = true;
                true
            }
            IdleAction::Unblank => {
                self.blanked = false;
                self.overlays.dirty = true;
                true
            }
            IdleAction::Lock => {
                self.lock_session();
                true
            }
            IdleAction::Suspend => {
                if self.config.idle.lock_on_suspend {
                    self.lock_session();
                }
                if let Err(error) = std::process::Command::new("systemctl")
                    .arg("suspend")
                    .spawn()
                {
                    warn!(%error, "cannot suspend the session");
                }
                true
            }
            IdleAction::None => false,
        }
    }

    fn perform_shortcut(&mut self, action: CompositorShortcut) {
        match action {
            CompositorShortcut::Minimize => self.minimize_focused_window(),
            CompositorShortcut::Restore => self.restore_last_minimized_window(),
            CompositorShortcut::Close => self.close_focused_window(),
            CompositorShortcut::Launcher => {
                if self.config.launcher.enabled {
                    self.launcher.toggle();
                    self.overlays.dirty = true;
                }
            }
            CompositorShortcut::Lock => self.lock_session(),
            CompositorShortcut::Terminal => {
                if let Err(error) = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&self.config.core.terminal)
                    .spawn()
                {
                    warn!(%error, "cannot launch the configured terminal");
                }
            }
            CompositorShortcut::WorkspacePrevious => {
                let index = previous_workspace(self.active_workspace, self.workspace_count);
                let _ = self.activate_workspace(index);
            }
            CompositorShortcut::WorkspaceNext => {
                let index = next_workspace(self.active_workspace, self.workspace_count);
                let _ = self.activate_workspace(index);
            }
            CompositorShortcut::Workspace(index) => {
                let _ = self.activate_workspace(index);
            }
            CompositorShortcut::MoveToWorkspace(index) => {
                self.move_focused_to_workspace(index);
            }
        }
    }

    /// Routes a pressed key to the lock screen, the launcher or a compositor
    /// shortcut. Returns true when the key must not reach the focused client.
    fn handle_key_press(
        &mut self,
        keysym: Keysym,
        raw_keysym: Keysym,
        modifiers: ModifiersState,
    ) -> bool {
        if self.lock.active {
            match self.lock.handle_key(keysym, modifiers, &self.config) {
                LockAction::Unlocked => {
                    self.idle.reset_after_unlock();
                    self.overlays.dirty = true;
                    self.focus_topmost_window();
                }
                LockAction::Redraw => self.overlays.dirty = true,
                LockAction::None => {}
            }
            return true;
        }

        if self.context_menu.visible {
            let action = self.context_menu.handle_key(keysym);
            if action == MenuAction::None {
                self.overlays.dirty = true;
            } else {
                self.execute_menu_action(action);
            }
            return true;
        }

        if self.launcher.visible {
            let action = self.launcher.handle_key(keysym, modifiers, &self.config);
            match action {
                LauncherAction::Close | LauncherAction::Redraw => self.overlays.dirty = true,
                LauncherAction::None => {}
            }
            return true;
        }

        if modifiers.alt && keysym == Keysym::Tab {
            if suppress_shortcut_press(&mut self.suppressed_keys, keysym) {
                self.focus_next_window();
            }
            return true;
        }

        let (action, suppress) = if let Some(action) = compositor_shortcut(modifiers, keysym) {
            (action, keysym)
        } else if let Some(action) = workspace_shortcut(modifiers, raw_keysym) {
            (action, raw_keysym)
        } else {
            return false;
        };
        if suppress_shortcut_press(&mut self.suppressed_keys, suppress) {
            self.perform_shortcut(action);
        }
        true
    }

    fn note_activity(&mut self) {
        if self.idle.activity() == IdleAction::Unblank {
            self.blanked = false;
            self.overlays.dirty = true;
        }
    }

    fn move_pointer_to(
        &mut self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
        time: u32,
    ) {
        let size = self.output_size();
        let max_x = (size.w.max(1) - 1) as f64;
        let max_y = (size.h.max(1) - 1) as f64;
        let location = Point::from((location.x.clamp(0.0, max_x), location.y.clamp(0.0, max_y)));
        self.pointer_location = location;
        if self.context_menu.visible
            && self
                .context_menu
                .handle_motion(self.pointer_location.x, self.pointer_location.y)
        {
            self.overlays.dirty = true;
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(pointer) = self.pointer.clone() {
            let focus = if self.lock.active || self.launcher.visible {
                None
            } else {
                self.pointer_focus(location)
            };
            pointer.motion(
                self,
                focus,
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
        }
    }

    /// Handles a backend input event for both Winit and libinput.
    pub(crate) fn process_input_event<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: InputEvent<B>,
    ) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                self.note_activity();
                if let Some(keyboard) = self.keyboard.clone() {
                    let key_state = event.state();
                    let time = event.time_msec();
                    keyboard.input::<(), _>(
                        self,
                        event.key_code(),
                        key_state,
                        SERIAL_COUNTER.next_serial(),
                        time,
                        |compositor, modifiers, key| {
                            let keysym = key.modified_sym();
                            let raw_keysym = key.raw_syms().first().copied().unwrap_or(keysym);
                            if key_state == smithay::backend::input::KeyState::Pressed {
                                if compositor.handle_key_press(keysym, raw_keysym, *modifiers) {
                                    FilterResult::Intercept(())
                                } else {
                                    FilterResult::Forward
                                }
                            } else if consume_shortcut_release(
                                &mut compositor.suppressed_keys,
                                keysym,
                            ) {
                                FilterResult::Intercept(())
                            } else {
                                FilterResult::Forward
                            }
                        },
                    );
                }
            }
            InputEvent::PointerMotion { event, .. } => {
                self.note_activity();
                let location = self.pointer_location + event.delta();
                self.move_pointer_to(location, event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.note_activity();
                let size = self.output_size();
                if size.w > 0 && size.h > 0 {
                    let location = event.position_transformed(size);
                    self.move_pointer_to(location, event.time_msec());
                }
            }
            InputEvent::PointerButton { event, .. } => {
                self.note_activity();
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                let consumed = self.handle_pointer_click(event.button_code());
                if consumed {
                    return;
                }
                let mut decoration_move = None;
                if event.state() == ButtonState::Pressed {
                    if let Some((window, action)) = self.decoration_hit_test(self.pointer_location)
                    {
                        if action == DecorationAction::Move {
                            decoration_move = Some(window.clone());
                        }
                        self.handle_decoration_action(&window, action);
                    } else {
                        self.focus_window_under_pointer(serial);
                    }
                }

                if let Some(pointer) = self.pointer.clone() {
                    pointer.button(
                        self,
                        &ButtonEvent {
                            serial,
                            time,
                            button: event.button_code(),
                            state: event.state(),
                        },
                    );
                    pointer.frame(self);

                    if let Some(window) = decoration_move {
                        if event.state() == ButtonState::Pressed && event.button_code() == 0x110 {
                            if let Some(location) = self
                                .windows
                                .iter()
                                .find(|entry| entry.window == window)
                                .map(|entry| entry.location)
                            {
                                let start_data = GrabStartData {
                                    focus: None,
                                    button: event.button_code(),
                                    location: self.pointer_location,
                                };
                                pointer.set_grab(
                                    self,
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
            InputEvent::PointerAxis { event, .. } => {
                self.note_activity();
                let time = event.time_msec();
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

                if let Some(pointer) = self.pointer.clone() {
                    pointer.axis(self, axis_frame);
                    pointer.frame(self);
                }
            }
            _ => {}
        }
    }

    /// Builds the overlay layer (cursor, bar, menus, launcher, notifications,
    /// lock screen) in front-to-back order.
    pub(crate) fn overlay_elements(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        scale: f64,
    ) -> Vec<BubbleRenderElement> {
        let mut elements = Vec::new();
        if self.blanked {
            return elements;
        }

        if !self.lock.active && !self.cursor_hidden {
            if let Some((_, pixmap, xhot, yhot)) = self.cursor_cache.as_ref() {
                let location = self.pointer_location - Point::from((*xhot as f64, *yhot as f64));
                if let Some(element) = pixmap.element_at(renderer, location, scale) {
                    elements.push(element.into());
                }
            }
        }
        for pixmap in [self.overlays.lock.as_ref(), self.overlays.launcher.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(element) = pixmap.element(renderer, scale) {
                elements.push(element.into());
            }
        }

        // The connectivity/taskbar menus grow out of their capsule instead of
        // popping in place. The pixmap is rendered once at full size and the
        // transform anchors its top-right corner to the bar.
        if let Some(pixmap) = self.overlays.menu.as_ref()
            && let Some(element) = pixmap.element(renderer, scale)
        {
            if let Some(progress) = self.context_menu.opening_progress()
                && progress < 1.0
            {
                let eased = 1.0 - (1.0 - progress).powi(3);
                let menu_scale = 0.72 + 0.28 * f64::from(eased);
                let width = f64::from(self.context_menu.width()) * scale;
                let height = f64::from(self.context_menu.height()) * scale;
                let offset = Point::from((
                    (1.0 - menu_scale) * width / 2.0,
                    -(1.0 - menu_scale) * height / 2.0,
                ));
                elements.push(BubbleRenderElement::AnimatedMemory(AnimatedElement::new(
                    element, menu_scale, eased, offset,
                )));
            } else {
                elements.push(element.into());
            }
        }

        for pixmap in [
            self.overlays.notifications.as_ref(),
            self.overlays.bar.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(element) = pixmap.element(renderer, scale) {
                elements.push(element.into());
            }
        }
        elements
    }

    /// Builds the desktop layer (window masks, decorations, client surfaces
    /// and wallpaper) in front-to-back order.
    pub(crate) fn backdrop_elements(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        scale: f64,
    ) -> Vec<BubbleRenderElement> {
        let mut elements = Vec::new();
        if self.blanked {
            return elements;
        }
        for pixmap in &self.overlays.window_masks {
            if let Some(element) = pixmap.element(renderer, scale) {
                elements.push(element.into());
            }
        }
        for element in self.decoration_render_elements(renderer) {
            elements.push(element);
        }

        let now = Instant::now();
        let output_scale = smithay::utils::Scale::from(scale);
        let window_rects = self
            .windows
            .iter()
            .filter(|entry| {
                entry.workspace == self.active_workspace
                    && !entry.minimized
                    && self.space.element_location(&entry.window).is_some()
            })
            .filter_map(|entry| {
                let transform = self.animation_transform(entry, now);
                if transform.is_identity() {
                    return None;
                }
                let rect = self
                    .window_outer_rect(entry)
                    .to_f64()
                    .to_physical(scale)
                    .to_i32_round();
                Some((rect, transform))
            })
            .collect::<Vec<_>>();
        let layer_rects = {
            let map = layer_map_for_output(&self.output);
            map.layers()
                .filter_map(|layer| map.layer_geometry(layer))
                .map(|rect| rect.to_f64().to_physical(scale).to_i32_round())
                .collect::<Vec<_>>()
        };

        match self
            .space
            .render_elements_for_output(renderer, &self.output, self.window_opacity())
        {
            Ok(space_elements) => {
                // `render_elements_for_output` already returns front-to-back.
                for element in space_elements {
                    let geometry = element.geometry(output_scale);
                    let transform = if layer_rects.iter().any(|rect| rect.overlaps(geometry)) {
                        None
                    } else {
                        window_rects
                            .iter()
                            .find(|(rect, _)| rect.overlaps(geometry))
                            .map(|(_, transform)| *transform)
                    };
                    if let Some(transform) = transform.filter(|transform| !transform.is_identity())
                    {
                        elements.push(BubbleRenderElement::Animated(AnimatedElement::new(
                            element,
                            f64::from(transform.scale),
                            transform.alpha,
                            Point::from((
                                f64::from(transform.offset_x) * scale,
                                f64::from(transform.offset_y) * scale,
                            )),
                        )));
                    } else {
                        elements.push(element.into());
                    }
                }
            }
            Err(error) => warn!(%error, "cannot assemble the output scene"),
        }
        if let Some(pixmap) = self
            .wallpaper
            .as_ref()
            .and_then(|wallpaper| wallpaper.pixmap.as_ref())
        {
            if let Some(element) = pixmap.element(renderer, scale) {
                elements.push(element.into());
            }
        }
        elements
    }

    /// Builds the full scene in front-to-back order for `DrmCompositor`.
    pub(crate) fn scene_elements(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        scale: f64,
    ) -> Vec<BubbleRenderElement> {
        let mut elements = self.overlay_elements(renderer, scale);
        elements.extend(self.backdrop_elements(renderer, scale));
        elements
    }

    fn handle_pointer_click(&mut self, button: u32) -> bool {
        if self.lock.active {
            return true;
        }
        if self.context_menu.visible {
            if button == 0x110 {
                let action = self
                    .context_menu
                    .handle_click(self.pointer_location.x, self.pointer_location.y);
                self.execute_menu_action(action);
            } else if button == 0x111 {
                self.context_menu.close();
                self.overlays.dirty = true;
            }
            return true;
        }
        if self.launcher.visible {
            if button == 0x110 {
                let size = self.output_size();
                let action = self.launcher.handle_click(
                    self.pointer_location.x as i32,
                    self.pointer_location.y as i32,
                    size,
                    &self.config,
                );
                if action != LauncherAction::None {
                    self.overlays.dirty = true;
                }
            }
            return true;
        }
        if button == 0x111 {
            let strip_height = (self.config.bar.margin * 2 + self.config.bar.height).max(0) as f64;
            if self.config.bar.enabled && self.pointer_location.y < strip_height {
                return true;
            }
            if self.space.element_under(self.pointer_location).is_none() {
                self.open_desktop_menu();
                return true;
            }
            return false;
        }
        self.handle_bar_click(button)
    }

    fn handle_bar_click(&mut self, button: u32) -> bool {
        if !self.config.bar.enabled || self.blanked {
            return false;
        }
        let strip_height = (self.config.bar.margin * 2 + self.config.bar.height).max(0) as f64;
        if self.pointer_location.y >= strip_height {
            return false;
        }
        // The whole strip is compositor-owned; do not forward clicks to the
        // client underneath, even between pills.
        if button != 0x110 {
            return true;
        }
        let hit = self
            .bar_hits
            .iter()
            .find(|hit| hit.contains(self.pointer_location.x, self.pointer_location.y))
            .map(|hit| hit.action);
        match hit {
            Some(BarAction::Launcher) => {
                if self.config.launcher.enabled {
                    self.launcher.toggle();
                    self.overlays.dirty = true;
                }
            }
            Some(BarAction::Workspace(index)) => {
                let _ = self.activate_workspace(index);
            }
            Some(BarAction::Network) => self.open_network_menu(),
            Some(BarAction::WindowMenu) => self.open_window_menu(),
            Some(BarAction::ToggleMute) => crate::status::toggle_mute(),
            Some(BarAction::Lock) => self.lock_session(),
            None => {}
        }
        true
    }

    fn overlay_geometry_changed(&mut self) {
        let size = self.output_size();
        if self.overlays.output_size != size {
            self.overlays.output_size = size;
            self.overlays.dirty = true;
        }
    }

    pub(crate) fn refresh_overlays(&mut self) {
        let size = self.output_size();
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        self.overlay_geometry_changed();

        // The GPU blur is live: every surface samples the actual scene behind
        // it. When it is unavailable, the CPU wallpaper blur remains a
        // fallback for the window corner masks.
        let real_blur = self.gpu_blur_enabled
            && self.config.effects.enabled
            && self.config.effects.blur
            && self
                .wallpaper
                .as_ref()
                .is_some_and(|wallpaper| wallpaper.glass.is_some());
        self.real_blur = real_blur;
        self.blur_pipeline
            .set_radius(self.config.effects.blur_strength);
        if let Some(wallpaper) = self.wallpaper.as_mut()
            && let Some(glass) = wallpaper.glass.as_mut()
        {
            glass.gpu = real_blur;
        }
        let glass = self
            .wallpaper
            .as_ref()
            .and_then(|wallpaper| wallpaper.glass.as_ref());
        let mut regions = Vec::new();

        if self.launcher.visible && self.overlays.dirty {
            self.overlays.launcher = self.launcher.render(
                &self.fonts,
                &self.config,
                self.active_theme.as_deref(),
                size,
                glass,
            );
        } else if !self.launcher.visible {
            self.overlays.launcher = None;
        }
        if self.overlays.launcher.is_some() {
            regions.extend(self.launcher.glass_region(&self.config, size));
        }

        if self.notifications.is_empty() {
            self.overlays.notifications = None;
        } else if self.overlays.dirty {
            self.overlays.notifications = self.notifications.render(
                &self.fonts,
                &self.config,
                self.active_theme.as_deref(),
                size,
                glass,
            );
        }
        if self.overlays.notifications.is_some() {
            regions.extend(self.notifications.glass_regions(&self.config, size));
        }

        if self.context_menu.visible && self.overlays.dirty {
            self.overlays.menu = self.context_menu.render(
                &self.fonts,
                &self.config,
                self.active_theme.as_deref(),
                glass,
            );
        } else if !self.context_menu.visible {
            self.overlays.menu = None;
        }
        if self.overlays.menu.is_some() {
            let region = self.context_menu.glass_region();
            if let Some(progress) = self.context_menu.opening_progress()
                && progress < 1.0
            {
                let eased = 1.0 - (1.0 - progress).powi(3);
                let menu_scale = 0.72 + 0.28 * f64::from(eased);
                let width = (f64::from(region.width) * menu_scale).round() as i32;
                let height = (f64::from(region.height) * menu_scale).round() as i32;
                let x = region.x + region.width - width;
                let radius = (f64::from(region.radius) * menu_scale).round() as i32;
                regions.push(GlassRegion::new(
                    x,
                    region.y,
                    width.max(1),
                    height.max(1),
                    radius,
                ));
            } else {
                regions.push(region);
            }
        }

        if self.overlays.dirty || self.overlays.window_masks_dirty {
            self.overlays.window_masks = self.window_corner_masks();
            self.overlays.window_masks_dirty = false;
        }

        let clock = crate::lock::clock_strings().0;
        let bar_refresh = self.overlays.dirty || self.overlays.bar_minute != clock;
        if self.config.bar.enabled && !self.lock.active && bar_refresh {
            let title = self.focused_window_title();
            let occupied = self.workspace_occupancy();
            let model = BarModel {
                workspaces: self.workspace_count,
                active_workspace: self.active_workspace,
                occupied: &occupied,
                title: title.as_deref(),
                status: &self.bar_status,
                clock: &clock,
            };
            if let Some(frame) = bar::render(
                &self.fonts,
                &self.config,
                size,
                &model,
                self.active_theme.as_deref(),
                glass,
            ) {
                self.overlays.bar = Some(frame.pixmap);
                self.bar_hits = frame.hits;
                regions.extend(frame.regions);
            }
            self.overlays.bar_minute = clock;
        } else if !self.config.bar.enabled || self.lock.active {
            self.overlays.bar = None;
            self.bar_hits.clear();
        }

        let minute = crate::lock::clock_strings().0.clone();
        let minute_key = minute
            .split(':')
            .next()
            .and_then(|hour| hour.parse::<i64>().ok())
            .unwrap_or(-1);
        let lock_refresh = self
            .overlays
            .lock_rendered_at
            .map(|rendered| rendered.elapsed() >= Duration::from_secs(1))
            .unwrap_or(true)
            || self.overlays.lock_minute != minute_key;
        if self.lock.active && (self.overlays.dirty || lock_refresh) {
            self.overlays.lock = self.lock.render(
                &self.fonts,
                &self.config,
                self.active_theme.as_deref(),
                size,
                glass,
            );
            self.overlays.lock_rendered_at = Some(Instant::now());
            self.overlays.lock_minute = minute_key;
        } else if !self.lock.active {
            self.overlays.lock = None;
            self.overlays.lock_rendered_at = None;
        }
        if self.overlays.lock.is_some() {
            regions.extend(self.lock.glass_regions(&self.config, size));
        }

        self.overlays.glass_regions = regions;
        self.overlays.dirty = false;
    }

    pub(crate) fn cursor_pixmap(&mut self, scale: f64) -> Option<&Pixmap> {
        if self.cursor_hidden {
            return None;
        }
        if self
            .cursor_cache
            .as_ref()
            .is_none_or(|(cached_scale, _, _, _)| (*cached_scale - scale).abs() > f64::EPSILON)
        {
            let (pixmap, xhot, yhot) = self.cursor_theme.pixmap(scale)?;
            self.cursor_cache = Some((scale, pixmap, xhot, yhot));
        }
        self.cursor_cache.as_ref().map(|(_, pixmap, _, _)| pixmap)
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

    fn managed_location_for_surface(&self, surface: &WlSurface) -> Option<Point<i32, Logical>> {
        self.windows
            .iter()
            .find(|entry| {
                entry
                    .window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .map(|entry| entry.location)
    }

    fn update_transient_parent(&mut self, surface: &ToplevelSurface) {
        let root = surface.wl_surface().clone();
        let parent = surface.parent();
        let parent_location = parent
            .as_ref()
            .and_then(|parent| self.managed_location_for_surface(parent));

        let Some(index) = self.windows.iter().position(|entry| {
            entry
                .window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == &root)
        }) else {
            return;
        };

        if let Some(parent_workspace) = parent.as_ref().and_then(|parent| {
            self.windows
                .iter()
                .find(|entry| {
                    entry
                        .window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.wl_surface() == parent)
                })
                .map(|entry| entry.workspace)
        }) {
            self.windows[index].workspace = parent_workspace;
        }
        self.windows[index].parent_surface = parent;
        let Some(parent_location) = parent_location else {
            return;
        };

        let location = parent_location + Size::from((WINDOW_CASCADE_STEP, WINDOW_CASCADE_STEP));
        self.windows[index].location = location;
        let window = self.windows[index].window.clone();
        if self.space.element_location(&window).is_some()
            && !self.windows[index].minimized
            && self.windows[index].workspace == self.active_workspace
        {
            let content_location = self.content_location(&window, location);
            self.space.map_element(window, content_location, false);
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

        let family_windows = self
            .windows
            .iter()
            .filter(|entry| {
                !entry.minimized
                    && entry
                        .window
                        .toplevel()
                        .is_some_and(|surface| family.contains(surface.wl_surface()))
            })
            .map(|entry| entry.window.clone())
            .collect::<Vec<_>>();

        for member in family_windows {
            self.space.raise_element(&member, member == *window);
        }
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
        let animate = self.animations.enabled();

        let mut removed_any = false;
        for entry in &mut self.windows {
            let belongs_to_family = entry
                .window
                .toplevel()
                .is_some_and(|toplevel| family.contains(toplevel.wl_surface()));
            if !belongs_to_family || entry.minimized {
                continue;
            }

            if animate {
                let id = entry.id;
                if !self.animations.begin_minimize(id) {
                    self.space.unmap_elem(&entry.window);
                    removed_any = true;
                }
            } else if self.space.element_location(&entry.window).is_some() {
                self.space.unmap_elem(&entry.window);
                removed_any = true;
            }
            entry.minimized = true;
            entry.minimized_order = Some(order);
        }

        if removed_any {
            self.focus_topmost_window();
        }
        self.overlays.dirty = true;
        self.overlays.window_masks_dirty = true;
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
            .filter(|entry| entry.minimized && entry.workspace == self.active_workspace)
            .max_by_key(|entry| entry.minimized_order)
            .and_then(|entry| entry.window.toplevel().map(|s| s.wl_surface().clone()));

        let Some(target) = target else {
            return;
        };
        let root = self.root_surface_for_toplevel(&target);
        let family = self.window_family_surfaces(&root);
        let mut restored = Vec::new();

        let mut restored_ids = Vec::new();
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
            restored_ids.push(entry.id);
            restored.push((entry.window.clone(), entry.location));
        }

        for (window, location) in &restored {
            let content_location = self.content_location(window, *location);
            self.space
                .map_element(window.clone(), content_location, false);
        }
        for id in restored_ids {
            self.animations.begin_restore(id);
        }

        if let Some(window) = self.managed_window_for_surface(&root) {
            self.raise_window_family(&window);
            if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel()) {
                keyboard.set_focus(
                    self,
                    Some(toplevel.wl_surface().clone()),
                    SERIAL_COUNTER.next_serial(),
                );
            }
        }
    }

    fn close_focused_window(&mut self) {
        let id = self
            .space
            .elements()
            .last()
            .and_then(|window| self.windows.iter().find(|entry| entry.window == *window))
            .map(|entry| entry.id);
        if let Some(id) = id {
            self.close_window_by_id(id);
        }
    }

    fn decoration_hit_test(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(Window, DecorationAction)> {
        for window in self.space.elements().rev() {
            if !Self::uses_server_decorations(window) {
                continue;
            }
            let Some(entry) = self.windows.iter().find(|entry| entry.window == *window) else {
                continue;
            };
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

    fn decoration_render_elements(
        &self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    ) -> Vec<BubbleRenderElement> {
        let mut elements = Vec::new();
        let scale = self.output.current_scale().fractional_scale();
        let now = Instant::now();
        let focused_surface = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus());

        // Front-to-back: the topmost window is pushed first so its
        // decorations stay above the decorations of lower windows.
        for window in self.space.elements().rev() {
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
            let outer_height = window.geometry().size.h + TITLEBAR_HEIGHT;
            let transform = self.animation_transform(entry, now);
            let focused = focused_surface.as_ref().is_some_and(|focused| {
                window
                    .toplevel()
                    .is_some_and(|surface| surface.wl_surface() == focused)
            });

            let palette = self.active_theme.as_deref();
            let opacity = self.window_opacity();
            let titlebar_opacity = self.config.decoration.titlebar_opacity.clamp(0.0, 1.0);
            let titlebar_alpha = opacity * titlebar_opacity * if focused { 1.0 } else { 0.92 };
            let titlebar_color = palette
                .map(|theme| {
                    let color = if focused {
                        theme.colors.surface_elevated
                    } else {
                        theme.colors.surface_overlay
                    };
                    Color32F::new(
                        color.r as f32 / 255.0,
                        color.g as f32 / 255.0,
                        color.b as f32 / 255.0,
                        titlebar_alpha,
                    )
                })
                .unwrap_or_else(|| {
                    parse_color(
                        if focused {
                            &self.config.decoration.active_color
                        } else {
                            &self.config.decoration.inactive_color
                        },
                        titlebar_alpha,
                    )
                });
            let accent = palette
                .map(|theme| {
                    Color32F::new(
                        theme.colors.primary.r as f32 / 255.0,
                        theme.colors.primary.g as f32 / 255.0,
                        theme.colors.primary.b as f32 / 255.0,
                        1.0,
                    )
                })
                .unwrap_or_else(|| parse_color(&self.config.decoration.accent_color, 1.0));
            let outline = if self.config.decoration.outline_color.trim().is_empty() {
                accent
            } else {
                parse_color(&self.config.decoration.outline_color, 1.0)
            };
            let outline_alpha = self.config.decoration.outline_opacity.clamp(0.0, 1.0)
                * if focused { 0.9 } else { 0.38 };

            let mut local: Vec<BubbleRenderElement> = Vec::new();

            let close_x =
                entry.location.x + width - DECORATION_BUTTON_MARGIN - DECORATION_BUTTON_SIZE;
            let maximize_x = close_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let minimize_x = maximize_x - DECORATION_BUTTON_GAP - DECORATION_BUTTON_SIZE;
            let button_y = entry.location.y + (TITLEBAR_HEIGHT - DECORATION_BUTTON_SIZE) / 2;

            for (x, color) in [
                (
                    minimize_x,
                    palette
                        .map(|theme| color32_from_rgb(theme.colors.warning))
                        .unwrap_or_else(|| Color32F::new(0.86, 0.68, 0.24, 1.0)),
                ),
                (
                    maximize_x,
                    palette
                        .map(|theme| color32_from_rgb(theme.colors.success))
                        .unwrap_or_else(|| Color32F::new(0.24, 0.68, 0.48, 1.0)),
                ),
                (
                    close_x,
                    palette
                        .map(|theme| color32_from_rgb(theme.colors.error))
                        .unwrap_or_else(|| Color32F::new(0.82, 0.28, 0.31, 1.0)),
                ),
            ] {
                if let Some(element) = solid_decoration(
                    (x, button_y).into(),
                    (DECORATION_BUTTON_SIZE, DECORATION_BUTTON_SIZE).into(),
                    color,
                    scale,
                ) {
                    push_decoration(&mut local, element, transform, scale);
                }
            }

            // The title stays crisp and readable on the glass strip.
            if transform.is_identity() {
                let reserved = DECORATION_BUTTON_MARGIN
                    + 3 * DECORATION_BUTTON_SIZE
                    + 2 * DECORATION_BUTTON_GAP
                    + 14;
                let text_width = width - reserved - 12;
                if text_width > 24
                    && let Some(mut canvas) = Canvas::new(text_width, TITLEBAR_HEIGHT)
                {
                    let font_size = self.config.theme.font_size.clamp(8, 16) as f32;
                    let title = self.window_title(window);
                    let mut display = String::new();
                    for character in title.chars() {
                        display.push(character);
                        if canvas.text_width(&self.fonts, &display, font_size, false)
                            > text_width - 6
                        {
                            display.pop();
                            display.push('…');
                            break;
                        }
                    }
                    let text_color = palette
                        .map(|theme| {
                            let color = theme.colors.on_surface;
                            UiColor::rgba(
                                color.r,
                                color.g,
                                color.b,
                                if focused { 235 } else { 155 },
                            )
                        })
                        .unwrap_or_else(|| {
                            UiColor::from_css(
                                &self.config.decoration.text_color,
                                if focused { 0.92 } else { 0.62 },
                            )
                        });
                    canvas.text(
                        &self.fonts,
                        &display,
                        0,
                        (TITLEBAR_HEIGHT - font_size as i32) / 2,
                        font_size,
                        text_color,
                        false,
                    );
                    let pixmap = canvas.into_pixmap((entry.location.x + 10, entry.location.y));
                    if let Some(element) = pixmap.element(renderer, scale) {
                        local.push(element.into());
                    }
                }
            }

            let glow = if self.config.effects.enabled && self.config.effects.glow {
                0.16
            } else {
                0.0
            };
            if glow > 0.0
                && let Some(element) = solid_decoration(
                    (entry.location.x, entry.location.y + TITLEBAR_HEIGHT - 4).into(),
                    (width, 3).into(),
                    Color32F::new(accent.r(), accent.g(), accent.b(), glow),
                    scale,
                )
            {
                push_decoration(&mut local, element, transform, scale);
            }
            if let Some(element) = solid_decoration(
                (entry.location.x, entry.location.y + TITLEBAR_HEIGHT - 1).into(),
                (width, 1).into(),
                Color32F::new(
                    accent.r(),
                    accent.g(),
                    accent.b(),
                    if focused { 0.95 } else { 0.42 },
                ),
                scale,
            ) {
                push_decoration(&mut local, element, transform, scale);
            }
            // A thin BubbleColor outline follows the whole window, not only
            // the titlebar. Corner masks round it off afterwards.
            for (location, size) in [
                (entry.location, (width, 1)),
                (
                    (entry.location.x, entry.location.y + outer_height - 1).into(),
                    (width, 1),
                ),
                (entry.location, (1, outer_height)),
                (
                    (entry.location.x + width - 1, entry.location.y).into(),
                    (1, outer_height),
                ),
            ] {
                if let Some(element) = solid_decoration(
                    location,
                    size.into(),
                    Color32F::new(outline.r(), outline.g(), outline.b(), outline_alpha),
                    scale,
                ) {
                    push_decoration(&mut local, element, transform, scale);
                }
            }

            // The glass strip is the bottom layer of the decoration stack.
            if let Some(element) = solid_decoration(
                entry.location,
                (width, TITLEBAR_HEIGHT).into(),
                titlebar_color,
                scale,
            ) {
                push_decoration(&mut local, element, transform, scale);
            }

            elements.extend(local);
        }

        elements
    }

    fn handle_decoration_action(&mut self, window: &Window, action: DecorationAction) {
        self.raise_window_family(window);

        let Some(surface) = window.toplevel() else {
            return;
        };
        match action {
            DecorationAction::Close => {
                let id = self
                    .windows
                    .iter()
                    .find(|entry| entry.window == *window)
                    .map(|entry| entry.id);
                if let Some(id) = id {
                    self.close_window_by_id(id);
                }
            }
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
        let Some(bounds) = self.space.output_geometry(&self.output) else {
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
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(window) = self.managed_window_for_surface(&root) else {
            return;
        };
        let Some(window_geometry) = self.space.element_geometry(&window) else {
            return;
        };
        let Some(mut target) = self.space.output_geometry(&self.output) else {
            return;
        };

        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= window_geometry.loc;
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
        if let Some(entry) = self
            .windows
            .iter_mut()
            .find(|entry| entry.window == *window)
        {
            entry.location = location;
        }
        let content_location = self.content_location(window, location);
        self.space
            .map_element(window.clone(), content_location, activate);
        self.overlays.window_masks_dirty = true;
    }

    /// Switches to a workspace and remaps only that workspace's windows.
    ///
    /// Windows keep their frame geometry and stacking order while they are
    /// absent from the Space, so switching back restores the exact desktop.
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

        let direction = if index > self.active_workspace {
            1.0
        } else {
            -1.0
        };
        self.active_workspace = index;
        self.remap_active_workspace();
        self.focus_topmost_window();
        self.animations.begin_workspace(direction);
        self.overlays.dirty = true;
        self.overlays.window_masks_dirty = true;
        Ok(())
    }

    /// Makes the Space contain exactly the active workspace's live windows.
    fn remap_active_workspace(&mut self) {
        let windows = self
            .windows
            .iter()
            .map(|entry| (entry.window.clone(), entry.workspace))
            .collect::<Vec<_>>();
        for (window, workspace) in &windows {
            if *workspace != self.active_workspace && self.space.element_location(window).is_some()
            {
                self.space.unmap_elem(window);
            }
        }

        let visible = self
            .windows
            .iter()
            .filter(|entry| entry.workspace == self.active_workspace && !entry.minimized)
            .filter(|entry| {
                entry.window.toplevel().is_some_and(|toplevel| {
                    with_renderer_surface_state(toplevel.wl_surface(), |state| {
                        state.buffer().is_some()
                    })
                    .unwrap_or(false)
                })
            })
            .map(|entry| (entry.window.clone(), entry.location))
            .collect::<Vec<_>>();
        for (window, location) in visible {
            let content_location = self.content_location(&window, location);
            self.space.map_element(window, content_location, false);
        }
        self.overlays.window_masks_dirty = true;
    }

    fn move_focused_to_workspace(&mut self, index: u32) {
        if index >= self.workspace_count {
            return;
        }
        let Some(target) = self.space.elements().last().cloned() else {
            return;
        };
        let Some(entry) = self.windows.iter_mut().find(|entry| entry.window == target) else {
            return;
        };
        if entry.workspace == index {
            return;
        }
        entry.workspace = index;
        if index != self.active_workspace {
            self.space.unmap_elem(&target);
            self.focus_topmost_window();
        }
        self.overlays.dirty = true;
    }

    fn focus_next_window(&mut self) {
        let windows = self.space.elements().cloned().collect::<Vec<_>>();
        if windows.len() < 2 {
            return;
        }
        let target = windows[0].clone();
        self.raise_window_family(&target);
        if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), target.toplevel()) {
            keyboard.set_focus(
                self,
                Some(toplevel.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
        self.overlays.dirty = true;
    }

    fn window_title(&self, window: &Window) -> String {
        window
            .toplevel()
            .and_then(|toplevel| {
                with_states(toplevel.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|data| {
                            let data = data.lock().unwrap();
                            data.title.clone().or_else(|| data.app_id.clone())
                        })
                })
            })
            .unwrap_or_else(|| "Fenêtre".into())
    }

    fn focused_window_title(&self) -> Option<String> {
        self.space
            .elements()
            .last()
            .map(|window| self.window_title(window))
    }

    fn window_list_snapshot(&self) -> Vec<WindowSummary> {
        let focused = self
            .space
            .elements()
            .last()
            .and_then(|window| self.windows.iter().find(|entry| entry.window == *window))
            .map(|entry| entry.id);
        let mut entries = self.windows.iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| {
            if Some(entry.id) == focused {
                0
            } else if entry.workspace == self.active_workspace && !entry.minimized {
                1
            } else {
                2
            }
        });
        entries
            .into_iter()
            .map(|entry| WindowSummary {
                id: entry.id,
                title: self.window_title(&entry.window),
                workspace: entry.workspace,
                minimized: entry.minimized,
                active: Some(entry.id) == focused,
            })
            .collect()
    }

    /// Stable, top-to-bottom list of every managed window.
    fn window_summaries(&self) -> Vec<PopupItem> {
        let mut ordered = Vec::new();
        for window in self.space.elements().rev() {
            if let Some(entry) = self.windows.iter().find(|entry| entry.window == *window)
                && !ordered.contains(&entry.id)
            {
                ordered.push(entry.id);
            }
        }
        for entry in self.windows.iter().rev() {
            if !ordered.contains(&entry.id) {
                ordered.push(entry.id);
            }
        }

        let focused = self
            .space
            .elements()
            .last()
            .and_then(|window| self.windows.iter().find(|entry| entry.window == *window))
            .map(|entry| entry.id);
        let mut items = Vec::new();
        for id in ordered {
            let Some(entry) = self.windows.iter().find(|entry| entry.id == id) else {
                continue;
            };
            let detail = if entry.minimized {
                format!("réduite · espace {}", entry.workspace + 1)
            } else if Some(id) == focused {
                "active".into()
            } else {
                format!("espace {}", entry.workspace + 1)
            };
            items.push(
                PopupItem::new(
                    self.window_title(&entry.window),
                    detail,
                    Icon::Window,
                    MenuAction::FocusWindow(id),
                )
                .with_window_buttons(MenuAction::MinimizeWindow(id), MenuAction::CloseWindow(id)),
            );
        }
        if items.is_empty() {
            items.push(PopupItem {
                label: "Aucune fenêtre ouverte".into(),
                detail: String::new(),
                icon: Icon::Window,
                action: MenuAction::None,
                minimize: None,
                close: None,
                enabled: false,
            });
        }
        items
    }

    fn bar_hit(&self, action: BarAction) -> Option<&BarHit> {
        self.bar_hits.iter().find(|hit| hit.action == action)
    }

    fn menu_anchor(&self, action: BarAction) -> (i32, i32) {
        let size = self.output_size();
        if let Some(hit) = self.bar_hit(action) {
            return (hit.x, hit.y + hit.height + 6);
        }
        (
            size.w - 376,
            self.config.bar.margin * 2 + self.config.bar.height + 6,
        )
    }

    fn open_desktop_menu(&mut self) {
        let size = self.output_size();
        self.context_menu = PopupMenu::desktop(
            self.pointer_location.x as i32,
            self.pointer_location.y as i32,
            size,
        );
        self.overlays.dirty = true;
    }

    fn open_window_menu(&mut self) {
        let size = self.output_size();
        let items = self.window_summaries();
        let count = items.iter().filter(|item| item.enabled).count();
        let subtitle = format!(
            "{} fenêtre{} · espace {}/{}",
            count,
            if count > 1 { "s" } else { "" },
            self.active_workspace + 1,
            self.workspace_count
        );
        let (x, y) = self.menu_anchor(BarAction::WindowMenu);
        self.context_menu = PopupMenu::windows(x, y, size, "Fenêtres".into(), subtitle, items);
        self.overlays.dirty = true;
    }

    fn open_network_menu(&mut self) {
        let size = self.output_size();
        let (x, y) = self.menu_anchor(BarAction::Network);
        self.context_menu = PopupMenu::network(x, y, size, &self.bar_status);
        if let Some(runtime) = self.status_runtime.as_ref() {
            runtime.refresh();
        }
        self.overlays.dirty = true;
    }

    fn execute_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::None | MenuAction::Close => {}
            MenuAction::Terminal => {
                if let Err(error) = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&self.config.core.terminal)
                    .spawn()
                {
                    warn!(%error, "cannot launch the configured terminal");
                }
            }
            MenuAction::Launcher => {
                if self.config.launcher.enabled {
                    self.launcher.toggle();
                }
            }
            MenuAction::Settings => {
                if let Err(error) = std::process::Command::new("bubble-settings").spawn() {
                    warn!(%error, "cannot launch bubble-settings");
                }
            }
            MenuAction::Lock => self.lock_session(),
            MenuAction::Reload => self.reload_config(),
            MenuAction::FocusWindow(id) => self.focus_window_by_id(id),
            MenuAction::MinimizeWindow(id) => self.minimize_window_by_id(id),
            MenuAction::CloseWindow(id) => self.close_window_by_id(id),
            MenuAction::WifiConnect(ssid) => {
                crate::status::connect_wifi(&ssid);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::WifiConnectAsk(ssid) => {
                crate::status::connect_wifi_ask(&self.config.core.terminal, &ssid);
            }
            MenuAction::WifiConnectSaved(name) => {
                crate::status::connect_saved(&name);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::WifiDisconnect => {
                crate::status::disconnect_wifi();
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::WifiToggle => {
                crate::status::set_wifi_enabled(!self.bar_status.wifi_enabled);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::BluetoothConnect(address) => {
                crate::status::connect_bluetooth(&address);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::BluetoothDisconnect(address) => {
                crate::status::disconnect_bluetooth(&address);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::BluetoothToggle => {
                crate::status::set_bluetooth_enabled(!self.bar_status.bluetooth_enabled);
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::BluetoothScan => {
                crate::status::scan_bluetooth();
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                }
            }
            MenuAction::NetworkSettings => {
                crate::status::open_network_settings(&self.config.core.terminal);
            }
        }
        self.overlays.dirty = true;
    }

    fn focus_window_by_id(&mut self, id: u64) {
        let Some(index) = self.windows.iter().position(|entry| entry.id == id) else {
            return;
        };
        let workspace = self.windows[index].workspace;
        self.windows[index].minimized = false;
        self.windows[index].minimized_order = None;
        let window = self.windows[index].window.clone();
        self.active_workspace = workspace;
        self.remap_active_workspace();
        self.raise_window_family(&window);
        if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel()) {
            keyboard.set_focus(
                self,
                Some(toplevel.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
        self.overlays.dirty = true;
    }

    fn minimize_window_by_id(&mut self, id: u64) {
        let toplevel = self
            .windows
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.window.toplevel())
            .cloned();
        if let Some(toplevel) = toplevel {
            self.minimize_window(&toplevel);
        }
    }

    fn close_window_by_id(&mut self, id: u64) {
        let Some(entry) = self.windows.iter().find(|entry| entry.id == id) else {
            return;
        };
        if self.animations.begin_close(id) {
            self.overlays.dirty = true;
            return;
        }
        if let Some(toplevel) = entry.window.toplevel() {
            toplevel.send_close();
        }
    }

    /// Outer frame rectangle, including server-side decorations.
    fn window_outer_rect(&self, entry: &ManagedWindow) -> Rectangle<i32, Logical> {
        let geometry = entry.window.geometry();
        let height = geometry.size.h
            + if Self::uses_server_decorations(&entry.window) {
                TITLEBAR_HEIGHT
            } else {
                0
            };
        Rectangle::new(entry.location, Size::from((geometry.size.w, height)))
    }

    /// Global client surface opacity. Transparency effects opt in; the
    /// compositor falls back to fully opaque windows when disabled.
    fn window_opacity(&self) -> f32 {
        if self.config.effects.enabled && self.config.effects.transparency {
            self.config.decoration.window_opacity.clamp(0.25, 1.0)
        } else {
            1.0
        }
    }

    fn animation_transform(&self, entry: &ManagedWindow, now: Instant) -> WindowTransform {
        let mut transform = self.animations.window_transform(entry.id, now);
        if entry.workspace == self.active_workspace {
            let workspace = self.animations.workspace_transform(now);
            transform.scale *= workspace.scale;
            transform.alpha *= workspace.alpha;
            transform.offset_x += workspace.offset_x;
            transform.offset_y += workspace.offset_y;
        }
        transform
    }

    /// Applies the animation transform to a logical rectangle.
    fn transformed_rect(
        &self,
        rect: Rectangle<i32, Logical>,
        transform: WindowTransform,
    ) -> Rectangle<i32, Logical> {
        let center_x = rect.loc.x as f32 + rect.size.w as f32 / 2.0;
        let center_y = rect.loc.y as f32 + rect.size.h as f32 / 2.0;
        let width = (rect.size.w as f32 * transform.scale).round().max(1.0) as i32;
        let height = (rect.size.h as f32 * transform.scale).round().max(1.0) as i32;
        Rectangle::new(
            Point::from((
                (center_x - width as f32 / 2.0 + transform.offset_x).round() as i32,
                (center_y - height as f32 / 2.0 + transform.offset_y).round() as i32,
            )),
            Size::from((width, height)),
        )
    }

    /// Builds four wallpaper-sampled corner masks for every visible window.
    ///
    /// The masks are composited above the window frame, which gives rounded
    /// corners without requiring a scene capture or a GL clipping shader.
    fn window_corner_masks(&self) -> Vec<Pixmap> {
        if !self.config.decoration.rounded_corners {
            return Vec::new();
        }
        let source = self
            .wallpaper
            .as_ref()
            .and_then(|wallpaper| wallpaper.source.as_ref());
        let radius = self.config.decoration.corner_radius.max(0);
        if radius == 0 {
            return Vec::new();
        }
        let now = Instant::now();
        let mut masks = Vec::new();
        for entry in &self.windows {
            if entry.workspace != self.active_workspace || entry.minimized {
                continue;
            }
            if self.space.element_location(&entry.window).is_none() {
                continue;
            }
            let transform = self.animation_transform(entry, now);
            let rect = self.transformed_rect(self.window_outer_rect(entry), transform);
            let radius = radius.min(rect.size.w / 2).min(rect.size.h / 2).max(1);
            let outline = if self.config.decoration.outline_color.trim().is_empty() {
                self.active_theme
                    .as_deref()
                    .map(|theme| {
                        let color = theme.colors.primary;
                        UiColor::rgba(
                            color.r,
                            color.g,
                            color.b,
                            (self.config.decoration.outline_opacity.clamp(0.0, 1.0) * 255.0) as u8,
                        )
                    })
                    .unwrap_or_else(|| {
                        UiColor::from_css(
                            &self.config.theme.accent,
                            self.config.decoration.outline_opacity.clamp(0.0, 1.0),
                        )
                    })
            } else {
                UiColor::from_css(
                    &self.config.decoration.outline_color,
                    self.config.decoration.outline_opacity.clamp(0.0, 1.0),
                )
            };
            for corner in [
                Corner::TopLeft,
                Corner::TopRight,
                Corner::BottomLeft,
                Corner::BottomRight,
            ] {
                if let Some(outline_arc) = rounded_outline_corner(rect, radius, corner, outline) {
                    masks.push(outline_arc);
                }
                if self.config.effects.enabled
                    && let Some(source) = source
                    && let Some(mask) = corner_mask(source, rect, radius, corner)
                {
                    masks.push(mask);
                }
            }
        }
        masks
    }

    fn finish_minimized_window(&mut self, id: u64) {
        if let Some(entry) = self.windows.iter().find(|entry| entry.id == id) {
            if self.space.element_location(&entry.window).is_some() {
                self.space.unmap_elem(&entry.window);
            }
            self.focus_topmost_window();
        }
        self.animations.remove(id);
        self.overlays.dirty = true;
    }

    fn tick_animations(&mut self) {
        let now = Instant::now();
        for (id, action) in self.animations.tick(now) {
            match action {
                FinishAction::Minimize => self.finish_minimized_window(id),
                FinishAction::Close => {
                    if let Some(toplevel) = self
                        .windows
                        .iter()
                        .find(|entry| entry.id == id)
                        .and_then(|entry| entry.window.toplevel())
                    {
                        toplevel.send_close();
                    }
                }
                FinishAction::None => self.animations.remove(id),
            }
        }
        if self.animations.active(now) {
            self.overlays.window_masks_dirty = true;
        }
    }

    fn workspace_occupancy(&self) -> Vec<bool> {
        (0..self.workspace_count)
            .map(|index| self.windows.iter().any(|entry| entry.workspace == index))
            .collect()
    }

    fn focus_topmost_window(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let target = self.space.elements().last().cloned();

        if let Some(window) = target.as_ref() {
            self.space.raise_element(window, true);
            self.configure_toplevels();
        }

        if let Some(keyboard) = self.keyboard.clone() {
            let focus = target.and_then(|window| {
                window
                    .toplevel()
                    .map(|toplevel| toplevel.wl_surface().clone())
            });
            keyboard.set_focus(self, focus, serial);
        }
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
        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);

        if !has_buffer {
            if self.space.element_location(&window).is_some() {
                self.space.unmap_elem(&window);
                self.focus_topmost_window();
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
            let window_id = self.windows[index].id;
            let activate = first_map || self.space.elements().next().is_none();
            self.windows[index].mapped_once = true;
            self.space
                .map_element(window.clone(), content_location, activate);
            if first_map {
                self.animations.begin_open(window_id);
            }
            if activate {
                if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel())
                {
                    keyboard.set_focus(
                        self,
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
    }

    fn begin_move(&mut self, surface: &ToplevelSurface, seat: &Seat<Self>, serial: Serial) {
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
        self.space.refresh();
        info!(width = size.w, height = size.h, "Winit output mode changed");
    }

    fn focus_window_under_pointer(&mut self, serial: Serial) {
        if let Some((surface, _)) = self.layer_pointer_focus(self.pointer_location) {
            if let Some(keyboard) = self.keyboard.clone() {
                keyboard.set_focus(self, Some(surface), serial);
            }
            return;
        }
        let target = self
            .space
            .element_under(self.pointer_location)
            .map(|(window, _location)| window.clone());
        let Some(window) = target else {
            return;
        };

        self.raise_window_family(&window);

        if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel()) {
            keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
        }
    }

    fn ensure_layer_initial_configure(&mut self, surface: &WlSurface) {
        let output = self
            .space
            .outputs()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned();
        let Some(output) = output else {
            return;
        };
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });
        let mut map = layer_map_for_output(&output);
        map.arrange();
        if !initial_configure_sent {
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                layer.layer_surface().send_configure();
            }
        }
    }
}

impl WlrLayerShellHandler for CompositorStateData {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: smithay::wayland::shell::wlr_layer::LayerSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: WlrLayer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.output.clone());
        let mut map = layer_map_for_output(&output);
        if let Err(error) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            warn!(%error, "cannot map a layer surface");
        }
    }

    fn layer_destroyed(&mut self, surface: smithay::wayland::shell::wlr_layer::LayerSurface) {
        if let Some((mut map, layer)) = self.space.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer))
        }) {
            map.unmap_layer(&layer);
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
        self.ensure_layer_initial_configure(surface);

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
        self.windows.push(ManagedWindow {
            id: self.next_window_id,
            window,
            location,
            mapped_once: false,
            restore_geometry: None,
            parent_surface: surface.parent(),
            minimized: false,
            minimized_order: None,
            workspace: self.active_workspace,
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

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        self.overlays.dirty = true;
    }

    fn app_id_changed(&mut self, _surface: ToplevelSurface) {
        self.overlays.dirty = true;
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
            self.animations.remove(entry.id);
        }

        if self
            .resize_session
            .as_ref()
            .is_some_and(|session| session.surface == root)
        {
            self.resize_session = None;
        }

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
        let Some(window) = self.managed_window_for_surface(&root) else {
            return;
        };
        if self.space.element_location(&window).is_none() {
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

impl SeatHandler for CompositorStateData {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        match image {
            CursorImageStatus::Hidden => {
                self.cursor_hidden = true;
            }
            CursorImageStatus::Named(icon) => {
                self.cursor_hidden = false;
                self.cursor_theme.set_icon(icon);
                self.cursor_cache = None;
            }
            CursorImageStatus::Surface(_) => {
                // Client-provided cursor surfaces are not imported yet; keep a
                // legible themed arrow instead of leaving a stale cursor.
                self.cursor_hidden = false;
                self.cursor_theme
                    .set_icon(smithay::input::pointer::CursorIcon::Default);
                self.cursor_cache = None;
            }
        }
    }
}

fn color32_from_rgb(color: Rgb) -> Color32F {
    Color32F::new(
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    )
}

fn solid_decoration(
    location: Point<i32, Logical>,
    size: Size<i32, Logical>,
    color: Color32F,
    scale: f64,
) -> Option<SolidColorRenderElement> {
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    let buffer = SolidColorBuffer::new(size, color);
    let physical_location = location.to_physical_precise_round(scale);
    Some(SolidColorRenderElement::from_buffer(
        &buffer,
        physical_location,
        scale,
        1.0,
        Kind::Unspecified,
    ))
}

fn push_decoration(
    elements: &mut Vec<BubbleRenderElement>,
    element: SolidColorRenderElement,
    transform: WindowTransform,
    scale: f64,
) {
    if transform.is_identity() {
        elements.push(element.into());
    } else {
        elements.push(BubbleRenderElement::AnimatedSolid(AnimatedElement::new(
            element,
            f64::from(transform.scale),
            transform.alpha,
            Point::from((
                f64::from(transform.offset_x) * scale,
                f64::from(transform.offset_y) * scale,
            )),
        )));
    }
}

#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn wallpaper_pixel(source: &WallpaperSource, x: i32, y: i32) -> [u8; 4] {
    let width = source.size.w.max(1);
    let height = source.size.h.max(1);
    let x = x.clamp(0, width - 1);
    let y = y.clamp(0, height - 1);
    let offset = ((y * width + x) * 4) as usize;
    [
        source.rgba.get(offset).copied().unwrap_or(0),
        source.rgba.get(offset + 1).copied().unwrap_or(0),
        source.rgba.get(offset + 2).copied().unwrap_or(0),
        source.rgba.get(offset + 3).copied().unwrap_or(255),
    ]
}

/// Builds one rounded-corner mask by sampling the wallpaper behind it.
fn corner_mask(
    source: &WallpaperSource,
    rect: Rectangle<i32, Logical>,
    radius: i32,
    corner: Corner,
) -> Option<Pixmap> {
    if radius <= 0 || rect.size.w < radius || rect.size.h < radius {
        return None;
    }
    let (origin_x, origin_y, center_x, center_y) = match corner {
        Corner::TopLeft => (rect.loc.x, rect.loc.y, radius, radius),
        Corner::TopRight => (rect.loc.x + rect.size.w - radius, rect.loc.y, 0, radius),
        Corner::BottomLeft => (rect.loc.x, rect.loc.y + rect.size.h - radius, radius, 0),
        Corner::BottomRight => (
            rect.loc.x + rect.size.w - radius,
            rect.loc.y + rect.size.h - radius,
            0,
            0,
        ),
    };
    let mut pixels = vec![0u8; (radius * radius * 4) as usize];
    let radius_squared = (radius * radius) as f32;
    for py in 0..radius {
        for px in 0..radius {
            let mut covered = 0u32;
            for sy in [0.25f32, 0.75] {
                for sx in [0.25f32, 0.75] {
                    let dx = px as f32 + sx - center_x as f32;
                    let dy = py as f32 + sy - center_y as f32;
                    if dx * dx + dy * dy > radius_squared {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let color = wallpaper_pixel(source, origin_x + px, origin_y + py);
            let offset = ((py * radius + px) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
            pixels[offset + 3] = (255 * covered / 4) as u8;
        }
    }
    Some(Pixmap::from_rgba(
        &pixels,
        (origin_x, origin_y).into(),
        (radius, radius).into(),
    ))
}

/// Builds the antialiased quarter-circle segment of a window's outline.
/// These arcs sit above the wallpaper corner masks, which otherwise cover the
/// straight border where it meets each rounded corner.
fn rounded_outline_corner(
    rect: Rectangle<i32, Logical>,
    radius: i32,
    corner: Corner,
    color: UiColor,
) -> Option<Pixmap> {
    if radius <= 0 || rect.size.w < radius || rect.size.h < radius {
        return None;
    }
    let (origin_x, origin_y, center_x, center_y) = match corner {
        Corner::TopLeft => (rect.loc.x, rect.loc.y, radius as f32, radius as f32),
        Corner::TopRight => (
            rect.loc.x + rect.size.w - radius,
            rect.loc.y,
            0.0,
            radius as f32,
        ),
        Corner::BottomLeft => (
            rect.loc.x,
            rect.loc.y + rect.size.h - radius,
            radius as f32,
            0.0,
        ),
        Corner::BottomRight => (
            rect.loc.x + rect.size.w - radius,
            rect.loc.y + rect.size.h - radius,
            0.0,
            0.0,
        ),
    };
    let mut pixels = vec![0u8; (radius * radius * 4) as usize];
    let outer = radius as f32 + 0.25;
    let inner = (radius as f32 - 1.0).max(0.0);
    for py in 0..radius {
        for px in 0..radius {
            let mut covered = 0u32;
            for sy in [0.25f32, 0.75] {
                for sx in [0.25f32, 0.75] {
                    let dx = px as f32 + sx - center_x;
                    let dy = py as f32 + sy - center_y;
                    let distance = (dx * dx + dy * dy).sqrt();
                    if (inner..=outer).contains(&distance) {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let offset = ((py * radius + px) * 4) as usize;
            pixels[offset] = color.r;
            pixels[offset + 1] = color.g;
            pixels[offset + 2] = color.b;
            pixels[offset + 3] = (u32::from(color.a) * covered / 4) as u8;
        }
    }
    Some(Pixmap::from_rgba(
        &pixels,
        (origin_x, origin_y).into(),
        (radius, radius).into(),
    ))
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

#[derive(Default)]
pub(crate) struct WaylandClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for WaylandClientState {
    fn initialized(&self, _client_id: ClientId) {
        info!("Wayland client connected");
    }

    fn disconnected(&self, _client_id: ClientId, reason: DisconnectReason) {
        info!(?reason, "Wayland client disconnected");
    }
}

delegate_compositor!(CompositorStateData);
delegate_output!(CompositorStateData);
delegate_seat!(CompositorStateData);
delegate_shm!(CompositorStateData);
delegate_data_device!(CompositorStateData);
delegate_xdg_decoration!(CompositorStateData);
delegate_xdg_shell!(CompositorStateData);
delegate_layer_shell!(CompositorStateData);

/// Selects and starts the requested compositor backend.
pub fn run(config: Config) -> Result<()> {
    let config_path = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"))
        .map(PathBuf::from)
        .or_else(|| Some(Config::default_path()));
    match config.core.backend.as_str() {
        "winit" => run_winit(config, config_path),
        "drm" => crate::drm::run(config, config_path),
        "auto" => {
            if std::env::var_os("WAYLAND_DISPLAY").is_some()
                || std::env::var_os("DISPLAY").is_some()
            {
                run_winit(config, config_path)
            } else {
                crate::drm::run(config, config_path)
            }
        }
        backend => Err(anyhow!("unsupported compositor backend: {backend}")),
    }
}

/// Starts the Winit-backed Wayland compositor and owns IPC for its complete
/// lifetime.
fn run_winit(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    install_signal_handlers();
    let (mut backend, mut winit_loop) =
        winit::init::<smithay::backend::renderer::gles::GlesRenderer>()
            .map_err(|error| anyhow!("cannot initialize the Winit/EGL backend: {error}"))?;
    let initial_size = backend.window_size();

    let mut display: Display<CompositorStateData> =
        Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

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

    let start_locked = (std::env::args().any(|argument| argument == "--locked")
        || std::env::var("BUBBLEWM_LOCKED").is_ok_and(|value| value == "1"))
        && config.lock.enabled;
    let mut state = CompositorStateData::create(
        &display_handle,
        config,
        config_path,
        output.clone(),
        start_locked,
    )?;
    state.enable_gpu_blur(true);

    let wayland_listener = ListeningSocket::bind_auto("wayland", 1..33)
        .context("cannot create a Wayland socket in XDG_RUNTIME_DIR")?;
    let wayland_display = wayland_listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("Wayland listener did not provide a display name")?;

    // The socket is advertised before starting the IPC endpoint. If IPC setup
    // fails, dropping the listener closes the Wayland socket as startup unwinds.
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

    while running {
        let status = winit_loop.dispatch_new_events(|event| {
            if matches!(&event, WinitEvent::Input(_)) {
                state.note_activity();
            }
            match event {
                WinitEvent::CloseRequested => running = false,
                WinitEvent::Resized { size, .. } => {
                    state.update_output_mode(size);
                    state.reload_wallpaper();
                    state.overlays.dirty = true;
                }
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::Focus(_) | WinitEvent::Redraw => {}
            }
        });

        if matches!(status, PumpStatus::Exit(_)) {
            break;
        }

        // IPC commands are consumed on the compositor thread. No IPC worker
        // callback accesses Smithay state directly.
        ipc_commands.dispatch_pending(|request| state.handle_ipc_request(request));

        state.housekeeping();

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
        state.refresh();
        display
            .flush_clients()
            .context("cannot flush Wayland client events")?;

        let size = backend.window_size();
        if size.w <= 0 || size.h <= 0 {
            thread::sleep(FRAME_INTERVAL);
            continue;
        }
        let damage = Rectangle::from_size(size);
        let scale = 1.0;

        state.refresh_overlays();
        state.cursor_pixmap(scale);

        {
            let (renderer, mut framebuffer) = backend
                .bind()
                .context("cannot bind the Winit render target")?;
            let elements = state.scene_elements(renderer, scale);

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .context("cannot begin a Winit frame")?;
            let background = state.clear_color();
            frame
                .clear(background, &[damage])
                .context("cannot clear the Winit frame")?;
            if !state.blanked {
                draw_render_elements(&mut frame, scale, &elements, &[damage])
                    .context("cannot render the compositor scene")?;
            }
            let sync_point = frame.finish().context("cannot finish the Winit frame")?;
            loop {
                if sync_point.wait().is_ok() {
                    break;
                }
            }
        }

        for surface in state.xdg_shell_state.toplevel_surfaces() {
            send_frames_surface_tree(
                surface.wl_surface(),
                start_time.elapsed().as_millis() as u32,
            );
        }

        backend
            .submit(Some(&[damage]))
            .context("cannot present the Winit frame")?;

        // Smithay's Winit event source is pumped with a zero timeout. A small
        // frame interval avoids spinning at full CPU when the host backend has
        // vsync disabled, while keeping input latency bounded.
        thread::sleep(FRAME_INTERVAL);
    }

    ipc_runtime
        .shutdown()
        .context("cannot shut down the BubbleWM IPC worker")?;
    info!("BubbleWM compositor is shutting down");
    Ok(())
}

impl CompositorStateData {
    /// Dispatches a control request on the compositor thread.
    ///
    /// Workspace and focus actions mutate live state; the remaining requests
    /// are stateless snapshots handled by [`handle_ipc_request`].
    pub(crate) fn handle_ipc_request(&mut self, request: Request) -> Response {
        let workspace_response = |active: u32, count: u32| Response::Success {
            data: ResponseData::WorkspaceState { active, count },
        };
        match request {
            Request::WorkspacePrevious => {
                let index = previous_workspace(self.active_workspace, self.workspace_count);
                match self.activate_workspace(index) {
                    Ok(()) => workspace_response(self.active_workspace, self.workspace_count),
                    Err(response) => response,
                }
            }
            Request::WorkspaceNext => {
                let index = next_workspace(self.active_workspace, self.workspace_count);
                match self.activate_workspace(index) {
                    Ok(()) => workspace_response(self.active_workspace, self.workspace_count),
                    Err(response) => response,
                }
            }
            Request::ActivateWorkspace { index } => match self.activate_workspace(index) {
                Ok(()) => workspace_response(self.active_workspace, self.workspace_count),
                Err(response) => response,
            },
            Request::ReloadConfig => {
                self.reload_config();
                Response::Success {
                    data: ResponseData::Acknowledged,
                }
            }
            Request::GetSystemStatus => {
                let status = &self.bar_status;
                Response::Success {
                    data: ResponseData::SystemStatus {
                        status: SystemStatus {
                            network_manager: status.network_manager,
                            wifi_enabled: status.wifi_enabled,
                            ssid: status.ssid.clone(),
                            signal: status.signal,
                            volume: status.volume,
                            muted: status.muted,
                        },
                    },
                }
            }
            Request::GetWindowList => Response::Success {
                data: ResponseData::WindowList {
                    windows: self.window_list_snapshot(),
                },
            },
            Request::GetNetworkList => {
                let mut networks = self.bar_status.networks.clone();
                networks.sort_by(|left, right| {
                    right
                        .active
                        .cmp(&left.active)
                        .then(right.signal.cmp(&left.signal))
                        .then(left.ssid.cmp(&right.ssid))
                });
                networks.truncate(20);
                Response::Success {
                    data: ResponseData::NetworkList {
                        networks: networks
                            .into_iter()
                            .map(|network| WifiNetworkSummary {
                                ssid: network.ssid,
                                signal: network.signal,
                                secured: network.secured,
                                active: network.active,
                            })
                            .collect(),
                        saved: self
                            .bar_status
                            .saved
                            .iter()
                            .filter(|connection| connection.kind == "802-11-wireless")
                            .take(10)
                            .map(|connection| SavedWifiSummary {
                                name: connection.name.clone(),
                                active: connection.active,
                            })
                            .collect(),
                    },
                }
            }
            Request::RefreshNetworkStatus => {
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.refresh();
                    Response::Success {
                        data: ResponseData::Acknowledged,
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "session status worker is unavailable".into(),
                        },
                    }
                }
            }
            Request::ConnectWifi { ssid } => {
                let network = self
                    .bar_status
                    .networks
                    .iter()
                    .find(|network| network.ssid == ssid);
                if let Some(network) = network
                    .filter(|_| self.bar_status.network_manager && self.bar_status.wifi_enabled)
                {
                    if let Some(runtime) = self.status_runtime.as_ref() {
                        runtime.connect_wifi(
                            ssid,
                            network.secured,
                            self.config.core.terminal.clone(),
                        );
                        Response::Success {
                            data: ResponseData::Acknowledged,
                        }
                    } else {
                        Response::Failure {
                            error: ErrorResponse {
                                code: ErrorCode::Unavailable,
                                message: "session status worker is unavailable".into(),
                            },
                        }
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "the requested Wi-Fi network is no longer available".into(),
                        },
                    }
                }
            }
            Request::ConnectSavedWifi { name } => {
                let available = self.bar_status.network_manager
                    && self.bar_status.saved.iter().any(|connection| {
                        connection.kind == "802-11-wireless" && connection.name == name
                    });
                if available {
                    if let Some(runtime) = self.status_runtime.as_ref() {
                        runtime.connect_saved_wifi(name);
                        Response::Success {
                            data: ResponseData::Acknowledged,
                        }
                    } else {
                        Response::Failure {
                            error: ErrorResponse {
                                code: ErrorCode::Unavailable,
                                message: "session status worker is unavailable".into(),
                            },
                        }
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "the saved Wi-Fi profile is no longer available".into(),
                        },
                    }
                }
            }
            Request::DisconnectWifi if self.bar_status.ssid.is_some() => {
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.disconnect_wifi();
                    Response::Success {
                        data: ResponseData::Acknowledged,
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "session status worker is unavailable".into(),
                        },
                    }
                }
            }
            Request::DisconnectWifi => Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "there is no active Wi-Fi connection".into(),
                },
            },
            Request::FocusWindow { id } => {
                self.focus_window_by_id(id);
                Response::Success {
                    data: ResponseData::Acknowledged,
                }
            }
            Request::ToggleWifi if self.bar_status.network_manager => {
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.toggle_wifi();
                    Response::Success {
                        data: ResponseData::Acknowledged,
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "session status worker is unavailable".into(),
                        },
                    }
                }
            }
            Request::ToggleWifi => Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "NetworkManager is unavailable".into(),
                },
            },
            Request::ToggleMute if self.bar_status.volume.is_some() => {
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.toggle_mute();
                    Response::Success {
                        data: ResponseData::Acknowledged,
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "session status worker is unavailable".into(),
                        },
                    }
                }
            }
            Request::ToggleMute => Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "audio status is unavailable".into(),
                },
            },
            Request::AdjustVolume { delta_percent }
                if !(-20..=20).contains(&delta_percent) || delta_percent == 0 =>
            {
                Response::Failure {
                    error: ErrorResponse {
                        code: ErrorCode::InvalidRequest,
                        message: "volume changes must be between -20 and 20 percent".into(),
                    },
                }
            }
            Request::AdjustVolume { delta_percent } if self.bar_status.volume.is_some() => {
                if let Some(runtime) = self.status_runtime.as_ref() {
                    runtime.adjust_volume(delta_percent);
                    Response::Success {
                        data: ResponseData::Acknowledged,
                    }
                } else {
                    Response::Failure {
                        error: ErrorResponse {
                            code: ErrorCode::Unavailable,
                            message: "session status worker is unavailable".into(),
                        },
                    }
                }
            }
            Request::AdjustVolume { .. } => Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "audio status is unavailable".into(),
                },
            },
            other => handle_ipc_request(
                other,
                self.theme_palette(),
                Some((self.active_workspace, self.workspace_count)),
            ),
        }
    }
}

pub(crate) fn handle_ipc_request(
    request: Request,
    palette: Option<ThemePalette>,
    workspaces: Option<(u32, u32)>,
) -> Response {
    match request {
        Request::GetCapabilities => Response::Success {
            data: ResponseData::Capabilities {
                workspace_control: workspaces.is_some(),
            },
        },
        Request::GetThemePalette => Response::Success {
            data: ResponseData::ThemePalette { palette },
        },
        Request::GetWorkspaceState => match workspaces {
            Some((active, count)) => Response::Success {
                data: ResponseData::WorkspaceState { active, count },
            },
            None => Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "workspace management is not initialized yet".into(),
                },
            },
        },
        Request::WorkspacePrevious
        | Request::WorkspaceNext
        | Request::ActivateWorkspace { .. }
        | Request::FocusWindow { .. }
        | Request::GetWindowList
        | Request::GetNetworkList
        | Request::ToggleWifi
        | Request::ToggleMute
        | Request::AdjustVolume { .. }
        | Request::RefreshNetworkStatus
        | Request::ConnectWifi { .. }
        | Request::ConnectSavedWifi { .. }
        | Request::DisconnectWifi
        | Request::ReloadConfig
        | Request::GetSystemStatus => Response::Failure {
            error: ErrorResponse {
                code: ErrorCode::Unavailable,
                message: "this action requires a live compositor".into(),
            },
        },
    }
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
        assert!(suppress_shortcut_press(&mut suppressed, Keysym::Tab));
        assert!(!suppress_shortcut_press(&mut suppressed, Keysym::Tab));
        assert_eq!(suppressed, vec![Keysym::Tab]);
        assert!(consume_shortcut_release(&mut suppressed, Keysym::Tab));
        assert!(suppressed.is_empty());
        assert!(suppress_shortcut_press(&mut suppressed, Keysym::Tab));
    }

    #[test]
    fn compositor_shortcuts_map_restore_and_close_keys() {
        let modifiers = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::Tab),
            Some(CompositorShortcut::Restore)
        );
        assert_eq!(
            compositor_shortcut(modifiers, Keysym::q),
            Some(CompositorShortcut::Close)
        );
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
    fn capability_query_reports_workspace_control_only_when_live() {
        assert_eq!(
            handle_ipc_request(Request::GetCapabilities, None, None),
            Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: false
                }
            }
        );
        assert_eq!(
            handle_ipc_request(Request::GetCapabilities, None, Some((0, 4))),
            Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: true
                }
            }
        );
    }

    #[test]
    fn workspace_state_reports_active_index_and_count() {
        assert_eq!(
            handle_ipc_request(Request::GetWorkspaceState, None, Some((2, 6))),
            Response::Success {
                data: ResponseData::WorkspaceState {
                    active: 2,
                    count: 6
                }
            }
        );
        assert_eq!(
            handle_ipc_request(Request::GetWorkspaceState, None, None),
            Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "workspace management is not initialized yet".into(),
                }
            }
        );
    }

    #[test]
    fn workspace_actions_require_a_live_compositor() {
        let requests = [
            Request::WorkspacePrevious,
            Request::WorkspaceNext,
            Request::ActivateWorkspace { index: 0 },
            Request::ReloadConfig,
        ];

        for request in requests {
            assert_eq!(
                handle_ipc_request(request, None, None),
                Response::Failure {
                    error: ErrorResponse {
                        code: ErrorCode::Unavailable,
                        message: "this action requires a live compositor".into(),
                    }
                }
            );
        }
    }

    #[test]
    fn workspace_navigation_wraps_in_both_directions() {
        assert_eq!(previous_workspace(0, 4), 3);
        assert_eq!(previous_workspace(2, 4), 1);
        assert_eq!(next_workspace(3, 4), 0);
        assert_eq!(next_workspace(1, 4), 2);
        assert_eq!(previous_workspace(0, 0), 0);
        assert_eq!(next_workspace(0, 0), 0);
    }

    #[test]
    fn workspace_shortcuts_use_raw_digits_and_shift_moves_windows() {
        let logo = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            workspace_shortcut(logo, Keysym::_1),
            Some(CompositorShortcut::Workspace(0))
        );
        assert_eq!(
            workspace_shortcut(logo, Keysym::_9),
            Some(CompositorShortcut::Workspace(8))
        );
        let logo_shift = ModifiersState {
            logo: true,
            shift: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            workspace_shortcut(logo_shift, Keysym::_4),
            Some(CompositorShortcut::MoveToWorkspace(3))
        );
        assert_eq!(
            workspace_shortcut(logo, Keysym::Left),
            Some(CompositorShortcut::WorkspacePrevious)
        );
        assert_eq!(
            workspace_shortcut(logo, Keysym::Right),
            Some(CompositorShortcut::WorkspaceNext)
        );
        assert_eq!(
            workspace_shortcut(ModifiersState::default(), Keysym::_1),
            None
        );
    }
}
