//! BubbleWM application entry point.
//!
//! This milestone provides configuration loading and diagnostics only.
//! The Smithay compositor state and event loop are implemented in the next milestone.

mod config;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config_path = std::env::args().nth(1);
    let config = config::load(config_path.as_deref())?;

    tracing::info!("BubbleWM {}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configured backend: {}", config.core.backend);
    tracing::info!("Configured layout: {}", config.core.layout);
    tracing::warn!("No Wayland compositor is started in milestone 0.1.0.");

    Ok(())
}
