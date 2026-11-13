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
                    .context("could not initialize the native DRM discovery probe")?;
                tracing::info!(
                    seat = %probe.seat,
                    session_active = probe.session_active,
                    drm_devices = probe.drm_devices.len(),
                    "Native session and DRM discovery completed"
                );
                for path in &probe.drm_devices {
                    tracing::info!(device = %path.display(), "DRM device discovered");
                }
                bail!(
                    "native session and DRM discovery succeeded, but DRM/KMS rendering is not implemented yet"
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
