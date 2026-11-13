//! Native session and DRM device discovery.
//!
//! This module deliberately stops before opening a DRM device or configuring a
//! display. Device access must go through the active libseat session when the
//! DRM/KMS runtime is implemented.

use anyhow::{Context, Result};
use smithay::backend::{
    session::{Session, libseat::LibSeatSession},
    udev::UdevBackend,
};
use std::path::PathBuf;

/// Snapshot of the active seat and DRM card nodes visible to that seat.
#[derive(Debug)]
pub struct NativeProbe {
    pub seat: String,
    pub session_active: bool,
    pub drm_devices: Vec<PathBuf>,
}

/// Initializes libseat and enumerates DRM devices assigned to its seat.
///
/// The session notifier is intentionally kept alive until enumeration ends.
/// This is a diagnostic preparation step, not a running compositor backend.
pub fn probe() -> Result<NativeProbe> {
    let (session, _notifier) =
        LibSeatSession::new().context("failed to acquire a libseat session")?;
    let seat = session.seat();
    let session_active = session.is_active();

    let udev = UdevBackend::new(&seat)
        .with_context(|| format!("failed to enumerate DRM devices for seat {seat}"))?;
    let drm_devices = udev
        .device_list()
        .map(|(_, path)| path.to_path_buf())
        .collect();

    Ok(NativeProbe {
        seat,
        session_active,
        drm_devices,
    })
}
