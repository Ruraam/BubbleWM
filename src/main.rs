//! BubbleWM application entry point.

#[cfg(feature = "native-backend")]
mod backend;
mod config;

use anyhow::{Context, Result, bail};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--native-session-monitor") {
        #[cfg(feature = "native-backend")]
        {
            return backend::native::run_session_monitor();
        }
        #[cfg(not(feature = "native-backend"))]
        {
            bail!(
                "native session monitoring is not compiled in; rebuild with --features native-backend"
            );
        }
    }

    let config_path = std::env::args().nth(1);
    let config = config::load(config_path.as_deref())?;

    tracing::info!("BubbleWM {}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configured backend: {}", config.core.backend);
    tracing::info!("Configured layout: {}", config.core.layout);

    match config.core.backend.as_str() {
        "winit" => bubblewm::compositor::run_winit(&config.core.layout)
            .context("BubbleWM Winit compositor failed"),
        "drm" => {
            #[cfg(feature = "native-backend")]
            {
                let probe = backend::native::probe()
                    .context("could not initialize the native DRM device probe")?;
                tracing::info!(
                    seat = %probe.seat,
                    session_active = probe.session_active,
                    drm_devices = probe.drm_devices.len(),
                    "Native session and DRM discovery completed"
                );
                for path in &probe.drm_devices {
                    tracing::info!(device = %path.display(), "DRM device discovered");
                }
                if let Some(device) = &probe.initialized_device {
                    tracing::info!(
                        device = %device.path.display(),
                        device_id = device.device_id,
                        atomic_modesetting = device.atomic_modesetting,
                        crtcs = device.crtc_count,
                        "Smithay DRM device initialized and released through libseat"
                    );
                } else {
                    tracing::warn!("No DRM card device is available on the active seat");
                }
                bail!(
                    "native session and Smithay DRM device initialization completed, but KMS output setup and rendering are not implemented yet"
                );
            }
            #[cfg(not(feature = "native-backend"))]
            {
                bail!(
                    "DRM discovery support is not compiled in; rebuild with --features native-backend"
                );
            }
        }
        backend => bail!("unsupported compositor backend: {backend}"),
    }
}
