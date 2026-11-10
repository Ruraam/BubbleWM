//! TOML configuration loading and validation.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub core: CoreConfig,
    pub effects: EffectsConfig,
    pub theme: ThemeConfig,
    pub decoration: DecorationConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    pub backend: String,
    pub layout: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EffectsConfig {
    pub enabled: bool,
    pub profile: String,
    pub blur: bool,
    pub transparency: bool,
    pub shadows: bool,
    pub wobbly: bool,
    pub workspace_cube: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub source: String,
    pub automatic_contrast: bool,
    pub smooth_transitions: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecorationConfig {
    pub mode: String,
    pub titlebar: bool,
    pub rounded_corners: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            core: CoreConfig::default(),
            effects: EffectsConfig::default(),
            theme: ThemeConfig::default(),
            decoration: DecorationConfig::default(),
        }
    }
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            backend: "winit".into(),
            layout: "floating".into(),
        }
    }
}
impl Default for EffectsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: "advanced".into(),
            blur: true,
            transparency: true,
            shadows: true,
            wobbly: true,
            workspace_cube: true,
        }
    }
}
impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            source: "wallpaper".into(),
            automatic_contrast: true,
            smooth_transitions: true,
        }
    }
}
impl Default for DecorationConfig {
    fn default() -> Self {
        Self {
            mode: "server".into(),
            titlebar: true,
            rounded_corners: true,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.core.backend == "winit" || self.core.backend == "drm",
            "core.backend must be 'winit' or 'drm'"
        );
        anyhow::ensure!(
            self.core.layout == "floating" || self.core.layout == "tiling",
            "core.layout must be 'floating' or 'tiling'"
        );
        anyhow::ensure!(
            ["lite", "balanced", "advanced"].contains(&self.effects.profile.as_str()),
            "effects.profile must be 'lite', 'balanced' or 'advanced'"
        );
        anyhow::ensure!(
            self.theme.source == "wallpaper" || self.theme.source == "manual",
            "theme.source must be 'wallpaper' or 'manual'"
        );
        anyhow::ensure!(
            self.decoration.mode == "server" || self.decoration.mode == "client",
            "decoration.mode must be 'server' or 'client'"
        );
        Ok(())
    }
}

pub fn load(path: Option<&str>) -> Result<Config> {
    let Some(path) = path else {
        return Ok(Config::default());
    };
    let raw = fs::read_to_string(Path::new(path))
        .with_context(|| format!("cannot read configuration file: {path}"))?;
    let config: Config =
        toml::from_str(&raw).with_context(|| format!("invalid TOML configuration: {path}"))?;
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default()
            .validate()
            .expect("default config should be valid");
    }

    #[test]
    fn rejects_unknown_backend() {
        let config = Config {
            core: CoreConfig {
                backend: "made-up".into(),
                ..CoreConfig::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
