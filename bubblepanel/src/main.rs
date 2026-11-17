//! BubblePanel's first Wayland layer-shell prototype.
//!
//! This milestone provides an independent Wayland client, a bottom layer-shell
//! surface and a software-rendered three-zone layout. The clock is sampled when
//! the surface is configured; the clock refreshes on a low-overhead event-loop
//! timeout, while interactive modules follow in a later implementation step.

use std::{
    num::NonZeroU32,
    os::fd::{AsFd, AsRawFd},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use bubble_ipc::{Request, Response, ResponseData, ThemePalette, unix::UnixIpcClient};
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

    for index in 0..4_u8 {
        let x = 82 + u32::from(index) * 24;
        regions.push(HitRegion {
            bounds: HitRect {
                x,
                y: 14,
                width: 16.min(width.saturating_sub(x)),
                height: 12.min(panel_height.saturating_sub(14)),
            },
            action: PanelAction::Workspace(index + 1),
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
        closed: false,
        clock_minute: current_clock_minute(),
        pointer: None,
        pointer_interaction: PointerInteraction::default(),
        workspace_client: WorkspaceIpcClient::from_environment(),
        workspace_status: WorkspaceStatus::default(),
        theme_palette: None,
    };
    panel.refresh_workspace_status();
    panel.refresh_theme_palette();

    info!("Waiting for the compositor to configure BubblePanel");
    let wayland_fd = connection.as_fd().as_raw_fd();
    let mut next_workspace_refresh = Instant::now() + Duration::from_secs(1);
    while !panel.closed {
        // Flush queued Wayland requests before waiting for readable events.
        // Otherwise the client can wait for a configure that the compositor
        // cannot send because get_layer_surface/commit are still buffered.
        connection
            .flush()
            .context("cannot flush pending Wayland requests")?;

        // Poll instead of blocking indefinitely so the clock and workspace
        // indicators can refresh without a background thread or frame loop.
        let mut descriptor = libc::pollfd {
            fd: wayland_fd,
            events: libc::POLLIN,
            revents: 0,
        };
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

        let now = Instant::now();
        let refresh_due = now >= next_workspace_refresh;
        let (workspace_changed, theme_changed) = if refresh_due {
            next_workspace_refresh = now + Duration::from_secs(1);
            (
                panel.refresh_workspace_status(),
                panel.refresh_theme_palette(),
            )
        } else {
            (false, false)
        };
        let clock_changed = panel.clock_minute != current_clock_minute();
        if workspace_changed || theme_changed || clock_changed {
            panel.draw().context("cannot refresh BubblePanel")?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceStatus {
    active: u32,
    count: u32,
    control: bool,
}

impl Default for WorkspaceStatus {
    fn default() -> Self {
        Self {
            active: 0,
            count: 4,
            control: false,
        }
    }
}

struct WorkspaceIpcClient {
    client: UnixIpcClient,
}

impl WorkspaceIpcClient {
    fn from_environment() -> Option<Self> {
        match UnixIpcClient::connect_default() {
            Ok(client) => Some(Self { client }),
            Err(error) => {
                warn!(%error, "BubbleWM workspace IPC is unavailable");
                None
            }
        }
    }

    fn request(&mut self, request: Request) -> Result<Response> {
        self.client.request(request).map_err(|error| anyhow!(error))
    }

    fn query_theme_palette(&mut self) -> Result<Option<ThemePalette>> {
        match self.request(Request::GetThemePalette)? {
            Response::Success {
                data: ResponseData::ThemePalette { palette },
            } => Ok(palette.filter(|palette| palette.schema_version == 1)),
            Response::Failure { error } => {
                bail!("BubbleWM theme palette query failed: {}", error.message)
            }
            Response::Success { .. } => {
                bail!("BubbleWM returned an invalid theme palette response")
            }
        }
    }

    fn query_status(&mut self) -> Result<WorkspaceStatus> {
        let control = match self.request(Request::GetCapabilities)? {
            Response::Success {
                data: ResponseData::Capabilities { workspace_control },
            } => workspace_control,
            Response::Failure { error } => {
                bail!("BubbleWM capability query failed: {}", error.message)
            }
            Response::Success { .. } => bail!("BubbleWM returned an invalid capability response"),
        };

        match self.request(Request::GetWorkspaceState)? {
            Response::Success {
                data: ResponseData::WorkspaceState { active, count },
            } if count > 0 && active < count => Ok(WorkspaceStatus {
                active,
                count,
                control,
            }),
            Response::Success { .. } => bail!("BubbleWM returned invalid workspace state"),
            Response::Failure { error } => {
                bail!("BubbleWM workspace query failed: {}", error.message)
            }
        }
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
    closed: bool,
    clock_minute: String,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_interaction: PointerInteraction,
    workspace_client: Option<WorkspaceIpcClient>,
    workspace_status: WorkspaceStatus,
    theme_palette: Option<ThemePalette>,
}

impl BubblePanel {
    fn refresh_workspace_status(&mut self) -> bool {
        let Some(client) = self.workspace_client.as_mut() else {
            return false;
        };
        match client.query_status() {
            Ok(status) => {
                let changed = self.workspace_status != status;
                self.workspace_status = status;
                changed
            }
            Err(error) => {
                warn!(%error, "Cannot refresh BubbleWM workspace state");
                false
            }
        }
    }

    fn refresh_theme_palette(&mut self) -> bool {
        let Some(client) = self.workspace_client.as_mut() else {
            return false;
        };
        match client.query_theme_palette() {
            Ok(palette) => {
                let changed = self.theme_palette != palette;
                self.theme_palette = palette;
                changed
            }
            Err(error) => {
                tracing::debug!(%error, "Cannot refresh BubbleColor palette for BubblePanel");
                false
            }
        }
    }

    fn send_workspace_request(&mut self, request: Request) {
        if !self.workspace_status.control {
            info!("Workspace control is not available from this compositor");
            return;
        }

        let result = self
            .workspace_client
            .as_mut()
            .ok_or_else(|| anyhow!("BubbleWM IPC client is unavailable"))
            .and_then(|client| client.request(request));

        match result {
            Ok(Response::Success { .. }) => {
                self.refresh_workspace_status();
                if let Err(error) = self.draw() {
                    warn!(%error, "Cannot redraw BubblePanel after workspace change");
                }
            }
            Ok(Response::Failure { error }) => {
                warn!(message = %error.message, ?error.code, "BubbleWM rejected workspace request");
            }
            Err(error) => warn!(%error, "BubblePanel workspace request failed"),
        }
    }

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
        let palette = self.theme_palette.as_ref();
        let background = palette.map_or(0xFF1B202B, |theme| argb(theme.surface));
        fill_rect(canvas, width, height, 0, 0, width, height, background);
        draw_brand(canvas, width, height, palette);
        draw_workspaces(
            canvas,
            width,
            height,
            self.workspace_status.active,
            self.workspace_status.count,
            palette,
        );
        draw_status_and_clock(canvas, width, height, palette);

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
            let regions = interactive_regions(self.width, self.height);
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
                            PanelAction::Workspace(index) => {
                                self.send_workspace_request(Request::ActivateWorkspace {
                                    index: u32::from(index.saturating_sub(1)),
                                });
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
                            let request = match workspace_direction {
                                WorkspaceScrollDirection::Previous => Request::WorkspacePrevious,
                                WorkspaceScrollDirection::Next => Request::WorkspaceNext,
                            };
                            self.send_workspace_request(request);
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

fn argb(color: [u8; 3]) -> u32 {
    0xFF00_0000 | (u32::from(color[0]) << 16) | (u32::from(color[1]) << 8) | u32::from(color[2])
}

fn draw_brand(canvas: &mut [u8], width: u32, height: u32, palette: Option<&ThemePalette>) {
    let accent = palette.map_or(0xFF62D6E8, |theme| argb(theme.primary));
    let foreground = palette.map_or(0xFFE7EDF5, |theme| argb(theme.on_surface));
    let surface = palette.map_or(0xFF1B202B, |theme| argb(theme.surface));
    fill_rect(canvas, width, height, 16, 12, 16, 16, accent);
    fill_rect(canvas, width, height, 20, 16, 8, 8, surface);
    fill_rect(canvas, width, height, 40, 18, 2, 4, foreground);
    fill_rect(canvas, width, height, 47, 18, 2, 4, foreground);
    fill_rect(canvas, width, height, 54, 18, 2, 4, foreground);
}

fn draw_workspaces(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    active_workspace: u32,
    workspace_count: u32,
    palette: Option<&ThemePalette>,
) {
    for index in 0..4_u32 {
        let x = 82 + index * 24;
        let visible = index < workspace_count;
        let active = visible && index == active_workspace;
        let color = if active {
            palette.map_or(0xFF62D6E8, |theme| argb(theme.primary))
        } else if visible {
            palette.map_or(0xFF465060, |theme| argb(theme.surface_variant))
        } else {
            palette.map_or(0xFF303744, |theme| argb(theme.outline))
        };
        fill_rect(canvas, width, height, x, 14, 16, 12, color);
        if !active {
            fill_rect(
                canvas,
                width,
                height,
                x + 3,
                17,
                10,
                6,
                palette.map_or(0xFF1B202B, |theme| argb(theme.surface)),
            );
        }
    }
}

fn draw_status_and_clock(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    palette: Option<&ThemePalette>,
) {
    let clock = chrono::Local::now().format("%H:%M").to_string();
    let origin = width.saturating_sub(106);
    let accent = palette.map_or(0xFF62D6E8, |theme| argb(theme.primary));
    let foreground = palette.map_or(0xFFE7EDF5, |theme| argb(theme.on_surface));
    let outline = palette.map_or(0xFF657184, |theme| argb(theme.outline));
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(20),
        17,
        2,
        6,
        outline,
    );
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(12),
        16,
        4,
        8,
        accent,
    );
    fill_rect(
        canvas,
        width,
        height,
        origin.saturating_sub(5),
        13,
        4,
        11,
        accent,
    );

    let mut cursor = origin;
    for character in clock.chars() {
        if character == ':' {
            fill_rect(canvas, width, height, cursor + 5, 15, 3, 3, foreground);
            fill_rect(canvas, width, height, cursor + 5, 22, 3, 3, foreground);
            cursor += 12;
        } else if let Some(digit) = character.to_digit(10) {
            draw_digit(canvas, width, height, cursor, digit, foreground);
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
    fn rgb_palette_colors_convert_to_opaque_argb() {
        assert_eq!(argb([0x12, 0x34, 0x56]), 0xFF123456);
    }

    #[test]
    fn digit_drawing_ignores_invalid_values() {
        let mut canvas = vec![0; 40 * 40 * 4];
        draw_digit(&mut canvas, 40, 40, 0, 99, 0xFFFFFFFF);
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
