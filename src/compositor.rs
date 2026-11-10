//! Initial nested Smithay compositor backend.
//!
//! This is deliberately a small server milestone: it creates a Winit window,
//! exports the core compositor, shm, seat and xdg-shell globals, accepts clients,
//! and renders committed xdg toplevel surfaces. Window management, output
//! advertisement, pointer focus, workspaces and layer-shell are later milestones.

use std::{sync::Arc, time::Instant};

use anyhow::{Context, Result};
use bubble_ipc::{ErrorCode, ErrorResponse, Request, Response, ResponseData};
use smithay::{
    backend::{
        input::{InputEvent, KeyboardKeyEvent},
        renderer::{
            Color32F, Frame, Renderer,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler},
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState, keyboard::FilterResult},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
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
    utils::{Rectangle, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_surface_tree_downward,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::info;

use crate::ipc::{CoreCommandReceiver, IpcRuntime};

struct CompositorStateData {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
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
    }
}

impl ShmHandler for CompositorStateData {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for CompositorStateData {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("New xdg_toplevel surface");
        surface.send_configure();
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

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
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
delegate_shm!(CompositorStateData);
delegate_seat!(CompositorStateData);
delegate_xdg_shell!(CompositorStateData);

/// Starts the initial Winit-backed Wayland compositor and owns IPC for its
/// complete lifetime.
pub fn run_winit() -> Result<()> {
    let mut display: Display<CompositorStateData> =
        Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

    let compositor_state = CompositorState::new::<CompositorStateData>(&display_handle);
    let shm_state = ShmState::new::<CompositorStateData>(&display_handle, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&display_handle, "bubblewm");

    let mut state = CompositorStateData {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<CompositorStateData>(&display_handle),
        shm_state,
        seat_state,
        seat,
    };

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
        .context("cannot initialize the virtual keyboard")?;

    let wayland_listener = ListeningSocket::bind_auto("wayland", 1..33)
        .context("cannot create a Wayland socket in XDG_RUNTIME_DIR")?;
    let wayland_display = wayland_listener
        .socket_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("Wayland listener did not provide a display name")?;

    let (mut backend, mut winit_loop) =
        winit::init::<GlesRenderer>().context("cannot initialize the Winit/EGL backend")?;

    // The socket is advertised before starting the IPC endpoint. If IPC setup
    // fails, dropping the listener closes the Wayland socket as startup unwinds.
    let (ipc_runtime, ipc_commands) =
        IpcRuntime::start_default().context("cannot initialize BubbleWM IPC")?;

    info!(
        wayland_display = %wayland_display,
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
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                // Until proper pointer-based focus is available, the first
                // mapped toplevel receives keyboard focus.
                if let Some(surface) = state.xdg_shell_state.toplevel_surfaces().first().cloned() {
                    keyboard.set_focus(
                        &mut state,
                        Some(surface.wl_surface().clone()),
                        Serial::from(0),
                    );
                }

                keyboard.input::<(), _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    Serial::from(0),
                    0,
                    |_, _, _| FilterResult::Forward,
                );
            }
            WinitEvent::Resized { .. }
            | WinitEvent::Focus(_)
            | WinitEvent::Redraw
            | WinitEvent::Input(_) => {}
            _ => {}
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
        display
            .flush_clients()
            .context("cannot flush Wayland client events")?;

        let size = backend.window_size();
        if size.w <= 0 || size.h <= 0 {
            continue;
        }
        let damage = Rectangle::from_size(size);

        {
            let (renderer, mut framebuffer) = backend
                .bind()
                .context("cannot bind the Winit render target")?;
            let elements = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|surface| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

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
