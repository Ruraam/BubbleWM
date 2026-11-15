//! Native DRM/KMS backend.
//!
//! The backend is implemented on top of Smithay's udev/libseat/GBM stack. It
//! takes over the primary GPU on a bare TTY, runs a libinput event loop and
//! presents the same scene as the nested Winit backend.
//!
//! Version 1 targets a single primary GPU and a single connected output.
//! Monitor hotplug reconfiguration is intentionally deferred: the output is
//! discovered once at startup and the compositor keeps running on it.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            format::FormatSet,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmNode,
            compositor::{FrameError, FrameFlags, RenderFrameError},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportMemWl, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{all_gpus, primary_gpu},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Scale},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        drm::{
            Device as _,
            control::{Mode as DrmMode, ModeTypeFlags, connector, crtc},
        },
        input::{Device as InputDevice, DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Transform},
    wayland::socket::ListeningSocketSource,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, error, info, warn};

use crate::{
    compositor::{CompositorStateData, install_signal_handlers, shutdown_requested},
    config::Config,
    ipc::{CoreCommandReceiver, IpcRuntime},
    render::BubbleRenderElement,
};

/// Scanout formats tried in order when creating the primary plane swapchain.
const SUPPORTED_FORMATS: &[Fourcc] = &[
    Fourcc::Abgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr8888,
    Fourcc::Argb8888,
];

type BubbleDrmManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

type BubbleDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

struct DrmBackend {
    /// Kept alive for the lifetime of the seat session.
    _session: LibSeatSession,
    /// Clone of the libinput context, used for suspend/resume.
    libinput: Libinput,
    manager: BubbleDrmManager,
    drm_output: BubbleDrmOutput,
    renderer: GlesRenderer,
    crtc: crtc::Handle,
    frame_pending: bool,
    render_scheduled: bool,
    needs_redraw: bool,
    frame_duration: Duration,
    start_time: Instant,
    keyboards: Vec<InputDevice>,
}

struct DrmState {
    compositor: CompositorStateData,
    backend: DrmBackend,
    handle: LoopHandle<'static, DrmState>,
    ipc_commands: CoreCommandReceiver,
}

impl DrmState {
    /// Schedules a repaint, coalescing multiple requests into one callback.
    fn schedule_render(&mut self, delay: Duration) {
        if self.backend.render_scheduled || self.backend.frame_pending {
            return;
        }
        self.backend.render_scheduled = true;

        if delay.is_zero() {
            self.handle.insert_idle(|data: &mut DrmState| {
                data.backend.render_scheduled = false;
                data.render();
            });
        } else {
            self.handle
                .insert_source(Timer::from_duration(delay), |_, _, data: &mut DrmState| {
                    data.backend.render_scheduled = false;
                    data.render();
                    TimeoutAction::Drop
                })
                .map_err(|error| anyhow!("cannot schedule a DRM repaint: {error}"))
                .ok();
        }
    }

    fn request_redraw(&mut self) {
        self.backend.needs_redraw = true;
        self.schedule_render(Duration::ZERO);
    }

    /// Builds the scene and submits it to the DRM compositor.
    fn render(&mut self) {
        if self.backend.frame_pending {
            return;
        }

        let scale = 1.0;
        self.compositor.refresh_overlays();
        self.compositor.cursor_pixmap(scale);
        let elements = self
            .compositor
            .scene_elements(&mut self.backend.renderer, scale);
        let clear_color = self.compositor.clear_color();

        let result = self.backend.drm_output.render_frame(
            &mut self.backend.renderer,
            &elements,
            clear_color,
            FrameFlags::DEFAULT,
        );

        match result {
            Ok(frame) => {
                self.backend.needs_redraw = false;
                if frame.is_empty {
                    // Nothing changed since the last frame. The next input or
                    // client commit will request a new repaint.
                    return;
                }
                match self.backend.drm_output.queue_frame(()) {
                    Ok(()) => self.backend.frame_pending = true,
                    Err(error) => {
                        warn!(%error, "cannot queue a DRM frame");
                        self.schedule_render(self.backend.frame_duration);
                    }
                }
            }
            Err(error) => {
                self.backend.needs_redraw = true;
                if matches!(
                    &error,
                    RenderFrameError::PrepareFrame(FrameError::DrmError(DrmError::DeviceInactive))
                ) {
                    debug!("DRM device is inactive; waiting for session resume");
                    return;
                }
                if matches!(
                    &error,
                    RenderFrameError::PrepareFrame(FrameError::DrmError(DrmError::TestFailed(_)))
                ) {
                    if let Err(error) = self.backend.manager.device_mut().reset_state() {
                        warn!(%error, "cannot reset the DRM device state");
                    }
                }
                warn!(%error, "cannot render a DRM frame");
                self.schedule_render(self.backend.frame_duration);
            }
        }
    }

    /// Handles a vblank for the output CRTC.
    fn frame_finished(&mut self, crtc: crtc::Handle) {
        if crtc != self.backend.crtc || !self.backend.frame_pending {
            return;
        }

        self.backend.frame_pending = false;
        if let Err(error) = self.backend.drm_output.frame_submitted() {
            warn!(%error, "cannot complete a DRM frame");
        }

        let time = self.backend.start_time.elapsed().as_millis() as u32;
        self.compositor.send_frame_callbacks(time);
        if let Err(error) = self.compositor.flush_clients() {
            warn!(%error, "cannot flush Wayland clients");
        }

        // Leave clients some time to react to frame callbacks and submit new
        // buffers before the next repaint.
        self.schedule_render(self.backend.frame_duration.mul_f64(0.6));
    }

    /// Periodic housekeeping shared with the Winit backend.
    fn tick(&mut self) {
        let ipc_commands = &self.ipc_commands;
        let compositor = &mut self.compositor;
        ipc_commands.dispatch_pending(|request| compositor.handle_ipc_request(request));

        let blanked_before = self.compositor.is_blanked();
        self.compositor.housekeeping();
        let blanked_after = self.compositor.is_blanked();

        if blanked_before != blanked_after
            || self.compositor.overlays_dirty()
            || self.compositor.animations_active()
        {
            self.request_redraw();
        }
    }
}

/// Returns the primary DRM device path, honoring `BUBBLEWM_DRM_DEVICE`.
fn find_primary_device(seat: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("BUBBLEWM_DRM_DEVICE") {
        return Ok(PathBuf::from(path));
    }

    if let Ok(Some(path)) = primary_gpu(seat) {
        return Ok(path);
    }

    let devices = all_gpus(seat).context("cannot enumerate DRM devices")?;
    devices
        .into_iter()
        .find(|path| {
            DrmNode::from_path(path)
                .map(|node| node.ty() == smithay::backend::drm::NodeType::Primary)
                .unwrap_or(false)
        })
        .context("no primary DRM device found on this seat")
}

/// Picks the first connected connector that has a usable CRTC and mode.
fn pick_connector(
    scanner: &mut DrmScanner,
    device: &DrmDevice,
) -> Result<(connector::Info, crtc::Handle, DrmMode)> {
    let scan = scanner
        .scan_connectors(device)
        .context("cannot scan DRM connectors")?;

    for event in scan {
        let DrmScanEvent::Connected {
            connector,
            crtc: Some(crtc),
        } = event
        else {
            continue;
        };

        let mode = connector
            .modes()
            .iter()
            .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| connector.modes().first())
            .copied();

        if let Some(mode) = mode {
            return Ok((connector, crtc, mode));
        }
    }

    Err(anyhow!(
        "no connected display with a usable CRTC and mode was found"
    ))
}

pub fn run(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    install_signal_handlers();

    let (mut session, session_notifier) =
        LibSeatSession::new().context("cannot open a libseat session")?;
    let seat_name = session.seat();
    info!(seat = %seat_name, "starting the BubbleWM DRM backend");

    let display: Display<CompositorStateData> =
        Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

    // Publish the Wayland socket before any worker thread starts so that
    // clients spawned by the session inherit a usable environment.
    let socket_source =
        ListeningSocketSource::new_auto().context("cannot create a Wayland socket")?;
    let socket_name = socket_source.socket_name().to_string_lossy().into_owned();
    // SAFETY: no worker thread reads the environment before this point.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::set_var("XDG_CURRENT_DESKTOP", "BubbleWM");
        std::env::set_var("XDG_SESSION_DESKTOP", "BubbleWM");
    }
    info!(name = %socket_name, "listening on the Wayland socket");

    let device_path = find_primary_device(&seat_name)?;
    info!(path = %device_path.display(), "using DRM device");
    let node = DrmNode::from_path(&device_path).context("cannot identify the DRM node")?;

    let fd = session
        .open(
            &device_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .context("cannot open the DRM device through libseat")?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) =
        DrmDevice::new(fd.clone(), true).context("cannot initialize the DRM device")?;
    let gbm = GbmDevice::new(fd).context("cannot initialize the GBM device")?;

    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
        .context("cannot create an EGL display from the GBM device")?;
    let egl_context = EGLContext::new_with_priority(&egl_display, ContextPriority::High)
        .context("cannot create the EGL context")?;
    let mut renderer = unsafe { GlesRenderer::new(egl_context) }
        .context("cannot create the OpenGL ES renderer")?;

    let mut scanner = DrmScanner::new();
    let (connector, crtc, drm_mode) = pick_connector(&mut scanner, &drm)?;

    let output_name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );
    let (physical_width, physical_height) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        output_name.clone(),
        PhysicalProperties {
            size: (physical_width as i32, physical_height as i32).into(),
            subpixel: connector.subpixel().into(),
            make: "BubbleOS".into(),
            model: format!("BubbleWM {}", connector.interface().as_str()),
        },
    );
    let wl_mode = WlMode::from(drm_mode);
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.create_global::<CompositorStateData>(&display_handle);

    let start_locked = (std::env::args().any(|argument| argument == "--locked")
        || std::env::var("BUBBLEWM_LOCKED").is_ok_and(|value| value == "1"))
        && config.lock.enabled;

    let mut compositor = CompositorStateData::create(
        &display_handle,
        config,
        config_path,
        output.clone(),
        start_locked,
    )?;
    compositor.update_shm_formats(renderer.shm_formats());

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
    let render_formats: FormatSet = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect();
    let mut manager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        SUPPORTED_FORMATS.iter().copied(),
        render_formats,
    );

    let mut planes = manager
        .device()
        .planes(&crtc)
        .context("cannot query the CRTC planes")?;

    // Overlay planes are known to break scanout on the NVIDIA proprietary
    // driver. The NVK stack also benefits from the conservative path.
    if manager
        .device()
        .get_driver()
        .map(|driver| {
            let name = driver.name().to_string_lossy().to_lowercase();
            let description = driver.description().to_string_lossy().to_lowercase();
            name.contains("nvidia") || description.contains("nvidia")
        })
        .unwrap_or(false)
    {
        planes.overlay.clear();
    }

    let drm_output = manager
        .initialize_output::<GlesRenderer, BubbleRenderElement>(
            crtc,
            drm_mode,
            &[connector.handle()],
            &output,
            Some(planes),
            &mut renderer,
            &DrmOutputRenderElements::new(),
        )
        .map_err(|error| anyhow!("cannot initialize the DRM output: {error}"))?;

    let frame_duration = if wl_mode.refresh > 0 {
        Duration::from_secs_f64(1_000.0 / wl_mode.refresh as f64)
    } else {
        Duration::from_millis(16)
    };

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|()| anyhow!("cannot assign libinput to the seat"))?;

    let backend = DrmBackend {
        _session: session,
        libinput,
        manager,
        drm_output,
        renderer,
        crtc,
        frame_pending: false,
        render_scheduled: false,
        needs_redraw: true,
        frame_duration,
        start_time: Instant::now(),
        keyboards: Vec::new(),
    };

    let mut event_loop: EventLoop<'static, DrmState> =
        EventLoop::try_new().context("cannot create the DRM event loop")?;
    let handle = event_loop.handle();

    let (ipc_runtime, ipc_commands) =
        IpcRuntime::start_default().context("cannot initialize BubbleWM IPC")?;

    let mut state = DrmState {
        compositor,
        backend,
        handle: handle.clone(),
        ipc_commands,
    };

    // Accept Wayland clients.
    handle
        .insert_source(socket_source, |stream, _, data: &mut DrmState| {
            if let Err(error) = data.compositor.insert_client(stream) {
                warn!(%error, "cannot register a Wayland client");
            }
        })
        .map_err(|error| anyhow!("cannot insert the Wayland socket source: {error}"))?;

    // Dispatch Wayland requests.
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, data: &mut DrmState| {
                // SAFETY: the display is owned by this event source and is
                // never dropped while the source is registered.
                let display = unsafe { display.get_mut() };
                match display.dispatch_clients(&mut data.compositor) {
                    Ok(_) => {
                        data.compositor.refresh();
                        data.request_redraw();
                    }
                    Err(error) => warn!(%error, "Wayland client dispatch failed"),
                }
                if let Err(error) = data.compositor.flush_clients() {
                    warn!(%error, "cannot flush Wayland clients");
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| anyhow!("cannot insert the Wayland display source: {error}"))?;

    // Vblank events drive frame completion and the next repaint.
    handle
        .insert_source(
            drm_notifier,
            |event, _metadata, data: &mut DrmState| match event {
                DrmEvent::VBlank(crtc) => data.frame_finished(crtc),
                DrmEvent::Error(error) => warn!(%error, "DRM device error"),
            },
        )
        .map_err(|error| anyhow!("cannot insert the DRM notifier source: {error}"))?;

    // Session pause/resume (VT switching, suspend).
    handle
        .insert_source(
            session_notifier,
            |event, _, data: &mut DrmState| match event {
                SessionEvent::PauseSession => {
                    info!("session paused");
                    data.backend.libinput.suspend();
                    data.backend.manager.pause();
                    data.backend.frame_pending = false;
                    data.backend.needs_redraw = true;
                }
                SessionEvent::ActivateSession => {
                    info!("session resumed");
                    if data.backend.libinput.resume().is_err() {
                        warn!("cannot resume libinput");
                    }
                    match data.backend.manager.activate(false) {
                        Ok(()) => {
                            data.backend.frame_pending = false;
                            data.request_redraw();
                        }
                        Err(error) => warn!(%error, "cannot activate the DRM device"),
                    }
                }
            },
        )
        .map_err(|error| anyhow!("cannot insert the session notifier source: {error}"))?;

    // Physical input.
    let libinput_backend = LibinputInputBackend::new(state.backend.libinput.clone());
    handle
        .insert_source(libinput_backend, |mut event, _, data: &mut DrmState| {
            if let InputEvent::DeviceAdded { device } = &mut event {
                if device.has_capability(DeviceCapability::Keyboard) {
                    if let Some(led_state) = data.compositor.keyboard_led_state() {
                        device.led_update(led_state.into());
                    }
                    data.backend.keyboards.push(device.clone());
                }
            } else if let InputEvent::DeviceRemoved { device } = &event {
                data.backend.keyboards.retain(|item| item != device);
            }

            data.compositor.process_input_event(event);
            data.request_redraw();
        })
        .map_err(|error| anyhow!("cannot insert the libinput source: {error}"))?;

    // Configuration reloads, notifications, idle policy and IPC.
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(16)),
            |_, _, data: &mut DrmState| {
                data.tick();
                TimeoutAction::ToDuration(Duration::from_millis(16))
            },
        )
        .map_err(|error| anyhow!("cannot insert the housekeeping timer: {error}"))?;

    info!(output = %output_name, "BubbleWM DRM compositor is ready");
    state.request_redraw();

    let mut event_loop_error = None;
    loop {
        if shutdown_requested() {
            break;
        }

        match event_loop.dispatch(Some(Duration::from_millis(16)), &mut state) {
            Ok(()) => {}
            Err(error) if shutdown_requested() => {
                debug!(%error, "event loop interrupted during shutdown");
                break;
            }
            Err(error) => {
                error!(%error, "DRM event loop failed");
                event_loop_error = Some(error);
                break;
            }
        }
    }

    drop(state);
    drop(ipc_runtime);
    info!("BubbleWM DRM backend stopped");
    if let Some(error) = event_loop_error {
        return Err(error).context("DRM event loop failed");
    }
    Ok(())
}
