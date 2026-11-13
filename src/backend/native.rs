//! Native session and DRM device discovery.
//!
//! Device access is performed only through the active libseat session. This
//! diagnostic probe opens at most one seat-assigned DRM card node, validates
//! the descriptor, initializes Smithay's DrmDevice without disabling
//! connectors, then returns the descriptor to libseat. It does not create
//! rendering surfaces or perform modesetting.

use anyhow::{Context, Result, anyhow, bail, ensure};
use smithay::{
    reexports::calloop::EventLoop,
    backend::{
        drm::{DrmDevice, DrmDeviceFd},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::UdevBackend,
    },
    reexports::{calloop::EventLoop, rustix::fs::OFlags},
    utils::DeviceFd,
};
use std::{
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::OwnedFd,
    },
    path::PathBuf,
    time::Duration,
};

/// Information collected while Smithay's DRM device is initialized.
#[derive(Debug)]
pub struct NativeDrmDevice {
    pub path: PathBuf,
    pub device_id: u64,
    pub atomic_modesetting: bool,
    pub crtc_count: usize,
}

/// Current access state reported by the libseat session notifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSessionState {
    /// The compositor currently owns the seat and may access its devices.
    Active,
    /// The seat has been revoked; device access must stop until activation.
    Paused,
}

/// Result of applying a libseat session event to the lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSessionTransition {
    /// The session was active and has now been paused.
    Pause,
    /// The session was paused and has now been activated.
    Activate,
    /// The event repeated the state already in effect.
    Unchanged,
}

/// Small, deterministic state machine for libseat pause/activation events.
///
/// This policy is kept separate from device ownership so transitions can be
/// tested without DRM hardware. The calloop notifier and device close/reopen
/// integration are deliberately a later step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSessionLifecycle {
    state: NativeSessionState,
}

impl NativeSessionLifecycle {
    /// Creates lifecycle state from the session's initial active flag.
    pub fn new(active: bool) -> Self {
        Self {
            state: if active {
                NativeSessionState::Active
            } else {
                NativeSessionState::Paused
            },
        }
    }

    /// Returns the current session state.
    pub fn state(&self) -> NativeSessionState {
        self.state
    }

    /// Returns whether opening or using seat devices is currently permitted.
    pub fn permits_device_access(&self) -> bool {
        self.state == NativeSessionState::Active
    }

    /// Applies a libseat event and reports whether the state changed.
    pub fn apply(&mut self, event: SessionEvent) -> NativeSessionTransition {
        match (self.state, event) {
            (NativeSessionState::Active, SessionEvent::PauseSession) => {
                self.state = NativeSessionState::Paused;
                NativeSessionTransition::Pause
            }
            (NativeSessionState::Paused, SessionEvent::ActivateSession) => {
                self.state = NativeSessionState::Active;
                NativeSessionTransition::Activate
            }
            _ => NativeSessionTransition::Unchanged,
        }
    }
}

/// Snapshot of the active seat and DRM card nodes visible to that seat.
#[derive(Debug)]
pub struct NativeProbe {
    pub seat: String,
    pub session_active: bool,
    pub drm_devices: Vec<PathBuf>,
    /// The one DRM device initialized and released through libseat, if present.
    pub initialized_device: Option<NativeDrmDevice>,
}

/// Initializes libseat, enumerates its DRM devices, and safely probes one node.
///
/// The session notifier is intentionally kept alive for the entire operation.
/// No device is opened when the session is inactive. The descriptor is retained
/// through a recovery handle while Smithay initializes and drops its DRM device,
/// so it can always be returned through Session::close, including on errors.
pub fn probe() -> Result<NativeProbe> {
    let (mut session, _notifier) =
        LibSeatSession::new().context("failed to acquire a libseat session")?;
    let seat = session.seat();
    let session_active = session.is_active();

    if !session_active {
        bail!("libseat session is not active; refusing to open a DRM device");
    }

    let udev = UdevBackend::new(&seat)
        .with_context(|| format!("failed to enumerate DRM devices for seat {seat}"))?;

    let mut devices: Vec<_> = udev
        .device_list()
        .map(|(device_id, path)| (device_id, path.to_path_buf()))
        .collect();
    devices.sort_by(|left, right| left.1.cmp(&right.1));

    let drm_devices = devices.iter().map(|(_, path)| path.clone()).collect();

    let initialized_device = if let Some((device_id, path)) = devices.first() {
        Some(open_validate_and_initialize(
            &mut session,
            *device_id,
            path,
        )?)
    } else {
        None
    };

    Ok(NativeProbe {
        seat,
        session_active,
        drm_devices,
        initialized_device,
    })
}

fn open_validate_and_initialize(
    session: &mut LibSeatSession,
    expected_device_id: u64,
    path: &std::path::Path,
) -> Result<NativeDrmDevice> {
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .with_context(|| format!("libseat could not open DRM device {}", path.display()))?;

    let file = std::fs::File::from(fd);
    let metadata_result = file.metadata();
    let fd: OwnedFd = file.into();

    let validation_result = (|| -> Result<()> {
        let metadata = metadata_result
            .with_context(|| format!("could not inspect opened DRM device {}", path.display()))?;
        ensure!(
            metadata.file_type().is_char_device(),
            "opened DRM node {} is not a character device",
            path.display()
        );
        ensure!(
            metadata.rdev() == expected_device_id,
            "opened DRM node {} no longer matches the udev device identity",
            path.display()
        );
        Ok(())
    })();

    if let Err(error) = validation_result {
        session
            .close(fd)
            .with_context(|| format!("libseat could not close DRM device {}", path.display()))?;
        return Err(error);
    }

    // DeviceFd is reference-counted. Keep one recovery reference while
    // DrmDevice::new consumes its own handle. Once the notifier and device are
    // dropped, this becomes the unique owner and can be returned to libseat.
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let recovery_fd = drm_fd.device_fd();

    // Do not disable connectors during this diagnostic initialization.
    // No DrmSurface is created and no display mode is selected or committed.
    let initialization_result = DrmDevice::new(drm_fd, false)
        .map(|(device, notifier)| {
            let snapshot = (
                device.device_id() as u64,
                device.is_atomic(),
                device.crtcs().len(),
            );
            drop(notifier);
            drop(device);
            snapshot
        })
        .map_err(|error| {
            anyhow!(error).context(format!(
                "Smithay could not initialize DRM device {}",
                path.display()
            ))
        });

    let fd: OwnedFd = recovery_fd
        .try_into()
        .map_err(|_| anyhow!("Smithay retained unexpected DRM descriptor references"))?;
    session
        .close(fd)
        .with_context(|| format!("libseat could not close DRM device {}", path.display()))?;

    let (device_id, atomic_modesetting, crtc_count) = initialization_result?;
    ensure!(
        device_id == expected_device_id,
        "Smithay DRM device identity does not match udev for {}",
        path.display()
    );

    Ok(NativeDrmDevice {
        path: path.to_path_buf(),
        device_id,
        atomic_modesetting,
        crtc_count,
    })
}

#[cfg(test)]
mod tests {
    use super::{NativeSessionLifecycle, NativeSessionState, NativeSessionTransition};
    use smithay::backend::session::Event as SessionEvent;

    #[test]
    fn initially_inactive_session_cannot_access_devices() {
        let lifecycle = NativeSessionLifecycle::new(false);

        assert_eq!(lifecycle.state(), NativeSessionState::Paused);
        assert!(!lifecycle.permits_device_access());
    }

    #[test]
    fn pause_revokes_device_access_and_activation_restores_it() {
        let mut lifecycle = NativeSessionLifecycle::new(true);

        assert!(lifecycle.permits_device_access());
        assert_eq!(
            lifecycle.apply(SessionEvent::PauseSession),
            NativeSessionTransition::Pause
        );
        assert_eq!(lifecycle.state(), NativeSessionState::Paused);
        assert!(!lifecycle.permits_device_access());

        assert_eq!(
            lifecycle.apply(SessionEvent::ActivateSession),
            NativeSessionTransition::Activate
        );
        assert_eq!(lifecycle.state(), NativeSessionState::Active);
        assert!(lifecycle.permits_device_access());
    }

    #[test]
    fn duplicate_session_events_are_idempotent() {
        let mut lifecycle = NativeSessionLifecycle::new(true);

        assert_eq!(
            lifecycle.apply(SessionEvent::PauseSession),
            NativeSessionTransition::Pause
        );
        assert_eq!(
            lifecycle.apply(SessionEvent::PauseSession),
            NativeSessionTransition::Unchanged
        );
        assert_eq!(
            lifecycle.apply(SessionEvent::ActivateSession),
            NativeSessionTransition::Activate
        );
        assert_eq!(
            lifecycle.apply(SessionEvent::ActivateSession),
            NativeSessionTransition::Unchanged
        );
    }
}


/// Runs an explicit, diagnostic-only calloop monitor for libseat session events.
///
/// This is intentionally separate from the Winit compositor loop. It does not
/// initialize DRM devices or claim to run a native compositor; it only proves
/// that the real libseat notifier is dispatched and that the tested lifecycle
/// policy follows pause/activation events.
pub fn run_session_monitor() -> Result<()> {
    let (session, notifier) =
        LibSeatSession::new().context("failed to acquire a libseat session")?;
    let seat = session.seat();
    let active = session.is_active();
    let mut lifecycle = NativeSessionLifecycle::new(active);
    let mut event_loop = EventLoop::<NativeSessionLifecycle>::try_new()
        .context("failed to create the native calloop event loop")?;

    event_loop
        .handle()
        .insert_source(notifier, |event, _, lifecycle| {
            let transition = lifecycle.apply(event);
            tracing::info!(
                ?event,
                ?transition,
                state = ?lifecycle.state(),
                device_access = lifecycle.permits_device_access(),
                "libseat session event"
            );
        })
        .context("failed to register the libseat session notifier")?;

    tracing::info!(
        seat = %seat,
        session_active = active,
        "Native libseat session monitor started; press Ctrl+C to stop"
    );

    // Keep the session handle alive for as long as its notifier is registered.
    let _session = session;
    loop {
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut lifecycle)
            .context("native libseat event loop failed")?;
    }
}
