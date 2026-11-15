//! BubblePanel's first Wayland layer-shell prototype.
//!
//! This milestone provides an independent Wayland client, a bottom layer-shell
//! surface and a software-rendered three-zone layout. The clock is refreshed
//! on a low-overhead event-loop timeout, and theme snapshots arrive on a worker
//! thread; interactive modules follow in a later implementation step.

use std::{
    num::NonZeroU32,
    os::fd::{AsFd, AsRawFd},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bubble_ipc::{Request, Response, ResponseData, ThemePalette, unix};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{AxisScroll, BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};

const PANEL_HEIGHT: u32 = 40;
const INITIAL_WIDTH: u32 = 1280;
const MAX_BUFFER_WIDTH: u32 = 3840;

/// A half-open rectangle in panel-local logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl HitRect {
    /// Returns true when the point is inside the rectangle (right/bottom excluded).
    pub fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }
}

/// Actions that can be associated with panel regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    Launcher,
    Workspace(u8),
    Clock,
}

/// Maps a panel-local rectangle to the action it will trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitRegion {
    pub bounds: HitRect,
    pub action: PanelAction,
}

/// Returns the first region containing the supplied panel-local point.
pub fn hit_test(regions: &[HitRegion], x: u32, y: u32) -> Option<PanelAction> {
    regions
        .iter()
        .find(|region| region.bounds.contains(x, y))
        .map(|region| region.action)
}

/// Describes the initial launcher, workspace and clock hit regions.
pub fn interactive_regions(width: u32, height: u32) -> Vec<HitRegion> {
    interactive_regions_with_count(width, height, 4)
}

fn interactive_regions_with_count(width: u32, height: u32, workspace_count: u32) -> Vec<HitRegion> {
    let panel_height = height.min(PANEL_HEIGHT);
    let launcher_x = 8;
    let mut regions = vec![HitRegion {
        bounds: HitRect {
            x: launcher_x,
            y: 8,
            width: 32.min(width.saturating_sub(launcher_x)),
            height: 24.min(panel_height.saturating_sub(8)),
        },
        action: PanelAction::Launcher,
    }];

    for index in 0..visible_workspace_count(width, workspace_count) {
        let x = 82 + index * 24;
        regions.push(HitRegion {
            bounds: HitRect {
                x,
                y: 14,
                width: 16.min(width.saturating_sub(x)),
                height: 12.min(panel_height.saturating_sub(14)),
            },
            action: PanelAction::Workspace((index + 1) as u8),
        });
    }

    regions.push(HitRegion {
        bounds: HitRect {
            x: width.saturating_sub(106),
            y: 8,
            width: 96.min(width),
            height: 24.min(panel_height.saturating_sub(8)),
        },
        action: PanelAction::Clock,
    });
    regions
}

fn visible_workspace_count(width: u32, workspace_count: u32) -> u32 {
    let available = width.saturating_sub(222) / 24;
    workspace_count.min(available).min(u8::MAX.into())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("BubblePanel {}", env!("CARGO_PKG_VERSION"));
    let connection = Connection::connect_to_env()
        .context("cannot connect to the Wayland compositor (check WAYLAND_DISPLAY)")?;
    let (globals, mut event_queue) =
        registry_queue_init(&connection).context("cannot initialize the Wayland registry")?;
    let queue_handle = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &queue_handle)
        .context("the compositor does not expose wl_compositor")?;
    let layer_shell = LayerShell::bind(&globals, &queue_handle)
        .context("the compositor does not support wlr-layer-shell; BubblePanel cannot be placed")?;
    let shm =
        Shm::bind(&globals, &queue_handle).context("the compositor does not expose wl_shm")?;

    let surface = compositor.create_surface(&queue_handle);
    let layer = layer_shell.create_layer_surface(
        &queue_handle,
        surface,
        Layer::Top,
        Some("bubblepanel"),
        None,
    );

    layer.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, PANEL_HEIGHT);
    layer.set_exclusive_zone(PANEL_HEIGHT as i32);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();

    let pool = SlotPool::new((MAX_BUFFER_WIDTH * PANEL_HEIGHT * 4) as usize, &shm)
        .context("cannot allocate the shared-memory drawing pool")?;

    let mut panel = BubblePanel {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &queue_handle),
        seat_state: SeatState::new(&globals, &queue_handle),
        shm,
        pool,
        layer,
        width: INITIAL_WIDTH,
        height: PANEL_HEIGHT,
        first_configure: true,
        closed: false,
        clock_minute: current_clock_minute(),
        snapshot: PanelSnapshot::default(),
        ipc_runtime: PanelIpcRuntime::start().context("cannot start BubblePanel IPC worker")?,
        pointer: None,
        pointer_interaction: PointerInteraction::default(),
    };

    info!("Waiting for the compositor to configure BubblePanel");
    let wayland_fd = connection.as_fd().as_raw_fd();
    while !panel.closed {
        // Poll instead of blocking indefinitely so the clock can refresh without
        // introducing a background thread or continuously requesting frames.
        let mut descriptor = libc::pollfd {
            fd: wayland_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // A one-second timeout keeps the loop responsive while the clock itself
        // is only redrawn when its displayed minute changes.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 1_000) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("cannot poll the Wayland connection");
        }
        if ready > 0 {
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                anyhow::bail!("Wayland connection closed or became invalid");
            }
            event_queue
                .blocking_dispatch(&mut panel)
                .context("Wayland event dispatch failed")?;
        }
        let state_changed = panel.adopt_ipc_update();
        if state_changed || panel.clock_minute != current_clock_minute() {
            panel.draw().context("cannot refresh the panel clock")?;
        }
    }

    info!("BubblePanel surface was closed by the compositor");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HorizontalScrollDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerticalScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollDirections {
    horizontal: Option<HorizontalScrollDirection>,
    vertical: Option<VerticalScrollDirection>,
}

fn axis_sign(axis: &AxisScroll) -> Option<bool> {
    if axis.value120 != 0 {
        return Some(axis.value120.is_positive());
    }
    if axis.discrete != 0 {
        return Some(axis.discrete.is_positive());
    }
    if axis.absolute.is_finite() && axis.absolute != 0.0 {
        return Some(axis.absolute.is_sign_positive());
    }
    None
}

/// Converts Wayland axis data into logical directions without treating stop-only
/// or zero-movement axis frames as scroll gestures.
fn scroll_directions(horizontal: &AxisScroll, vertical: &AxisScroll) -> Option<ScrollDirections> {
    let horizontal = axis_sign(horizontal).map(|positive| {
        if positive {
            HorizontalScrollDirection::Right
        } else {
            HorizontalScrollDirection::Left
        }
    });
    let vertical = axis_sign(vertical).map(|positive| {
        if positive {
            VerticalScrollDirection::Down
        } else {
            VerticalScrollDirection::Up
        }
    });

    (horizontal.is_some() || vertical.is_some()).then_some(ScrollDirections {
        horizontal,
        vertical,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceScrollDirection {
    Previous,
    Next,
}

/// Workspace navigation is intentionally limited to vertical scrolling over
/// a workspace hit region. Horizontal scrolling and other panel modules do not
/// implicitly change the active workspace.
fn workspace_scroll_direction(
    action: Option<PanelAction>,
    directions: ScrollDirections,
) -> Option<WorkspaceScrollDirection> {
    if !matches!(action, Some(PanelAction::Workspace(_))) {
        return None;
    }

    match directions.vertical {
        Some(VerticalScrollDirection::Up) => Some(WorkspaceScrollDirection::Previous),
        Some(VerticalScrollDirection::Down) => Some(WorkspaceScrollDirection::Next),
        None => None,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PointerInteraction {
    hovered: Option<PanelAction>,
    pressed: Option<PanelAction>,
}

impl PointerInteraction {
    /// Updates the hovered action and reports whether the visible target changed.
    fn update_hover(&mut self, action: Option<PanelAction>) -> bool {
        if self.hovered == action {
            return false;
        }
        self.hovered = action;
        true
    }

    fn press(&mut self, action: Option<PanelAction>) {
        self.pressed = action;
    }

    /// Completes a click only when press and release target the same panel action.
    fn release(&mut self, action: Option<PanelAction>) -> Option<PanelAction> {
        let pressed = self.pressed.take()?;
        (Some(pressed) == action).then_some(pressed)
    }

    /// Clears transient pointer state when the pointer leaves the panel.
    fn leave(&mut self) -> bool {
        self.pressed = None;
        self.hovered.take().is_some()
    }
}

struct BubblePanel {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    width: u32,
    height: u32,
    first_configure: bool,
    closed: bool,
    clock_minute: String,
    snapshot: PanelSnapshot,
    ipc_runtime: PanelIpcRuntime,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_interaction: PointerInteraction,
}

impl BubblePanel {
    fn draw(&mut self) -> Result<()> {
        let width = self.width.clamp(1, MAX_BUFFER_WIDTH);
        let height = self.height.clamp(1, PANEL_HEIGHT);
        let stride = (width * 4) as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .context("cannot create a shared-memory panel buffer")?;

        canvas.fill(0);
        let colors = PanelColors::from_snapshot(&self.snapshot);
        fill_rect(canvas, width, height, 0, 0, width, height, colors.glass);
        draw_brand(canvas, width, height, colors);
        let workspace_count = self
            .snapshot
            .workspace
            .as_ref()
            .map_or(4, |state| state.count);
        let active_workspace = self
            .snapshot
            .workspace
            .as_ref()
            .map_or(0, |state| state.active);
        draw_workspaces(
            canvas,
            width,
            height,
            colors,
            workspace_count,
            active_workspace,
        );
        draw_status_and_clock(canvas, width, height, colors);

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer
            .attach_to(self.layer.wl_surface())
            .context("cannot attach the panel buffer")?;
        self.layer.commit();
        self.clock_minute = current_clock_minute();
        Ok(())
    }

    fn adopt_ipc_update(&mut self) -> bool {
        let Some(snapshot) = self.ipc_runtime.try_take_update() else {
            return false;
        };
        if snapshot == self.snapshot {
            return false;
        }
        self.snapshot = snapshot;
        info!("BubblePanel adopted the current theme and workspace state");
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceState {
    active: u32,
    count: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct PanelSnapshot {
    theme: Option<ThemePalette>,
    workspace: Option<WorkspaceState>,
    workspace_control: bool,
}

impl Default for PanelSnapshot {
    fn default() -> Self {
        Self {
            theme: None,
            workspace: None,
            workspace_control: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum PanelCommand {
    PreviousWorkspace,
    NextWorkspace,
    ActivateWorkspace(u32),
}

/// Polls compositor state and sends control requests outside BubblePanel's
/// Wayland event loop. Identical snapshots are coalesced before publication.
struct PanelIpcRuntime {
    updates: Receiver<PanelSnapshot>,
    commands: Sender<PanelCommand>,
    shutdown: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl PanelIpcRuntime {
    fn start() -> std::io::Result<Self> {
        let (update_tx, updates) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let (shutdown, shutdown_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("bubblepanel-ipc".into())
            .spawn(move || {
                let mut snapshot = PanelSnapshot::default();
                let mut published: Option<PanelSnapshot> = None;
                let mut next_poll = Instant::now();
                loop {
                    while let Ok(command) = command_rx.try_recv() {
                        let request = match command {
                            PanelCommand::PreviousWorkspace => Request::WorkspacePrevious,
                            PanelCommand::NextWorkspace => Request::WorkspaceNext,
                            PanelCommand::ActivateWorkspace(index) => {
                                Request::ActivateWorkspace { index }
                            }
                        };
                        match request_bubblewm(request) {
                            Some(Response::Success {
                                data: ResponseData::WorkspaceState { active, count },
                            }) => {
                                snapshot.workspace = Some(WorkspaceState { active, count });
                            }
                            Some(Response::Failure { error }) => {
                                warn!(%error.message, "BubbleWM rejected a workspace request");
                            }
                            _ => warn!("BubbleWM did not return workspace state after a request"),
                        }
                    }

                    if Instant::now() >= next_poll {
                        if let Some(workspace_control) = read_workspace_capability() {
                            snapshot.workspace_control = workspace_control;
                            if !workspace_control {
                                snapshot.workspace = None;
                            }
                        }
                        if let Some(workspace) = read_workspace_state() {
                            snapshot.workspace = Some(workspace);
                        }
                        if let Some(theme) = read_theme_palette() {
                            snapshot.theme = theme;
                        }
                        next_poll = Instant::now() + Duration::from_secs(5);
                    }

                    if published.as_ref() != Some(&snapshot) {
                        if update_tx.send(snapshot.clone()).is_err() {
                            break;
                        }
                        published = Some(snapshot.clone());
                    }
                    match shutdown_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            updates,
            commands,
            shutdown,
            worker: Some(worker),
        })
    }

    fn try_take_update(&self) -> Option<PanelSnapshot> {
        let mut latest = None;
        while let Ok(snapshot) = self.updates.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }

    fn send_command(&self, command: PanelCommand) {
        if let Err(error) = self.commands.send(command) {
            warn!(%error, "BubblePanel IPC worker stopped before handling an action");
        }
    }
}

impl Drop for PanelIpcRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn request_bubblewm(request: Request) -> Option<Response> {
    let socket_path = unix::default_socket_path().ok()?;
    unix::send_request(socket_path, request).ok()
}

fn read_theme_palette() -> Option<Option<ThemePalette>> {
    match request_bubblewm(Request::GetThemePalette)? {
        Response::Success {
            data: ResponseData::ThemePalette { palette },
        } => Some(palette),
        _ => None,
    }
}

fn read_workspace_capability() -> Option<bool> {
    match request_bubblewm(Request::GetCapabilities)? {
        Response::Success {
            data: ResponseData::Capabilities { workspace_control },
        } => Some(workspace_control),
        _ => None,
    }
}

fn read_workspace_state() -> Option<WorkspaceState> {
    match request_bubblewm(Request::GetWorkspaceState)? {
        Response::Success {
            data: ResponseData::WorkspaceState { active, count },
        } => Some(WorkspaceState { active, count }),
        _ => None,
    }
}

impl CompositorHandler for BubblePanel {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for BubblePanel {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for BubblePanel {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.closed = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.width = NonZeroU32::new(configure.new_size.0).map_or(INITIAL_WIDTH, NonZeroU32::get);
        self.height = NonZeroU32::new(configure.new_size.1).map_or(PANEL_HEIGHT, NonZeroU32::get);

        if self.first_configure {
            self.first_configure = false;
        }
        if let Err(error) = self.draw() {
            warn!(%error, "BubblePanel rendering failed");
            self.closed = true;
        }
    }
}

impl SeatHandler for BubblePanel {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    info!("Wayland pointer capability is available");
                    self.pointer = Some(pointer);
                }
                Err(error) => {
                    warn!(%error, "cannot initialize the Wayland pointer");
                }
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
                info!("Wayland pointer capability was removed");
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl PointerHandler for BubblePanel {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }

            if matches!(&event.kind, PointerEventKind::Leave { .. }) {
                if self.pointer_interaction.leave() {
                    info!("Pointer left BubblePanel");
                }
                continue;
            }

            let Some((x, y)) = pointer_position_to_coords(event.position, self.width, self.height)
            else {
                continue;
            };
            let workspace_count = self
                .snapshot
                .workspace
                .as_ref()
                .map_or(4, |state| state.count);
            let regions = interactive_regions_with_count(self.width, self.height, workspace_count);
            let action = hit_test(&regions, x, y);

            match &event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    if self.pointer_interaction.update_hover(action) {
                        info!(?action, x, y, "BubblePanel pointer target changed");
                    }
                }
                PointerEventKind::Press { button, .. } if *button == BTN_LEFT => {
                    self.pointer_interaction.press(action);
                }
                PointerEventKind::Release { button, .. } if *button == BTN_LEFT => {
                    if let Some(action) = self.pointer_interaction.release(action) {
                        match action {
                            PanelAction::Workspace(workspace)
                                if self.snapshot.workspace_control =>
                            {
                                self.ipc_runtime
                                    .send_command(PanelCommand::ActivateWorkspace(u32::from(
                                        workspace - 1,
                                    )));
                            }
                            _ => info!(?action, x, y, "BubblePanel left-click completed"),
                        }
                    }
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    if let Some(directions) = scroll_directions(horizontal, vertical) {
                        if let Some(workspace_direction) =
                            workspace_scroll_direction(action, directions)
                        {
                            if self.snapshot.workspace_control {
                                let command = match workspace_direction {
                                    WorkspaceScrollDirection::Previous => {
                                        PanelCommand::PreviousWorkspace
                                    }
                                    WorkspaceScrollDirection::Next => PanelCommand::NextWorkspace,
                                };
                                self.ipc_runtime.send_command(command);
                            }
                        } else {
                            info!(
                                ?directions,
                                ?action,
                                x,
                                y,
                                "BubblePanel pointer scroll detected"
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for BubblePanel {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(BubblePanel);

impl ProvidesRegistryState for BubblePanel {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(BubblePanel);

/// Converts Wayland surface-local floating-point coordinates into bounded
/// panel-local logical coordinates.
fn pointer_position_to_coords(position: (f64, f64), width: u32, height: u32) -> Option<(u32, u32)> {
    let (x, y) = position;
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(width)
        || y >= f64::from(height)
    {
        return None;
    }

    Some((x.floor() as u32, y.floor() as u32))
}

fn current_clock_minute() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

fn fill_rect(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    rect_width: u32,
    rect_height: u32,
    color: u32,
) {
    let x_end = x.saturating_add(rect_width).min(width);
    let y_end = y.saturating_add(rect_height).min(height);
    for row in y.min(height)..y_end {
        for column in x.min(width)..x_end {
            let offset = ((row * width + column) * 4) as usize;
            if let Ok(pixel) = <&mut [u8; 4]>::try_from(&mut canvas[offset..offset + 4]) {
                *pixel = color.to_le_bytes();
            }
        }
    }
}

#[derive(Clone, Copy)]
struct PanelColors {
    glass: u32,
    surface: u32,
    primary: u32,
    text: u32,
    muted: u32,
}

impl PanelColors {
    fn from_snapshot(snapshot: &PanelSnapshot) -> Self {
        let Some(palette) = snapshot.theme.as_ref() else {
            return Self {
                glass: 0xFF1B202B,
                surface: 0xFF1B202B,
                primary: 0xFF62D6E8,
                text: 0xFFE7EDF5,
                muted: 0xFF657184,
            };
        };
        let opacity = if palette.glass_opacity.is_finite() {
            palette.glass_opacity.clamp(0.0, 1.0)
        } else {
            0.78
        };
        Self {
            glass: argb(palette.glass_tint, (opacity * 255.0).round() as u32),
            surface: argb(palette.surface, 255),
            primary: argb(palette.primary, 255),
            text: argb(palette.on_surface, 255),
            muted: argb(palette.outline_variant, 255),
        }
    }
}

fn argb(color: [u8; 3], alpha: u32) -> u32 {
    (alpha << 24) | (u32::from(color[0]) << 16) | (u32::from(color[1]) << 8) | u32::from(color[2])
}

fn draw_brand(canvas: &mut [u8], width: u32, height: u32, colors: PanelColors) {
    fill_rect(canvas, width, height, 16, 12, 16, 16, colors.primary);
    fill_rect(canvas, width, height, 20, 16, 8, 8, colors.surface);
    fill_rect(canvas, width, height, 40, 18, 2, 4, colors.text);
    fill_rect(canvas, width, height, 47, 18, 2, 4, colors.text);
    fill_rect(canvas, width, height, 54, 18, 2, 4, colors.text);
}

fn draw_workspaces(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    colors: PanelColors,
    workspace_count: u32,
    active_workspace: u32,
) {
    for index in 0..visible_workspace_count(width, workspace_count) {
        let x = 82 + index * 24;
        let color = if index == active_workspace {
            colors.primary
        } else {
            colors.muted
        };
        fill_rect(canvas, width, height, x, 14, 16, 12, color);
        if index != active_workspace {
            fill_rect(canvas, width, height, x + 3, 17, 10, 6, colors.surface);
        }
    }
}

fn draw_status_and_clock(canvas: &mut [u8], width: u32, height: u32, colors: PanelColors) {
    let clock = chrono::Local::now().format("%H:%M").to_string();
    let origin = width.saturating_sub(106);
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(20),
        17,
        2,
        6,
        colors.muted,
    );
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(12),
        16,
        4,
        8,
        colors.primary,
    );
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(5),
        13,
        4,
        11,
        colors.primary,
    );

    let mut cursor = origin;
    for character in clock.chars() {
        if character == ':' {
            fill_rect(canvas, width, height, cursor + 5, 15, 3, 3, colors.text);
            fill_rect(canvas, width, height, cursor + 5, 22, 3, 3, colors.text);
            cursor += 12;
        } else if let Some(digit) = character.to_digit(10) {
            draw_digit(canvas, width, height, cursor, digit, colors.text);
            cursor += 18;
        }
    }
}

fn draw_digit(canvas: &mut [u8], width: u32, height: u32, x: u32, digit: u32, color: u32) {
    const SEGMENTS: [u8; 10] = [
        0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110, 0b1101101, 0b1111101, 0b0000111,
        0b1111111, 0b1101111,
    ];
    let Some(mask) = SEGMENTS.get(digit as usize).copied() else {
        return;
    };
    let segments = [
        (2, 0, 9, 2),
        (11, 2, 2, 8),
        (11, 12, 2, 8),
        (2, 21, 9, 2),
        (0, 12, 2, 8),
        (0, 2, 2, 8),
        (2, 10, 9, 2),
    ];
    for (index, (sx, sy, sw, sh)) in segments.into_iter().enumerate() {
        if mask & (1 << index) != 0 {
            fill_rect(canvas, width, height, x + sx, sy + 8, sw, sh, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_drawing_clips_to_canvas_bounds() {
        let mut canvas = vec![0; 4 * 4 * 4];
        fill_rect(&mut canvas, 4, 4, 3, 3, 8, 8, 0xFF112233);
        assert_eq!(&canvas[15 * 4..16 * 4], &[0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(&canvas[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn digit_drawing_ignores_invalid_values() {
        let mut canvas = vec![0; 40 * 40 * 4];
        draw_digit(&mut canvas, 40, 40, 0, 99);
        assert!(canvas.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn pointer_coordinates_are_floored_and_bounded() {
        assert_eq!(
            pointer_position_to_coords((12.9, 7.1), 1280, PANEL_HEIGHT),
            Some((12, 7))
        );
        assert_eq!(
            pointer_position_to_coords((1280.0, 1.0), 1280, PANEL_HEIGHT),
            None
        );
        assert_eq!(
            pointer_position_to_coords((-0.1, 1.0), 1280, PANEL_HEIGHT),
            None
        );
        assert_eq!(
            pointer_position_to_coords((f64::NAN, 1.0), 1280, PANEL_HEIGHT),
            None
        );
        assert_eq!(
            pointer_position_to_coords((1.0, f64::INFINITY), 1280, PANEL_HEIGHT),
            None
        );
    }

    #[test]
    fn hit_rect_uses_half_open_bounds() {
        let rect = HitRect {
            x: 10,
            y: 5,
            width: 20,
            height: 10,
        };
        assert!(rect.contains(10, 5));
        assert!(rect.contains(29, 14));
        assert!(!rect.contains(30, 14));
        assert!(!rect.contains(29, 15));
        assert!(!rect.contains(9, 5));
    }

    #[test]
    fn hit_test_returns_the_matching_panel_action() {
        let regions = interactive_regions(1280, PANEL_HEIGHT);
        assert_eq!(hit_test(&regions, 12, 12), Some(PanelAction::Launcher));
        assert_eq!(hit_test(&regions, 85, 17), Some(PanelAction::Workspace(1)));
        assert_eq!(hit_test(&regions, 1200, 20), Some(PanelAction::Clock));
        assert_eq!(hit_test(&regions, 600, 20), None);
    }

    #[test]
    fn hit_test_ignores_zero_sized_rectangles() {
        let regions = [HitRegion {
            bounds: HitRect {
                x: 0,
                y: 0,
                width: 0,
                height: 10,
            },
            action: PanelAction::Launcher,
        }];
        assert_eq!(hit_test(&regions, 0, 0), None);
    }

    #[test]
    fn scroll_direction_uses_high_resolution_axis_data_first() {
        let axis = AxisScroll {
            value120: 120,
            discrete: -1,
            absolute: -15.0,
            ..AxisScroll::default()
        };
        assert_eq!(axis_sign(&axis), Some(true));
    }

    #[test]
    fn scroll_direction_falls_back_to_discrete_then_absolute_data() {
        let discrete = AxisScroll {
            discrete: -1,
            absolute: 10.0,
            ..AxisScroll::default()
        };
        assert_eq!(axis_sign(&discrete), Some(false));

        let absolute = AxisScroll {
            absolute: 0.5,
            ..AxisScroll::default()
        };
        assert_eq!(axis_sign(&absolute), Some(true));
    }

    #[test]
    fn scroll_directions_support_vertical_horizontal_and_diagonal_motion() {
        let horizontal = AxisScroll {
            absolute: -1.0,
            ..AxisScroll::default()
        };
        let vertical = AxisScroll {
            value120: 120,
            ..AxisScroll::default()
        };
        assert_eq!(
            scroll_directions(&horizontal, &vertical),
            Some(ScrollDirections {
                horizontal: Some(HorizontalScrollDirection::Left),
                vertical: Some(VerticalScrollDirection::Down),
            })
        );
    }

    #[test]
    fn scroll_direction_ignores_zero_stop_only_and_non_finite_motion() {
        let no_motion = AxisScroll {
            stop: true,
            ..AxisScroll::default()
        };
        let invalid_motion = AxisScroll {
            absolute: f64::NAN,
            ..AxisScroll::default()
        };
        assert_eq!(axis_sign(&no_motion), None);
        assert_eq!(axis_sign(&invalid_motion), None);
        assert_eq!(scroll_directions(&no_motion, &invalid_motion), None);
    }

    #[test]
    fn workspace_scroll_maps_vertical_direction_to_navigation() {
        let up = ScrollDirections {
            horizontal: None,
            vertical: Some(VerticalScrollDirection::Up),
        };
        let down = ScrollDirections {
            horizontal: None,
            vertical: Some(VerticalScrollDirection::Down),
        };
        assert_eq!(
            workspace_scroll_direction(Some(PanelAction::Workspace(2)), up),
            Some(WorkspaceScrollDirection::Previous)
        );
        assert_eq!(
            workspace_scroll_direction(Some(PanelAction::Workspace(2)), down),
            Some(WorkspaceScrollDirection::Next)
        );
    }

    #[test]
    fn workspace_scroll_ignores_horizontal_motion_and_other_panel_regions() {
        let horizontal = ScrollDirections {
            horizontal: Some(HorizontalScrollDirection::Right),
            vertical: None,
        };
        let vertical = ScrollDirections {
            horizontal: None,
            vertical: Some(VerticalScrollDirection::Down),
        };
        assert_eq!(
            workspace_scroll_direction(Some(PanelAction::Workspace(1)), horizontal),
            None
        );
        assert_eq!(
            workspace_scroll_direction(Some(PanelAction::Launcher), vertical),
            None
        );
        assert_eq!(
            workspace_scroll_direction(Some(PanelAction::Clock), vertical),
            None
        );
        assert_eq!(workspace_scroll_direction(None, vertical), None);
    }

    #[test]
    fn pointer_hover_changes_only_when_the_target_changes() {
        let mut pointer = PointerInteraction::default();
        assert!(pointer.update_hover(Some(PanelAction::Launcher)));
        assert!(!pointer.update_hover(Some(PanelAction::Launcher)));
        assert!(pointer.update_hover(Some(PanelAction::Workspace(1))));
        assert!(pointer.update_hover(None));
        assert!(!pointer.update_hover(None));
    }

    #[test]
    fn pointer_click_requires_press_and_release_on_the_same_action() {
        let mut pointer = PointerInteraction::default();
        pointer.press(Some(PanelAction::Launcher));
        assert_eq!(
            pointer.release(Some(PanelAction::Launcher)),
            Some(PanelAction::Launcher)
        );

        pointer.press(Some(PanelAction::Launcher));
        assert_eq!(pointer.release(Some(PanelAction::Clock)), None);
        assert_eq!(pointer.release(Some(PanelAction::Launcher)), None);
    }

    #[test]
    fn pointer_leave_clears_hover_and_cancels_a_pending_click() {
        let mut pointer = PointerInteraction::default();
        pointer.update_hover(Some(PanelAction::Launcher));
        pointer.press(Some(PanelAction::Launcher));

        assert!(pointer.leave());
        assert!(!pointer.leave());
        assert_eq!(pointer.release(Some(PanelAction::Launcher)), None);
    }

    #[test]
    fn panel_height_fits_seven_segment_clock() {
        assert!(PANEL_HEIGHT >= 32);
    }
}
