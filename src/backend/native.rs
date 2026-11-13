//! Native session and DRM device discovery.
//!
//! Device access is performed only through the active libseat session. This
//! diagnostic probe opens at most one seat-assigned DRM card node, validates
//! the opened descriptor, and closes it again. It does not initialize KMS.

use anyhow::{Context, Result, bail, ensure};
use smithay::{
    backend::{
        session::{Session, libseat::LibSeatSession},
        udev::UdevBackend,
    },
    reexports::rustix::fs::OFlags,
};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::PathBuf,
};

/// Snapshot of the active seat and DRM card nodes visible to that seat.
#[derive(Debug)]
pub struct NativeProbe {
    pub seat: String,
    pub session_active: bool,
    pub drm_devices: Vec<PathBuf>,
    /// The one DRM card node opened and closed through libseat, if present.
    pub opened_device: Option<PathBuf>,
}

/// Initializes libseat, enumerates its DRM devices, and safely probes one node.
///
/// The session notifier is intentionally kept alive for the entire operation.
/// No device is opened when the session is inactive. The opened descriptor is
/// always returned to the session close method, including when inspection fails.
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

    let opened_device = if let Some((device_id, path)) = devices.first() {
        open_validate_and_close(&mut session, *device_id, path)?;
        Some(path.clone())
    } else {
        None
    };

    Ok(NativeProbe {
        seat,
        session_active,
        drm_devices,
        opened_device,
    })
}

fn open_validate_and_close(
    session: &mut LibSeatSession,
    expected_device_id: u64,
    path: &std::path::Path,
) -> Result<()> {
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .with_context(|| format!("libseat could not open DRM device {}", path.display()))?;

    // Keep the descriptor alive while collecting metadata. Convert it back to
    // OwnedFd so libseat, not a plain close(2), releases the device.
    let file = std::fs::File::from(fd);
    let metadata_result = file.metadata();
    let fd = file.into();

    session
        .close(fd)
        .with_context(|| format!("libseat could not close DRM device {}", path.display()))?;

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
}
