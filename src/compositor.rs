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
            utils::{draw_render_elements, on_commit_buffer_handler},
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_server::{
            Client, Display, ListeningSocket,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer, wl_seat,
                wl_surface::{self, WlSurface},
            },
        },
        winit::platform::pump_events::PumpStatus,
    },
    utils::{Rectangle, SERIAL_COUNTER, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_surface_tree_downward,
        },
        output::OutputHandler,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
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
                toplevel.send_configure();
            }
        }
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

        for window in self.space.elements().cloned().collect::<Vec<_>>() {
            let is_toplevel = window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface);
            if is_toplevel {
                window.on_commit();
            }
        }
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

        let window = Window::new_wayland_window(surface);
        let cascade = self.next_window_position % WINDOW_CASCADE_COUNT;
        self.next_window_position = self.next_window_position.wrapping_add(1);
        let offset = cascade as i32 * WINDOW_CASCADE_STEP;
        self.space
            .map_element(window.clone(), (64 + offset, 64 + offset), true);
        self.space.refresh();
        self.configure_toplevels();

        if let (Some(keyboard), Some(toplevel)) = (self.keyboard.clone(), window.toplevel()) {
            keyboard.set_focus(
                self,
                Some(toplevel.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
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
