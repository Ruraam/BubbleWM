//! BubblePanel's first Wayland layer-shell prototype.
//!
//! This milestone provides an independent Wayland client, a bottom layer-shell
//! surface and a software-rendered three-zone layout. The clock is sampled when
//! the surface is configured; periodic refresh and interactive modules follow
//! in the next implementation step.

use std::{convert::TryInto, num::NonZeroU32};

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

const PANEL_HEIGHT: u32 = 40;
const INITIAL_WIDTH: u32 = 1280;
const MAX_BUFFER_WIDTH: u32 = 3840;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
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
    let shm = Shm::bind(&globals, &queue_handle)
        .context("the compositor does not expose wl_shm")?;

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
        shm,
        pool,
        layer,
        width: INITIAL_WIDTH,
        height: PANEL_HEIGHT,
        first_configure: true,
        closed: false,
    };

    info!("Waiting for the compositor to configure BubblePanel");
    while !panel.closed {
        event_queue
            .blocking_dispatch(&mut panel)
            .context("Wayland event dispatch failed")?;
    }

    info!("BubblePanel surface was closed by the compositor");
    Ok(())
}

struct BubblePanel {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    width: u32,
    height: u32,
    first_configure: bool,
    closed: bool,
}

impl BubblePanel {
    fn draw(&mut self) -> Result<()> {
        let width = self.width.clamp(1, MAX_BUFFER_WIDTH);
        let height = self.height.clamp(1, PANEL_HEIGHT);
        let stride = (width * 4) as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
            .context("cannot create a shared-memory panel buffer")?;

        canvas.fill(0);
        fill_rect(&mut canvas, width, height, 0, 0, width, height, 0xFF1B202B);
        draw_brand(&mut canvas, width, height);
        draw_workspaces(&mut canvas, width, height);
        draw_status_and_clock(&mut canvas, width, height);

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer
            .attach_to(self.layer.wl_surface())
            .context("cannot attach the panel buffer")?;
        self.layer.commit();
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
    ) {}

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {}

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {}

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}
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
    ) {}

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {}

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {}
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

    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_dispatch2!(BubblePanel);

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

fn draw_brand(canvas: &mut [u8], width: u32, height: u32) {
    fill_rect(canvas, width, height, 16, 12, 16, 16, 0xFF62D6E8);
    fill_rect(canvas, width, height, 20, 16, 8, 8, 0xFF1B202B);
    fill_rect(canvas, width, height, 40, 18, 2, 4, 0xFFE7EDF5);
    fill_rect(canvas, width, height, 47, 18, 2, 4, 0xFFE7EDF5);
    fill_rect(canvas, width, height, 54, 18, 2, 4, 0xFFE7EDF5);
}

fn draw_workspaces(canvas: &mut [u8], width: u32, height: u32) {
    for index in 0..4 {
        let x = 82 + index * 24;
        let color = if index == 0 { 0xFF62D6E8 } else { 0xFF465060 };
        fill_rect(canvas, width, height, x, 14, 16, 12, color);
        if index != 0 {
            fill_rect(canvas, width, height, x + 3, 17, 10, 6, 0xFF1B202B);
        }
    }
}

fn draw_status_and_clock(canvas: &mut [u8], width: u32, height: u32) {
    let clock = chrono::Local::now().format("%H:%M").to_string();
    let origin = width.saturating_sub(106);
    fill_rect(canvas, width, height, origin.saturating_sub(20), 17, 2, 6, 0xFF657184);
    fill_rect(canvas, width, height, origin.saturating_sub(12), 16, 4, 8, 0xFF62D6E8);
    fill_rect(canvas, width, height, origin.saturating_sub(5), 13, 4, 11, 0xFF62D6E8);

    let mut cursor = origin;
    for character in clock.chars() {
        if character == ':' {
            fill_rect(canvas, width, height, cursor + 5, 15, 3, 3, 0xFFE7EDF5);
            fill_rect(canvas, width, height, cursor + 5, 22, 3, 3, 0xFFE7EDF5);
            cursor += 12;
        } else if let Some(digit) = character.to_digit(10) {
            draw_digit(canvas, width, height, cursor, digit);
            cursor += 18;
        }
    }
}

fn draw_digit(canvas: &mut [u8], width: u32, height: u32, x: u32, digit: u32) {
    const SEGMENTS: [u8; 10] = [
        0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110,
        0b1101101, 0b1111101, 0b0000111, 0b1111111, 0b1101111,
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
            fill_rect(canvas, width, height, x + sx, sy + 8, sw, sh, 0xFFE7EDF5);
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
    fn panel_height_fits_seven_segment_clock() {
        assert!(PANEL_HEIGHT >= 32);
    }
}
