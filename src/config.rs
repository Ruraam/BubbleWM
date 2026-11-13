//! TOML configuration loading, validation and atomic persistence.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const CONFIG_SCHEMA_VERSION: u32 = 1;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Incremented when an explicit configuration migration is introduced.
    pub schema_version: u32,
    pub core: CoreConfig,
    pub effects: EffectsConfig,
    pub theme: ThemeConfig,
    pub appearance: AppearanceConfig,
    pub bubble_color: BubbleColorConfig,
    pub decoration: DecorationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    pub backend: String,
    pub layout: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub source: String,
    pub automatic_contrast: bool,
    pub smooth_transitions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceConfig {
    /// Optional image path. Wallpaper rendering is provided by a future desktop service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<String>,
    pub wallpaper_mode: String,
    pub icon_theme: String,
    pub cursor_theme: String,
    pub cursor_size: u16,
    pub font_family: String,
    pub monospace_font_family: String,
    pub font_size: u16,
    pub color_scheme: String,
    pub accent_color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BubbleColorConfig {
    pub enabled: bool,
    pub engine: String,
    pub palette_source: String,
    pub palette_variant: String,
    pub enforce_contrast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecorationConfig {
    pub mode: String,
    pub titlebar: bool,
    pub rounded_corners: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            core: CoreConfig::default(),
            effects: EffectsConfig::default(),
            theme: ThemeConfig::default(),
            appearance: AppearanceConfig::default(),
            bubble_color: BubbleColorConfig::default(),
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

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            wallpaper: None,
            wallpaper_mode: "zoom".into(),
            icon_theme: "Bubble".into(),
            cursor_theme: "default".into(),
            cursor_size: 24,
            font_family: "Sans".into(),
            monospace_font_family: "Monospace".into(),
            font_size: 10,
            color_scheme: "dark".into(),
            accent_color: "#8B5CF6".into(),
        }
    }
}

impl Default for BubbleColorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            engine: "bubble-color".into(),
            palette_source: "wallpaper".into(),
            palette_variant: "vivid".into(),
            enforce_contrast: true,
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
            self.schema_version == CONFIG_SCHEMA_VERSION,
            "unsupported configuration schema version {}; supported version is {}",
            self.schema_version,
            CONFIG_SCHEMA_VERSION
        );
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
            ["server", "client"].contains(&self.decoration.mode.as_str()),
            "decoration.mode must be 'server' or 'client'"
        );
        anyhow::ensure!(
            ["center", "contain", "cover", "zoom", "tile"]
                .contains(&self.appearance.wallpaper_mode.as_str()),
            "appearance.wallpaper_mode must be center, contain, cover, zoom or tile"
        );
        anyhow::ensure!(
            ["dark", "light", "system"].contains(&self.appearance.color_scheme.as_str()),
            "appearance.color_scheme must be dark, light or system"
        );
        anyhow::ensure!(
            (16..=128).contains(&self.appearance.cursor_size),
            "appearance.cursor_size must be between 16 and 128"
        );
        anyhow::ensure!(
            (6..=32).contains(&self.appearance.font_size),
            "appearance.font_size must be between 6 and 32"
        );
        anyhow::ensure!(
            !self.appearance.icon_theme.trim().is_empty(),
            "appearance.icon_theme must not be empty"
        );
        anyhow::ensure!(
            !self.appearance.font_family.trim().is_empty()
                && !self.appearance.monospace_font_family.trim().is_empty(),
            "appearance font families must not be empty"
        );
        anyhow::ensure!(
            is_hex_color(&self.appearance.accent_color),
            "appearance.accent_color must be a #RRGGBB color"
        );
        if let Some(wallpaper) = &self.appearance.wallpaper {
            anyhow::ensure!(
                !wallpaper.trim().is_empty() && !wallpaper.contains('\0'),
                "appearance.wallpaper must be a non-empty path without NUL bytes"
            );
        }
        anyhow::ensure!(
            self.bubble_color.engine == "bubble-color",
            "bubble_color.engine must be 'bubble-color'"
        );
        anyhow::ensure!(
            ["wallpaper", "manual"].contains(&self.bubble_color.palette_source.as_str()),
            "bubble_color.palette_source must be wallpaper or manual"
        );
        anyhow::ensure!(
            ["subtle", "balanced", "vivid", "neon"]
                .contains(&self.bubble_color.palette_variant.as_str()),
            "bubble_color.palette_variant must be subtle, balanced, vivid or neon"
        );
        Ok(())
    }
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

/// Return the standard per-user configuration path without creating it.
pub fn default_path() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg);
        if path.is_absolute() {
            return Ok(path.join("bubblewm/config.toml"));
        }
    }

    let home = std::env::var_os("HOME")
        .context("neither absolute XDG_CONFIG_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".config/bubblewm/config.toml"))
}

pub fn load(path: Option<&str>) -> Result<Config> {
    let path = match path {
        Some(path) => PathBuf::from(path),
        None => default_path()?,
    };
    if !path.exists() {
        return Ok(Config::default());
    }

    let display_path = path.display().to_string();
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("cannot read configuration file: {display_path}"))?;
    let config: Config =
        toml::from_str(&raw)
            .with_context(|| format!("invalid TOML configuration: {display_path}"))?;
    config.validate()?;
    Ok(config)
}

/// Validate and atomically replace a configuration file.
///
/// The temporary file is created beside the destination so rename stays on one filesystem.
/// On Unix it is private to the current user (0600). The active file is never truncated in place.
pub fn save_atomic(path: &Path, config: &Config) -> Result<()> {
    config
        .validate()
        .context("refusing to save invalid configuration")?;
    let serialized =
        toml::to_string_pretty(config).context("cannot serialize configuration")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("cannot create configuration directory: {}", parent.display()))?;

    let file_name = path.file_name().context("configuration path has no file name")?;
    let mut temporary_path = None;
    let mut temporary_file = None;
    for _ in 0..32 {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = file_name.to_os_string();
        temporary_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let candidate = parent.join(temporary_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                temporary_path = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("cannot create temporary configuration file"),
        }
    }

    let temporary_path =
        temporary_path.context("could not allocate a unique temporary config file")?;
    let write_result = (|| -> Result<()> {
        let mut file = temporary_file.expect("temporary path and file are created together");
        file.write_all(serialized.as_bytes())
            .context("cannot write temporary configuration")?;
        file.sync_all().context("cannot sync temporary configuration")?;
        fs::rename(&temporary_path, path)
            .with_context(|| {
                format!("cannot atomically replace configuration: {}", path.display())
            })?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .context("cannot sync configuration directory")?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
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

    #[test]
    fn rejects_invalid_appearance_values() {
        let mut config = Config::default();
        config.appearance.accent_color = "purple".into();
        assert!(config.validate().is_err());

        config = Config::default();
        config.appearance.cursor_size = 8;
        assert!(config.validate().is_err());
    }

    #[test]
    fn legacy_config_uses_new_appearance_defaults() {
        let config: Config = toml::from_str(
            r#"
            [core]
            backend = "winit"
            layout = "floating"

            [effects]
            enabled = true
            profile = "advanced"
            blur = true
            transparency = true
            shadows = true
            wobbly = true
            workspace_cube = true

            [theme]
            source = "wallpaper"
            automatic_contrast = true
            smooth_transitions = true

            [decoration]
            mode = "server"
            titlebar = true
            rounded_corners = true
            "#,
        )
        .expect("legacy configuration should deserialize");
        config
            .validate()
            .expect("legacy config should remain valid");
        assert_eq!(config.appearance.icon_theme, "Bubble");
        assert_eq!(config.bubble_color.engine, "bubble-color");
    }

    #[test]
    fn atomic_save_round_trips() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let mut config = Config::default();
        config.appearance.icon_theme = "Bubble-Neon".into();

        save_atomic(&path, &config).expect("save should succeed");
        let loaded: Config =
            toml::from_str(&fs::read_to_string(path).expect("saved file"))
            .expect("saved TOML should parse");
        assert_eq!(loaded.appearance.icon_theme, "Bubble-Neon");
    }

    #[test]
    fn invalid_config_does_not_replace_existing_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "keep me").expect("seed file");
        let mut config = Config::default();
        config.appearance.accent_color = "not-a-color".into();

        assert!(save_atomic(&path, &config).is_err());
        assert_eq!(
            fs::read_to_string(path).expect("existing file"),
            "keep me"
        );
    }
}
