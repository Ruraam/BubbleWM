//! Initial nested Smithay compositor backend.
//!
//! This milestone creates a Winit window, exports the core compositor, shm,
//! seat, xdg-shell and wl_output globals, accepts clients, keeps a Smithay
//! desktop space, routes pointer/keyboard events, and renders mapped windows.
//! Decorations, workspaces, layer-shell and native DRM/KMS remain future work.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use bubble_ipc::{ErrorCode, ErrorResponse, Request, Response, ResponseData};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, ButtonState, Event as BackendInputEvent, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    backend::{
        renderer::{
            Color32F, Frame, Renderer,
            utils::{
                draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state,
            },
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle},
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
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Display, ListeningSocket, Resource,
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
            TraversalAction, add_post_commit_hook, with_surface_tree_downward, with_states,
        },
        output::OutputHandler,
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::info;

use crate::ipc::IpcRuntime;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_REFRESH_MILLIHZ: i32 = 60_000;
const WINDOW_CASCADE_STEP: i32 = 32;
const WINDOW_CASCADE_COUNT: u32 = 8;
const RESIZE_EDGE_TOP: u32 = 1;
const RESIZE_EDGE_BOTTOM: u32 = 2;
const RESIZE_EDGE_LEFT: u32 = 4;
const RESIZE_EDGE_RIGHT: u32 = 8;

struct ManagedWindow {
    window: Window,
    location: Point<i32, Logical>,
    mapped_once: bool,
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
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    keyboard: Option<KeyboardHandle<Self>>,
    pointer: Option<PointerHandle<Self>>,
    pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    output: Output,
    space: Space<Window>,
    windows: Vec<ManagedWindow>,
    resize_session: Option<ResizeSession>,
    next_window_position: u32,
}

impl CompositorStateData {
    fn pointer_focus(
        &self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        let (window, window_location) = self.space.element_under(location)?;
        let local_location = location - window_location.to_f64();
        let (surface, surface_location) =
            window.surface_under(local_location, WindowSurfaceType::ALL)?;
        Some((
            surface,
            window_location.to_f64() + surface_location.to_f64(),
        ))
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
                entry.window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .map(|entry| entry.window.clone())
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
        self.space.map_element(window.clone(), location, activate);
    }

    fn focus_topmost_window(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let target = self.space.elements().last().cloned();
        if let Some(keyboard) = self.keyboard.clone() {
            let focus = target
                .and_then(|window| window.toplevel().map(|toplevel| toplevel.wl_surface().clone()));
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
        let has_buffer = with_renderer_surface_state(surface, |state| state.buffer().is_some())
            .unwrap_or(false);

        if !has_buffer {
            if let Some(location) = self.space.element_location(&window) {
                self.windows[index].location = location;
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

        if self.space.element_location(&window).is_none() {
            let location = self.windows[index].location;
            let activate =
                !self.windows[index].mapped_once || self.space.elements().next().is_none();
            self.windows[index].mapped_once = true;
            self.space.map_element(window.clone(), location, activate);
            if activate {
                if let (Some(keyboard), Some(toplevel)) =
                    (self.keyboard.clone(), window.toplevel())
                {
                    keyboard.set_focus(
                        self,
                        Some(toplevel.wl_surface().clone()),
                        SERIAL_COUNTER.next_serial(),
                    );
                }
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
                location.x = session.initial_location.x
                    + (session.initial_size.w - geometry.size.w);
            }
            if session.edges & RESIZE_EDGE_TOP != 0 {
                location.y = session.initial_location.y
                    + (session.initial_size.h - geometry.size.h);
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

    fn begin_move(
        &mut self,
        surface: &ToplevelSurface,
        seat: &Seat<Self>,
        serial: Serial,
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
        let Some(initial_location) = self.space.element_location(&window) else {
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
        let Some(initial_location) = self.space.element_location(&window) else {
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
        let target = self
            .space
            .element_under(self.pointer_location)
            .map(|(window, _location)| window.clone());
        let Some(window) = target else {
            return;
        };

        self.space.raise_element(&window, true);
        self.configure_toplevels();

        if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel()) {
            keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
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
        self.space.refresh();
    }
}

impl ShmHandler for CompositorStateData {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
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
            window,
            location,
            mapped_once: false,
        });

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

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        // Popup placement and grabs are introduced with window management.
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Popup grabs are not available in this initial milestone.
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
        // Popup repositioning is not available in this initial milestone.
    }
}

impl SeatHandler for CompositorStateData {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
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
struct WaylandClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for WaylandClientState {
    fn initialized(&self, _client_id: ClientId) {
        info!("Wayland client connected");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        info!("Wayland client disconnected");
    }
}

delegate_compositor!(CompositorStateData);
delegate_output!(CompositorStateData);
delegate_seat!(CompositorStateData);
delegate_shm!(CompositorStateData);
delegate_xdg_shell!(CompositorStateData);

/// Starts the initial Winit-backed Wayland compositor and owns IPC for its
/// complete lifetime.
pub fn run_winit() -> Result<()> {
    let (mut backend, mut winit_loop) =
        winit::init::<smithay::backend::renderer::gles::GlesRenderer>()
            .map_err(|error| anyhow!("cannot initialize the Winit/EGL backend: {error}"))?;
    let initial_size = backend.window_size();

    let mut display: Display<CompositorStateData> =
        Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

    let compositor_state = CompositorState::new::<CompositorStateData>(&display_handle);
    let shm_state = ShmState::new::<CompositorStateData>(&display_handle, vec![]);
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

    let mut state = CompositorStateData {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<CompositorStateData>(&display_handle),
        shm_state,
        seat_state,
        seat,
        keyboard: None,
        pointer: None,
        pointer_location: (0.0, 0.0).into(),
        output: output.clone(),
        space: Space::default(),
        windows: Vec::new(),
        resize_session: None,
        next_window_position: 0,
    };
    state.space.map_output(&output, (0, 0));

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
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
        let status = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => running = false,
            WinitEvent::Resized { size, .. } => state.update_output_mode(size),
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                if let Some(keyboard) = state.keyboard.clone() {
                    keyboard.input::<(), _>(
                        &mut state,
                        event.key_code(),
                        event.state(),
                        SERIAL_COUNTER.next_serial(),
                        (event.time() / 1000) as u32,
                        |_, _, _| FilterResult::Forward,
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
                    state.focus_window_under_pointer(serial);
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

        if matches!(status, PumpStatus::Exit(_)) {
            break;
        }

        // IPC commands are consumed on the compositor thread. No IPC worker
        // callback accesses Smithay state directly.
        ipc_commands.dispatch_pending(handle_ipc_request);

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
        state.space.refresh();
        display
            .flush_clients()
            .context("cannot flush Wayland client events")?;

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

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .context("cannot begin a Winit frame")?;
            frame
                .clear(Color32F::new(0.075, 0.09, 0.12, 1.0), &[damage])
                .context("cannot clear the Winit frame")?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .context("cannot render Wayland surfaces")?;
            frame.finish().context("cannot finish the Winit frame")?;
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

fn handle_ipc_request(request: Request) -> Response {
    match request {
        Request::GetCapabilities => Response::Success {
            data: ResponseData::Capabilities {
                workspace_control: false,
            },
        },
        Request::GetWorkspaceState
        | Request::WorkspacePrevious
        | Request::WorkspaceNext
        | Request::ActivateWorkspace { .. } => Response::Failure {
            error: ErrorResponse {
                code: ErrorCode::Unavailable,
                message: "workspace management is not initialized yet".into(),
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
            (500.0, 500.0).into(),
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
    fn capability_query_does_not_advertise_unimplemented_workspace_control() {
        assert_eq!(
            handle_ipc_request(Request::GetCapabilities),
            Response::Success {
                data: ResponseData::Capabilities {
                    workspace_control: false
                }
            }
        );
    }

    #[test]
    fn workspace_requests_report_unavailable_until_workspace_state_exists() {
        let requests = [
            Request::GetWorkspaceState,
            Request::WorkspacePrevious,
            Request::WorkspaceNext,
            Request::ActivateWorkspace { index: 0 },
        ];

        for request in requests {
            assert_eq!(
                handle_ipc_request(request),
                Response::Failure {
                    error: ErrorResponse {
                        code: ErrorCode::Unavailable,
                        message: "workspace management is not initialized yet".into(),
                    }
                }
            );
        }
    }
}
