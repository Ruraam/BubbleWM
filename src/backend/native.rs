//! Native session and DRM device discovery.
//!
//! Device access is performed only through the active libseat session. This
//! diagnostic probe opens at most one seat-assigned DRM card node, validates
//! the descriptor, initializes Smithay's DrmDevice without disabling
//! connectors, then returns the descriptor to libseat. It does not create
//! rendering surfaces or perform modesetting.

use anyhow::{Context, Result, anyhow, bail, ensure};
use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd},
        session::{Session, libseat::LibSeatSession},
        udev::UdevBackend,
    },
    reexports::rustix::fs::OFlags,
    utils::DeviceFd,
};
use std::{
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::OwnedFd,
    },
    path::PathBuf,
};

/// Information collected while Smithay's DRM device is initialized.
#[derive(Debug)]
pub struct NativeDrmDevice {
    pub path: PathBuf,
    pub device_id: u64,
    pub atomic_modesetting: bool,
    pub crtc_count: usize,
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
