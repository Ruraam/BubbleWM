//! Typed TOML configuration for BubbleWM.
//!
//! The configuration file is the single source of truth for the compositor,
//! the lock screen, the launcher, notifications and the built-in wallpaper.
//! `bubble-settings` edits the same files through [`Config::save`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Directory used when no user configuration exists yet.
pub const SYSTEM_CONFIG: &str = "/etc/bubblewm/config.toml";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub core: CoreConfig,
    pub input: InputConfig,
    pub decoration: DecorationConfig,
    pub theme: ThemeConfig,
    pub wallpaper: WallpaperConfig,
    pub launcher: LauncherConfig,
    pub notifications: NotificationsConfig,
    pub bar: BarConfig,
    pub idle: IdleConfig,
    pub lock: LockConfig,
    pub effects: EffectsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    /// `auto`, `winit` or `drm`.
    pub backend: String,
    /// `floating` or `tiling`.
    pub layout: String,
    pub terminal: String,
    pub workspaces: u32,
    pub mod_key: String,
    pub window_gap: i32,
    pub focus_policy: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    pub keyboard_layout: String,
    pub keyboard_variant: String,
    pub keyboard_model: String,
    pub keyboard_options: String,
    pub repeat_delay: u32,
    pub repeat_rate: u32,
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    pub left_handed: bool,
    pub pointer_speed: f64,
    pub accel_profile: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DecorationConfig {
    /// `server` or `client`.
    pub mode: String,
    pub titlebar: bool,
    pub rounded_corners: bool,
    /// Global window opacity. Lower values reveal the wallpaper through every
    /// client surface while keeping text legible.
    pub window_opacity: f32,
    /// Multiplier applied to the titlebar glass, independent from the client
    /// surface opacity.
    pub titlebar_opacity: f32,
    /// Opacity of the thin BubbleColor outline drawn around every window.
    pub outline_opacity: f32,
    /// Optional outline color. Empty means the BubbleColor primary role.
    pub outline_color: String,
    pub titlebar_height: i32,
    pub border_width: i32,
    pub corner_radius: i32,
    pub active_color: String,
    pub inactive_color: String,
    pub text_color: String,
    pub accent_color: String,
    pub button_style: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// `wallpaper` or `manual`.
    pub source: String,
    pub automatic_contrast: bool,
    /// `dark` or `light`. Used when `source = "wallpaper"`.
    pub palette_mode: String,
    /// `balanced`, `vivid` or `muted`. Used when `source = "wallpaper"`.
    pub palette_style: String,
    /// WCAG contrast target between 4.5 and 7.0.
    pub contrast_target: f32,
    pub smooth_transitions: bool,
    pub accent: String,
    pub background: String,
    pub foreground: String,
    pub font: String,
    pub font_size: i32,
    pub icon_theme: String,
    pub cursor_theme: String,
    pub cursor_size: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WallpaperConfig {
    pub enabled: bool,
    pub path: String,
    /// `fill`, `fit`, `stretch`, `center` or `tile`.
    pub mode: String,
    pub color: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    pub enabled: bool,
    pub width: i32,
    pub max_results: usize,
    pub keybind: String,
    pub terminal: String,
    pub show_desktop_entries: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub timeout: u32,
    pub width: i32,
    pub max_visible: usize,
    /// `top-right`, `top-left`, `bottom-right` or `bottom-left`.
    pub position: String,
    pub margin: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BarConfig {
    pub enabled: bool,
    pub height: i32,
    pub margin: i32,
    pub show_logo: bool,
    pub show_wifi: bool,
    pub show_workspaces: bool,
    pub show_title: bool,
    pub show_clock: bool,
    pub show_volume: bool,
    pub show_power: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IdleConfig {
    pub enabled: bool,
    /// Screen blanking delay. Zero disables blanking.
    pub dpms_seconds: u32,
    /// Lock delay. Zero disables automatic locking.
    pub lock_seconds: u32,
    /// Suspend delay. Zero disables suspend.
    pub suspend_seconds: u32,
    pub lock_on_suspend: bool,
    pub inhibit_when_fullscreen: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LockConfig {
    pub enabled: bool,
    pub pam_service: String,
    pub show_clock: bool,
    pub background_dim: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct EffectsConfig {
    pub enabled: bool,
    pub profile: String,
    pub blur: bool,
    pub transparency: bool,
    pub shadows: bool,
    pub glow: bool,
    pub wobbly: bool,
    pub workspace_cube: bool,
    /// Enables Compiz-style window and workspace transitions.
    pub animations: bool,
    /// Transition duration in milliseconds.
    pub animation_duration: u32,
    /// Easing curve: linear, ease-out, ease-in-out, back or elastic.
    pub animation_easing: String,
    /// Window opening style: none, fade, zoom, glide, pop or slide.
    pub open_effect: String,
    /// Window closing style: none, fade, zoom, glide, burn or slide.
    pub close_effect: String,
    /// Minimize style: none, fade, zoom or glide.
    pub minimize_effect: String,
    /// Workspace switch style: none, fade, zoom, slide or cube.
    pub workspace_effect: String,
    /// Blur strength used by the live Dual Kawase backdrop and CPU fallback.
    pub blur_strength: f32,
    /// Optional glass tint. Empty means the BubbleColor surface role.
    pub glass_tint: String,
    /// Opacity of the bar capsules.
    pub bar_opacity: f32,
    /// Opacity of taskbar and desktop popup menus.
    pub menu_opacity: f32,
    /// Opacity of the application launcher panel.
    pub launcher_opacity: f32,
    /// Opacity of notification cards.
    pub notification_opacity: f32,
    /// Opacity of the lock screen card.
    pub lock_opacity: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            core: CoreConfig::default(),
            input: InputConfig::default(),
            decoration: DecorationConfig::default(),
            theme: ThemeConfig::default(),
            wallpaper: WallpaperConfig::default(),
            launcher: LauncherConfig::default(),
            notifications: NotificationsConfig::default(),
            bar: BarConfig::default(),
            idle: IdleConfig::default(),
            lock: LockConfig::default(),
            effects: EffectsConfig::default(),
        }
    }
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            layout: "floating".into(),
            terminal: "foot".into(),
            workspaces: 4,
            mod_key: "super".into(),
            window_gap: 8,
            focus_policy: "click".into(),
        }
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard_layout: "us".into(),
            keyboard_variant: String::new(),
            keyboard_model: String::new(),
            keyboard_options: String::new(),
            repeat_delay: 200,
            repeat_rate: 25,
            natural_scroll: false,
            tap_to_click: true,
            left_handed: false,
            pointer_speed: 0.0,
            accel_profile: "adaptive".into(),
        }
    }
}

impl Default for DecorationConfig {
    fn default() -> Self {
        Self {
            mode: "server".into(),
            titlebar: true,
            rounded_corners: true,
            window_opacity: 0.92,
            titlebar_opacity: 0.82,
            outline_opacity: 0.72,
            outline_color: String::new(),
            titlebar_height: 32,
            border_width: 1,
            corner_radius: 18,
            active_color: "#0c2340".into(),
            inactive_color: "#0f172a".into(),
            text_color: "#e0f2fe".into(),
            accent_color: "#22d3ee".into(),
            button_style: "dots".into(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            source: "wallpaper".into(),
            automatic_contrast: true,
            palette_mode: "dark".into(),
            palette_style: "balanced".into(),
            contrast_target: 4.5,
            smooth_transitions: true,
            accent: "#22d3ee".into(),
            background: "#0c2340".into(),
            foreground: "#e0f2fe".into(),
            font: "Inter".into(),
            font_size: 11,
            icon_theme: "Papirus".into(),
            cursor_theme: "bubble".into(),
            cursor_size: 24,
        }
    }
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/usr/share/backgrounds/bubbleos/bubbleos-wallpaper.png".into(),
            mode: "fill".into(),
            color: "#0c2340".into(),
        }
    }
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 720,
            max_results: 8,
            keybind: "super+d".into(),
            terminal: "foot".into(),
            show_desktop_entries: true,
        }
    }
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: 5000,
            width: 380,
            max_visible: 4,
            position: "top-right".into(),
            margin: 16,
        }
    }
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 34,
            margin: 8,
            show_logo: true,
            show_wifi: true,
            show_workspaces: true,
            show_title: true,
            show_clock: true,
            show_volume: true,
            show_power: true,
        }
    }
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dpms_seconds: 600,
            lock_seconds: 300,
            suspend_seconds: 0,
            lock_on_suspend: true,
            inhibit_when_fullscreen: true,
        }
    }
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pam_service: "bubblewm".into(),
            show_clock: true,
            background_dim: 0.55,
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
            glow: true,
            wobbly: true,
            workspace_cube: true,
            animations: true,
            animation_duration: 280,
            animation_easing: "ease-out".into(),
            open_effect: "zoom".into(),
            close_effect: "zoom".into(),
            minimize_effect: "glide".into(),
            workspace_effect: "slide".into(),
            blur_strength: 3.0,
            glass_tint: String::new(),
            bar_opacity: 0.44,
            menu_opacity: 0.62,
            launcher_opacity: 0.60,
            notification_opacity: 0.62,
            lock_opacity: 0.55,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure_one_of(
            "core.backend",
            &self.core.backend,
            &["auto", "winit", "drm"],
        )?;
        ensure_one_of("core.layout", &self.core.layout, &["floating", "tiling"])?;
        ensure_one_of(
            "core.focus_policy",
            &self.core.focus_policy,
            &["click", "sloppy"],
        )?;
        if self.core.workspaces == 0 || self.core.workspaces > 32 {
            bail!("core.workspaces must be between 1 and 32");
        }
        if self.core.window_gap < 0 || self.core.window_gap > 256 {
            bail!("core.window_gap must be between 0 and 256");
        }
        if self.input.repeat_rate == 0 || self.input.repeat_rate > 200 {
            bail!("input.repeat_rate must be between 1 and 200");
        }
        if !(-1.0..=1.0).contains(&self.input.pointer_speed) {
            bail!("input.pointer_speed must be between -1.0 and 1.0");
        }
        ensure_one_of(
            "input.accel_profile",
            &self.input.accel_profile,
            &["none", "flat", "adaptive"],
        )?;
        ensure_one_of(
            "decoration.mode",
            &self.decoration.mode,
            &["server", "client"],
        )?;
        if self.decoration.titlebar_height < 0
            || self.decoration.titlebar_height > 128
            || self.decoration.border_width < 0
            || self.decoration.border_width > 32
            || self.decoration.corner_radius < 0
            || self.decoration.corner_radius > 64
        {
            bail!("decoration sizes must stay within sensible bounds");
        }
        if !(0.25..=1.0).contains(&self.decoration.window_opacity) {
            bail!("decoration.window_opacity must be between 0.25 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.decoration.titlebar_opacity) {
            bail!("decoration.titlebar_opacity must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.decoration.outline_opacity) {
            bail!("decoration.outline_opacity must be between 0.0 and 1.0");
        }
        if !self.decoration.outline_color.trim().is_empty() {
            validate_color("decoration.outline_color", &self.decoration.outline_color)?;
        }
        validate_color("decoration.active_color", &self.decoration.active_color)?;
        validate_color("decoration.inactive_color", &self.decoration.inactive_color)?;
        validate_color("decoration.text_color", &self.decoration.text_color)?;
        validate_color("decoration.accent_color", &self.decoration.accent_color)?;
        ensure_one_of("theme.source", &self.theme.source, &["wallpaper", "manual"])?;
        ensure_one_of(
            "theme.palette_mode",
            &self.theme.palette_mode,
            &["dark", "light"],
        )?;
        ensure_one_of(
            "theme.palette_style",
            &self.theme.palette_style,
            &["balanced", "vivid", "muted"],
        )?;
        if !(4.5..=7.0).contains(&self.theme.contrast_target) {
            bail!("theme.contrast_target must be between 4.5 and 7.0");
        }
        validate_color("theme.accent", &self.theme.accent)?;
        validate_color("theme.background", &self.theme.background)?;
        validate_color("theme.foreground", &self.theme.foreground)?;
        if self.theme.font_size < 6 || self.theme.font_size > 72 {
            bail!("theme.font_size must be between 6 and 72");
        }
        if self.theme.cursor_size < 8 || self.theme.cursor_size > 128 {
            bail!("theme.cursor_size must be between 8 and 128");
        }
        ensure_one_of(
            "wallpaper.mode",
            &self.wallpaper.mode,
            &["fill", "fit", "stretch", "center", "tile"],
        )?;
        validate_color("wallpaper.color", &self.wallpaper.color)?;
        if self.launcher.width < 320 || self.launcher.width > 2048 {
            bail!("launcher.width must be between 320 and 2048");
        }
        if self.launcher.max_results == 0 || self.launcher.max_results > 64 {
            bail!("launcher.max_results must be between 1 and 64");
        }
        if self.notifications.width < 200 || self.notifications.width > 1024 {
            bail!("notifications.width must be between 200 and 1024");
        }
        if self.bar.height < 20 || self.bar.height > 64 {
            bail!("bar.height must be between 20 and 64");
        }
        if self.bar.margin < 0 || self.bar.margin > 64 {
            bail!("bar.margin must be between 0 and 64");
        }
        ensure_one_of(
            "notifications.position",
            &self.notifications.position,
            &["top-right", "top-left", "bottom-right", "bottom-left"],
        )?;
        if self.lock.background_dim < 0.0 || self.lock.background_dim > 1.0 {
            bail!("lock.background_dim must be between 0.0 and 1.0");
        }
        ensure_one_of(
            "effects.profile",
            &self.effects.profile,
            &["lite", "balanced", "advanced"],
        )?;
        if self.effects.animation_duration > 2000 {
            bail!("effects.animation_duration must be at most 2000 milliseconds");
        }
        ensure_one_of(
            "effects.animation_easing",
            &self.effects.animation_easing,
            &["linear", "ease-out", "ease-in-out", "back", "elastic"],
        )?;
        ensure_one_of(
            "effects.open_effect",
            &self.effects.open_effect,
            &["none", "fade", "zoom", "glide", "pop", "slide"],
        )?;
        ensure_one_of(
            "effects.close_effect",
            &self.effects.close_effect,
            &["none", "fade", "zoom", "glide", "burn", "slide"],
        )?;
        ensure_one_of(
            "effects.minimize_effect",
            &self.effects.minimize_effect,
            &["none", "fade", "zoom", "glide"],
        )?;
        ensure_one_of(
            "effects.workspace_effect",
            &self.effects.workspace_effect,
            &["none", "fade", "zoom", "slide", "cube"],
        )?;
        if !(0.0..=16.0).contains(&self.effects.blur_strength) {
            bail!("effects.blur_strength must be between 0.0 and 16.0");
        }
        if !self.effects.glass_tint.trim().is_empty() {
            validate_color("effects.glass_tint", &self.effects.glass_tint)?;
        }
        for (name, value) in [
            ("effects.bar_opacity", self.effects.bar_opacity),
            ("effects.menu_opacity", self.effects.menu_opacity),
            ("effects.launcher_opacity", self.effects.launcher_opacity),
            (
                "effects.notification_opacity",
                self.effects.notification_opacity,
            ),
            ("effects.lock_opacity", self.effects.lock_opacity),
        ] {
            if !(0.0..=1.0).contains(&value) {
                bail!("{name} must be between 0.0 and 1.0");
            }
        }
        Ok(())
    }

    /// Returns the user configuration path, falling back to the system default.
    pub fn default_path() -> PathBuf {
        if let Some(runtime) = env::var_os("BUBBLEWM_CONFIG") {
            return PathBuf::from(runtime);
        }
        let base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        base.map(|base| base.join("bubblewm/config.toml"))
            .unwrap_or_else(|| PathBuf::from(SYSTEM_CONFIG))
    }

    /// Loads a configuration file. A missing user file falls back to defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(Self::default_path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("cannot read configuration file: {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid TOML configuration: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Writes the configuration atomically so a crash cannot truncate it.
    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(Self::default_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "cannot create configuration directory: {}",
                    parent.display()
                )
            })?;
        }
        let rendered = toml::to_string_pretty(self).context("cannot serialize configuration")?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, rendered)
            .with_context(|| format!("cannot write configuration: {}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("cannot install configuration: {}", path.display()))?;
        Ok(())
    }
}

fn ensure_one_of(name: &str, value: &str, choices: &[&str]) -> Result<()> {
    if choices.contains(&value) {
        Ok(())
    } else {
        bail!("{name} must be one of: {}", choices.join(", "))
    }
}

fn validate_color(name: &str, value: &str) -> Result<()> {
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        bail!("{name} must use the #rrggbb form")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().expect("valid defaults");
    }

    #[test]
    fn rejects_unknown_backend() {
        let mut config = Config::default();
        config.core.backend = "made-up".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_colors() {
        let mut config = Config::default();
        config.theme.accent = "cyan".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_bubblecolor_options() {
        let mut config = Config::default();
        config.theme.palette_style = "rainbow".into();
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.theme.palette_mode = "sepia".into();
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.theme.contrast_target = 3.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).expect("serialize");
        let decoded: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(config, decoded);
    }

    #[test]
    fn save_and_load_are_atomic_and_lossless() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let mut config = Config::default();
        config.theme.accent = "#38bdf8".into();
        config.save(Some(&path)).expect("save");
        let loaded = Config::load(Some(&path)).expect("load");
        assert_eq!(config, loaded);
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
