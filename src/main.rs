//! BubbleWM application entry point.

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
        "drm" => bail!("the DRM/KMS backend is not implemented yet; use core.backend = \"winit\""),
        backend => bail!("unsupported compositor backend: {backend}"),
    }
}
