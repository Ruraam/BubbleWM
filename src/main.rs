//! BubbleWM application entry point.

use anyhow::{Context, Result};
use bubblewm::config;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"));
    let config = config::Config::load(config_path.as_deref().map(std::path::Path::new))?;

    tracing::info!("BubbleWM {}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configured backend: {}", config.core.backend);
    tracing::info!("Configured layout: {}", config.core.layout);

    bubblewm::compositor::run(config).context("BubbleWM compositor failed")
}
